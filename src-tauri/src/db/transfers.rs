use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::transfer::ResumeState;

use super::Db;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Upload,
    Download,
}

impl Direction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::Upload => "upload",
            Direction::Download => "download",
        }
    }

    pub fn parse(s: &str) -> AppResult<Self> {
        match s {
            "upload" => Ok(Direction::Upload),
            "download" => Ok(Direction::Download),
            other => Err(AppError::InvalidInput(format!("direction: {other}"))),
        }
    }
}

/// Who started a transfer. `NightWatch` = silent background sync; the UI
/// suppresses its notifications (only failures surface).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TransferOrigin {
    #[default]
    User,
    #[serde(rename = "nightwatch")]
    NightWatch,
}

impl TransferOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransferOrigin::User => "user",
            TransferOrigin::NightWatch => "nightwatch",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "nightwatch" => TransferOrigin::NightWatch,
            _ => TransferOrigin::User,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TransferStatus {
    Pending,
    Active,
    Paused,
    Done,
    Failed,
    Canceled,
}

impl TransferStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransferStatus::Pending => "pending",
            TransferStatus::Active => "active",
            TransferStatus::Paused => "paused",
            TransferStatus::Done => "done",
            TransferStatus::Failed => "failed",
            TransferStatus::Canceled => "canceled",
        }
    }

    pub fn parse(s: &str) -> AppResult<Self> {
        match s {
            "pending" => Ok(Self::Pending),
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            other => Err(AppError::InvalidInput(format!("status: {other}"))),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Transfer {
    pub id: String,
    pub account_id: String,
    pub bucket: String,
    pub key: String,
    pub direction: Direction,
    pub local_path: String,
    pub bytes_total: Option<i64>,
    pub bytes_done: i64,
    pub status: TransferStatus,
    pub upload_id: Option<String>,
    #[serde(skip_serializing)]
    pub parts_json: Option<String>,
    /// PutOptions/GetOptions captured at enqueue time so `retry_transfer`
    /// reapplies the original content_type / SSE / ACL / range. Empty if none.
    pub options_json: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub origin: TransferOrigin,
    /// Owning folder upload job; such rows are hidden from the normal list.
    pub job_id: Option<String>,
}

/// Error stored on rows reaped at startup; wire JSON so the FE can translate it.
fn reaped_error(detail: &str) -> String {
    AppError::Interrupted(detail.into()).to_wire_string()
}

fn map_row(row: &rusqlite::Row) -> rusqlite::Result<Transfer> {
    let direction_raw: String = row.get(4)?;
    let status_raw: String = row.get(8)?;
    let direction = match Direction::parse(&direction_raw) {
        Ok(d) => d,
        Err(_) => {
            tracing::warn!("transfers.direction has unexpected value {direction_raw:?}; coercing to Upload");
            Direction::Upload
        }
    };
    let status = match TransferStatus::parse(&status_raw) {
        Ok(s) => s,
        Err(_) => {
            tracing::warn!("transfers.status has unexpected value {status_raw:?}; coercing to Pending");
            TransferStatus::Pending
        }
    };
    Ok(Transfer {
        id: row.get(0)?,
        account_id: row.get(1)?,
        bucket: row.get(2)?,
        key: row.get(3)?,
        direction,
        local_path: row.get(5)?,
        bytes_total: row.get(6)?,
        bytes_done: row.get(7)?,
        status,
        upload_id: row.get(9)?,
        parts_json: row.get(10)?,
        options_json: row.get(11)?,
        error: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        origin: {
            let raw: String = row.get(15)?;
            TransferOrigin::parse(&raw)
        },
        job_id: row.get(16)?,
    })
}

#[derive(Debug, Clone)]
pub struct NewTransfer {
    pub id: String,
    pub account_id: String,
    pub bucket: String,
    pub key: String,
    pub direction: Direction,
    pub local_path: String,
    pub options_json: Option<String>,
    pub origin: TransferOrigin,
    pub job_id: Option<String>,
}

// Column order expected by map_row; parts_json is index 10.
const COLS_HEAD: &str = "id, account_id, bucket, key, direction, local_path, bytes_total, bytes_done, status, upload_id";
const COLS_TAIL: &str = "options_json, error, created_at, updated_at, origin, job_id";

impl Db {
    pub async fn insert_transfer(&self, new: NewTransfer) -> AppResult<Transfer> {
        let now = Utc::now().timestamp();
        let row = Transfer {
            id: new.id.clone(),
            account_id: new.account_id,
            bucket: new.bucket,
            key: new.key,
            direction: new.direction,
            local_path: new.local_path,
            bytes_total: None,
            bytes_done: 0,
            status: TransferStatus::Pending,
            upload_id: None,
            parts_json: None,
            options_json: new.options_json,
            error: None,
            created_at: now,
            updated_at: now,
            origin: new.origin,
            job_id: new.job_id,
        };
        let r = row.clone();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO transfers (id, account_id, bucket, key, direction, local_path, bytes_total, bytes_done, status, upload_id, parts_json, options_json, error, created_at, updated_at, origin, job_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                    params![
                        r.id,
                        r.account_id,
                        r.bucket,
                        r.key,
                        r.direction.as_str(),
                        r.local_path,
                        r.bytes_total,
                        r.bytes_done,
                        r.status.as_str(),
                        r.upload_id,
                        r.parts_json,
                        r.options_json,
                        r.error,
                        r.created_at,
                        r.updated_at,
                        r.origin.as_str(),
                        r.job_id,
                    ],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(row)
    }

    pub async fn get_transfer(&self, id: &str) -> AppResult<Transfer> {
        let id = id.to_string();
        let row = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {COLS_HEAD}, parts_json, {COLS_TAIL} FROM transfers WHERE id = ?1"
                ))?;
                let row = stmt.query_row(params![id], map_row).optional()?;
                Ok::<_, tokio_rusqlite::Error>(row)
            })
            .await?;
        row.ok_or_else(|| AppError::NotFound("transfer".into()))
    }

    /// UI listing; `parts_json` is left out (NULL) since only resume needs it. Job rows are hidden.
    pub async fn list_transfers(&self, status: Option<TransferStatus>) -> AppResult<Vec<Transfer>> {
        let status_str = status.map(|s| s.as_str().to_string());
        let rows = self
            .conn
            .call(move |conn| {
                let has_filter = status_str.is_some();
                let filter = if has_filter { "AND status = ?1" } else { "" };
                let sql = format!(
                    "SELECT {COLS_HEAD}, NULL, {COLS_TAIL} FROM transfers WHERE job_id IS NULL {filter} ORDER BY created_at DESC"
                );
                let mut stmt = conn.prepare(&sql)?;
                let mut out = Vec::new();
                if has_filter {
                    let iter = stmt.query_map(params![status_str.unwrap()], map_row)?;
                    for r in iter { out.push(r?); }
                } else {
                    let iter = stmt.query_map([], map_row)?;
                    for r in iter { out.push(r?); }
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(rows)
    }

    /// Return transfer IDs that are active or pending for a given account.
    /// Used by `TransferManager::cancel_for_account` to avoid two full-list queries.
    pub async fn list_cancellable_ids_for_account(&self, account_id: &str) -> AppResult<Vec<String>> {
        let account_id = account_id.to_string();
        let ids = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id FROM transfers WHERE account_id = ?1 AND status IN ('active', 'pending')",
                )?;
                let out: Vec<String> = stmt
                    .query_map(params![account_id], |row| row.get(0))?
                    .collect::<Result<_, _>>()?;
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(ids)
    }

    pub async fn list_cancellable_ids_for_bucket(
        &self,
        account_id: &str,
        bucket: &str,
    ) -> AppResult<Vec<String>> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let ids = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id FROM transfers WHERE account_id = ?1 AND bucket = ?2 AND status IN ('active', 'pending')",
                )?;
                let out: Vec<String> = stmt
                    .query_map(params![account_id, bucket], |row| row.get(0))?
                    .collect::<Result<_, _>>()?;
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(ids)
    }

    pub async fn update_transfer_status(
        &self,
        id: &str,
        status: TransferStatus,
        error: Option<String>,
    ) -> AppResult<()> {
        let id = id.to_string();
        let now = Utc::now().timestamp();
        let status_s = status.as_str().to_string();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "UPDATE transfers SET status = ?1, error = ?2, updated_at = ?3 WHERE id = ?4",
                    params![status_s, error, now, id],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    pub async fn update_transfer_bytes(&self, id: &str, bytes_done: i64, bytes_total: Option<i64>) -> AppResult<()> {
        let id = id.to_string();
        let now = Utc::now().timestamp();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "UPDATE transfers SET bytes_done = ?1, bytes_total = COALESCE(?2, bytes_total), updated_at = ?3 WHERE id = ?4",
                    params![bytes_done, bytes_total, now, id],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Persists the whole resume state (parts + source fingerprint + part size) in parts_json.
    pub async fn update_transfer_multipart(
        &self,
        id: &str,
        upload_id: Option<String>,
        state: &ResumeState,
    ) -> AppResult<()> {
        let id = id.to_string();
        let now = Utc::now().timestamp();
        let parts_json = serde_json::to_string(state).unwrap_or_else(|_| "{}".to_string());
        self.conn
            .call(move |conn| {
                conn.execute(
                    "UPDATE transfers SET upload_id = COALESCE(?1, upload_id), parts_json = ?2, updated_at = ?3 WHERE id = ?4",
                    params![upload_id, parts_json, now, id],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    pub async fn clear_transfer_multipart(&self, id: &str) -> AppResult<()> {
        let id = id.to_string();
        let now = Utc::now().timestamp();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "UPDATE transfers SET upload_id = NULL, parts_json = NULL, updated_at = ?1 WHERE id = ?2",
                    params![now, id],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Finished non-job rows still holding a multipart upload (crash-reaped); aborted before clearing.
    pub async fn list_finished_with_upload(&self) -> AppResult<Vec<Transfer>> {
        let rows = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {COLS_HEAD}, NULL, {COLS_TAIL} FROM transfers
                     WHERE status IN ('done', 'canceled', 'failed') AND upload_id IS NOT NULL AND job_id IS NULL"
                ))?;
                let out = stmt
                    .query_map([], map_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(rows)
    }

    /// Keep only the newest `keep` finished non-job rows. Rows holding an upload_id survive so
    /// their multipart upload can still be aborted on clear. Job rows are never pruned here.
    pub async fn prune_finished_transfers(&self, keep: i64) -> AppResult<usize> {
        let n = self
            .conn
            .call(move |conn| {
                let n = conn.execute(
                    "DELETE FROM transfers WHERE status IN ('done', 'canceled', 'failed') AND upload_id IS NULL
                     AND job_id IS NULL
                     AND id NOT IN (
                        SELECT id FROM transfers WHERE status IN ('done', 'canceled', 'failed') AND job_id IS NULL
                        ORDER BY updated_at DESC LIMIT ?1
                     )",
                    params![keep],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n)
    }

    pub async fn delete_transfer(&self, id: &str) -> AppResult<()> {
        let id = id.to_string();
        let n = self.conn.call(move |conn| {
            let n = conn.execute("DELETE FROM transfers WHERE id = ?1", params![id])?;
            Ok::<_, tokio_rusqlite::Error>(n)
        }).await?;
        if n == 0 {
            return Err(AppError::NotFound("transfer".into()));
        }
        Ok(())
    }

    /// Startup recovery: rows still active/pending were owned by a worker that
    /// died with the previous process; mark them failed (job rows too). Returns rows touched.
    pub async fn reap_orphan_transfers(&self) -> AppResult<usize> {
        let now = chrono::Utc::now().timestamp();
        let n = self
            .conn
            .call(move |conn| {
                let n = conn.execute(
                    "UPDATE transfers SET status='failed', error=COALESCE(error, ?2), updated_at=?1
                     WHERE status IN ('active','pending')",
                    params![now, reaped_error("orphaned at startup")],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n)
    }

    /// Origin-scoped orphan reap: on Android the main process owns `user`
    /// transfers and must not touch live `nightwatch` rows owned by the service.
    pub async fn reap_orphan_transfers_by_origin(&self, origin: &str) -> AppResult<usize> {
        let now = chrono::Utc::now().timestamp();
        let origin = origin.to_string();
        let n = self
            .conn
            .call(move |conn| {
                let n = conn.execute(
                    "UPDATE transfers SET status='failed', error=COALESCE(error, ?3), updated_at=?1
                     WHERE status IN ('active','pending') AND origin=?2",
                    params![now, origin, reaped_error("app closed")],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n)
    }

    /// Cancel an active/pending row whose worker died so the UI can dismiss
    /// the ghost. Returns true if a row was updated.
    pub async fn mark_canceled_if_active(&self, id: &str) -> AppResult<bool> {
        let now = chrono::Utc::now().timestamp();
        let id = id.to_string();
        let n = self
            .conn
            .call(move |conn| {
                let n = conn.execute(
                    "UPDATE transfers SET status='canceled', updated_at=?1
                     WHERE id=?2 AND status IN ('active','pending')",
                    params![now, id],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n > 0)
    }

    /// Clears finished non-job rows; folder job rows are owned by their job.
    pub async fn clear_completed_transfers(&self) -> AppResult<usize> {
        let n = self.conn.call(|conn| {
            let n = conn.execute(
                "DELETE FROM transfers WHERE status IN ('done', 'canceled', 'failed') AND job_id IS NULL",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(n)
        }).await?;
        Ok(n)
    }

    /// Every row owned by a folder job, with resume state, oldest first.
    pub async fn list_job_transfers(&self, job_id: &str) -> AppResult<Vec<Transfer>> {
        let job_id = job_id.to_string();
        let rows = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {COLS_HEAD}, parts_json, {COLS_TAIL} FROM transfers WHERE job_id = ?1 ORDER BY created_at, id"
                ))?;
                let out = stmt
                    .query_map(params![job_id], map_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(rows)
    }

    /// Rows whose job_id names no existing folder job (startup sweep).
    pub async fn list_orphan_job_transfers(&self) -> AppResult<Vec<Transfer>> {
        let rows = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {COLS_HEAD}, parts_json, {COLS_TAIL} FROM transfers
                     WHERE job_id IS NOT NULL AND job_id NOT IN (SELECT id FROM folder_upload_jobs)"
                ))?;
                let out = stmt.query_map([], map_row)?.collect::<Result<Vec<_>, _>>()?;
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(rows)
    }

    /// Deletes the given rows in one transaction; missing ids are ignored. Returns rows deleted.
    pub async fn delete_transfer_rows(&self, ids: Vec<String>) -> AppResult<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let n = self
            .conn
            .call(move |conn| {
                let tx = conn.transaction()?;
                let mut n = 0;
                {
                    let mut stmt = tx.prepare("DELETE FROM transfers WHERE id = ?1")?;
                    for id in &ids {
                        n += stmt.execute(params![id])?;
                    }
                }
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n)
    }
}
