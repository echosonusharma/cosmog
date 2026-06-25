//! Object-level Tauri commands.
//!
//! Mutating commands (delete, copy) update the local search cache
//! ([`crate::db::cache`]) immediately after the remote write succeeds. The
//! refresh is best-effort — a cache write failure does not roll back the
//! remote operation, only logs a warning.

use chrono::Utc;
use tauri::State;
use tracing::warn;

use crate::db::capabilities::{CapState, WriteOp};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::validate;
use crate::store::{
    CannedAcl, DeleteObjectsResult, ListOptions, ListPage, ObjectMeta, ObjectPreview, ObjectTag,
    ObjectVersion,
};

/// Expire the prefix TTL on cache write failure so the next browse_prefix call
/// triggers a background re-sync and auto-corrects the stale entry.
fn expire_prefix_on_cache_err(state: &AppState, account_id: &str, bucket: &str, key: &str, err: &crate::error::AppError) {
    warn!("cache write failed for {key}: {err} — expiring prefix TTL to trigger re-sync");
    let db = state.db.clone();
    let account_id = account_id.to_string();
    let bucket = bucket.to_string();
    // Derive the *parent* listing prefix. Strip a trailing slash first so
    // folder-marker keys like "foo/bar/" resolve to "foo/" instead of themselves.
    let prefix = {
        let stripped = key.trim_end_matches('/');
        stripped.rfind('/').map(|i| &stripped[..=i]).unwrap_or("").to_string()
    };
    tokio::spawn(async move {
        let _ = db.prefix_sync_expire(&account_id, &bucket, &prefix).await;
    });
}

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
        expire_prefix_on_cache_err(&state, &account_id, &bucket, &meta.key, &e);
    }
    Ok(meta)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn create_folder(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
) -> AppResult<()> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    let prefix = validate::require_non_empty("prefix", &prefix)?;
    let store = state.store_for(&account_id).await?;
    let key = format!("{}/", prefix.trim_end_matches('/'));
    let res = store.create_folder(&bucket, &prefix).await;
    record_write(&state, &account_id, &bucket, WriteOp::Put, &res).await;
    res?;
    // Upsert the new directory marker into the local cache so browse_prefix
    // reflects it immediately without waiting for the next background sync.
    let meta = ObjectMeta {
        key: key.clone(),
        size: 0,
        etag: None,
        last_modified: Some(Utc::now().timestamp()),
        storage_class: None,
        content_type: Some("application/x-directory".into()),
        version_id: None,
    };
    if let Err(e) = state.db.cache_upsert_object(&account_id, &bucket, &meta).await {
        expire_prefix_on_cache_err(&state, &account_id, &bucket, &meta.key, &e);
    }
    Ok(())
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
        expire_prefix_on_cache_err(&state, &account_id, &bucket, &key, &e);
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
            expire_prefix_on_cache_err(&state, &account_id, &bucket, key, &e);
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
                expire_prefix_on_cache_err(&state, &account_id, &dst_bucket, &meta.key, &e);
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
        if let Err(e) = state
            .db
            .cache_upsert_object(&account_id, &dst_bucket, &meta)
            .await
        {
            expire_prefix_on_cache_err(&state, &account_id, &dst_bucket, &dst_key, &e);
        }
    }
    let del_res = store.delete_object(&src_bucket, &src_key).await;
    record_write(&state, &account_id, &src_bucket, WriteOp::Delete, &del_res).await;
    if let Err(e) = del_res {
        // Copy succeeded but source delete failed. Both src and dst now exist in
        // S3. Surface the dst key so the user can decide which to delete.
        return Err(AppError::Internal(format!(
            "copied to \"{dst_key}\" but could not delete source \"{src_key}\": {e}. \
             Both keys exist — delete the unwanted one manually."
        )));
    }
    if let Err(e) = state
        .db
        .cache_remove_object(&account_id, &src_bucket, &src_key)
        .await
    {
        expire_prefix_on_cache_err(&state, &account_id, &src_bucket, &src_key, &e);
    }
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

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn put_object_text(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    content: String,
    content_type: String,
) -> AppResult<()> {
    let store = state.store_for(&account_id).await?;
    let data = content.into_bytes();
    let res = store.put_object_bytes(&bucket, &key, &content_type, data).await;
    record_write(&state, &account_id, &bucket, WriteOp::Put, &res).await;
    res?;
    // Refresh cache entry with updated size/metadata.
    match store.head_object(&bucket, &key).await {
        Ok(meta) => {
            if let Err(e) = state.db.cache_upsert_object(&account_id, &bucket, &meta).await {
                expire_prefix_on_cache_err(&state, &account_id, &bucket, &meta.key, &e);
            }
        }
        Err(e) => warn!("head after put_object_text failed: {e}"),
    }
    Ok(())
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn put_object_bytes_cmd(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    bytes: Vec<u8>,
    content_type: String,
) -> AppResult<()> {
    let store = state.store_for(&account_id).await?;
    let res = store.put_object_bytes(&bucket, &key, &content_type, bytes).await;
    record_write(&state, &account_id, &bucket, WriteOp::Put, &res).await;
    res?;
    match store.head_object(&bucket, &key).await {
        Ok(meta) => {
            if let Err(e) = state.db.cache_upsert_object(&account_id, &bucket, &meta).await {
                expire_prefix_on_cache_err(&state, &account_id, &bucket, &meta.key, &e);
            }
        }
        Err(e) => warn!("head after put_object_bytes failed: {e}"),
    }
    Ok(())
}

/// List every object key under `prefix` by paging S3 directly (no cache).
/// Used by delete-folder and empty-bucket operations so stale cache doesn't
/// cause silent misses.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn list_keys_under_prefix(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
) -> AppResult<Vec<String>> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    let store = state.store_for(&account_id).await?;
    let mut keys = Vec::new();
    let mut continuation: Option<String> = None;
    loop {
        let page = store
            .list_objects(
                &bucket,
                ListOptions {
                    prefix: if prefix.is_empty() { None } else { Some(prefix.clone()) },
                    delimiter: None,
                    continuation: continuation.clone(),
                    max_keys: Some(1000),
                },
            )
            .await?;
        for obj in &page.objects {
            keys.push(obj.key.clone());
        }
        if page.is_truncated {
            continuation = page.continuation;
        } else {
            break;
        }
    }
    Ok(keys)
}
