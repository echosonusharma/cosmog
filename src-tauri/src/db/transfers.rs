use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::transfer::CompletedPart;

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
    pub parts_json: Option<String>,
    /// JSON-serialized `PutOptions` or `GetOptions` (depending on direction)
    /// captured at enqueue time so a `retry_transfer` can reapply the same
    /// content_type / SSE / ACL / range as the original. Empty when no
    /// options were supplied.
    pub options_json: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
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
    /// JSON-serialized options blob (PutOptions or GetOptions). `None` to use
    /// defaults.
    pub options_json: Option<String>,
}

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
        };
        let r = row.clone();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO transfers (id, account_id, bucket, key, direction, local_path, bytes_total, bytes_done, status, upload_id, parts_json, options_json, error, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                let mut stmt = conn.prepare(
                    "SELECT id, account_id, bucket, key, direction, local_path, bytes_total, bytes_done, status, upload_id, parts_json, options_json, error, created_at, updated_at FROM transfers WHERE id = ?1",
                )?;
                let row = stmt.query_row(params![id], map_row).optional()?;
                Ok::<_, tokio_rusqlite::Error>(row)
            })
            .await?;
        row.ok_or_else(|| AppError::NotFound("transfer".into()))
    }

    pub async fn list_transfers(&self, status: Option<TransferStatus>) -> AppResult<Vec<Transfer>> {
        let status_str = status.map(|s| s.as_str().to_string());
        let rows = self
            .conn
            .call(move |conn| {
                let (sql, has_filter) = match status_str.as_deref() {
                    Some(_) => (
                        "SELECT id, account_id, bucket, key, direction, local_path, bytes_total, bytes_done, status, upload_id, parts_json, options_json, error, created_at, updated_at FROM transfers WHERE status = ?1 ORDER BY created_at DESC",
                        true,
                    ),
                    None => (
                        "SELECT id, account_id, bucket, key, direction, local_path, bytes_total, bytes_done, status, upload_id, parts_json, options_json, error, created_at, updated_at FROM transfers ORDER BY created_at DESC",
                        false,
                    ),
                };
                let mut stmt = conn.prepare(sql)?;
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

    pub async fn update_transfer_multipart(
        &self,
        id: &str,
        upload_id: Option<String>,
        parts: &[CompletedPart],
    ) -> AppResult<()> {
        let id = id.to_string();
        let now = Utc::now().timestamp();
        let parts_json = serde_json::to_string(parts).unwrap_or_else(|_| "[]".to_string());
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

    /// On startup, mark any rows still in `active` or `pending` as `failed`.
    /// They were owned by a worker that died with the previous process and
    /// have no live cancellation token associated with them. Returns the
    /// number of rows touched.
    pub async fn reap_orphan_transfers(&self) -> AppResult<usize> {
        let now = chrono::Utc::now().timestamp();
        let n = self
            .conn
            .call(move |conn| {
                let n = conn.execute(
                    "UPDATE transfers SET status='failed', error=COALESCE(error, 'orphaned at startup'), updated_at=?1
                     WHERE status IN ('active','pending')",
                    params![now],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n)
    }

    pub async fn clear_completed_transfers(&self) -> AppResult<usize> {
        let n = self.conn.call(|conn| {
            let n = conn.execute(
                "DELETE FROM transfers WHERE status IN ('done', 'canceled', 'failed')",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(n)
        }).await?;
        Ok(n)
    }
}
