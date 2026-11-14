//! Item rows of `folder_upload_items`: the approved manifest and per-item progress.

use chrono::Utc;
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, OptionalExtension};
use serde::Serialize;

use crate::db::Db;
use crate::error::{AppError, AppResult};

use super::{Drift, ExcludeReason, ItemFilter, ItemKind, ItemState};

/// S3 key limit in UTF-8 bytes for `dest_prefix || rel_path` (plus '/' for dir markers).
pub const MAX_KEY_BYTES: i64 = 1024;

#[derive(Debug, Clone)]
pub struct NewItem {
    pub seq: i64,
    pub kind: ItemKind,
    /// '/'-joined; dirs without a trailing '/'.
    pub rel_path: String,
    pub source_ref: Option<String>,
    pub size: i64,
    pub mtime_ms: i64,
    pub ext: Option<String>,
    pub hidden: bool,
    pub link_target: Option<String>,
    pub state: ItemState,
    pub exclude_reason: Option<ExcludeReason>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemRow {
    pub job_id: String,
    pub seq: i64,
    pub kind: ItemKind,
    pub rel_path: String,
    pub source_ref: Option<String>,
    pub size: i64,
    pub mtime_ms: i64,
    pub ext: Option<String>,
    pub hidden: bool,
    pub link_target: Option<String>,
    pub state: ItemState,
    pub exclude_reason: Option<ExcludeReason>,
    pub remote_exists: bool,
    pub remote_size: Option<i64>,
    pub remote_mtime: Option<i64>,
    pub attempts: i64,
    pub next_attempt_at: i64,
    pub transfer_id: Option<String>,
    pub drift: Option<Drift>,
    pub error: Option<String>,
    pub updated_at: i64,
    /// ETag of the object this job wrote for the item.
    pub done_etag: Option<String>,
}

/// Per-state counts over all item kinds, plus derived figures.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ItemCounts {
    pub pending: i64,
    pub uploading: i64,
    pub done: i64,
    pub skipped_exists: i64,
    pub skipped_missing: i64,
    pub failed: i64,
    pub canceled: i64,
    pub excluded: i64,
    /// Done items of kind file only.
    pub done_files: i64,
    pub drift: i64,
    /// skipped_exists items the remote check did not see.
    pub appeared_after_check: i64,
    /// Became un-uploadable after approval (an error, unlike walk-time exclusions).
    pub excluded_late: i64,
    pub key_too_long: i64,
    /// Earliest next_attempt_at among pending items.
    pub next_due_at: Option<i64>,
}

const ITEM_COLS: &str = "job_id, seq, kind, rel_path, source_ref, size, mtime_ms, ext, hidden, \
    link_target, state, exclude_reason, remote_exists, remote_size, remote_mtime, attempts, \
    next_attempt_at, transfer_id, drift, error, updated_at, done_etag";

// UTF-8 byte length of the object key; dir markers get a trailing '/'.
const KEY_BYTES_SQL: &str = "length(CAST(?2 || rel_path || \
    CASE WHEN kind = 'dir' AND substr(rel_path, -1) != '/' THEN '/' ELSE '' END AS BLOB))";

fn map_item(r: &rusqlite::Row) -> rusqlite::Result<ItemRow> {
    Ok(ItemRow {
        job_id: r.get(0)?,
        seq: r.get(1)?,
        kind: r.get(2)?,
        rel_path: r.get(3)?,
        source_ref: r.get(4)?,
        size: r.get(5)?,
        mtime_ms: r.get(6)?,
        ext: r.get(7)?,
        hidden: r.get::<_, i64>(8)? != 0,
        link_target: r.get(9)?,
        state: r.get(10)?,
        exclude_reason: r.get(11)?,
        remote_exists: r.get::<_, i64>(12)? != 0,
        remote_size: r.get(13)?,
        remote_mtime: r.get(14)?,
        attempts: r.get(15)?,
        next_attempt_at: r.get(16)?,
        transfer_id: r.get(17)?,
        drift: r.get(18)?,
        error: r.get(19)?,
        updated_at: r.get(20)?,
        done_etag: r.get(21)?,
    })
}

