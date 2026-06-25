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

use crate::db::cache::CachedObjectMeta;
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
    pub truncated: bool,
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn browse_prefix(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
    force: Option<bool>,
) -> AppResult<BrowseResult> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    let force = force.unwrap_or(false);

    // Derive direct children from the cache using LIKE + instr — no
    // parent_prefix column needed; the tree is parsed from slashes at query time.
    let (objects, subprefixes, truncated) = state
        .db
        .browse_children(&account_id, &bucket, &prefix)
        .await?;

    let last = state
        .db
        .prefix_sync_get(&account_id, &bucket, &prefix)
        .await?;
    let ttl = state.load_settings().await?.prefix_sync_ttl_secs as i64;
    let now = Utc::now().timestamp();
    let stale = force || match last {
        Some(t) => now - t > ttl,
        None => true,
    };

    let mut refreshing = false;
    if stale {
        // `claim_prefix_sync` is an atomic DashSet insert: returns true only
        // for the one caller that wins the slot. All others see in-flight=true
        // and skip spawning, preventing concurrent mark/sweep corruptions.
        if state.claim_prefix_sync(&account_id, &bucket, &prefix) {
            let db = state.db.clone();
            let store = Arc::clone(&state.store_for(&account_id).await?);
            let state_for_task = state.inner().clone();
            let account = account_id.clone();
            let buck = bucket.clone();
            let pref = prefix.clone();
            tokio::spawn(async move {
                // Drop-guard ensures the slot is released even if sync_prefix_direct
                // panics, so subsequent claim_prefix_sync calls can re-spawn.
                struct SyncGuard {
                    state: crate::state::AppState,
                    account: String,
                    bucket: String,
                    prefix: String,
                }
                impl Drop for SyncGuard {
                    fn drop(&mut self) {
                        self.state.unregister_prefix_sync(&self.account, &self.bucket, &self.prefix);
                    }
                }
                let _guard = SyncGuard {
                    state: state_for_task,
                    account: account.clone(),
                    bucket: buck.clone(),
                    prefix: pref.clone(),
                };
                if let Err(e) = sync_prefix_direct(&db, store, &account, &buck, &pref).await {
                    tracing::warn!(account = %account, bucket = %buck, prefix = %pref, "background sync_prefix_direct failed: {e}");
                }
            });
        }
        // Whether we just spawned or one was already in flight, tell the FE
        // to keep polling until this prefix's sync completes.
        refreshing = true;
    }

    Ok(BrowseResult {
        objects,
        subprefixes,
        last_synced_at: last,
        stale,
        refreshing,
        truncated,
    })
}
