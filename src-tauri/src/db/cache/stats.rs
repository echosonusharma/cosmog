use rusqlite::params;
use serde::Serialize;

use crate::db::Db;
use crate::error::AppResult;

#[derive(Debug, Clone, Default, Serialize)]
pub struct BucketStats {
    pub object_count: i64,
    pub total_bytes: i64,
    pub by_storage_class: Vec<StorageClassStat>,
    pub by_extension: Vec<FileTypeStat>,
    // Distinct extensions, independent of the top-N cap on by_extension.
    pub extension_count: i64,
    pub by_month: Vec<MonthStat>,
    pub largest: Vec<LargeObjectStat>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageClassStat {
    pub storage_class: String,
    pub object_count: i64,
    pub total_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileTypeStat {
    pub extension: String,
    pub object_count: i64,
    pub total_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonthStat {
    pub month: String,
    pub object_count: i64,
    pub total_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LargeObjectStat {
    pub key: String,
    pub basename: String,
    pub size: i64,
}

impl Db {
    /// Cached rows only: accurate after a full bucket scan.
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
                // Empty extension folds into "(none)".
                let mut stmt = conn.prepare(
                    "SELECT COALESCE(NULLIF(extension, ''), '(none)'), COUNT(*), COALESCE(SUM(size), 0)
                       FROM cached_objects
                       WHERE account_id = ?1 AND bucket = ?2
                       GROUP BY COALESCE(NULLIF(extension, ''), '(none)')
                       ORDER BY SUM(size) DESC
                       LIMIT 20",
                )?;
                let iter = stmt.query_map(params![account_id, bucket], |row| {
                    Ok(FileTypeStat {
                        extension: row.get(0)?,
                        object_count: row.get(1)?,
                        total_bytes: row.get(2)?,
                    })
                })?;
                for r in iter {
                    stats.by_extension.push(r?);
                }
                // by_extension is capped, so its length under-reports the distinct count.
                stats.extension_count = conn.query_row(
                    "SELECT COUNT(DISTINCT COALESCE(NULLIF(extension, ''), '(none)'))
                       FROM cached_objects
                       WHERE account_id = ?1 AND bucket = ?2",
                    params![account_id, bucket],
                    |row| row.get(0),
                )?;
                let mut stmt = conn.prepare(
                    "SELECT strftime('%Y-%m', last_modified, 'unixepoch'), COUNT(*), COALESCE(SUM(size), 0)
                       FROM cached_objects
                       WHERE account_id = ?1 AND bucket = ?2 AND last_modified IS NOT NULL
                       GROUP BY 1
                       ORDER BY 1 ASC",
                )?;
                let iter = stmt.query_map(params![account_id, bucket], |row| {
                    Ok(MonthStat {
                        month: row.get(0)?,
                        object_count: row.get(1)?,
                        total_bytes: row.get(2)?,
                    })
                })?;
                for r in iter {
                    stats.by_month.push(r?);
                }
                let mut stmt = conn.prepare(
                    "SELECT key, basename, size
                       FROM cached_objects
                       WHERE account_id = ?1 AND bucket = ?2
                       ORDER BY size DESC
                       LIMIT 10",
                )?;
                let iter = stmt.query_map(params![account_id, bucket], |row| {
                    Ok(LargeObjectStat {
                        key: row.get(0)?,
                        basename: row.get(1)?,
                        size: row.get(2)?,
                    })
                })?;
                for r in iter {
                    stats.largest.push(r?);
                }
                Ok::<_, tokio_rusqlite::Error>(stats)
            })
            .await?;
        Ok(stats)
    }
}
