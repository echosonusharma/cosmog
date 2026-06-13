use tauri::State;

use crate::error::AppResult;
use crate::state::AppState;
use crate::store::{Bucket, CannedAcl, PendingMultipartUpload};

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn list_buckets(state: State<'_, AppState>, account_id: String) -> AppResult<Vec<Bucket>> {
    state.store_for(&account_id).await?.list_buckets().await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn create_bucket(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
    region: Option<String>,
) -> AppResult<()> {
    state
        .store_for(&account_id)
        .await?
        .create_bucket(&name, region.as_deref())
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn delete_bucket(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
) -> AppResult<()> {
    state.store_for(&account_id).await?.delete_bucket(&name).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn head_bucket(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
) -> AppResult<()> {
    state.store_for(&account_id).await?.head_bucket(&name).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn get_bucket_location(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
) -> AppResult<Option<String>> {
    state
        .store_for(&account_id)
        .await?
        .get_bucket_location(&name)
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn put_bucket_acl(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
    acl: CannedAcl,
) -> AppResult<()> {
    state
        .store_for(&account_id)
        .await?
        .put_bucket_acl(&name, acl)
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn get_bucket_versioning(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
) -> AppResult<bool> {
    state
        .store_for(&account_id)
        .await?
        .get_bucket_versioning(&name)
        .await
}

#[derive(serde::Serialize)]
pub struct MultipartUploadsPage {
    pub uploads: Vec<PendingMultipartUpload>,
    pub continuation: Option<String>,
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn list_multipart_uploads(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: Option<String>,
    continuation: Option<String>,
) -> AppResult<MultipartUploadsPage> {
    let (uploads, continuation) = state
        .store_for(&account_id)
        .await?
        .list_multipart_uploads(&bucket, prefix.as_deref(), continuation)
        .await?;
    Ok(MultipartUploadsPage {
        uploads,
        continuation,
    })
}

/// Abort every in-progress multipart upload in `bucket` older than
/// `older_than_secs`. Walks the list_multipart_uploads pages, aborts each
/// match individually. Returns the count of aborted uploads.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn cleanup_stale_multiparts(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    older_than_secs: i64,
) -> AppResult<usize> {
    let now = chrono::Utc::now().timestamp();
    let cutoff = now - older_than_secs;
    let store = state.store_for(&account_id).await?;
    let mut continuation: Option<String> = None;
    let mut aborted = 0usize;
    loop {
        let prev = continuation.clone();
        let (page, next) = store
            .list_multipart_uploads(&bucket, None, continuation.clone())
            .await?;
        for u in &page {
            if u.initiated_at.unwrap_or(now) < cutoff {
                store
                    .abort_multipart_upload(&bucket, &u.key, &u.upload_id)
                    .await?;
                aborted += 1;
            }
        }
        // Guard: some non-AWS providers return is_truncated=true with no
        // next_key_marker, which would otherwise spin this loop forever.
        // Break if the continuation didn't advance.
        match next {
            None => break,
            Some(ref t) if Some(t) == prev.as_ref() => break,
            Some(t) => continuation = Some(t),
        }
    }
    Ok(aborted)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn abort_multipart_upload(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    upload_id: String,
) -> AppResult<()> {
    state
        .store_for(&account_id)
        .await?
        .abort_multipart_upload(&bucket, &key, &upload_id)
        .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn put_bucket_versioning(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
    enabled: bool,
) -> AppResult<()> {
    state
        .store_for(&account_id)
        .await?
        .put_bucket_versioning(&name, enabled)
        .await
}