fn filter_sql(f: ItemFilter) -> &'static str {
    match f {
        ItemFilter::All => "",
        ItemFilter::Existing => "AND remote_exists = 1 AND state != 'excluded'",
        ItemFilter::Excluded => "AND state = 'excluded'",
        ItemFilter::Failed => "AND state = 'failed'",
        ItemFilter::Skipped => "AND state IN ('skipped_exists','skipped_missing')",
        ItemFilter::Drift => "AND drift IS NOT NULL",
        ItemFilter::Hidden => "AND hidden = 1",
    }
}

fn opt_text(s: Option<&str>) -> Value {
    s.map_or(Value::Null, |s| Value::Text(s.to_string()))
}

impl Db {
    /// Inserts a batch in one transaction; any failure (e.g. duplicate rel_path) rolls back all of it.
    pub async fn fu_insert_items(&self, job_id: &str, items: Vec<NewItem>) -> AppResult<()> {
        let job_id = job_id.to_string();
        let now = Utc::now().timestamp();
        self.conn
            .call(move |conn| {
                let tx = conn.transaction()?;
                {
                    let mut stmt = tx.prepare_cached(
                        "INSERT INTO folder_upload_items (job_id, seq, kind, rel_path, source_ref, size, mtime_ms, ext, hidden, link_target, state, exclude_reason, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    )?;
                    for it in &items {
                        stmt.execute(params![
                            job_id, it.seq, it.kind, it.rel_path, it.source_ref, it.size, it.mtime_ms,
                            it.ext, it.hidden, it.link_target, it.state, it.exclude_reason, now
                        ])?;
                    }
                }
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// New destination (only before approval): resets and invalidates any remote check,
    /// re-evaluates key_too_long. Returns the key_too_long count.
    pub async fn fu_set_dest(&self, job_id: &str, dest_prefix: &str) -> AppResult<i64> {
        let job_id = job_id.to_string();
        let dest = dest_prefix.to_string();
        let now = Utc::now().timestamp();
        let n = self
            .conn
            .call(move |conn| {
                let tx = conn.transaction()?;
                let found = tx.execute(
                    "UPDATE folder_upload_jobs SET dest_prefix = ?2, remote_state = 'pending', remote_prefix = NULL,
                        remote_existing = 0, remote_existing_bytes = 0, remote_versioning = NULL,
                        remote_gen = remote_gen + 1, updated_at = ?3
                     WHERE id = ?1 AND state IN ('analyzing','analyzed')",
                    params![job_id, dest, now],
                )?;
                if found == 0 {
                    let exists = tx.query_row("SELECT 1 FROM folder_upload_jobs WHERE id = ?1", params![job_id], |_| Ok(())).optional()?;
                    return Ok(Some(exists.map(|_| -1)));
                }
                tx.execute(
                    "UPDATE folder_upload_items SET remote_exists = 0, remote_size = NULL, remote_mtime = NULL
                     WHERE job_id = ?1 AND (remote_exists != 0 OR remote_size IS NOT NULL OR remote_mtime IS NOT NULL)",
                    params![job_id],
                )?;
                tx.execute(
                    &format!(
                        "UPDATE folder_upload_items SET state = 'excluded', exclude_reason = 'key_too_long', updated_at = ?3
                         WHERE job_id = ?1 AND state = 'pending' AND {KEY_BYTES_SQL} > ?4"
                    ),
                    params![job_id, dest, now, MAX_KEY_BYTES],
                )?;
                // OR IGNORE: a row that would collide with a live duplicate stays excluded.
                tx.execute(
                    &format!(
                        "UPDATE OR IGNORE folder_upload_items SET state = 'pending', exclude_reason = NULL, updated_at = ?3
                         WHERE job_id = ?1 AND state = 'excluded' AND exclude_reason = 'key_too_long' AND {KEY_BYTES_SQL} <= ?4"
                    ),
                    params![job_id, dest, now, MAX_KEY_BYTES],
                )?;
                let n: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM folder_upload_items WHERE job_id = ?1 AND state = 'excluded' AND exclude_reason = 'key_too_long'",
                    params![job_id],
                    |r| r.get(0),
                )?;
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(Some(Some(n)))
            })
            .await?;
        match n {
            Some(Some(n)) if n >= 0 => Ok(n),
            Some(Some(_)) => Err(AppError::InvalidInput("job is not awaiting approval".into())),
            _ => Err(AppError::NotFound("folder upload job".into())),
        }
    }

    /// Marks remote hits (rel_path, size, mtime_ms) in one transaction, only while check `gen`
    /// is the job's current one. Returns items updated; Err(Canceled) once superseded.
    pub async fn fu_mark_remote(
        &self,
        job_id: &str,
        gen: i64,
        hits: Vec<(String, i64, Option<i64>)>,
    ) -> AppResult<usize> {
        let job_id = job_id.to_string();
        let n = self
            .conn
            .call(move |conn| {
                let tx = conn.transaction()?;
                let current: Option<i64> = tx
                    .query_row(
                        "SELECT remote_gen FROM folder_upload_jobs WHERE id = ?1 AND state = 'analyzed'",
                        params![job_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                let mut n = None;
                if current == Some(gen) {
                    // `state != 'excluded'` lets SQLite use the partial unique index.
                    let mut stmt = tx.prepare_cached(
                        "UPDATE folder_upload_items SET remote_exists = 1, remote_size = ?3, remote_mtime = ?4
                         WHERE job_id = ?1 AND rel_path = ?2 AND state != 'excluded'",
                    )?;
                    let mut c = 0;
                    for (rel, size, mtime) in &hits {
                        c += stmt.execute(params![job_id, rel, size, mtime])?;
                    }
                    n = Some(c);
                }
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        n.ok_or_else(|| AppError::Canceled("remote check superseded".into()))
    }

    /// Pending file and dir items due at `now`, in walk order.
    pub async fn fu_next_batch(&self, job_id: &str, limit: i64, now: i64) -> AppResult<Vec<ItemRow>> {
        self.fu_select_items(
            job_id,
            // `+` keeps next_attempt_at out of index choice: walk order comes from idx_fui_order.
            "AND state = 'pending' AND kind IN ('file','dir') AND +next_attempt_at <= ?2 ORDER BY seq LIMIT ?3",
            vec![Value::Integer(now), Value::Integer(limit)],
        )
        .await
    }

    pub async fn fu_get_item(&self, job_id: &str, seq: i64) -> AppResult<ItemRow> {
        self.fu_select_items(job_id, "AND seq = ?2", vec![Value::Integer(seq)])
            .await?
            .pop()
            .ok_or_else(|| AppError::NotFound("folder upload item".into()))
    }

    /// Pending items still pointing at a kept (parked) transfer row.
    pub async fn fu_select_pointing(&self, job_id: &str) -> AppResult<Vec<ItemRow>> {
        self.fu_select_items(job_id, "AND state = 'pending' AND transfer_id IS NOT NULL", vec![]).await
    }

    /// Items currently `uploading`, in walk order.
    pub async fn fu_inflight(&self, job_id: &str) -> AppResult<Vec<ItemRow>> {
        self.fu_select_items(job_id, "AND state = 'uploading' ORDER BY seq", vec![]).await
    }

    pub async fn fu_item_started(&self, job_id: &str, seq: i64, transfer_id: &str) -> AppResult<()> {
        self.fu_update_item(
            job_id,
            seq,
            "state = 'uploading', transfer_id = ?4",
            vec![Value::Text(transfer_id.to_string())],
        )
        .await
    }

    /// Terminal (or excluded) outcome; clears transfer_id. `drift = None` keeps the stored drift.
    pub async fn fu_item_finished(
        &self,
        job_id: &str,
        seq: i64,
        state: ItemState,
        error: Option<String>,
        drift: Option<Drift>,
    ) -> AppResult<()> {
        self.fu_update_item(
            job_id,
            seq,
            "state = ?4, error = ?5, drift = COALESCE(?6, drift), transfer_id = NULL",
            vec![
                Value::Text(state.as_str().into()),
                opt_text(error.as_deref()),
                opt_text(drift.as_ref().map(Drift::as_str)),
            ],
        )
        .await
    }

    /// Became un-uploadable after approval: `excluded` with `reason`, drift `changed` (an error).
    pub async fn fu_item_exclude(&self, job_id: &str, seq: i64, reason: ExcludeReason) -> AppResult<()> {
        self.fu_update_item(
            job_id,
            seq,
            "state = 'excluded', exclude_reason = ?4, transfer_id = NULL, drift = COALESCE(drift, 'changed')",
            vec![Value::Text(reason.as_str().into())],
        )
        .await
    }

    /// Uploaded by this job: done, with the ETag it got.
    pub async fn fu_item_done(&self, job_id: &str, seq: i64, etag: Option<String>) -> AppResult<()> {
        self.fu_update_item(
            job_id,
            seq,
            "state = 'done', error = NULL, transfer_id = NULL, done_etag = COALESCE(?4, done_etag)",
            vec![opt_text(etag.as_deref())],
        )
        .await
    }

    /// Network loss: back to `pending` but still pointing at its paused row (kept multipart upload).
    pub async fn fu_item_park(&self, job_id: &str, seq: i64, transfer_id: &str, error: String, count_attempt: bool) -> AppResult<()> {
        self.fu_update_item(
            job_id,
            seq,
            "state = 'pending', transfer_id = ?4, next_attempt_at = 0, error = ?5, attempts = attempts + ?6",
            vec![Value::Text(transfer_id.into()), Value::Text(error), Value::Integer(count_attempt as i64)],
        )
        .await
    }

    /// Stores the stat seen at enqueue time and marks the drift kind.
    pub async fn fu_item_drift(&self, job_id: &str, seq: i64, size: i64, mtime_ms: i64, drift: Drift) -> AppResult<()> {
        self.fu_update_item(
            job_id,
            seq,
            "size = ?4, mtime_ms = ?5, drift = ?6",
            vec![Value::Integer(size), Value::Integer(mtime_ms), Value::Text(drift.as_str().into())],
        )
        .await
    }

    /// Back to `pending` until `next_attempt_at`; `count_attempt` bumps attempts.
    pub async fn fu_item_retry_later(
        &self,
        job_id: &str,
        seq: i64,
        next_attempt_at: i64,
        error: Option<String>,
        count_attempt: bool,
    ) -> AppResult<()> {
        self.fu_update_item(
            job_id,
            seq,
            "state = 'pending', transfer_id = NULL, next_attempt_at = ?4, error = ?5, attempts = attempts + ?6",
            vec![
                Value::Integer(next_attempt_at),
                opt_text(error.as_deref()),
                Value::Integer(count_attempt as i64),
            ],
        )
        .await
    }

    /// One page in walk order, plus the total matching `filter`.
    pub async fn fu_list_items(
        &self,
        job_id: &str,
        filter: ItemFilter,
        offset: i64,
        limit: i64,
    ) -> AppResult<(Vec<ItemRow>, i64)> {
        let where_ = filter_sql(filter);
        let job = job_id.to_string();
        let total = self
            .conn
            .call(move |conn| {
                let sql = format!("SELECT COUNT(*) FROM folder_upload_items WHERE job_id = ?1 {where_}");
                let n: i64 = conn.query_row(&sql, params![job], |r| r.get(0))?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        let rows = self
            .fu_select_items(
                job_id,
                &format!("{where_} ORDER BY seq LIMIT ?2 OFFSET ?3"),
                vec![Value::Integer(limit), Value::Integer(offset.max(0))],
            )
            .await?;
        Ok((rows, total))
    }

    /// (count, bytes) of pending file items: what an approval will upload.
    pub async fn fu_pending_totals(&self, job_id: &str) -> AppResult<(i64, i64)> {
        self.fu_pair(job_id, "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM folder_upload_items
            WHERE job_id = ?1 AND state = 'pending' AND kind = 'file'").await
    }

    /// (count, remote bytes) of non-excluded items the remote check found.
    pub async fn fu_existing_totals(&self, job_id: &str) -> AppResult<(i64, i64)> {
        self.fu_pair(job_id, "SELECT COUNT(*), COALESCE(SUM(remote_size), 0) FROM folder_upload_items
            WHERE job_id = ?1 AND remote_exists = 1 AND state != 'excluded'").await
    }

    /// Distinct first path components of nested items (the subtrees the remote check lists).
    pub async fn fu_top_dirs(&self, job_id: &str) -> AppResult<Vec<String>> {
        let job_id = job_id.to_string();
        let rows = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT substr(rel_path, 1, instr(rel_path, '/') - 1) FROM folder_upload_items
                     WHERE job_id = ?1 AND instr(rel_path, '/') > 0 AND state != 'excluded'",
                )?;
                let out = stmt.query_map(params![job_id], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(rows)
    }

    async fn fu_pair(&self, job_id: &str, sql: &'static str) -> AppResult<(i64, i64)> {
        let job_id = job_id.to_string();
        let r = self
            .conn
            .call(move |conn| Ok::<_, tokio_rusqlite::Error>(conn.query_row(sql, params![job_id], |r| Ok((r.get(0)?, r.get(1)?)))?))
            .await?;
        Ok(r)
    }

    /// failed -> pending with a fresh attempt budget. Returns items reset.
    pub async fn fu_reset_failed(&self, job_id: &str) -> AppResult<i64> {
        self.fu_bulk_update(
            job_id,
            "state = 'pending', attempts = 0, next_attempt_at = 0, error = NULL, transfer_id = NULL WHERE job_id = ?1 AND state = 'failed'",
        )
        .await
    }

    /// pending -> canceled. Returns items canceled.
    pub async fn fu_cancel_pending(&self, job_id: &str) -> AppResult<i64> {
        self.fu_bulk_update(job_id, "state = 'canceled' WHERE job_id = ?1 AND state = 'pending'").await
    }

    async fn fu_bulk_update(&self, job_id: &str, set_where: &'static str) -> AppResult<i64> {
        let job_id = job_id.to_string();
        let now = Utc::now().timestamp();
        let n = self
            .conn
            .call(move |conn| {
                let sql = format!("UPDATE folder_upload_items SET updated_at = ?2, {set_where}");
                Ok::<_, tokio_rusqlite::Error>(conn.execute(&sql, params![job_id, now])?)
            })
            .await?;
        Ok(n as i64)
    }

    // `set` uses ?4.. for `extra`; ?1 = updated_at, ?2 = job_id, ?3 = seq.
    async fn fu_update_item(&self, job_id: &str, seq: i64, set: &'static str, extra: Vec<Value>) -> AppResult<()> {
        let now = Utc::now().timestamp();
        let mut args = vec![Value::Integer(now), Value::Text(job_id.to_string()), Value::Integer(seq)];
        args.extend(extra);
        let n = self
            .conn
            .call(move |conn| {
                let sql = format!("UPDATE folder_upload_items SET {set}, updated_at = ?1 WHERE job_id = ?2 AND seq = ?3");
                let mut stmt = conn.prepare_cached(&sql)?;
                Ok::<_, tokio_rusqlite::Error>(stmt.execute(params_from_iter(args))?)
            })
            .await?;
        if n == 0 {
            return Err(AppError::NotFound("folder upload item".into()));
        }
        Ok(())
    }

    // `tail` follows "WHERE job_id = ?1"; `extra` binds ?2...
    async fn fu_select_items(&self, job_id: &str, tail: &str, extra: Vec<Value>) -> AppResult<Vec<ItemRow>> {
        let sql = format!("SELECT {ITEM_COLS} FROM folder_upload_items WHERE job_id = ?1 {tail}");
        let mut args = vec![Value::Text(job_id.to_string())];
        args.extend(extra);
        let rows = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare_cached(&sql)?;
                let out = stmt.query_map(params_from_iter(args), map_item)?.collect::<Result<Vec<_>, _>>()?;
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(rows)
    }
}
