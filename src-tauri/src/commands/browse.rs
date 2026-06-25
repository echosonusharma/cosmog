//! Cache-aware browse command.
//!
//! Two modes, selected per bucket:
//!
//! - **Indexed mode** — bucket has full-bucket indexing enabled and a
//!   completed scan. Returns the direct children of the prefix from the
//!   local cache. The scheduler refreshes the index in the background.
//!   Cheap, sorted, supports search.
//!
//! - **Live mode** — bucket is not indexed. Each call hits S3 once with
//!   `delimiter='/'` and a 1000-key page. The FE drives pagination via
//!   the `continuation` field. We upsert the page into `cached_objects`
//!   as a warm best-effort cache but never sweep — orphan rows here are
//!   harmless and only fully reconciled by a full bucket scan.
//!
//! Live mode replaces the previous "background sync_prefix_direct" flow.
//! That flow walked every page of the prefix on each TTL expiry, which is
//! unworkable for prefixes with millions of children.

use serde::Serialize;
use tauri::State;

use crate::db::cache::{CachedObjectMeta, KeyParts};
use crate::error::AppResult;
use crate::state::AppState;
use crate::store::ListOptions;
use crate::validate;

const LIVE_PAGE_SIZE: i32 = 1000;

#[derive(Debug, Serialize)]
pub struct BrowseResult {
    pub objects: Vec<CachedObjectMeta>,
    pub subprefixes: Vec<String>,
    /// `"indexed"` when the bucket has a completed full scan; `"live"`
    /// otherwise. FE uses this to decide whether to paginate via
    /// `continuation` or treat the response as a complete listing.
    pub mode: &'static str,
    /// S3 continuation token for fetching the next page in live mode.
    /// Always `None` in indexed mode.
    pub continuation: Option<String>,
    /// `true` in live mode when more pages exist; in indexed mode mirrors
    /// the cache's truncation flag.
    pub truncated: bool,
    /// When the bucket index last completed a full scan. Only meaningful
    /// in indexed mode.
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
        let (objects, subprefixes, truncated) = state
            .db
            .browse_children(&account_id, &bucket, &prefix)
            .await?;
        return Ok(BrowseResult {
            objects,
            subprefixes,
            mode: "indexed",
            continuation: None,
            truncated,
            last_synced_at: index_status.last_full_sync_at,
        });
    }

    // Live mode: single LIST page, upsert into cache, return immediately.
    let store = state.store_for(&account_id).await?;
    let page = match store
        .list_objects(
            &bucket,
            ListOptions {
                prefix: if prefix.is_empty() { None } else { Some(prefix.clone()) },
                delimiter: Some("/".to_string()),
                continuation: continuation.clone(),
                max_keys: Some(LIVE_PAGE_SIZE),
            },
        )
        .await
    {
        Ok(p) => p,
        Err(e) => {
            // Log at warn instead of error: cross-region buckets, denied
            // ListBucket perms, and non-existent buckets are all expected
            // failure modes when the account has heterogeneous access.
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
