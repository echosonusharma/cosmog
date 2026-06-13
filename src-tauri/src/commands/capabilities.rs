//! Capability discovery commands. The FE calls [`probe_account_capabilities`]
//! once after add-account, and [`probe_bucket_capabilities`] when the user
//! opens a bucket. Results are cached in the DB and re-queried via
//! [`get_account_capabilities`] / [`get_bucket_capabilities`].

use tauri::State;

use crate::db::capabilities::{
    probe_account, probe_bucket, AccountCapabilities, BucketCapabilities,
};
use crate::error::AppResult;
use crate::state::AppState;
use crate::validate;

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn probe_account_capabilities(
    state: State<'_, AppState>,
    account_id: String,
) -> AppResult<AccountCapabilities> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let store = state.store_for(&account_id).await?;
    let caps = probe_account(store, &account_id).await?;
    state.db.account_capabilities_upsert(&caps).await?;
    Ok(caps)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn probe_bucket_capabilities(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> AppResult<BucketCapabilities> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    let store = state.store_for(&account_id).await?;
    let caps = probe_bucket(store, &account_id, &bucket).await?;
    state.db.bucket_capabilities_upsert(&caps).await?;
    Ok(caps)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn get_account_capabilities(
    state: State<'_, AppState>,
    account_id: String,
) -> AppResult<Option<AccountCapabilities>> {
    state.db.account_capabilities_get(&account_id).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn get_bucket_capabilities(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> AppResult<Option<BucketCapabilities>> {
    state.db.bucket_capabilities_get(&account_id, &bucket).await
}
