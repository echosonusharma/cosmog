//! Search + indexing Tauri commands.
//!
//! All search runs locally against [`crate::db::cache`]. Sync commands talk to
//! S3 to refresh the cache. The full-bucket scan is cancellable + resumable.

use tauri::ipc::Channel;
use tauri::State;

use crate::db::cache::{BucketIndexStatus, BucketStats, SearchQuery, SearchResult};
use crate::error::AppResult;
use crate::state::AppState;
use crate::sync::{full_bucket_scan, sync_prefix_direct, sync_prefix_recursive, SyncStats};
use crate::transfer::{ProgressSink, TransferEvent};
use crate::validate;

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn search_objects(
    state: State<'_, AppState>,
    query: SearchQuery,
) -> AppResult<SearchResult> {
    let account_id = validate::require_non_empty("account_id", &query.account_id)?;
    let bucket = validate::require_non_empty("bucket", &query.bucket)?;
    let mut q = query;
    q.account_id = account_id;
    q.bucket = bucket;
    state.db.search_objects(q).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn sync_prefix(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
    recursive: bool,
) -> AppResult<SyncStats> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    let store = state.store_for(&account_id).await?;
    if recursive {
        sync_prefix_recursive(&state.db, store, &account_id, &bucket, &prefix).await
    } else {
        sync_prefix_direct(&state.db, store, &account_id, &bucket, &prefix).await
    }
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn bucket_index_status(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> AppResult<BucketIndexStatus> {
    state.db.bucket_index_get(&account_id, &bucket).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn enable_bucket_index(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    on_event: Channel<TransferEvent>,
) -> AppResult<SyncStats> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;

    state
        .db
        .bucket_index_set_enabled(&account_id, &bucket, true)
        .await?;

    let store = state.store_for(&account_id).await?;
    let sink = ProgressSink::from_fn(move |event| {
        let _ = on_event.send(event);
    });
    let scan_id = uuid::Uuid::new_v4().to_string();

    let cancel = state.register_scan(&account_id, &bucket);
    let result = full_bucket_scan(
        &state.db,
        store,
        &account_id,
        &bucket,
        sink,
        scan_id,
        cancel,
    )
    .await;
    state.unregister_scan(&account_id, &bucket);
    result
}

/// Cancel an in-flight bucket scan. Idempotent — succeeds when no scan is
/// running. The current page completes, the continuation token is persisted,
/// and the scan can be resumed by calling [`enable_bucket_index`] again.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn cancel_bucket_scan(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> AppResult<()> {
    state.cancel_scan(&account_id, &bucket);
    Ok(())
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn disable_bucket_index(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> AppResult<()> {
    // Cancel any running scan first so it doesn't fight us writing to the
    // cleared table.
    state.cancel_scan(&account_id, &bucket);
    state.db.cache_clear_bucket(&account_id, &bucket).await
}

/// Enable or disable automatic periodic re-indexing for a bucket. Pass
/// `None` for `secs` to disable; pass `Some(N)` to re-scan whenever the
/// last full sync is older than N seconds. The scheduler polls once per
/// minute, so the effective resolution is ~60s.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn set_bucket_auto_reindex(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    secs: Option<i64>,
) -> AppResult<()> {
    state
        .db
        .bucket_index_set_auto_reindex(&account_id, &bucket, secs)
        .await
}

/// Aggregated stats over whatever is currently cached for a bucket. Accurate
/// only after a full bucket scan; otherwise reflects the partial index.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn bucket_stats(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> AppResult<BucketStats> {
    state.db.bucket_stats(&account_id, &bucket).await
}
