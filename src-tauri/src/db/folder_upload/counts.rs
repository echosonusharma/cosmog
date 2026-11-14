//! Item counts from the trigger-kept `folder_upload_counts` table (no scan of the items).

use rusqlite::params;

use super::ItemCounts;
use crate::db::Db;
use crate::error::AppResult;

impl Db {
    /// From the trigger-kept counts table plus one index seek; no scan of the items.
    pub async fn fu_counts(&self, job_id: &str) -> AppResult<ItemCounts> {
        Ok(self.fu_counts_and_bytes(job_id).await?.0)
    }

    /// Sum of local sizes of done file items.
    pub async fn fu_done_bytes(&self, job_id: &str) -> AppResult<i64> {
        Ok(self.fu_counts_and_bytes(job_id).await?.1)
    }

    pub async fn fu_counts_and_bytes(&self, job_id: &str) -> AppResult<(ItemCounts, i64)> {
        let job_id = job_id.to_string();
        let c = self
            .conn
            .call(move |conn| {
                let mut c = ItemCounts::default();
                let mut done_bytes = 0;
                let mut stmt = conn.prepare_cached(
                    "SELECT state, kind, reason, drift, late, n, bytes FROM folder_upload_counts WHERE job_id = ?1 AND n != 0",
                )?;
                let mut rows = stmt.query(params![job_id])?;
                while let Some(r) = rows.next()? {
                    let (state, kind, reason): (String, String, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
                    let (drift, late, n, bytes): (bool, bool, i64, i64) = (r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?);
                    match state.as_str() {
                        "pending" => c.pending += n,
                        "uploading" => c.uploading += n,
                        "done" => c.done += n,
                        "skipped_exists" => c.skipped_exists += n,
                        "skipped_missing" => c.skipped_missing += n,
                        "failed" => c.failed += n,
                        "canceled" => c.canceled += n,
                        "excluded" => c.excluded += n,
                        _ => {}
                    }
                    if state == "done" && kind == "file" {
                        c.done_files += n;
                        done_bytes += bytes;
                    }
                    if drift {
                        c.drift += n;
                    }
                    if late {
                        c.appeared_after_check += n;
                    }
                    if state == "excluded" && reason == "key_too_long" {
                        c.key_too_long += n;
                    }
                    if state == "excluded" && drift {
                        c.excluded_late += n;
                    }
                }
                c.next_due_at = conn
                    .prepare_cached("SELECT MIN(next_attempt_at) FROM folder_upload_items WHERE job_id = ?1 AND state = 'pending'")?
                    .query_row(params![job_id], |r| r.get(0))?;
                Ok::<_, tokio_rusqlite::Error>((c, done_bytes))
            })
            .await?;
        Ok(c)
    }
}
