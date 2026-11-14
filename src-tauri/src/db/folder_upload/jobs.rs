//! Job rows of `folder_upload_jobs`.

use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::db::Db;
use crate::error::{AppError, AppResult};

use super::{JobState, OverwritePolicy, RemoteState, ACTIVE_JOB_STATES, FINISHED_JOB_STATES};

/// Creation input; the job starts in `analyzing`.
#[derive(Debug, Clone)]
pub struct NewJob {
    pub id: String,
    pub account_id: String,
    pub bucket: String,
    /// "path" on desktop, "saf_tree" on Android later.
    pub source_kind: String,
    pub source_root: String,
    /// Already normalized: "" or ends with '/'.
    pub dest_prefix: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobRow {
    pub id: String,
    pub account_id: String,
    pub bucket: String,
    pub source_kind: String,
    pub source_root: String,
    pub dest_prefix: String,
    pub state: JobState,
    pub pause_reason: Option<String>,
    pub overwrite_policy: Option<OverwritePolicy>,
    pub create_dir_markers: bool,
    pub enc_recipient: Option<String>,
    pub analysis_json: Option<String>,
    pub total_files: i64,
    pub total_bytes: i64,
    pub remote_state: RemoteState,
    pub remote_prefix: Option<String>,
    pub remote_existing: i64,
    pub remote_existing_bytes: i64,
    pub remote_versioning: Option<String>,
    /// Bumped by every dest change and check start; only the current check may write.
    pub remote_gen: i64,
    pub error: Option<String>,
    pub created_at: i64,
    pub approved_at: Option<i64>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub updated_at: i64,
}

const JOB_COLS: &str = "id, account_id, bucket, source_kind, source_root, dest_prefix, state, \
    pause_reason, overwrite_policy, create_dir_markers, enc_recipient, analysis_json, total_files, \
    total_bytes, remote_state, remote_prefix, remote_existing, remote_existing_bytes, \
    remote_versioning, error, created_at, approved_at, started_at, finished_at, updated_at, remote_gen";

fn map_job(r: &rusqlite::Row) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: r.get(0)?,
        account_id: r.get(1)?,
        bucket: r.get(2)?,
        source_kind: r.get(3)?,
        source_root: r.get(4)?,
        dest_prefix: r.get(5)?,
        state: r.get(6)?,
        pause_reason: r.get(7)?,
        overwrite_policy: r.get(8)?,
        create_dir_markers: r.get::<_, i64>(9)? != 0,
        enc_recipient: r.get(10)?,
        analysis_json: r.get(11)?,
        total_files: r.get(12)?,
        total_bytes: r.get(13)?,
        remote_state: r.get(14)?,
        remote_prefix: r.get(15)?,
        remote_existing: r.get(16)?,
        remote_existing_bytes: r.get(17)?,
        remote_versioning: r.get(18)?,
        error: r.get(19)?,
        created_at: r.get(20)?,
        approved_at: r.get(21)?,
        started_at: r.get(22)?,
        finished_at: r.get(23)?,
        updated_at: r.get(24)?,
        remote_gen: r.get(25)?,
    })
}

fn not_found() -> AppError {
    AppError::NotFound("folder upload job".into())
}

impl Db {
    pub async fn fu_insert_job(&self, new: NewJob) -> AppResult<()> {
        let now = Utc::now().timestamp();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO folder_upload_jobs (id, account_id, bucket, source_kind, source_root, dest_prefix, state, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                    params![new.id, new.account_id, new.bucket, new.source_kind, new.source_root, new.dest_prefix, JobState::Analyzing, now],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    pub async fn fu_get_job(&self, id: &str) -> AppResult<JobRow> {
        let id = id.to_string();
        let row = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare_cached(&format!("SELECT {JOB_COLS} FROM folder_upload_jobs WHERE id = ?1"))?;
                Ok::<_, tokio_rusqlite::Error>(stmt.query_row(params![id], map_job).optional()?)
            })
            .await?;
        row.ok_or_else(not_found)
    }

