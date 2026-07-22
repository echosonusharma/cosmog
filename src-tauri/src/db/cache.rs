//! Cached object index for prefix + in-bucket search.
//!
//! Mirrors a subset of remote object listings into SQLite so search can run
//! locally against `cached_objects` + the `cached_objects_fts` FTS5 virtual
//! table. The cache is populated by [`crate::sync`] (full bucket index and
//! prefix sync) and by write-through hooks on the object-mutation commands.

use chrono::Utc;
use rusqlite::{params, types::Value, OptionalExtension};
use serde::{Deserialize, Serialize};

#[allow(unused_imports)]
use crate::error::{AppError, AppResult};
use crate::store::ObjectMeta;

use super::Db;

/// One row in `cached_objects`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedObjectMeta {
    pub account_id: String,
    pub bucket: String,
    pub key: String,
    pub size: i64,
    pub etag: Option<String>,
    pub last_modified: Option<i64>,
    pub storage_class: Option<String>,
    pub content_type: Option<String>,
    pub extension: Option<String>,
    pub basename: String,
    pub version_id: Option<String>,
    pub synced_at: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BucketStats {
    pub object_count: i64,
    pub total_bytes: i64,
    pub by_storage_class: Vec<StorageClassStat>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageClassStat {
    pub storage_class: String,
    pub object_count: i64,
    pub total_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucketIndexStatus {
    pub enabled: bool,
    pub last_full_sync_at: Option<i64>,
    pub object_count: i64,
    /// Non-null while a scan is in progress (interrupted or active). Stores
    /// the continuation token to resume from on next call to
    /// [`Db::full_bucket_scan_resume`].
    pub scan_continuation: Option<String>,
    pub scan_started_at: Option<i64>,
    /// If set, the scheduler re-runs a full scan whenever
    /// `now - last_full_sync_at > auto_reindex_secs`.
    pub auto_reindex_secs: Option<i64>,
}

/// Parts derived from an object key. Computed once at upsert time so search +
/// facet queries can use indexed columns instead of LIKE / instr().
#[derive(Debug, Clone)]
pub struct KeyParts {
    pub basename: String,
    pub extension: Option<String>,
}

impl KeyParts {
    pub fn from_key(key: &str) -> Self {
        let base = match key.rfind('/') {
            Some(idx) => key[idx + 1..].to_string(),
            None => key.to_string(),
        };
        let extension = base
            .rfind('.')
            // Skip hidden files like ".bashrc" (leading dot = no extension).
            .filter(|&i| i > 0 && i + 1 < base.len())
            .map(|i| base[i + 1..].to_lowercase())
            // Allow any Unicode alphanumeric; reject anything with whitespace,
            // separators, or control chars that would never be a real ext.
            .filter(|s| {
                s.chars().count() <= 16
                    && s.chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            });
        KeyParts {
            basename: base,
            extension,
        }
    }
}

/// Escape a prefix for use in a SQLite `LIKE` pattern and append `%`.
/// Escapes `%`, `_`, and `\` so that literal characters in the prefix aren't
/// treated as wildcards.
fn like_prefix(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 1);
    for c in prefix.chars() {
        match c {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.push('%');
    out
}

/// Build an FTS5 MATCH query for the trigram tokenizer.
/// Terms shorter than 3 chars are skipped (trigram minimum).
/// Returns None if no usable terms remain (caller should use LIKE fallback).
pub fn build_fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        .filter(|t| t.chars().count() >= 3)
        .map(|term| {
            let escaped = term.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect();
    if terms.is_empty() { None } else { Some(terms.join(" ")) }
}

/// Wrap a query string in `%…%` LIKE wildcards with proper escaping.
fn like_contains(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('%');
    for c in s.chars() {
        match c {
            '%' | '_' | '\\' => { out.push('\\'); out.push(c); }
            _ => out.push(c),
        }
    }
    out.push('%');
    out
}

fn row_to_cached(row: &rusqlite::Row) -> rusqlite::Result<CachedObjectMeta> {
    Ok(CachedObjectMeta {
        account_id: row.get(0)?,
        bucket: row.get(1)?,
        key: row.get(2)?,
        size: row.get(3)?,
        etag: row.get(4)?,
        last_modified: row.get(5)?,
        storage_class: row.get(6)?,
        content_type: row.get(7)?,
        extension: row.get(8)?,
        basename: row.get(9)?,
        version_id: row.get(10)?,
        synced_at: row.get(11)?,
    })
}

const SELECT_COLS: &str = "co.account_id, co.bucket, co.key, co.size, co.etag, co.last_modified, co.storage_class, co.content_type, co.extension, co.basename, co.version_id, co.synced_at";

impl Db {
    /// Upsert a cached object row from a freshly-listed [`ObjectMeta`]. The row
    /// is marked `seen=1` so prefix-sync sweeps don't garbage-collect it.
    pub async fn cache_upsert_object(
        &self,
        account_id: &str,
        bucket: &str,
        meta: &ObjectMeta,
    ) -> AppResult<()> {
        let parts = KeyParts::from_key(&meta.key);
        let now = Utc::now().timestamp();
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let key = meta.key.clone();
        let size = meta.size;
        let etag = meta.etag.clone();
        let last_modified = meta.last_modified;
        let storage_class = meta.storage_class.clone();
        let content_type = meta.content_type.clone();
        let version_id = meta.version_id.clone();

        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO cached_objects (account_id, bucket, key, size, etag, last_modified, storage_class, content_type, extension, basename, version_id, seen, synced_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, ?12)
                     ON CONFLICT(account_id, bucket, key) DO UPDATE SET
                        size = excluded.size,
                        etag = excluded.etag,
                        last_modified = excluded.last_modified,
                        storage_class = excluded.storage_class,
                        content_type = COALESCE(excluded.content_type, cached_objects.content_type),
                        extension = excluded.extension,
                        basename = excluded.basename,
                        version_id = excluded.version_id,
                        seen = 1,
                        synced_at = excluded.synced_at",
                    params![
                        account_id,
                        bucket,
                        key,
                        size,
                        etag,
                        last_modified,
                        storage_class,
                        content_type,
                        parts.extension,
                        parts.basename,
                        version_id,
                        now,
                    ],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Upsert a batch of cached object rows in a single transaction. More
    /// efficient than calling [`cache_upsert_object`] in a loop for large pages.
    /// Returns the number of rows upserted.
    pub async fn cache_upsert_objects_batch(
        &self,
        account_id: &str,
        bucket: &str,
        objects: &[crate::store::ObjectMeta],
    ) -> AppResult<usize> {
        let now = Utc::now().timestamp();
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        // Pre-compute all parts outside the call closure.
        struct Row {
            key: String,
            size: i64,
            etag: Option<String>,
            last_modified: Option<i64>,
            storage_class: Option<String>,
            content_type: Option<String>,
            version_id: Option<String>,
            extension: Option<String>,
            basename: String,
        }
        let rows: Vec<Row> = objects
            .iter()
            .map(|meta| {
                let parts = KeyParts::from_key(&meta.key);
                Row {
                    key: meta.key.clone(),
                    size: meta.size,
                    etag: meta.etag.clone(),
                    last_modified: meta.last_modified,
                    storage_class: meta.storage_class.clone(),
                    content_type: meta.content_type.clone(),
                    version_id: meta.version_id.clone(),
                    extension: parts.extension,
                    basename: parts.basename,
                }
            })
            .collect();

        let count = self
            .conn
            .call(move |conn| {
                let tx = conn.transaction()?;
                {
                    let mut stmt = tx.prepare_cached(
                        "INSERT INTO cached_objects (account_id, bucket, key, size, etag, last_modified, storage_class, content_type, extension, basename, version_id, seen, synced_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, ?12)
                         ON CONFLICT(account_id, bucket, key) DO UPDATE SET
                            size = excluded.size,
                            etag = excluded.etag,
                            last_modified = excluded.last_modified,
                            storage_class = excluded.storage_class,
                            content_type = COALESCE(excluded.content_type, cached_objects.content_type),
                            extension = excluded.extension,
                            basename = excluded.basename,
                            version_id = excluded.version_id,
                            seen = 1,
                            synced_at = excluded.synced_at",
                    )?;
                    for row in &rows {
                        stmt.execute(params![
                            account_id,
                            bucket,
                            row.key,
                            row.size,
                            row.etag,
                            row.last_modified,
                            row.storage_class,
                            row.content_type,
                            row.extension,
                            row.basename,
                            row.version_id,
                            now,
                        ])?;
                    }
                }
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(rows.len())
            })
            .await?;
        Ok(count)
    }

    /// Remove a single cached row (write-through after `delete_object`).
    pub async fn cache_remove_object(
        &self,
        account_id: &str,
        bucket: &str,
        key: &str,
    ) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let key = key.to_string();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM cached_objects WHERE account_id = ?1 AND bucket = ?2 AND key = ?3",
                    params![account_id, bucket, key],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Look up a single cached row, if present.
    pub async fn cache_get_object(
        &self,
        account_id: &str,
        bucket: &str,
        key: &str,
    ) -> AppResult<Option<CachedObjectMeta>> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let key = key.to_string();
        let sql = format!(
            "SELECT {SELECT_COLS} FROM cached_objects co WHERE co.account_id = ?1 AND co.bucket = ?2 AND co.key = ?3"
        );
        let row = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(&sql)?;
                let row = stmt
                    .query_row(params![account_id, bucket, key], row_to_cached)
                    .optional()?;
                Ok::<_, tokio_rusqlite::Error>(row)
            })
            .await?;
        Ok(row)
    }

    /// Mark every cached row in a (bucket, prefix) scope as `seen=0`. Called
    /// before a sync sweep; rows that remain `seen=0` at the end represent
    /// deletions.
    pub async fn cache_mark_unseen(
        &self,
        account_id: &str,
        bucket: &str,
        scope: SyncScope,
    ) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        self.conn
            .call(move |conn| {
                match scope {
                    SyncScope::Bucket => {
                        conn.execute(
                            "UPDATE cached_objects SET seen = 0 WHERE account_id = ?1 AND bucket = ?2",
                            params![account_id, bucket],
                        )?;
                    }
                    SyncScope::PrefixDirect { prefix } => {
                        let after = prefix.len() as i64 + 1;
                        let pat = like_prefix(&prefix);
                        conn.execute(
                            "UPDATE cached_objects SET seen = 0
                             WHERE account_id = ?1 AND bucket = ?2
                               AND key LIKE ?3 ESCAPE '\\'
                               AND instr(substr(key, ?4), '/') = 0",
                            params![account_id, bucket, pat, after],
                        )?;
                    }
                    SyncScope::PrefixRecursive { prefix } => {
                        let pat = like_prefix(&prefix);
                        conn.execute(
                            "UPDATE cached_objects SET seen = 0
                             WHERE account_id = ?1 AND bucket = ?2
                               AND key LIKE ?3 ESCAPE '\\'",
                            params![account_id, bucket, pat],
                        )?;
                    }
                }
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Delete rows still `seen=0` after a sweep finished. Returns count of
    /// rows removed.
    pub async fn cache_sweep_unseen(
        &self,
        account_id: &str,
        bucket: &str,
        scope: SyncScope,
    ) -> AppResult<usize> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let n = self
            .conn
            .call(move |conn| {
                let n = match scope {
                    SyncScope::Bucket => conn.execute(
                        "DELETE FROM cached_objects WHERE account_id = ?1 AND bucket = ?2 AND seen = 0",
                        params![account_id, bucket],
                    )?,
                    SyncScope::PrefixDirect { prefix } => {
                        let after = prefix.len() as i64 + 1;
                        let pat = like_prefix(&prefix);
                        conn.execute(
                            "DELETE FROM cached_objects
                             WHERE account_id = ?1 AND bucket = ?2
                               AND key LIKE ?3 ESCAPE '\\'
                               AND instr(substr(key, ?4), '/') = 0
                               AND seen = 0",
                            params![account_id, bucket, pat, after],
                        )?
                    }
                    SyncScope::PrefixRecursive { prefix } => {
                        let pat = like_prefix(&prefix);
                        conn.execute(
                            "DELETE FROM cached_objects
                             WHERE account_id = ?1 AND bucket = ?2
                               AND key LIKE ?3 ESCAPE '\\'
                               AND seen = 0",
                            params![account_id, bucket, pat],
                        )?
                    }
                };
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n)
    }

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

    /// Return all enabled bucket indexes with their account_id + bucket +
    /// next-due timestamp (last_full_sync_at + auto_reindex_secs). Used by
    /// the scheduler.
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
                    // saturating add: user-supplied `secs` could otherwise
                    // overflow i64 and underflow next_due.
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

    /// Mark the start of (or replace state for) an in-progress full scan.
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

    /// Save the most-recent continuation token between pages. Pass `None` when
    /// the scan reaches the last page (so a subsequent resume sees no token).
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
                conn.execute(
                    "UPDATE bucket_index SET scan_continuation = ?1 WHERE account_id = ?2 AND bucket = ?3",
                    params![continuation, account_id, bucket],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Clear scan state. Call on successful completion, cancellation, or abort.
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

    /// Recompute `bucket_index.object_count` and stamp `last_full_sync_at`.
    /// Call this when a full bucket scan completes successfully.
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

    /// Compute aggregated stats from cached rows. Counts only what's indexed —
    /// for an accurate total the user must have run a full bucket scan first.
    pub async fn bucket_stats(
        &self,
        account_id: &str,
        bucket: &str,
    ) -> AppResult<BucketStats> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let stats = self
            .conn
            .call(move |conn| {
                let mut stats = BucketStats::default();
                let (count, total): (i64, i64) = conn.query_row(
                    "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM cached_objects
                       WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                stats.object_count = count;
                stats.total_bytes = total;
                let mut stmt = conn.prepare(
                    "SELECT COALESCE(storage_class, ''), COUNT(*), COALESCE(SUM(size), 0)
                       FROM cached_objects
                       WHERE account_id = ?1 AND bucket = ?2
                       GROUP BY storage_class",
                )?;
                let iter = stmt.query_map(params![account_id, bucket], |row| {
                    Ok(StorageClassStat {
                        storage_class: row.get(0)?,
                        object_count: row.get(1)?,
                        total_bytes: row.get(2)?,
                    })
                })?;
                for r in iter {
                    stats.by_storage_class.push(r?);
                }
                Ok::<_, tokio_rusqlite::Error>(stats)
            })
            .await?;
        Ok(stats)
    }

    /// Purge every persisted row tied to a bucket. Used when the bucket itself
    /// is deleted from the remote — leaves no orphan index/capability/cache
    /// rows behind. Distinct from `cache_clear_bucket`, which keeps the
    /// bucket_index row (just disabled) because the bucket still exists.
    pub async fn bucket_purge_all(&self, account_id: &str, bucket: &str) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM cached_objects WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                conn.execute(
                    "DELETE FROM prefix_sync WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                conn.execute(
                    "DELETE FROM bucket_index WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                conn.execute(
                    "DELETE FROM bucket_capabilities WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Drop all cached rows for a bucket. Used when the user disables the
    /// bucket index.
    pub async fn cache_clear_bucket(&self, account_id: &str, bucket: &str) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM cached_objects WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                conn.execute(
                    "DELETE FROM prefix_sync WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                conn.execute(
                    "UPDATE bucket_index SET enabled = 0, last_full_sync_at = NULL, object_count = 0
                     WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Returns (files, subprefixes) for direct children of `prefix`.
    /// Derives the tree from keys at query time using LIKE + instr — no
    /// parent_prefix column needed.
    pub async fn browse_children(
        &self,
        account_id: &str,
        bucket: &str,
        prefix: &str,
    ) -> AppResult<(Vec<CachedObjectMeta>, Vec<String>, bool)> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let prefix = prefix.to_string();

        const FILE_LIMIT: usize = 5_000;

        self.conn
            .call(move |conn| {
                let after = prefix.len() as i64 + 1; // 1-indexed offset past prefix
                // Fetch one extra row to detect truncation without a separate COUNT query.
                let fetch_limit = (FILE_LIMIT + 1) as i64;

                let (mut files, subprefixes) = if prefix.is_empty() {
                    let files: Vec<CachedObjectMeta> = {
                        let mut stmt = conn.prepare(
                            "SELECT co.account_id, co.bucket, co.key, co.size, co.etag, co.last_modified, co.storage_class, co.content_type, co.extension, co.basename, co.version_id, co.synced_at
                             FROM cached_objects co
                             WHERE co.account_id = ?1 AND co.bucket = ?2
                               AND instr(co.key, '/') = 0
                             ORDER BY co.key LIMIT ?3",
                        )?;
                        let v: Vec<CachedObjectMeta> = stmt.query_map(params![account_id, bucket, fetch_limit], row_to_cached)?
                            .filter_map(|r| r.ok())
                            .collect();
                        v
                    };
                    let subprefixes: Vec<String> = {
                        let mut stmt = conn.prepare(
                            "SELECT DISTINCT substr(co.key, 1, instr(co.key, '/')) AS folder
                             FROM cached_objects co
                             WHERE co.account_id = ?1 AND co.bucket = ?2
                               AND instr(co.key, '/') > 0
                             ORDER BY folder",
                        )?;
                        let v: Vec<String> = stmt.query_map(params![account_id, bucket], |row| row.get(0))?
                            .filter_map(|r| r.ok())
                            .collect();
                        v
                    };
                    (files, subprefixes)
                } else {
                    let like_pat = like_prefix(&prefix);
                    let files: Vec<CachedObjectMeta> = {
                        let mut stmt = conn.prepare(
                            "SELECT co.account_id, co.bucket, co.key, co.size, co.etag, co.last_modified, co.storage_class, co.content_type, co.extension, co.basename, co.version_id, co.synced_at
                             FROM cached_objects co
                             WHERE co.account_id = ?1 AND co.bucket = ?2
                               AND co.key LIKE ?3 ESCAPE '\\'
                               AND co.key != ?4
                               AND instr(substr(co.key, ?5), '/') = 0
                             ORDER BY co.key LIMIT ?6",
                        )?;
                        let v: Vec<CachedObjectMeta> = stmt.query_map(params![account_id, bucket, like_pat, prefix, after, fetch_limit], row_to_cached)?
                            .filter_map(|r| r.ok())
                            .collect();
                        v
                    };
                    let subprefixes: Vec<String> = {
                        let mut stmt = conn.prepare(
                            "SELECT DISTINCT substr(co.key, 1, (?5 - 1) + instr(substr(co.key, ?5), '/')) AS folder
                             FROM cached_objects co
                             WHERE co.account_id = ?1 AND co.bucket = ?2
                               AND co.key LIKE ?3 ESCAPE '\\'
                               AND instr(substr(co.key, ?5), '/') > 0
                             ORDER BY folder",
                        )?;
                        let v: Vec<String> = stmt.query_map(params![account_id, bucket, like_pat, prefix, after], |row| row.get(0))?
                            .filter_map(|r| r.ok())
                            .collect();
                        v
                    };
                    (files, subprefixes)
                };

                let truncated = files.len() > FILE_LIMIT;
                if truncated { files.truncate(FILE_LIMIT); }

                Ok::<_, tokio_rusqlite::Error>((files, subprefixes, truncated))
            })
            .await
            .map_err(Into::into)
    }
}

/// Scope for sync sweeps. Used by both the mark-unseen and sweep-unseen helpers
/// so they delimit the same set of rows.
#[derive(Debug, Clone)]
pub enum SyncScope {
    Bucket,
    PrefixDirect { prefix: String },
    PrefixRecursive { prefix: String },
}

// ---------------------------------------------------------------------------
// Search / facet querying
// ---------------------------------------------------------------------------

/// User-supplied search parameters. See [`Db::search_objects`].
#[derive(Debug, Clone, Deserialize)]
pub struct SearchQuery {
    pub account_id: String,
    pub bucket: String,
    pub scope: SearchScope,
    pub query: Option<String>,
    #[serde(default)]
    pub filters: SearchFilters,
    #[serde(default)]
    pub sort: SortBy,
    #[serde(default)]
    pub sort_dir: SortDir,
    pub page_size: Option<u32>,
    pub cursor: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchScope {
    /// Restrict to a single prefix. `recursive=false` matches only direct
    /// children (one level), `true` matches everything under the prefix.
    Prefix { prefix: String, recursive: bool },
    /// Whole bucket. Requires bucket index to be enabled, but query will
    /// still run against whatever is cached.
    Bucket,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchFilters {
    #[serde(default)]
    pub extensions: Vec<String>,
    pub size_min: Option<i64>,
    pub size_max: Option<i64>,
    pub modified_after: Option<i64>,
    pub modified_before: Option<i64>,
    #[serde(default)]
    pub storage_classes: Vec<String>,
    #[serde(default)]
    pub content_types: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    #[default]
    Name,
    Size,
    Modified,
    Extension,
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortDir {
    Asc,
    #[default]
    Desc,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub objects: Vec<CachedObjectMeta>,
    pub total: i64,
    pub facets: Facets,
    pub next_cursor: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Facets {
    pub extensions: Vec<FacetBucket>,
    pub size_buckets: Vec<FacetBucket>,
    pub date_buckets: Vec<FacetBucket>,
    pub storage_classes: Vec<FacetBucket>,
    pub content_types: Vec<FacetBucket>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FacetBucket {
    pub value: String,
    pub count: i64,
}

/// Build the WHERE clause shared by search + facet queries.
///
/// Returns `(sql_fragment, params)` where the fragment starts with `WHERE` and
/// `params` is in argument-position order.
fn build_filter(
    account_id: &str,
    bucket: &str,
    scope: &SearchScope,
    filters: &SearchFilters,
    exclude_facet: Option<FacetDim>,
) -> (String, Vec<Value>) {
    let mut clauses = vec![
        String::from("co.account_id = ?"),
        String::from("co.bucket = ?"),
    ];
    let mut p: Vec<Value> = vec![
        Value::Text(account_id.to_string()),
        Value::Text(bucket.to_string()),
    ];

    match scope {
        SearchScope::Prefix { prefix, recursive } => {
            clauses.push("co.key LIKE ? ESCAPE '\\'".into());
            p.push(Value::Text(like_prefix(prefix)));
            if !recursive {
                // Exclude keys that have a slash after the prefix — those are
                // descendants, not direct children.
                let mut pat = String::with_capacity(prefix.len() + 4);
                for c in prefix.chars() {
                    match c {
                        '%' | '_' | '\\' => { pat.push('\\'); pat.push(c); }
                        _ => pat.push(c),
                    }
                }
                pat.push_str("%/%");
                clauses.push("co.key NOT LIKE ? ESCAPE '\\'".into());
                p.push(Value::Text(pat));
            }
        }
        SearchScope::Bucket => {}
    }

    if !filters.extensions.is_empty() && exclude_facet != Some(FacetDim::Extension) {
        let placeholders = vec!["?"; filters.extensions.len()].join(",");
        clauses.push(format!("co.extension IN ({placeholders})"));
        for e in &filters.extensions {
            p.push(Value::Text(e.clone()));
        }
    }
    if !filters.storage_classes.is_empty() && exclude_facet != Some(FacetDim::StorageClass) {
        let placeholders = vec!["?"; filters.storage_classes.len()].join(",");
        clauses.push(format!("co.storage_class IN ({placeholders})"));
        for e in &filters.storage_classes {
            p.push(Value::Text(e.clone()));
        }
    }
    if !filters.content_types.is_empty() && exclude_facet != Some(FacetDim::ContentType) {
        let placeholders = vec!["?"; filters.content_types.len()].join(",");
        clauses.push(format!("co.content_type IN ({placeholders})"));
        for e in &filters.content_types {
            p.push(Value::Text(e.clone()));
        }
    }
    if exclude_facet != Some(FacetDim::Size) {
        if let Some(min) = filters.size_min {
            clauses.push("co.size >= ?".into());
            p.push(Value::Integer(min));
        }
        if let Some(max) = filters.size_max {
            clauses.push("co.size <= ?".into());
            p.push(Value::Integer(max));
        }
    }
    if exclude_facet != Some(FacetDim::Date) {
        if let Some(after) = filters.modified_after {
            clauses.push("co.last_modified >= ?".into());
            p.push(Value::Integer(after));
        }
        if let Some(before) = filters.modified_before {
            clauses.push("co.last_modified <= ?".into());
            p.push(Value::Integer(before));
        }
    }

    let where_sql = format!("WHERE {}", clauses.join(" AND "));
    (where_sql, p)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FacetDim {
    Extension,
    Size,
    Date,
    StorageClass,
    ContentType,
}

impl Db {
    /// Execute a search query. Combines filter clauses, optional FTS5 MATCH on
    /// `key` + `basename`, sorted pagination, total count, and a `Facets`
    /// aggregate computed alongside.
    pub async fn search_objects(&self, q: SearchQuery) -> AppResult<SearchResult> {
        let q_clone = q.clone();
        let result = self
            .conn
            .call(move |conn| {
                let SearchQuery {
                    account_id,
                    bucket,
                    scope,
                    query,
                    filters,
                    sort,
                    sort_dir,
                    page_size,
                    cursor,
                } = q_clone;

                let limit = page_size.unwrap_or(100).min(1000) as i64;

                let (filter_sql, filter_params) =
                    build_filter(&account_id, &bucket, &scope, &filters, None);

                let raw_query = query.as_deref().map(str::trim).filter(|s| !s.is_empty());
                let fts = raw_query.and_then(build_fts_query);
                // For queries where all terms are <3 chars, fall back to LIKE on each term OR'd on basename.
                let short_like_terms: Vec<String> = if fts.is_none() {
                    raw_query
                        .iter()
                        .flat_map(|q| q.split_whitespace())
                        .filter(|t| !t.is_empty())
                        .map(like_contains)
                        .collect()
                } else {
                    vec![]
                };

                // When FTS active: FTS table is primary in FROM so that `cached_objects_fts MATCH`
                // and `fts.rank` (BM25) work correctly. filter_sql inner clauses appended after
                // the MATCH condition. fts_param must be first in the param list.
                let like_placeholders: String = short_like_terms
                    .iter()
                    .map(|_| "co.basename LIKE ? ESCAPE '\\'")
                    .collect::<Vec<_>>()
                    .join(" OR ");
                let like_clause: String = if like_placeholders.is_empty() {
                    String::new()
                } else {
                    format!(" AND ({like_placeholders})")
                };

                let (from_sql, where_sql, base_params) = if let Some(text) = &fts {
                    let filter_inner = filter_sql
                        .strip_prefix("WHERE ")
                        .expect("build_filter always produces a WHERE-prefixed clause");
                    let mut params = vec![Value::Text(text.clone())];
                    params.extend(filter_params.iter().cloned());
                    (
                        "cached_objects_fts fts JOIN cached_objects co ON co.rowid = fts.rowid".to_string(),
                        format!("WHERE cached_objects_fts MATCH ? AND {filter_inner}"),
                        params,
                    )
                } else {
                    (
                        "cached_objects co".to_string(),
                        filter_sql.clone(),
                        filter_params.clone(),
                    )
                };

                // ---------- TOTAL ----------
                let count_sql = format!(
                    "SELECT COUNT(*) FROM {from_sql} {where_sql} {like_clause}"
                );
                let total: i64 = {
                    let mut stmt = conn.prepare(&count_sql)?;
                    let mut all = base_params.clone();
                    for p in &short_like_terms {
                        all.push(Value::Text(p.clone()));
                    }
                    let refs: Vec<&dyn rusqlite::ToSql> =
                        all.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
                    stmt.query_row(refs.as_slice(), |row| row.get(0))?
                };

                // ---------- RESULTS ----------
                // When FTS active with default name sort, use BM25 relevance (fts.rank, lower = better).
                // FTS+Name sort is always ASC (rank). cursor_clause and rowid_order must use the
                // same effective direction to avoid pagination gaps.
                let fts_name_sort = fts.is_some() && matches!(sort, SortBy::Name);
                let (order_col, order_dir_str) = match (sort, fts.is_some()) {
                    (SortBy::Name, true)  => ("fts.rank", "ASC"),
                    (SortBy::Name, false) => ("co.key",            match sort_dir { SortDir::Asc => "ASC", SortDir::Desc => "DESC" }),
                    (SortBy::Size, _)     => ("co.size",           match sort_dir { SortDir::Asc => "ASC", SortDir::Desc => "DESC" }),
                    (SortBy::Modified, _) => ("co.last_modified",  match sort_dir { SortDir::Asc => "ASC", SortDir::Desc => "DESC" }),
                    (SortBy::Extension, _)=> ("co.extension",      match sort_dir { SortDir::Asc => "ASC", SortDir::Desc => "DESC" }),
                };
                // Cursor and rowid tie-break must use the effective direction, not the raw sort_dir,
                // so that FTS+Name (always ASC) paginates correctly.
                let effective_asc = fts_name_sort || matches!(sort_dir, SortDir::Asc);
                let cursor_clause = if cursor.is_some() {
                    if effective_asc { " AND co.rowid > ? " } else { " AND co.rowid < ? " }
                } else {
                    ""
                };
                let rowid_order = if effective_asc { "ASC" } else { "DESC" };

                let select_sql = format!(
                    "SELECT {SELECT_COLS}, co.rowid FROM {from_sql} {where_sql} {like_clause} {cursor_clause}
                     ORDER BY {order_col} {order_dir_str}, co.rowid {rowid_order}
                     LIMIT ?"
                );
                let mut stmt = conn.prepare(&select_sql)?;
                let mut all = base_params.clone();
                for p in &short_like_terms {
                    all.push(Value::Text(p.clone()));
                }
                if let Some(c) = cursor {
                    all.push(Value::Integer(c));
                }
                all.push(Value::Integer(limit));
                let refs: Vec<&dyn rusqlite::ToSql> =
                    all.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
                let mut rows = stmt.query(refs.as_slice())?;
                let mut objects = Vec::new();
                let mut last_rowid: Option<i64> = None;
                while let Some(row) = rows.next()? {
                    objects.push(row_to_cached(row)?);
                    last_rowid = row.get(13).ok();
                }
                let next_cursor = if objects.len() as i64 == limit { last_rowid } else { None };

                // ---------- FACETS ----------
                // Pass built FTS query (not raw input) so facets use same trigram matching.
                let facets = compute_facets(conn, &account_id, &bucket, &scope, &filters, fts.as_deref())?;

                Ok::<SearchResult, tokio_rusqlite::Error>(SearchResult {
                    objects,
                    total,
                    facets,
                    next_cursor,
                })
            })
            .await?;
        Ok(result)
    }
}

fn compute_facets(
    conn: &rusqlite::Connection,
    account_id: &str,
    bucket: &str,
    scope: &SearchScope,
    filters: &SearchFilters,
    fts: Option<&str>,
) -> Result<Facets, tokio_rusqlite::Error> {
    let mut facets = Facets::default();

    facets.extensions =
        facet_group(conn, account_id, bucket, scope, filters, fts, FacetDim::Extension, "co.extension")?;
    facets.storage_classes = facet_group(
        conn,
        account_id,
        bucket,
        scope,
        filters,
        fts,
        FacetDim::StorageClass,
        "co.storage_class",
    )?;
    facets.content_types = facet_group(
        conn,
        account_id,
        bucket,
        scope,
        filters,
        fts,
        FacetDim::ContentType,
        "co.content_type",
    )?;

    facets.size_buckets = facet_size_buckets(conn, account_id, bucket, scope, filters, fts)?;
    facets.date_buckets = facet_date_buckets(conn, account_id, bucket, scope, filters, fts)?;

    Ok(facets)
}

fn fts_join(query: &Option<String>) -> (String, Option<String>) {
    match query {
        Some(text) => (
            "JOIN cached_objects_fts fts ON fts.rowid = co.rowid".to_string(),
            Some(text.clone()),
        ),
        None => (String::new(), None),
    }
}

fn facet_group(
    conn: &rusqlite::Connection,
    account_id: &str,
    bucket: &str,
    scope: &SearchScope,
    filters: &SearchFilters,
    fts: Option<&str>,
    dim: FacetDim,
    col: &str,
) -> Result<Vec<FacetBucket>, tokio_rusqlite::Error> {
    let (filter_sql, mut params) = build_filter(account_id, bucket, scope, filters, Some(dim));
    let fts_owned = fts.map(|s| s.to_string());
    let (join_sql, fts_param) = fts_join(&fts_owned);
    let fts_clause = if fts_param.is_some() { " AND cached_objects_fts MATCH ? ".to_string() } else { String::new() };
    if let Some(text) = fts_param {
        params.push(Value::Text(text));
    }

    let sql = format!(
        "SELECT {col} AS v, COUNT(*) FROM cached_objects co {join_sql} {filter_sql} {fts_clause}
         AND {col} IS NOT NULL AND {col} != ''
         GROUP BY {col}
         ORDER BY COUNT(*) DESC
         LIMIT 50"
    );
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let mut rows = stmt.query(refs.as_slice())?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let v: Option<String> = row.get(0)?;
        let count: i64 = row.get(1)?;
        if let Some(v) = v {
            out.push(FacetBucket { value: v, count });
        }
    }
    Ok(out)
}

const SIZE_BUCKETS: &[(&str, i64, i64)] = &[
    ("<1KB", 0, 1024 - 1),
    ("1KB-1MB", 1024, 1024 * 1024 - 1),
    ("1MB-100MB", 1024 * 1024, 100 * 1024 * 1024 - 1),
    ("100MB-1GB", 100 * 1024 * 1024, 1024 * 1024 * 1024 - 1),
    (">1GB", 1024 * 1024 * 1024, i64::MAX),
];

fn facet_size_buckets(
    conn: &rusqlite::Connection,
    account_id: &str,
    bucket: &str,
    scope: &SearchScope,
    filters: &SearchFilters,
    fts: Option<&str>,
) -> Result<Vec<FacetBucket>, tokio_rusqlite::Error> {
    let mut out = Vec::with_capacity(SIZE_BUCKETS.len());
    for (label, lo, hi) in SIZE_BUCKETS {
        let (filter_sql, mut params) =
            build_filter(account_id, bucket, scope, filters, Some(FacetDim::Size));
        let fts_owned = fts.map(|s| s.to_string());
        let (join_sql, fts_param) = fts_join(&fts_owned);
        let fts_clause = if fts_param.is_some() { " AND cached_objects_fts MATCH ? ".to_string() } else { String::new() };
        if let Some(text) = fts_param {
            params.push(Value::Text(text));
        }
        params.push(Value::Integer(*lo));
        params.push(Value::Integer(*hi));

        let sql = format!(
            "SELECT COUNT(*) FROM cached_objects co {join_sql} {filter_sql} {fts_clause}
             AND co.size BETWEEN ? AND ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let count: i64 = stmt.query_row(refs.as_slice(), |row| row.get(0))?;
        out.push(FacetBucket {
            value: (*label).to_string(),
            count,
        });
    }
    Ok(out)
}

fn facet_date_buckets(
    conn: &rusqlite::Connection,
    account_id: &str,
    bucket: &str,
    scope: &SearchScope,
    filters: &SearchFilters,
    fts: Option<&str>,
) -> Result<Vec<FacetBucket>, tokio_rusqlite::Error> {
    let now = Utc::now().timestamp();
    let day = 86_400;
    let ranges: &[(&str, i64)] = &[
        ("Last 24h", now - day),
        ("Last 7 days", now - 7 * day),
        ("Last 30 days", now - 30 * day),
        ("Last year", now - 365 * day),
    ];

    let mut out = Vec::with_capacity(ranges.len() + 1);
    for (label, since) in ranges {
        let (filter_sql, mut params) =
            build_filter(account_id, bucket, scope, filters, Some(FacetDim::Date));
        let fts_owned = fts.map(|s| s.to_string());
        let (join_sql, fts_param) = fts_join(&fts_owned);
        let fts_clause = if fts_param.is_some() { " AND cached_objects_fts MATCH ? ".to_string() } else { String::new() };
        if let Some(text) = fts_param {
            params.push(Value::Text(text));
        }
        params.push(Value::Integer(*since));

        let sql = format!(
            "SELECT COUNT(*) FROM cached_objects co {join_sql} {filter_sql} {fts_clause}
             AND co.last_modified >= ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let count: i64 = stmt.query_row(refs.as_slice(), |row| row.get(0))?;
        out.push(FacetBucket {
            value: (*label).to_string(),
            count,
        });
    }

    // "Older" — no upper bound, just absence of last_modified-after window.
    let (filter_sql, mut params) =
        build_filter(account_id, bucket, scope, filters, Some(FacetDim::Date));
    let fts_owned = fts.map(|s| s.to_string());
    let (join_sql, fts_param) = fts_join(&fts_owned);
    let fts_clause = if fts_param.is_some() { " AND cached_objects_fts MATCH ? ".to_string() } else { String::new() };
    if let Some(text) = fts_param {
        params.push(Value::Text(text));
    }
    let year_ago = now - 365 * day;
    params.push(Value::Integer(year_ago));
    let sql = format!(
        "SELECT COUNT(*) FROM cached_objects co {join_sql} {filter_sql} {fts_clause}
         AND (co.last_modified IS NULL OR co.last_modified < ?)"
    );
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let count: i64 = stmt.query_row(refs.as_slice(), |row| row.get(0))?;
    out.push(FacetBucket {
        value: "Older".into(),
        count,
    });

    Ok(out)
}

