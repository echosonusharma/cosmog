//! Capability discovery + recall for an account / bucket.
//!
//! Every permission field is tri-state: [`CapState::Allowed`] (confirmed by a
//! recent probe or successful real op), [`CapState::Denied`] (server returned
//! AccessDenied), or [`CapState::Unknown`] (never probed, or last probe was
//! inconclusive). The FE uses this to grey-out buttons the current credentials
//! cannot fulfill.
//!
//! Read-side caps are populated proactively via a probe (`list_buckets`,
//! `head_bucket`, etc.). Write-side caps are populated *reactively* — we
//! cannot do a dry-run PUT/DELETE without side effects, so we update
//! `last_put_result` / `last_delete_result` whenever a real operation
//! succeeds or fails with AccessDenied.

use std::sync::Arc;

use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::store::ObjectStore;

use super::Db;

/// Tri-state capability flag stored as `Option<bool>` in the DB.
///
/// Serialized as `"allowed"` / `"denied"` / `"unknown"` for FE simplicity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapState {
    Allowed,
    Denied,
    Unknown,
}

impl CapState {
    fn from_db(v: Option<i64>) -> Self {
        match v {
            Some(1) => CapState::Allowed,
            Some(0) => CapState::Denied,
            _ => CapState::Unknown,
        }
    }
    fn to_db(self) -> Option<i64> {
        match self {
            CapState::Allowed => Some(1),
            CapState::Denied => Some(0),
            CapState::Unknown => None,
        }
    }

    /// Map a single probe result into a tri-state. `Ok` → Allowed,
    /// `Err(AccessDenied)` → Denied, anything else → Unknown (could be a
    /// network blip; we don't want to lock the user out).
    fn from_probe<T>(result: &AppResult<T>) -> Self {
        match result {
            Ok(_) => CapState::Allowed,
            Err(AppError::AccessDenied(_)) => CapState::Denied,
            Err(_) => CapState::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountCapabilities {
    pub account_id: String,
    pub list_buckets: CapState,
    pub create_bucket: CapState,
    pub probed_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BucketCapabilities {
    pub account_id: String,
    pub bucket: String,
    pub head_bucket: CapState,
    pub list_objects: CapState,
    pub get_versioning: CapState,
    pub get_location: CapState,
    pub last_put_result: CapState,
    pub last_put_at: Option<i64>,
    pub last_delete_result: CapState,
    pub last_delete_at: Option<i64>,
    pub probed_at: Option<i64>,
}

impl Db {
    pub async fn account_capabilities_get(
        &self,
        account_id: &str,
    ) -> AppResult<Option<AccountCapabilities>> {
        let account_id = account_id.to_string();
        let row = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT account_id, list_buckets, create_bucket, probed_at FROM account_capabilities WHERE account_id = ?1",
                )?;
                let row: Option<(String, Option<i64>, Option<i64>, i64)> = stmt
                    .query_row(params![account_id], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })
                    .optional()?;
                Ok::<_, tokio_rusqlite::Error>(row)
            })
            .await?;
        Ok(row.map(|(id, lb, cb, ts)| AccountCapabilities {
            account_id: id,
            list_buckets: CapState::from_db(lb),
            create_bucket: CapState::from_db(cb),
            probed_at: ts,
        }))
    }

