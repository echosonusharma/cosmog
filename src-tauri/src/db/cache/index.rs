use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::error::AppResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucketIndexStatus {
    pub enabled: bool,
    pub last_full_sync_at: Option<i64>,
    pub object_count: i64,
    pub scan_continuation: Option<String>,
    pub scan_started_at: Option<i64>,
    /// Re-run a full scan when `now - last_full_sync_at > auto_reindex_secs`.
    pub auto_reindex_secs: Option<i64>,
}

impl Db {
    pub async fn prefix_sync_expire(&self, account_id: &str, bucket: &str, prefix: &str) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let prefix = prefix.to_string();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO prefix_sync (account_id, bucket, prefix, synced_at)
                     VALUES (?1, ?2, ?3, 0)
                     ON CONFLICT(account_id, bucket, prefix) DO UPDATE SET synced_at = 0",
                    params![account_id, bucket, prefix],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    pub async fn prefix_sync_set(&self, account_id: &str, bucket: &str, prefix: &str) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let prefix = prefix.to_string();
        let now = Utc::now().timestamp();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO prefix_sync (account_id, bucket, prefix, synced_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(account_id, bucket, prefix) DO UPDATE SET synced_at = excluded.synced_at",
                    params![account_id, bucket, prefix, now],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    pub async fn prefix_sync_get(
        &self,
        account_id: &str,
        bucket: &str,
        prefix: &str,
    ) -> AppResult<Option<i64>> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let prefix = prefix.to_string();
        let v = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT synced_at FROM prefix_sync WHERE account_id = ?1 AND bucket = ?2 AND prefix = ?3",
                )?;
                let v: Option<i64> = stmt
                    .query_row(params![account_id, bucket, prefix], |row| row.get(0))
                    .optional()?;
                Ok::<_, tokio_rusqlite::Error>(v)
            })
            .await?;
        Ok(v)
    }

    pub async fn bucket_index_get(
        &self,
        account_id: &str,
        bucket: &str,
    ) -> AppResult<BucketIndexStatus> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let row = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT enabled, last_full_sync_at, object_count, scan_continuation, scan_started_at, auto_reindex_secs FROM bucket_index WHERE account_id = ?1 AND bucket = ?2",
                )?;
                let row: Option<(i64, Option<i64>, i64, Option<String>, Option<i64>, Option<i64>)> = stmt
                    .query_row(params![account_id, bucket], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
                    })
                    .optional()?;
                Ok::<_, tokio_rusqlite::Error>(row)
            })
            .await?;
        Ok(match row {
            Some((enabled, last, count, cont, started, auto)) => BucketIndexStatus {
                enabled: enabled != 0,
                last_full_sync_at: last,
                object_count: count,
                scan_continuation: cont,
                scan_started_at: started,
                auto_reindex_secs: auto,
            },
            None => BucketIndexStatus {
                enabled: false,
                last_full_sync_at: None,
                object_count: 0,
                scan_continuation: None,
                scan_started_at: None,
                auto_reindex_secs: None,
            },
        })
    }

    pub async fn bucket_index_set_auto_reindex(
        &self,
        account_id: &str,
        bucket: &str,
        secs: Option<i64>,
    ) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO bucket_index (account_id, bucket, enabled, auto_reindex_secs)
                     VALUES (?1, ?2, 1, ?3)
                     ON CONFLICT(account_id, bucket) DO UPDATE SET auto_reindex_secs = excluded.auto_reindex_secs",
                    params![account_id, bucket, secs],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Enabled indexes with next-due timestamps, for the scheduler.
    pub async fn bucket_index_due_list(&self) -> AppResult<Vec<(String, String, i64)>> {
        let rows = self
            .conn
            .call(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT account_id, bucket, COALESCE(last_full_sync_at, 0), auto_reindex_secs
                       FROM bucket_index
                      WHERE enabled = 1 AND auto_reindex_secs IS NOT NULL",
                )?;
                let iter = stmt.query_map([], |row| {
                    let account: String = row.get(0)?;
                    let bucket: String = row.get(1)?;
                    let last: i64 = row.get(2)?;
                    let secs: i64 = row.get(3)?;
                    // saturating_add: user-supplied `secs` could overflow i64.
                    Ok((account, bucket, last.saturating_add(secs)))
                })?;
                let mut out = Vec::new();
                for r in iter {
                    out.push(r?);
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(rows)
    }

    pub async fn bucket_scan_begin(
        &self,
        account_id: &str,
        bucket: &str,
    ) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let now = Utc::now().timestamp();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO bucket_index (account_id, bucket, enabled, scan_continuation, scan_started_at)
                     VALUES (?1, ?2, 1, NULL, ?3)
                     ON CONFLICT(account_id, bucket) DO UPDATE SET
                        enabled = 1,
                        scan_continuation = NULL,
                        scan_started_at = excluded.scan_started_at",
                    params![account_id, bucket, now],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Persist the latest continuation token; `None` at the final page.
    pub async fn bucket_scan_progress(
        &self,
        account_id: &str,
        bucket: &str,
        continuation: Option<String>,
    ) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        self.conn
            .call(move |conn| {
                // enabled guard: a scan racing a disable must not re-save its token.
                conn.execute(
                    "UPDATE bucket_index SET scan_continuation = ?1 WHERE account_id = ?2 AND bucket = ?3 AND enabled = 1",
                    params![continuation, account_id, bucket],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    pub async fn bucket_scan_clear(
        &self,
        account_id: &str,
        bucket: &str,
    ) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "UPDATE bucket_index SET scan_continuation = NULL, scan_started_at = NULL WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    pub async fn bucket_index_set_enabled(
        &self,
        account_id: &str,
        bucket: &str,
        enabled: bool,
    ) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let val: i64 = if enabled { 1 } else { 0 };
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO bucket_index (account_id, bucket, enabled) VALUES (?1, ?2, ?3)
                     ON CONFLICT(account_id, bucket) DO UPDATE SET enabled = excluded.enabled",
                    params![account_id, bucket, val],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Recompute `object_count` and stamp `last_full_sync_at` after a full scan.
    pub async fn bucket_index_finalize(&self, account_id: &str, bucket: &str) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let now = Utc::now().timestamp();
        self.conn
            .call(move |conn| {
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM cached_objects WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                    |row| row.get(0),
                )?;
                conn.execute(
                    "INSERT INTO bucket_index (account_id, bucket, enabled, last_full_sync_at, object_count)
                     VALUES (?1, ?2, 1, ?3, ?4)
                     ON CONFLICT(account_id, bucket) DO UPDATE SET
                        last_full_sync_at = excluded.last_full_sync_at,
                        object_count = excluded.object_count,
                        enabled = 1",
                    params![account_id, bucket, now, count],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// For remotely deleted buckets; unlike `cache_clear_bucket`, the bucket_index row goes too.
    pub async fn bucket_purge_all(&self, account_id: &str, bucket: &str) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        self.conn
            .call(move |conn| {
                // One transaction: a partial purge would leave inconsistent state.
                let tx = conn.transaction()?;
                tx.execute(
                    "DELETE FROM cached_objects WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                tx.execute(
                    "DELETE FROM prefix_sync WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                tx.execute(
                    "DELETE FROM bucket_index WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                tx.execute(
                    "DELETE FROM bucket_capabilities WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Disable the bucket's index and drop its cached rows and scan state.
    pub async fn cache_clear_bucket(&self, account_id: &str, bucket: &str) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        self.conn
            .call(move |conn| {
                let tx = conn.transaction()?;
                {
                    // Chunked delete bounds per-statement work on huge buckets.
                    const CHUNK: usize = 5000;
                    loop {
                        let deleted = tx.execute(
                            "DELETE FROM cached_objects
                              WHERE rowid IN (
                                  SELECT rowid FROM cached_objects
                                   WHERE account_id = ?1 AND bucket = ?2
                                   LIMIT ?3)",
                            params![account_id, bucket, CHUNK],
                        )?;
                        if deleted < CHUNK {
                            break;
                        }
                    }
                }
                tx.execute(
                    "DELETE FROM prefix_sync WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                tx.execute(
                    // Drop scan state too, else re-enable resumes mid-scan over an empty cache.
                    "UPDATE bucket_index SET enabled = 0, last_full_sync_at = NULL, object_count = 0,
                        scan_continuation = NULL, scan_started_at = NULL
                     WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }
}