    /// All jobs, oldest first (queued jobs are served in this order).
    pub async fn fu_list_jobs(&self) -> AppResult<Vec<JobRow>> {
        let rows = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(&format!("SELECT {JOB_COLS} FROM folder_upload_jobs ORDER BY created_at, rowid"))?;
                let out = stmt.query_map([], map_job)?.collect::<Result<Vec<_>, _>>()?;
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(rows)
    }

    /// An approved, unfinished (queued/running/paused) job for the same source and destination.
    pub async fn fu_find_unfinished(
        &self,
        account_id: &str,
        bucket: &str,
        source_root: &str,
        dest_prefix: &str,
    ) -> AppResult<Option<String>> {
        let args = [account_id, bucket, source_root, dest_prefix].map(str::to_string);
        let id = self
            .conn
            .call(move |conn| {
                let sql = format!(
                    "SELECT id FROM folder_upload_jobs WHERE account_id = ?1 AND bucket = ?2 AND source_root = ?3
                     AND dest_prefix = ?4 AND state IN {ACTIVE_JOB_STATES} ORDER BY created_at LIMIT 1"
                );
                let id = conn.query_row(&sql, params![args[0], args[1], args[2], args[3]], |r| r.get(0)).optional()?;
                Ok::<_, tokio_rusqlite::Error>(id)
            })
            .await?;
        Ok(id)
    }

