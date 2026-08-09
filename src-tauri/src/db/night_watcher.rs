//! Persistence for Night Watcher: watch definitions (`nw_watch`) and the
//! per-file sync state (`nw_file_state`) used to detect changes cheaply.
//!
//! `nw_file_state` is kept fully separate from the search cache
//! (`cached_objects` + its mark/sweep FTS triggers) on purpose. Sharing those
//! tables would let a Night Watcher reconcile corrupt the search index's
//! seen=0 markers, and vice versa.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::AppResult;

use super::Db;

/// A single watched local directory synced one-way to an S3 prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NightWatch {
    pub id: String,
    pub account_id: String,
    pub bucket: String,
    pub local_dir: String,
    pub key_prefix: String,
    pub ignore_file: Option<String>,
    /// Only `"keep"` for the MVP: remote objects are never deleted.
    pub delete_policy: String,
    pub full_scan_secs: i64,
    pub enabled: bool,
    pub last_scan_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    /// Android: the SAF tree `content://` URI to sync. NULL/`None` on desktop,
    /// where `local_dir` is a real filesystem path instead.
    pub tree_uri: Option<String>,
}

/// Fields a caller supplies to create a watch. `id` and `created_at` are
/// assigned by [`Db::insert_watch`].
#[derive(Debug, Clone, Deserialize)]
pub struct NewWatch {
    pub account_id: String,
    pub bucket: String,
    pub local_dir: String,
    #[serde(default)]
    pub key_prefix: String,
    pub ignore_file: Option<String>,
    pub full_scan_secs: i64,
    /// SAF tree URI on Android; `None` on desktop.
    #[serde(default)]
    pub tree_uri: Option<String>,
}

/// Optional per-field patch for [`Db::update_watch`]. `None` leaves a field
/// untouched.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WatchPatch {
    pub key_prefix: Option<String>,
    pub ignore_file: Option<String>,
    pub full_scan_secs: Option<i64>,
    pub delete_policy: Option<String>,
}

/// The recorded sync state of one file, keyed by (watch_id, rel_path).
#[derive(Debug, Clone)]
pub struct FileState {
    pub rel_path: String,
    pub hash: String,
    pub mtime: i64,
    pub size: i64,
    pub synced_etag: Option<String>,
}

fn row_to_watch(row: &rusqlite::Row) -> rusqlite::Result<NightWatch> {
    Ok(NightWatch {
        id: row.get(0)?,
        account_id: row.get(1)?,
        bucket: row.get(2)?,
        local_dir: row.get(3)?,
        key_prefix: row.get(4)?,
        ignore_file: row.get(5)?,
        delete_policy: row.get(6)?,
        full_scan_secs: row.get(7)?,
        enabled: row.get::<_, i64>(8)? != 0,
        last_scan_at: row.get(9)?,
        last_error: row.get(10)?,
        created_at: row.get(11)?,
        tree_uri: row.get(12)?,
    })
}

const WATCH_COLS: &str = "id, account_id, bucket, local_dir, key_prefix, ignore_file, \
    delete_policy, full_scan_secs, enabled, last_scan_at, last_error, created_at, tree_uri";