    pub async fn account_capabilities_upsert(
        &self,
        caps: &AccountCapabilities,
    ) -> AppResult<()> {
        let account_id = caps.account_id.clone();
        let lb = caps.list_buckets.to_db();
        let cb = caps.create_bucket.to_db();
        let probed_at = caps.probed_at;
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO account_capabilities (account_id, list_buckets, create_bucket, probed_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(account_id) DO UPDATE SET
                        list_buckets = excluded.list_buckets,
                        create_bucket = excluded.create_bucket,
                        probed_at = excluded.probed_at",
                    params![account_id, lb, cb, probed_at],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    pub async fn bucket_capabilities_get(
        &self,
        account_id: &str,
        bucket: &str,
    ) -> AppResult<Option<BucketCapabilities>> {
        let account_id_q = account_id.to_string();
        let bucket_q = bucket.to_string();
        let row = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT head_bucket, list_objects, get_versioning, get_location,
                            last_put_result, last_put_at, last_delete_result, last_delete_at, probed_at
                       FROM bucket_capabilities WHERE account_id = ?1 AND bucket = ?2",
                )?;
                let row: Option<(
                    Option<i64>, Option<i64>, Option<i64>, Option<i64>,
                    Option<String>, Option<i64>, Option<String>, Option<i64>,
                    Option<i64>,
                )> = stmt
                    .query_row(params![account_id_q, bucket_q], |row| {
                        Ok((
                            row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                            row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?,
                            row.get(8)?,
                        ))
                    })
                    .optional()?;
                Ok::<_, tokio_rusqlite::Error>(row)
            })
            .await?;
        Ok(row.map(|(hb, lo, gv, gl, lpr, lpa, ldr, lda, pa)| BucketCapabilities {
            account_id: account_id.to_string(),
            bucket: bucket.to_string(),
            head_bucket: CapState::from_db(hb),
            list_objects: CapState::from_db(lo),
            get_versioning: CapState::from_db(gv),
            get_location: CapState::from_db(gl),
            last_put_result: parse_result_str(lpr.as_deref()),
            last_put_at: lpa,
            last_delete_result: parse_result_str(ldr.as_deref()),
            last_delete_at: lda,
            probed_at: pa,
        }))
    }

    pub async fn bucket_capabilities_upsert(
        &self,
        caps: &BucketCapabilities,
    ) -> AppResult<()> {
        let c = caps.clone();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO bucket_capabilities (
                        account_id, bucket, head_bucket, list_objects, get_versioning, get_location,
                        last_put_result, last_put_at, last_delete_result, last_delete_at, probed_at
                     )
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                     ON CONFLICT(account_id, bucket) DO UPDATE SET
                        head_bucket = excluded.head_bucket,
                        list_objects = excluded.list_objects,
                        get_versioning = excluded.get_versioning,
                        get_location = excluded.get_location,
                        probed_at = excluded.probed_at",
                    params![
                        c.account_id,
                        c.bucket,
                        c.head_bucket.to_db(),
                        c.list_objects.to_db(),
                        c.get_versioning.to_db(),
                        c.get_location.to_db(),
                        cap_to_str(c.last_put_result),
                        c.last_put_at,
                        cap_to_str(c.last_delete_result),
                        c.last_delete_at,
                        c.probed_at,
                    ],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    /// Record the outcome of a real write op. Only updates the relevant
    /// `last_*` columns; doesn't disturb probe-derived read caps.
    pub async fn capability_record_write(
        &self,
        account_id: &str,
        bucket: &str,
        op: WriteOp,
        state: CapState,
    ) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let now = Utc::now().timestamp();
        let state_str = cap_to_str(state);
        self.conn
            .call(move |conn| {
                let (result_col, at_col) = match op {
                    WriteOp::Put => ("last_put_result", "last_put_at"),
                    WriteOp::Delete => ("last_delete_result", "last_delete_at"),
                };
                // INSERT-OR-UPDATE; only touches the one pair of columns for op.
                let sql = format!(
                    "INSERT INTO bucket_capabilities (account_id, bucket, {result_col}, {at_col})
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(account_id, bucket) DO UPDATE SET
                        {result_col} = excluded.{result_col},
                        {at_col} = excluded.{at_col}"
                );
                conn.execute(&sql, params![account_id, bucket, state_str, now])?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum WriteOp {
    Put,
    Delete,
}

fn cap_to_str(s: CapState) -> Option<&'static str> {
    match s {
        CapState::Allowed => Some("allowed"),
        CapState::Denied => Some("denied"),
        CapState::Unknown => None,
    }
}

fn parse_result_str(s: Option<&str>) -> CapState {
    match s {
        Some("allowed") => CapState::Allowed,
        Some("denied") => CapState::Denied,
        _ => CapState::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Probes
// ---------------------------------------------------------------------------

/// Probe the account-level capabilities. Always runs `list_buckets`. To probe
/// `create_bucket` we'd need to actually create one — there is no dry-run —
/// so we leave it `Unknown` here and update it reactively if the user ever
/// hits the button.
pub async fn probe_account(
    store: Arc<dyn ObjectStore>,
    account_id: &str,
) -> AppResult<AccountCapabilities> {
    let list_result = store.list_buckets().await;
    let list_buckets = CapState::from_probe(&list_result);

    Ok(AccountCapabilities {
        account_id: account_id.to_string(),
        list_buckets,
        create_bucket: CapState::Unknown,
        probed_at: Utc::now().timestamp(),
    })
}

/// Probe a bucket's read-side capabilities. Each call independently catches
/// `AccessDenied` so a partial-permissions key gets an accurate map.
pub async fn probe_bucket(
    store: Arc<dyn ObjectStore>,
    account_id: &str,
    bucket: &str,
) -> AppResult<BucketCapabilities> {
    let head = store.head_bucket(bucket).await;
    let head_bucket = CapState::from_probe(&head);

    let list = store
        .list_objects(
            bucket,
            crate::store::ListOptions {
                prefix: None,
                delimiter: Some("/".into()),
                continuation: None,
                max_keys: Some(1),
            },
        )
        .await;
    let list_objects = CapState::from_probe(&list);

    let versioning = store.get_bucket_versioning(bucket).await;
    let get_versioning = CapState::from_probe(&versioning);

    let location = store.get_bucket_location(bucket).await;
    let get_location = CapState::from_probe(&location);

    Ok(BucketCapabilities {
        account_id: account_id.to_string(),
        bucket: bucket.to_string(),
        head_bucket,
        list_objects,
        get_versioning,
        get_location,
        last_put_result: CapState::Unknown,
        last_put_at: None,
        last_delete_result: CapState::Unknown,
        last_delete_at: None,
        probed_at: Some(Utc::now().timestamp()),
    })
}
