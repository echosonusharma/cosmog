//! Search + indexing commands. Search runs locally against the cache; sync
//! and full-bucket-scan commands refresh it from S3 (scans cancellable/resumable).

use tauri::ipc::Channel;
use tauri::State;

use crate::db::cache::{BucketIndexStatus, BucketStats, SearchQuery, SearchResult};
use crate::error::{AppError, AppResult};
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

    // Acquire the store before claiming the slot: a failure bails with the
    // slot still free (claiming first would leak it on the `?`).
    let store = state.store_for(&account_id).await?;

    // Prefix syncs and full scans both run mark-unseen -> upsert -> sweep;
    // concurrent runs would delete each other's rows. Refuse overlap.
    if state.scan_in_flight(&account_id, &bucket) {
        return Err(AppError::Conflict(format!(
            "a full index scan is running for {bucket}; try again once it finishes"
        )));
    }
    // Atomically claim the prefix slot so overlapping syncs (FE double-invoke)
    // don't corrupt each other's sweep.
    if !state.claim_prefix_sync(&account_id, &bucket, &prefix) {
        return Err(AppError::Conflict(format!(
            "a sync is already running for this prefix in {bucket}"
        )));
    }

    let result = if recursive {
        sync_prefix_recursive(&state.db, store, &account_id, &bucket, &prefix).await
    } else {
        sync_prefix_direct(&state.db, store, &account_id, &bucket, &prefix).await
    };
    state.unregister_prefix_sync(&account_id, &bucket, &prefix);
    result
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

    // A concurrent prefix sync races the scan's mark-unseen/sweep. Refuse.
    if state.prefix_sync_in_flight_for_bucket(&account_id, &bucket) {
        return Err(AppError::Conflict(format!(
            "a prefix sync is running for {bucket}; try again once it finishes"
        )));
    }
    // Acquire the store before claiming the scan slot so a client-build failure
    // can't leak the slot on the `?`.
    let store = state.store_for(&account_id).await?;

    // Atomically claim the scan slot: `None` means a scan is already in flight
    // and a second walk would corrupt the shared seen markers.
    let cancel = state.try_register_scan(&account_id, &bucket).ok_or_else(|| {
        AppError::Conflict(format!("an index scan is already running for {bucket}"))
    })?;

    // From here the slot is held: unregister on any early-return error path.
    if let Err(e) = state.db.bucket_index_set_enabled(&account_id, &bucket, true).await {
        state.unregister_scan(&account_id, &bucket);
        return Err(e);
    }

    let sink = ProgressSink::from_fn(move |event| {
        let _ = on_event.send(event);
    });
    let scan_id = uuid::Uuid::new_v4().to_string();

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

/// Cancels an in-flight bucket scan (idempotent). The current page completes,
/// the continuation token is persisted, and `enable_bucket_index` can resume.
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

/// Forces a fresh full-bucket scan, discarding any in-progress continuation
/// token so it starts from page 1.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn reindex_bucket(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    on_event: Channel<TransferEvent>,
) -> AppResult<SyncStats> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;

    if state.prefix_sync_in_flight_for_bucket(&account_id, &bucket) {
        return Err(AppError::Conflict(format!(
            "a prefix sync is running for {bucket}; try again once it finishes"
        )));
    }

    // Acquire the store before claiming the scan slot so a client-build failure
    // can't leak the slot on the `?`.
    let store = state.store_for(&account_id).await?;

    // Signal any in-flight scan to stop; it unwinds on its next page and
    // frees the scan slot.
    state.cancel_scan(&account_id, &bucket);
    let cancel = state.try_register_scan(&account_id, &bucket).ok_or_else(|| {
        AppError::Conflict(format!(
            "the previous scan for {bucket} is still stopping; try again in a moment"
        ))
    })?;

    // From here the slot is held: unregister on any early-return error path.
    // Wipe the continuation token so the scan starts fresh instead of resuming.
    if let Err(e) = state.db.bucket_scan_clear(&account_id, &bucket).await {
        state.unregister_scan(&account_id, &bucket);
        return Err(e);
    }
    if let Err(e) = state.db.bucket_index_set_enabled(&account_id, &bucket, true).await {
        state.unregister_scan(&account_id, &bucket);
        return Err(e);
    }

    let sink = ProgressSink::from_fn(move |event| {
        let _ = on_event.send(event);
    });
    let scan_id = uuid::Uuid::new_v4().to_string();
    let result = full_bucket_scan(&state.db, store, &account_id, &bucket, sink, scan_id, cancel).await;
    state.unregister_scan(&account_id, &bucket);
    result
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

/// Sets automatic periodic re-indexing: `None` disables; `Some(N)` re-scans
/// when the last full sync is older than N secs (scheduler polls ~60s).
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

/// Stats over whatever is currently cached; accurate only after a full scan.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn bucket_stats(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> AppResult<BucketStats> {
    state.db.bucket_stats(&account_id, &bucket).await
}
