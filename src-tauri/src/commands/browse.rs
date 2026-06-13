//! Cache-aware browse command.
//!
//! Most file-explorer-style navigation in the FE wants the **direct children**
//! of a prefix, not a flat global search. [`browse_prefix`] returns whatever
//! the cache has *right now* (instant) and decides whether to kick a
//! background refresh:
//!
//! - Cache has rows for this prefix AND `prefix_sync.synced_at` is within the
//!   user's TTL → return cache, no network.
//! - Cache empty OR stale beyond TTL → return cache (possibly empty) AND
//!   spawn a `sync_prefix_direct` task. The FE listens for the new data on a
//!   subsequent poll or via a `prefix-synced` event (FE-driven).
//!
//! This pattern keeps the UI responsive and bounds API cost — typical
//! navigation hits the cache.

use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use tauri::State;

use crate::db::cache::{CachedObjectMeta, SearchQuery, SearchResult, SearchScope};
use crate::error::AppResult;
use crate::state::AppState;
use crate::sync::sync_prefix_direct;
use crate::validate;

#[derive(Debug, Serialize)]
pub struct BrowseResult {
    pub objects: Vec<CachedObjectMeta>,
    pub subprefixes: Vec<String>,
    pub last_synced_at: Option<i64>,
    pub stale: bool,
    pub refreshing: bool,
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn browse_prefix(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
) -> AppResult<BrowseResult> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;

    // Read cached children. SearchScope::Prefix non-recursive matches direct
    // children only; that's what a file-list UI wants.
    let cached: SearchResult = state
        .db
        .search_objects(SearchQuery {
            account_id: account_id.clone(),
            bucket: bucket.clone(),
            scope: SearchScope::Prefix {
                prefix: prefix.clone(),
                recursive: false,
            },
            query: None,
            filters: Default::default(),
            sort: Default::default(),
            sort_dir: Default::default(),
            page_size: Some(1000),
            cursor: None,
        })
        .await?;

    // Derive distinct sub-prefixes (one level deeper) from cached rows.
    let subprefixes = derive_subprefixes(&cached.objects, &prefix);

    let last = state
        .db
        .prefix_sync_get(&account_id, &bucket, &prefix)
        .await?;
    let ttl = state.db.settings_load().await?.prefix_sync_ttl_secs as i64;
    let now = Utc::now().timestamp();
    let stale = match last {
        Some(t) => now - t > ttl,
        None => true,
    };

    let mut refreshing = false;
    if stale {
        // Fire-and-forget sync. Cloned handles so the spawned task owns them.
        let db = state.db.clone();
        let store = Arc::clone(&state.store_for(&account_id).await?);
        let account = account_id.clone();
        let buck = bucket.clone();
        let pref = prefix.clone();
        tokio::spawn(async move {
            let _ = sync_prefix_direct(&db, store, &account, &buck, &pref).await;
        });
        refreshing = true;
    }

    Ok(BrowseResult {
        objects: cached.objects,
        subprefixes,
        last_synced_at: last,
        stale,
        refreshing,
    })
}

/// Pull the next path segment from each cached child key (relative to
/// `prefix`) and return the de-duplicated set in original order.
fn derive_subprefixes(rows: &[CachedObjectMeta], prefix: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let rest = match row.key.strip_prefix(prefix) {
            Some(r) => r,
            None => continue,
        };
        if let Some(slash) = rest.find('/') {
            let sub = format!("{prefix}{}", &rest[..=slash]);
            if seen.insert(sub.clone()) {
                out.push(sub);
            }
        }
    }
    out
}