impl Db {
    pub async fn list_watches(&self) -> AppResult<Vec<NightWatch>> {
        self.conn
            .call(move |conn| {
                let sql = format!("SELECT {WATCH_COLS} FROM nw_watch ORDER BY created_at");
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map([], row_to_watch)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn list_enabled_watches(&self) -> AppResult<Vec<NightWatch>> {
        self.conn
            .call(move |conn| {
                let sql = format!(
                    "SELECT {WATCH_COLS} FROM nw_watch WHERE enabled=1 ORDER BY created_at"
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map([], row_to_watch)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn get_watch(&self, id: &str) -> AppResult<Option<NightWatch>> {
        let id = id.to_string();
        self.conn
            .call(move |conn| {
                let sql = format!("SELECT {WATCH_COLS} FROM nw_watch WHERE id=?1");
                conn.query_row(&sql, params![id], row_to_watch)
                    .optional()
                    .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn insert_watch(&self, id: &str, w: NewWatch) -> AppResult<()> {
        let id = id.to_string();
        let now = chrono::Utc::now().timestamp();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO nw_watch(id, account_id, bucket, local_dir, key_prefix, \
                        ignore_file, delete_policy, full_scan_secs, enabled, created_at, tree_uri) \
                     VALUES(?1,?2,?3,?4,?5,?6,'keep',?7,1,?8,?9)",
                    params![
                        id,
                        w.account_id,
                        w.bucket,
                        w.local_dir,
                        w.key_prefix,
                        w.ignore_file,
                        w.full_scan_secs,
                        now,
                        w.tree_uri
                    ],
                )
                .map(|_| ())
                .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn update_watch(&self, id: &str, patch: WatchPatch) -> AppResult<()> {
        let id = id.to_string();
        self.conn
            .call(move |conn| {
                if let Some(v) = patch.key_prefix {
                    conn.execute("UPDATE nw_watch SET key_prefix=?2 WHERE id=?1", params![id, v])?;
                }
                if let Some(v) = patch.ignore_file {
                    conn.execute("UPDATE nw_watch SET ignore_file=?2 WHERE id=?1", params![id, v])?;
                }
                if let Some(v) = patch.full_scan_secs {
                    conn.execute("UPDATE nw_watch SET full_scan_secs=?2 WHERE id=?1", params![id, v])?;
                }
                if let Some(v) = patch.delete_policy {
                    conn.execute("UPDATE nw_watch SET delete_policy=?2 WHERE id=?1", params![id, v])?;
                }
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await
            .map_err(Into::into)
    }

    pub async fn set_watch_enabled(&self, id: &str, enabled: bool) -> AppResult<()> {
        let id = id.to_string();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "UPDATE nw_watch SET enabled=?2 WHERE id=?1",
                    params![id, enabled as i64],
                )
                .map(|_| ())
                .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }

    /// Record the outcome of a full scan: bump `last_scan_at` and store (or
    /// clear) the last error message.
    pub async fn set_watch_scan_result(
        &self,
        id: &str,
        scan_at: i64,
        error: Option<String>,
    ) -> AppResult<()> {
        let id = id.to_string();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "UPDATE nw_watch SET last_scan_at=?2, last_error=?3 WHERE id=?1",
                    params![id, scan_at, error],
                )
                .map(|_| ())
                .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn delete_watch(&self, id: &str) -> AppResult<()> {
        let id = id.to_string();
        self.conn
            .call(move |conn| {
                conn.execute("DELETE FROM nw_watch WHERE id=?1", params![id])
                    .map(|_| ())
                    .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn file_state_get(
        &self,
        watch_id: &str,
        rel_path: &str,
    ) -> AppResult<Option<FileState>> {
        let watch_id = watch_id.to_string();
        let rel_path = rel_path.to_string();
        self.conn
            .call(move |conn| {
                conn.query_row(
                    "SELECT rel_path, hash, mtime, size, synced_etag FROM nw_file_state \
                     WHERE watch_id=?1 AND rel_path=?2",
                    params![watch_id, rel_path],
                    |row| {
                        Ok(FileState {
                            rel_path: row.get(0)?,
                            hash: row.get(1)?,
                            mtime: row.get(2)?,
                            size: row.get(3)?,
                            synced_etag: row.get(4)?,
                        })
                    },
                )
                .optional()
                .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }

    /// Load every `nw_file_state` row for a watch into a `rel_path`-keyed map.
    /// A scan calls this once up front so the per-file fast-path is an in-memory
    /// lookup instead of one DB round-trip per file on the single connection.
    pub async fn file_state_map(
        &self,
        watch_id: &str,
    ) -> AppResult<std::collections::HashMap<String, FileState>> {
        let watch_id = watch_id.to_string();
        self.conn
            .call(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT rel_path, hash, mtime, size, synced_etag FROM nw_file_state \
                     WHERE watch_id=?1",
                )?;
                let rows = stmt.query_map(params![watch_id], |row| {
                    Ok(FileState {
                        rel_path: row.get(0)?,
                        hash: row.get(1)?,
                        mtime: row.get(2)?,
                        size: row.get(3)?,
                        synced_etag: row.get(4)?,
                    })
                })?;
                let mut map = std::collections::HashMap::new();
                for r in rows {
                    let st = r?;
                    map.insert(st.rel_path.clone(), st);
                }
                Ok::<_, tokio_rusqlite::Error>(map)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn file_state_upsert(
        &self,
        watch_id: &str,
        st: FileState,
    ) -> AppResult<()> {
        let watch_id = watch_id.to_string();
        let now = chrono::Utc::now().timestamp();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO nw_file_state(watch_id, rel_path, hash, mtime, size, synced_etag, synced_at) \
                     VALUES(?1,?2,?3,?4,?5,?6,?7) \
                     ON CONFLICT(watch_id, rel_path) DO UPDATE SET \
                        hash=excluded.hash, mtime=excluded.mtime, size=excluded.size, \
                        synced_etag=excluded.synced_etag, synced_at=excluded.synced_at",
                    params![
                        watch_id,
                        st.rel_path,
                        st.hash,
                        st.mtime,
                        st.size,
                        st.synced_etag,
                        now
                    ],
                )
                .map(|_| ())
                .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn file_state_delete(&self, watch_id: &str, rel_path: &str) -> AppResult<()> {
        let watch_id = watch_id.to_string();
        let rel_path = rel_path.to_string();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM nw_file_state WHERE watch_id=?1 AND rel_path=?2",
                    params![watch_id, rel_path],
                )
                .map(|_| ())
                .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }

    /// Delete many state rows in one transaction (one fsync). Used by the
    /// mark-and-sweep so removing a large directory is not N separate writes.
    pub async fn file_state_delete_many(
        &self,
        watch_id: &str,
        rel_paths: &[String],
    ) -> AppResult<u64> {
        if rel_paths.is_empty() {
            return Ok(0);
        }
        let watch_id = watch_id.to_string();
        let rels = rel_paths.to_vec();
        let n = self
            .conn
            .call(move |conn| {
                let tx = conn.transaction()?;
                let mut n = 0u64;
                {
                    let mut stmt = tx.prepare_cached(
                        "DELETE FROM nw_file_state WHERE watch_id=?1 AND rel_path=?2",
                    )?;
                    for rel in &rels {
                        n += stmt.execute(params![watch_id, rel])? as u64;
                    }
                }
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n)
    }

    /// List every `rel_path` with a recorded state for this watch. Used by the
    /// full-scan mark-and-sweep to prune rows for files no longer present.
    pub async fn file_state_list_rel_paths(&self, watch_id: &str) -> AppResult<Vec<String>> {
        let watch_id = watch_id.to_string();
        self.conn
            .call(move |conn| {
                let mut stmt = conn
                    .prepare("SELECT rel_path FROM nw_file_state WHERE watch_id=?1")?;
                let rows = stmt.query_map(params![watch_id], |row| row.get::<_, String>(0))?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn file_state_count(&self, watch_id: &str) -> AppResult<i64> {
        let watch_id = watch_id.to_string();
        self.conn
            .call(move |conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM nw_file_state WHERE watch_id=?1",
                    params![watch_id],
                    |row| row.get(0),
                )
                .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }

    /// Read the retry backoff row for a file: `(fail_count, retry_after)`.
    /// `None` means no failures recorded (clear to upload).
    pub async fn file_retry_get(
        &self,
        watch_id: &str,
        rel_path: &str,
    ) -> AppResult<Option<(i64, i64)>> {
        let watch_id = watch_id.to_string();
        let rel_path = rel_path.to_string();
        self.conn
            .call(move |conn| {
                conn.query_row(
                    "SELECT fail_count, retry_after FROM nw_file_retry \
                     WHERE watch_id=?1 AND rel_path=?2",
                    params![watch_id, rel_path],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }

    /// Record an upload failure: bump `fail_count` and set `retry_after`. Once
    /// the count reaches `max_retries`, `retry_after` = now + `pause_secs` so the
    /// file is skipped until the pause elapses. Returns the new fail_count.
    pub async fn file_retry_record_failure(
        &self,
        watch_id: &str,
        rel_path: &str,
        max_retries: i64,
        pause_secs: i64,
    ) -> AppResult<i64> {
        let watch_id = watch_id.to_string();
        let rel_path = rel_path.to_string();
        let now = chrono::Utc::now().timestamp();
        self.conn
            .call(move |conn| {
                let prev: i64 = conn
                    .query_row(
                        "SELECT fail_count FROM nw_file_retry WHERE watch_id=?1 AND rel_path=?2",
                        params![watch_id, rel_path],
                        |row| row.get(0),
                    )
                    .optional()?
                    .unwrap_or(0);
                let count = prev + 1;
                let retry_after = if count >= max_retries { now + pause_secs } else { 0 };
                conn.execute(
                    "INSERT INTO nw_file_retry(watch_id, rel_path, fail_count, retry_after, updated_at) \
                     VALUES(?1,?2,?3,?4,?5) \
                     ON CONFLICT(watch_id, rel_path) DO UPDATE SET \
                        fail_count=excluded.fail_count, retry_after=excluded.retry_after, \
                        updated_at=excluded.updated_at",
                    params![watch_id, rel_path, count, retry_after, now],
                )?;
                Ok::<_, tokio_rusqlite::Error>(count)
            })
            .await
            .map_err(Into::into)
    }

    /// Clear the retry row for a file (on success or when the pause elapses).
    pub async fn file_retry_clear(&self, watch_id: &str, rel_path: &str) -> AppResult<()> {
        let watch_id = watch_id.to_string();
        let rel_path = rel_path.to_string();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM nw_file_retry WHERE watch_id=?1 AND rel_path=?2",
                    params![watch_id, rel_path],
                )
                .map(|_| ())
                .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }
}
