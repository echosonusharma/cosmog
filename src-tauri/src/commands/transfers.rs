//! Tauri commands for the persistent transfer queue.
//!
//! Each enqueue command takes a [`tauri::ipc::Channel<TransferEvent>`] supplied
//! by the FE. The channel receives live progress events while the underlying
//! transfer is persisted to SQLite and tracked by [`TransferManager`].
//!
//! All local paths are validated through [`crate::validate`] before any
//! filesystem or S3 work happens.

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
        // Channel send fails silently when the FE has dropped the receiver.
        // That's intentional: the worker is the source of truth and continues.
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
    let path = validate::validate_upload_source(&local_path)?;

    let mut opts = options.unwrap_or_default();

    // If the bucket has encryption enabled, encrypt the source file to a temp
    // path before enqueuing. The transfer worker deletes the temp file via
    // opts.cleanup_path once the upload finishes (success or failure).
    let upload_path = if let Some(enc_cfg) = state.db.get_encryption_config(&account_id, &bucket).await? {
        // Stream-encrypt the source file to a temp path using the bucket's
        // age recipient. Constant-memory: age streams 64 KiB chunks with
        // per-chunk nonces + last-chunk marker.
        let recipient = crate::crypto::parse_recipient(&enc_cfg.recipient)?;

        let tmp_dir = state.db_path.parent()
            .ok_or_else(|| crate::error::AppError::Internal("db_path has no parent".into()))?
            .join("enc_tmp");
        tokio::fs::create_dir_all(&tmp_dir).await?;
        let tmp_path = tmp_dir.join(format!("{}.age", uuid::Uuid::new_v4()));

        crate::crypto::encrypt_file(&path, &tmp_path, recipient).await?;

        opts.cleanup_path = Some(tmp_path.clone());
        // Mark the object so download + UI know it's client-encrypted, and
        // record the payload format so future format changes stay unambiguous.
        opts.user_metadata.insert("cosmog-encrypted".into(), "1".into());
        opts.user_metadata.insert("cosmog-format".into(), crate::crypto::FORMAT_TAG.into());
        opts.user_metadata.insert("cosmog-recipient".into(), enc_cfg.recipient);
        tmp_path
    } else {
        path
    };

    let store = state.store_for(&account_id).await?;
    let id = state
        .transfers
        .enqueue_upload(
            store,
            account_id,
            bucket,
            key,
            upload_path,
            opts,
            channel_sink(on_event),
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
    let path = validate::validate_download_dest(&local_path)?;

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
    state.transfers.cancel(&id)
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
    state.transfers.clear_completed().await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn clear_transfer(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.transfers.delete_one(&id).await
}
