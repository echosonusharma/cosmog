use rusqlite::params;

use super::{prefix_upper_bound, row_to_cached, substr_offset_past_prefix, CachedObjectMeta};
use crate::db::Db;
use crate::error::AppResult;

impl Db {
    /// Returns (files, subprefixes, has_more) for direct children via key range + instr; subprefixes only when `offset == 0`.
    pub async fn browse_children(
        &self,
        account_id: &str,
        bucket: &str,
        prefix: &str,
        offset: i64,
    ) -> AppResult<(Vec<CachedObjectMeta>, Vec<String>, bool)> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let prefix = prefix.to_string();
        let offset = offset.max(0);

        const FILE_LIMIT: usize = 5_000;

        self.conn
            .call(move |conn| {
                // Character-based offset; see [`substr_offset_past_prefix`].
                let after = substr_offset_past_prefix(&prefix);
                // Fetch one extra row to detect a further page without a COUNT.
                let fetch_limit = (FILE_LIMIT + 1) as i64;
                let first_page = offset == 0;

                let (mut files, subprefixes) = if prefix.is_empty() {
                    let files: Vec<CachedObjectMeta> = {
                        let mut stmt = conn.prepare_cached(
                            "SELECT co.account_id, co.bucket, co.key, co.size, co.etag, co.last_modified, co.storage_class, co.content_type, co.extension, co.basename, co.version_id, co.synced_at
                             FROM cached_objects co
                             WHERE co.account_id = ?1 AND co.bucket = ?2
                               AND instr(co.key, '/') = 0
                             ORDER BY co.key LIMIT ?3 OFFSET ?4",
                        )?;
                        let mut v = Vec::new();
                        for r in stmt.query_map(params![account_id, bucket, fetch_limit, offset], row_to_cached)? {
                            v.push(r?);
                        }
                        v
                    };
                    let subprefixes: Vec<String> = if first_page {
                        let mut stmt = conn.prepare_cached(
                            "SELECT DISTINCT substr(co.key, 1, instr(co.key, '/')) AS folder
                             FROM cached_objects co
                             WHERE co.account_id = ?1 AND co.bucket = ?2
                               AND instr(co.key, '/') > 0
                             ORDER BY folder",
                        )?;
                        let mut v = Vec::new();
                        for r in stmt.query_map(params![account_id, bucket], |row| row.get(0))? {
                            v.push(r?);
                        }
                        v
                    } else {
                        Vec::new()
                    };
                    (files, subprefixes)
                } else {
                    let upper = prefix_upper_bound(&prefix);
                    let files: Vec<CachedObjectMeta> = {
                        let mut stmt = conn.prepare_cached(
                            "SELECT co.account_id, co.bucket, co.key, co.size, co.etag, co.last_modified, co.storage_class, co.content_type, co.extension, co.basename, co.version_id, co.synced_at
                             FROM cached_objects co
                             WHERE co.account_id = ?1 AND co.bucket = ?2
                               AND co.key > ?4 AND co.key < ?3
                               AND instr(substr(co.key, ?5), '/') = 0
                             ORDER BY co.key LIMIT ?6 OFFSET ?7",
                        )?;
                        let mut v = Vec::new();
                        for r in stmt.query_map(params![account_id, bucket, upper, prefix, after, fetch_limit, offset], row_to_cached)? {
                            v.push(r?);
                        }
                        v
                    };
                    let subprefixes: Vec<String> = if first_page {
                        let mut stmt = conn.prepare_cached(
                            "SELECT DISTINCT substr(co.key, 1, (?5 - 1) + instr(substr(co.key, ?5), '/')) AS folder
                             FROM cached_objects co
                             WHERE co.account_id = ?1 AND co.bucket = ?2
                               AND co.key >= ?4 AND co.key < ?3
                               AND instr(substr(co.key, ?5), '/') > 0
                             ORDER BY folder",
                        )?;
                        let mut v = Vec::new();
                        for r in stmt.query_map(params![account_id, bucket, upper, prefix, after], |row| row.get(0))? {
                            v.push(r?);
                        }
                        v
                    } else {
                        Vec::new()
                    };
                    (files, subprefixes)
                };

                let has_more = files.len() > FILE_LIMIT;
                if has_more { files.truncate(FILE_LIMIT); }

                Ok::<_, tokio_rusqlite::Error>((files, subprefixes, has_more))
            })
            .await
            .map_err(Into::into)
    }
}
