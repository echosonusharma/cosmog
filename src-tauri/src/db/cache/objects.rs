use chrono::Utc;
use rusqlite::{params, OptionalExtension};

use super::{prefix_upper_bound, row_to_cached, substr_offset_past_prefix, CachedObjectMeta, KeyParts, SELECT_COLS};
use crate::db::Db;
use crate::error::AppResult;
use crate::store::ObjectMeta;

/// Shared so mark-unseen and sweep-unseen delimit the same rows.
#[derive(Debug, Clone)]
pub enum SyncScope {
    Bucket,
    PrefixDirect { prefix: String },
    PrefixRecursive { prefix: String },
}

impl Db {
    /// Sets `seen=1` so sync sweeps won't garbage-collect it.
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
                let mut stmt = conn.prepare_cached(
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
                stmt.execute(
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

    pub async fn cache_upsert_objects_batch(
        &self,
        account_id: &str,
        bucket: &str,
        objects: &[crate::store::ObjectMeta],
    ) -> AppResult<usize> {
        let now = Utc::now().timestamp();
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
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
                let mut stmt = conn.prepare_cached(&sql)?;
                let row = stmt
                    .query_row(params![account_id, bucket, key], row_to_cached)
                    .optional()?;
                Ok::<_, tokio_rusqlite::Error>(row)
            })
            .await?;
        Ok(row)
    }

    /// Sweep phase 1: rows still `seen=0` at the end were deleted remotely.
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
                        let after = substr_offset_past_prefix(&prefix);
                        let upper = prefix_upper_bound(&prefix);
                        conn.execute(
                            "UPDATE cached_objects SET seen = 0
                             WHERE account_id = ?1 AND bucket = ?2
                               AND key >= ?3 AND key < ?5
                               AND instr(substr(key, ?4), '/') = 0",
                            params![account_id, bucket, prefix, after, upper],
                        )?;
                    }
                    SyncScope::PrefixRecursive { prefix } => {
                        let upper = prefix_upper_bound(&prefix);
                        conn.execute(
                            "UPDATE cached_objects SET seen = 0
                             WHERE account_id = ?1 AND bucket = ?2
                               AND key >= ?3 AND key < ?4",
                            params![account_id, bucket, prefix, upper],
                        )?;
                    }
                }
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Sweep phase 2: delete rows still `seen=0`; returns rows removed.
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
                        let after = substr_offset_past_prefix(&prefix);
                        let upper = prefix_upper_bound(&prefix);
                        conn.execute(
                            "DELETE FROM cached_objects
                             WHERE account_id = ?1 AND bucket = ?2
                               AND key >= ?3 AND key < ?5
                               AND instr(substr(key, ?4), '/') = 0
                               AND seen = 0",
                            params![account_id, bucket, prefix, after, upper],
                        )?
                    }
                    SyncScope::PrefixRecursive { prefix } => {
                        let upper = prefix_upper_bound(&prefix);
                        conn.execute(
                            "DELETE FROM cached_objects
                             WHERE account_id = ?1 AND bucket = ?2
                               AND key >= ?3 AND key < ?4
                               AND seen = 0",
                            params![account_id, bucket, prefix, upper],
                        )?
                    }
                };
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n)
    }

    /// Only synthetic application/x-directory markers; real trailing-slash objects are never touched.
    pub async fn cache_reconcile_dir_markers(
        &self,
        account_id: &str,
        bucket: &str,
        prefix: &str,
        keep: &[String],
    ) -> AppResult<usize> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let prefix = prefix.to_string();
        let keep: Vec<String> = keep.to_vec();
        let n = self
            .conn
            .call(move |conn| {
                let after = substr_offset_past_prefix(&prefix);
                let candidates: Vec<String> = if prefix.is_empty() {
                    let mut stmt = conn.prepare_cached(
                        "SELECT key FROM cached_objects
                         WHERE account_id = ?1 AND bucket = ?2
                           AND substr(key, -1) = '/'
                           AND instr(rtrim(key, '/'), '/') = 0
                           AND content_type = 'application/x-directory'",
                    )?;
                    // Propagate row errors: callers assume the set matches the DB.
                    let mut v = Vec::new();
                    for r in stmt.query_map(params![account_id, bucket], |row| row.get(0))? {
                        v.push(r?);
                    }
                    v
                } else {
                    let upper = prefix_upper_bound(&prefix);
                    let mut stmt = conn.prepare_cached(
                        "SELECT key FROM cached_objects
                         WHERE account_id = ?1 AND bucket = ?2
                           AND key > ?4 AND key < ?3
                           AND substr(key, -1) = '/'
                           AND instr(substr(key, ?5, length(key) - ?5), '/') = 0
                           AND content_type = 'application/x-directory'",
                    )?;
                    let mut v = Vec::new();
                    for r in stmt.query_map(params![account_id, bucket, upper, prefix, after], |row| row.get(0))? {
                        v.push(r?);
                    }
                    v
                };

                let keep_set: std::collections::HashSet<&str> =
                    keep.iter().map(String::as_str).collect();
                let mut removed = 0usize;
                for key in candidates {
                    if keep_set.contains(key.as_str()) {
                        continue;
                    }
                    removed += conn.execute(
                        "DELETE FROM cached_objects
                         WHERE account_id = ?1 AND bucket = ?2 AND key = ?3
                           AND content_type = 'application/x-directory'",
                        params![account_id, bucket, key],
                    )?;
                }
                Ok::<_, tokio_rusqlite::Error>(removed)
            })
            .await?;
        Ok(n)
    }
}