    /// Sets state, pause_reason and error as given (None clears). Running stamps started_at once;
    /// finished states stamp finished_at, others clear it.
    pub async fn fu_set_state(
        &self,
        id: &str,
        state: JobState,
        pause_reason: Option<String>,
        error: Option<String>,
    ) -> AppResult<()> {
        let id = id.to_string();
        let now = Utc::now().timestamp();
        let running = state == JobState::Running;
        let finished = state.is_finished();
        let n = self
            .conn
            .call(move |conn| {
                let n = conn.execute(
                    "UPDATE folder_upload_jobs SET state = ?2, pause_reason = ?3, error = ?4, updated_at = ?5,
                        started_at = CASE WHEN ?6 THEN COALESCE(started_at, ?5) ELSE started_at END,
                        finished_at = CASE WHEN ?7 THEN ?5 ELSE NULL END
                     WHERE id = ?1",
                    params![id, state, pause_reason, error, now, running, finished],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        if n == 0 {
            return Err(not_found());
        }
        Ok(())
    }

    /// Like `fu_set_state`, but only while the job is in `from`. Returns whether it applied.
    pub async fn fu_set_state_if(
        &self,
        id: &str,
        from: JobState,
        state: JobState,
        pause_reason: Option<String>,
        error: Option<String>,
    ) -> AppResult<bool> {
        let id = id.to_string();
        let now = Utc::now().timestamp();
        let finished = state.is_finished();
        let n = self
            .conn
            .call(move |conn| {
                let n = conn.execute(
                    "UPDATE folder_upload_jobs SET state = ?2, pause_reason = ?3, error = ?4, updated_at = ?5,
                        finished_at = CASE WHEN ?6 THEN ?5 ELSE NULL END
                     WHERE id = ?1 AND state = ?7",
                    params![id, state, pause_reason, error, now, finished, from],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n > 0)
    }

    pub async fn fu_set_analysis(
        &self,
        id: &str,
        analysis_json: String,
        total_files: i64,
        total_bytes: i64,
    ) -> AppResult<()> {
        let id = id.to_string();
        let now = Utc::now().timestamp();
        let n = self
            .conn
            .call(move |conn| {
                let n = conn.execute(
                    "UPDATE folder_upload_jobs SET analysis_json = ?2, total_files = ?3, total_bytes = ?4, updated_at = ?5 WHERE id = ?1",
                    params![id, analysis_json, total_files, total_bytes, now],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        if n == 0 {
            return Err(not_found());
        }
        Ok(())
    }

    /// Stores the approval and moves an `analyzed` job to `next`. InvalidInput if not analyzed.
    pub async fn fu_approve(
        &self,
        id: &str,
        policy: Option<OverwritePolicy>,
        create_dir_markers: bool,
        enc_recipient: Option<String>,
        next: JobState,
    ) -> AppResult<()> {
        let id = id.to_string();
        let id_c = id.clone();
        let now = Utc::now().timestamp();
        let running = next == JobState::Running;
        let n = self
            .conn
            .call(move |conn| {
                let n = conn.execute(
                    "UPDATE folder_upload_jobs SET overwrite_policy = ?2, create_dir_markers = ?3, enc_recipient = ?4,
                        state = ?5, approved_at = ?6, updated_at = ?6,
                        started_at = CASE WHEN ?7 THEN COALESCE(started_at, ?6) ELSE started_at END
                     WHERE id = ?1 AND state = 'analyzed' AND remote_state = 'done' AND remote_prefix = dest_prefix",
                    params![id_c, policy, create_dir_markers, enc_recipient, next, now, running],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        if n == 0 {
            self.fu_get_job(&id).await?;
            return Err(AppError::InvalidInput("folder upload job is not awaiting approval or its destination is not checked".into()));
        }
        Ok(())
    }

    /// Records the recipient a resumed job continues under (encryption confirm or key rotation).
    pub async fn fu_set_enc_recipient(&self, id: &str, recipient: Option<String>) -> AppResult<()> {
        let id = id.to_string();
        let now = Utc::now().timestamp();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "UPDATE folder_upload_jobs SET enc_recipient = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, recipient, now],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Deletes the job and its items. Job transfer rows without an upload_id go too;
    /// rows holding one are left for the engine to abort first.
    pub async fn fu_delete_job(&self, id: &str) -> AppResult<()> {
        let id = id.to_string();
        self.conn
            .call(move |conn| {
                let tx = conn.transaction()?;
                tx.execute("DELETE FROM transfers WHERE job_id = ?1 AND upload_id IS NULL", params![id])?;
                tx.execute("DELETE FROM folder_upload_jobs WHERE id = ?1", params![id])?;
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Startup: an unapproved job from an earlier run is meaningless. Jobs in `keep` (started in
    /// this run) and jobs created after `before` (unix secs) stay. Returns jobs deleted.
    pub async fn fu_startup_cleanup(&self, keep: Vec<String>, before: i64) -> AppResult<usize> {
        let n = self
            .conn
            .call(move |conn| {
                let tx = conn.transaction()?;
                let ids: Vec<String> = tx
                    .prepare("SELECT id FROM folder_upload_jobs WHERE state IN ('analyzing','analyzed') AND created_at <= ?1")?
                    .query_map(params![before], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                let mut n = 0;
                for id in ids.iter().filter(|i| !keep.contains(i)) {
                    n += tx.execute("DELETE FROM folder_upload_jobs WHERE id = ?1", params![id])?;
                }
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n)
    }

    /// Deletes finished jobs beyond the newest `keep` or finished before `older_than` (unix secs).
    /// `keep = 0` clears every finished job. Returns jobs deleted.
    pub async fn fu_prune_finished(&self, keep: i64, older_than: i64) -> AppResult<usize> {
        let n = self
            .conn
            .call(move |conn| {
                let tx = conn.transaction()?;
                let sel = format!(
                    "SELECT id FROM folder_upload_jobs WHERE state IN {FINISHED_JOB_STATES}
                     AND (COALESCE(finished_at, updated_at) < ?2 OR id NOT IN (
                        SELECT id FROM folder_upload_jobs WHERE state IN {FINISHED_JOB_STATES}
                        ORDER BY COALESCE(finished_at, updated_at) DESC, rowid DESC LIMIT ?1))"
                );
                let ids: Vec<String> = tx
                    .prepare(&sel)?
                    .query_map(params![keep, older_than], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                for id in &ids {
                    tx.execute("DELETE FROM transfers WHERE job_id = ?1 AND upload_id IS NULL", params![id])?;
                    tx.execute("DELETE FROM folder_upload_jobs WHERE id = ?1", params![id])?;
                }
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(ids.len())
            })
            .await?;
        Ok(n)
    }
}
