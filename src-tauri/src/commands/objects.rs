//! Object-level Tauri commands.
//!
//! Mutating commands (delete, copy) update the local search cache
//! ([`crate::db::cache`]) immediately after the remote write succeeds. The
//! refresh is best-effort — a cache write failure does not roll back the
//! remote operation, only logs a warning.

use tauri::State;
use tracing::warn;

use crate::db::capabilities::{CapState, WriteOp};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store::{
    CannedAcl, DeleteObjectsResult, ListOptions, ListPage, ObjectMeta, ObjectPreview, ObjectTag,
    ObjectVersion,
};

/// Record a write-op outcome against the capability cache. Treats Allowed +
/// Denied; ignores other error classes (network blip ≠ proof of denial).
async fn record_write(
    state: &AppState,
    account_id: &str,
    bucket: &str,
    op: WriteOp,
    result: &AppResult<()>,
) {
    let cap = match result {
        Ok(()) => CapState::Allowed,
        Err(AppError::AccessDenied(_)) => CapState::Denied,
        _ => return,
    };
    let _ = state
        .db
        .capability_record_write(account_id, bucket, op, cap)
        .await;
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn list_objects(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: Option<String>,
    delimiter: Option<String>,
    continuation: Option<String>,
    max_keys: Option<i32>,
) -> AppResult<ListPage> {
    let store = state.store_for(&account_id).await?;
    store
        .list_objects(
            &bucket,
            ListOptions {
                prefix,
                delimiter,
                continuation,
                max_keys,
            },
        )
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn head_object(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
) -> AppResult<ObjectMeta> {
    let meta = state
        .store_for(&account_id)
        .await?
        .head_object(&bucket, &key)
        .await?;
    // Refresh the cache entry while we have authoritative metadata.
    if let Err(e) = state.db.cache_upsert_object(&account_id, &bucket, &meta).await {
        warn!("cache upsert after head_object failed: {e}");
    }
    Ok(meta)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn delete_object(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
) -> AppResult<()> {
    let store = state.store_for(&account_id).await?;
    let res = store.delete_object(&bucket, &key).await;
    record_write(&state, &account_id, &bucket, WriteOp::Delete, &res).await;
    res?;
    if let Err(e) = state.db.cache_remove_object(&account_id, &bucket, &key).await {
        warn!("cache remove after delete_object failed: {e}");
    }
    Ok(())
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn delete_objects(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
) -> AppResult<DeleteObjectsResult> {
    let res = state
        .store_for(&account_id)
        .await?
        .delete_objects(&bucket, &keys)
        .await;
    // Per-key errors are inside the result; the outer Result reflects
    // request-level success. Mirror that into the capability tracker.
    let cap = match &res {
        Ok(_) => CapState::Allowed,
        Err(AppError::AccessDenied(_)) => CapState::Denied,
        _ => CapState::Unknown,
    };
    if !matches!(cap, CapState::Unknown) {
        let _ = state
            .db
            .capability_record_write(&account_id, &bucket, WriteOp::Delete, cap)
            .await;
    }
    let result = res?;
    // Only remove cache rows for keys the server confirmed deleted.
    for key in &result.deleted {
        if let Err(e) = state.db.cache_remove_object(&account_id, &bucket, key).await {
            warn!("cache remove after delete_objects failed: {e}");
        }
    }
    Ok(result)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn delete_object_version(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    version_id: String,
) -> AppResult<()> {
    state
        .store_for(&account_id)
        .await?
        .delete_object_version(&bucket, &key, &version_id)
        .await
    // We don't touch the cache here: only the live/latest version is mirrored.
}

#[derive(serde::Serialize)]
pub struct VersionsPage {
    pub versions: Vec<ObjectVersion>,
    pub continuation: Option<String>,
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn list_object_versions(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: Option<String>,
    continuation: Option<String>,
) -> AppResult<VersionsPage> {
    let (versions, continuation) = state
        .store_for(&account_id)
        .await?
        .list_object_versions(&bucket, prefix.as_deref(), continuation)
        .await?;
    Ok(VersionsPage {
        versions,
        continuation,
    })
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn copy_object(
    state: State<'_, AppState>,
    account_id: String,
    src_bucket: String,
    src_key: String,
    dst_bucket: String,
    dst_key: String,
) -> AppResult<()> {
    let store = state.store_for(&account_id).await?;
    let res = store
        .copy_object(&src_bucket, &src_key, &dst_bucket, &dst_key)
        .await;
    record_write(&state, &account_id, &dst_bucket, WriteOp::Put, &res).await;
    res?;

    // Mirror the new object into the cache by reading authoritative metadata
    // from the destination. Best-effort.
    match store.head_object(&dst_bucket, &dst_key).await {
        Ok(meta) => {
            if let Err(e) = state
                .db
                .cache_upsert_object(&account_id, &dst_bucket, &meta)
                .await
            {
                warn!("cache upsert after copy_object failed: {e}");
            }
        }
        Err(e) => warn!("head after copy_object failed: {e}"),
    }
    Ok(())
}

/// Move/rename an object. S3 has no atomic move — we do a server-side `copy`
/// followed by `delete_object` on the source. If the delete fails the
/// destination remains and is reported in the error message; the caller can
/// retry the delete or clean up manually.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn move_object(
    state: State<'_, AppState>,
    account_id: String,
    src_bucket: String,
    src_key: String,
    dst_bucket: String,
    dst_key: String,
) -> AppResult<()> {
    if src_bucket == dst_bucket && src_key == dst_key {
        return Err(AppError::InvalidInput("source equals destination".into()));
    }
    let store = state.store_for(&account_id).await?;
    let copy_res = store
        .copy_object(&src_bucket, &src_key, &dst_bucket, &dst_key)
        .await;
    record_write(&state, &account_id, &dst_bucket, WriteOp::Put, &copy_res).await;
    copy_res?;
    // Best-effort cache mirror at destination before deleting source so the
    // FE never sees a moment when neither key is in the index.
    if let Ok(meta) = store.head_object(&dst_bucket, &dst_key).await {
        let _ = state
            .db
            .cache_upsert_object(&account_id, &dst_bucket, &meta)
            .await;
    }
    let del_res = store.delete_object(&src_bucket, &src_key).await;
    record_write(&state, &account_id, &src_bucket, WriteOp::Delete, &del_res).await;
    del_res?;
    let _ = state
        .db
        .cache_remove_object(&account_id, &src_bucket, &src_key)
        .await;
    Ok(())
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn put_object_acl(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    acl: CannedAcl,
) -> AppResult<()> {
    state
        .store_for(&account_id)
        .await?
        .put_object_acl(&bucket, &key, acl)
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn preview_object(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    max_bytes: Option<u64>,
) -> AppResult<ObjectPreview> {
    let max = max_bytes.unwrap_or(1024 * 1024);
    state
        .store_for(&account_id)
        .await?
        .read_object_range(&bucket, &key, max)
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn get_object_tagging(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
) -> AppResult<Vec<ObjectTag>> {
    state
        .store_for(&account_id)
        .await?
        .get_object_tagging(&bucket, &key)
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn put_object_tagging(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    tags: Vec<ObjectTag>,
) -> AppResult<()> {
    state
        .store_for(&account_id)
        .await?
        .put_object_tagging(&bucket, &key, &tags)
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn delete_object_tagging(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
) -> AppResult<()> {
    state
        .store_for(&account_id)
        .await?
        .delete_object_tagging(&bucket, &key)
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn presign_get(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    expires_secs: Option<u64>,
) -> AppResult<String> {
    let expires = match expires_secs {
        Some(s) => s,
        None => state.db.settings_load().await?.presign_default_expires_secs,
    };
    state
        .store_for(&account_id)
        .await?
        .presign_get(&bucket, &key, expires)
        .await
}
