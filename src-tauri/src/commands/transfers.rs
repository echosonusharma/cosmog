//! Persistent transfer queue commands: each enqueue takes a progress
//! `Channel<TransferEvent>`; local paths are validated before any FS/S3 work.

use tauri::ipc::Channel;
use tauri::State;

use crate::db::transfers::{Transfer, TransferStatus};
use crate::error::AppResult;
use crate::state::AppState;
use crate::store::{GetOptions, PutOptions};
use crate::transfer::{ProgressSink, TransferEvent, TransferManager};
use crate::validate;

fn channel_sink(channel: Channel<TransferEvent>) -> ProgressSink {
    ProgressSink::from_fn(move |event| {
        // Send fails silently once the FE drops the receiver; the worker is
        // the source of truth and continues.
        let _ = channel.send(event);
    })
}

#[derive(Debug, serde::Serialize)]
pub struct EnqueueResult {
    pub transfer_id: String,
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn enqueue_upload(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    local_path: String,
    options: Option<PutOptions>,
    on_event: Channel<TransferEvent>,
) -> AppResult<EnqueueResult> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    let key = validate::require_non_empty("key", &key)?;
    let path = validate::validate_upload_source(&local_path).await?;

    let mut opts = options.unwrap_or_default();
    // Encryption (recipient + temp dir) is backend-decided; never accept it from IPC.
    opts.encrypt = None;
    crate::transfer::encrypt::encrypt_for_bucket_if_needed(&state, &account_id, &bucket, &path, &mut opts)
        .await?;

    let store = state.store_for(&account_id).await?;
    let id = state
        .transfers
        .enqueue_upload(
            store,
            account_id,
            bucket,
            key,
            path,
            opts,
            channel_sink(on_event),
            crate::db::transfers::TransferOrigin::User,
        )
        .await?;
    Ok(EnqueueResult { transfer_id: id })
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn enqueue_download(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    local_path: String,
    version_id: Option<String>,
    on_event: Channel<TransferEvent>,
) -> AppResult<EnqueueResult> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    let key = validate::require_non_empty("key", &key)?;
    let path = validate::validate_download_dest(&local_path).await?;

    let store = state.store_for(&account_id).await?;
    let id = state
        .transfers
        .enqueue_download(
            store,
            account_id,
            bucket,
            key,
            path,
            GetOptions {
                version_id,
                ..Default::default()
            },
            channel_sink(on_event),
        )
        .await?;
    Ok(EnqueueResult { transfer_id: id })
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn list_transfers(
    state: State<'_, AppState>,
    status: Option<TransferStatus>,
) -> AppResult<Vec<Transfer>> {
    state.transfers.list(status).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn get_transfer(state: State<'_, AppState>, id: String) -> AppResult<Transfer> {
    state.transfers.get(&id).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn cancel_transfer(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.transfers.cancel_or_reap(&id).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn retry_transfer(
    state: State<'_, AppState>,
    id: String,
    on_event: Channel<TransferEvent>,
) -> AppResult<EnqueueResult> {
    let row: Transfer = state.transfers.get(&id).await?;
    let store = state.store_for(&row.account_id).await?;
    let new_id: String = TransferManager::retry(&state.transfers, store, &id, channel_sink(on_event)).await?;
    Ok(EnqueueResult { transfer_id: new_id })
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn clear_completed_transfers(state: State<'_, AppState>) -> AppResult<usize> {
    for row in state.db.list_finished_with_upload().await? {
        abort_leftover_upload(&state, &row).await;
    }
    state.transfers.clear_completed().await
}

/// Crash-reaped rows still own a multipart upload; abort it (best-effort) before the
/// row, the only record of its upload_id, is deleted.
async fn abort_leftover_upload(state: &AppState, row: &Transfer) {
    let Some(upload_id) = row.upload_id.as_deref() else { return };
    if !matches!(row.status, TransferStatus::Done | TransferStatus::Failed | TransferStatus::Canceled) {
        return;
    }
    if let Ok(store) = state.store_for(&row.account_id).await {
        if let Err(e) = store.abort_multipart_upload(&row.bucket, &row.key, upload_id).await {
            tracing::warn!(transfer_id = %row.id, "abort leftover multipart failed: {e}");
        }
    }
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn clear_transfer(state: State<'_, AppState>, id: String) -> AppResult<()> {
    if let Ok(row) = state.transfers.get(&id).await {
        abort_leftover_upload(&state, &row).await;
    }
    state.transfers.delete_one(&id).await
}
