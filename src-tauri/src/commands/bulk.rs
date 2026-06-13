//! Folder-scoped bulk commands: recursive delete, recursive upload, recursive
//! download.
//!
//! Each command takes a [`Channel<TransferEvent>`] and forwards every
//! lifecycle event through it. For `upload_directory` / `download_directory`
//! the events stream the *parent* operation only (start/done/cancel/fail);
//! the per-file progress events go through each file's own enqueue-result
//! channel (which is internal to the bulk job here — the FE typically listens
//! on `list_transfers` and the transfer-level event channel for fine-grained
//! UI).

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
    // Bulk ops use their own registry so the cancel paths can't collide with
    // bucket scans or accidentally kill an unrelated job on account delete.
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

    // The same channel is reused for every file in the directory. Each file
    // emits its own events tagged with its own transfer_id, so the FE can
    // distinguish them.
    let channel = Arc::new(on_event);
    let factory = move |_key: &str| {
        let channel = channel.clone();
        ProgressSink::from_fn(move |event| {
            let _ = channel.send(event);
        })
    };
    upload_directory(
        &state.transfers,
        store,
        &account_id,
        &bucket,
        &prefix,
        &local_root,
        factory,
    )
    .await
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
    download_directory(
        &state.transfers,
        store,
        &account_id,
        &bucket,
        &prefix,
        &local_root,
        factory,
    )
    .await
}
