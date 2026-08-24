//! Cache-aware browse, two modes per bucket: **indexed** buckets serve prefix
//! children from the local cache; **live** mode hits S3 per page, warming cache.

use serde::Serialize;
use tauri::State;

use crate::db::cache::{CachedObjectMeta, KeyParts};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store::ListOptions;
use crate::validate;

const LIVE_PAGE_SIZE: i32 = 1000;

#[derive(Debug, Serialize)]
pub struct BrowseResult {
    pub objects: Vec<CachedObjectMeta>,
    pub subprefixes: Vec<String>,
    /// `"indexed"` or `"live"`; the FE paginates via `continuation` only in live mode.
    pub mode: &'static str,
    /// S3 continuation token (live mode only; always `None` in indexed mode).
    pub continuation: Option<String>,
    /// More pages exist (live); mirrors the cache truncation flag (indexed).
    pub truncated: bool,
    /// Last completed full scan; meaningful only in indexed mode.
    pub last_synced_at: Option<i64>,
}

#[tracing::instrument(skip_all, fields(bucket = %bucket, prefix = %prefix))]
#[tauri::command]
pub async fn browse_prefix(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
    continuation: Option<String>,
) -> AppResult<BrowseResult> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;

    let index_status = state.db.bucket_index_get(&account_id, &bucket).await?;
    let indexed = index_status.enabled && index_status.last_full_sync_at.is_some();

    if indexed {
        // In indexed mode the `continuation` token is a file offset into the
        // cached children (folders come back on the first page only).
        const INDEXED_PAGE: i64 = 5_000;
        let offset: i64 = continuation.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
        let (objects, subprefixes, has_more) = state
            .db
            .browse_children(&account_id, &bucket, &prefix, offset)
            .await?;
        let next = if has_more { Some((offset + INDEXED_PAGE).to_string()) } else { None };
        return Ok(BrowseResult {
            objects,
            subprefixes,
            mode: "indexed",
            continuation: next,
            truncated: has_more,
            last_synced_at: index_status.last_full_sync_at,
        });
    }

    let store = state.store_for(&account_id).await?;
    let list_opts = ListOptions {
        prefix: if prefix.is_empty() { None } else { Some(prefix.clone()) },
        delimiter: Some("/".to_string()),
        continuation: continuation.clone(),
        max_keys: Some(LIVE_PAGE_SIZE),
    };
    let page = match store.list_objects(&bucket, list_opts.clone()).await {
        Ok(p) => p,
        Err(AppError::RegionRedirect(_)) => {
            // Auto-correct: detect real region, rebuild client, retry once;
            // transparent to the FE.
            match state.fix_region_for_bucket(&account_id, &bucket).await {
                Ok(fixed_store) => match fixed_store.list_objects(&bucket, list_opts).await {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(code = %e.code(), "browse_prefix retry after region fix failed: {e}");
                        return Err(e);
                    }
                },
                Err(e) => {
                    tracing::warn!("browse_prefix region fix failed: {e}");
                    return Err(e);
                }
            }
        }
        Err(e) => {
            tracing::warn!(code = %e.code(), "browse_prefix live LIST failed: {e}");
            return Err(e);
        }
    };

    // Best-effort warm cache. Never sweep — we only saw one page.
    let _ = state
        .db
        .cache_upsert_objects_batch(&account_id, &bucket, &page.objects)
        .await;

    let now = chrono::Utc::now().timestamp();
    let objects: Vec<CachedObjectMeta> = page
        .objects
        .into_iter()
        .map(|meta| {
            let parts = KeyParts::from_key(&meta.key);
            CachedObjectMeta {
                account_id: account_id.clone(),
                bucket: bucket.clone(),
                key: meta.key,
                size: meta.size,
                etag: meta.etag,
                last_modified: meta.last_modified,
                storage_class: meta.storage_class,
                content_type: meta.content_type,
                extension: parts.extension,
                basename: parts.basename,
                version_id: meta.version_id,
                synced_at: now,
            }
        })
        .collect();

    Ok(BrowseResult {
        objects,
        subprefixes: page.prefixes,
        mode: "live",
        continuation: page.continuation,
        truncated: page.is_truncated,
        last_synced_at: None,
    })
}
