//! Bulk-op Tauri commands forwarding lifecycle events through a Channel. These stream
//! parent-op events only; per-file progress rides each file's own enqueue sink.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::ipc::Channel;
use tauri::State;
use uuid::Uuid;

use crate::bulk::{
    delete_folder, download_directory, upload_directory, BulkDeleteResult, BulkTransferResult,
};
use crate::error::AppResult;
use crate::state::AppState;
use crate::transfer::{ProgressSink, TransferEvent};
use crate::validate;

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn delete_folder_cmd(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
    on_event: Channel<TransferEvent>,
) -> AppResult<BulkDeleteResult> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    let prefix = validate::require_non_empty("prefix", &prefix)?;
    let store = state.store_for(&account_id).await?;

    let sink = ProgressSink::from_fn(move |event| {
        let _ = on_event.send(event);
    });
    let transfer_id = Uuid::new_v4().to_string();
    // Own registry so bulk cancels can't collide with scans or account-delete kills.
    let cancel = state.register_bulk(&transfer_id);
    let result = delete_folder(
        &state.db,
        store,
        &account_id,
        &bucket,
        &prefix,
        sink,
        transfer_id.clone(),
        cancel,
    )
    .await;
    state.unregister_bulk(&transfer_id);
    result
}

/// Cancel a previously-started bulk operation by the id returned in its
/// progress events (`Started.transfer_id`). Idempotent.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn cancel_bulk_op(state: State<'_, AppState>, op_id: String) -> AppResult<()> {
    state.cancel_bulk(&op_id);
    Ok(())
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn upload_directory_cmd(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
    local_root: String,
    on_event: Channel<TransferEvent>,
) -> AppResult<BulkTransferResult> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    let local_root = PathBuf::from(local_root);
    let store = state.store_for(&account_id).await?;

    // One channel reused for every file; events carry per-file transfer_ids.
    let channel = Arc::new(on_event);
    let factory = move |_key: &str| {
        let channel = channel.clone();
        ProgressSink::from_fn(move |event| {
            let _ = channel.send(event);
        })
    };
    let transfer_id = Uuid::new_v4().to_string();
    let cancel = state.register_bulk(&transfer_id);
    let result = upload_directory(
        &state.transfers,
        &state.db,
        &state.db_path,
        store,
        &account_id,
        &bucket,
        &prefix,
        &local_root,
        factory,
        cancel,
    )
    .await;
    state.unregister_bulk(&transfer_id);
    result
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn download_directory_cmd(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
    local_root: String,
    on_event: Channel<TransferEvent>,
) -> AppResult<BulkTransferResult> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    let prefix = validate::require_non_empty("prefix", &prefix)?;
    let local_root = PathBuf::from(local_root);
    let store = state.store_for(&account_id).await?;

    let channel = Arc::new(on_event);
    let factory = move |_key: &str| {
        let channel = channel.clone();
        ProgressSink::from_fn(move |event| {
            let _ = channel.send(event);
        })
    };
    // Same bulk registry wiring so cancel_bulk_op can abort running downloads too.
    let transfer_id = Uuid::new_v4().to_string();
    let cancel = state.register_bulk(&transfer_id);
    let result = download_directory(
        &state.transfers,
        store,
        &account_id,
        &bucket,
        &prefix,
        &local_root,
        factory,
        cancel,
    )
    .await;
    state.unregister_bulk(&transfer_id);
    result
}
