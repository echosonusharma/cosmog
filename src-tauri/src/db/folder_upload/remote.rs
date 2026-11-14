//! Remote check bookkeeping on `folder_upload_jobs`: each check has a generation, and only
//! the current one may write its results.

use chrono::Utc;
use rusqlite::{params, OptionalExtension};

use super::RemoteState;
use crate::db::Db;
use crate::error::AppResult;

impl Db {
    /// Starts a check of `prefix` on an analyzed job: returns its generation, None if the
    /// destination changed or the job left `analyzed`.
    pub async fn fu_begin_remote(&self, id: &str, prefix: &str) -> AppResult<Option<i64>> {
        let (id, prefix) = (id.to_string(), prefix.to_string());
        let now = Utc::now().timestamp();
        let gen = self
            .conn
            .call(move |conn| {
                let tx = conn.transaction()?;
                let gen: Option<i64> = tx
                    .query_row(
                        "UPDATE folder_upload_jobs SET remote_state = 'checking', remote_prefix = NULL, remote_existing = 0,
                            remote_existing_bytes = 0, remote_versioning = NULL, remote_gen = remote_gen + 1, updated_at = ?3
                         WHERE id = ?1 AND dest_prefix = ?2 AND state = 'analyzed' RETURNING remote_gen",
                        params![id, prefix, now],
                        |r| r.get(0),
                    )
                    .optional()?;
                if gen.is_some() {
                    // Flags from an earlier check of the same prefix must not leak into this one.
                    tx.execute(
                        "UPDATE folder_upload_items SET remote_exists = 0, remote_size = NULL, remote_mtime = NULL
                         WHERE job_id = ?1 AND (remote_exists != 0 OR remote_size IS NOT NULL OR remote_mtime IS NOT NULL)",
                        params![id],
                    )?;
                }
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(gen)
            })
            .await?;
        Ok(gen)
    }

    /// Records the outcome of check `gen` for `prefix`; ignored once superseded. Returns whether it applied.
    #[allow(clippy::too_many_arguments)]
    pub async fn fu_finish_remote(
        &self,
        id: &str,
        gen: i64,
        prefix: &str,
        state: RemoteState,
        existing: i64,
        bytes: i64,
        versioning: Option<String>,
    ) -> AppResult<bool> {
        let id = id.to_string();
        let prefix = prefix.to_string();
        let now = Utc::now().timestamp();
        let n = self
            .conn
            .call(move |conn| {
                let n = conn.execute(
                    "UPDATE folder_upload_jobs SET remote_state = ?3, remote_prefix = ?2, remote_existing = ?4,
                        remote_existing_bytes = ?5, remote_versioning = ?6, updated_at = ?7
                     WHERE id = ?1 AND dest_prefix = ?2 AND remote_gen = ?8 AND state = 'analyzed'",
                    params![id, prefix, state, existing, bytes, versioning, now, gen],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n > 0)
    }
}
