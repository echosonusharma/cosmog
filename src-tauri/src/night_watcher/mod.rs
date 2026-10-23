//! One-way local dir to S3 sync. The periodic full scan is the source of truth; notify is only an
//! accelerator (no inotify under SAF). delete_policy="keep" never deletes remote.

#[cfg(not(target_os = "android"))]
mod desktop;
mod fs;
mod reconcile;
mod scan;
mod upload;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::db::Db;
use crate::error::AppResult;
use crate::state::AppState;
use crate::store::ObjectStore;
use crate::transfer::TransferManager;

use scan::reconcile_watch;

/// Slice of `AppState` so integration tests can drive the real reconcile core with injected deps.
#[allow(async_fn_in_trait)]
pub trait NwCtx: Clone + Send + Sync + 'static {
    fn db(&self) -> &Db;
    fn db_path(&self) -> &Path;
    fn transfers(&self) -> &TransferManager;
    fn store_for(
        &self,
        account_id: &str,
    ) -> impl std::future::Future<Output = AppResult<Arc<dyn ObjectStore>>> + Send;
    fn nw_claim(&self, watch_id: &str, rel_path: &str) -> bool;
    fn nw_unclaim(&self, watch_id: &str, rel_path: &str);
    fn nw_scan_claim(&self, watch_id: &str) -> bool;
    fn nw_scan_unclaim(&self, watch_id: &str);
    /// Drops the cached client (e.g. after an auth failure) so the next use rebuilds it.
    fn invalidate_store(&self, _account_id: &str) {}
}

impl NwCtx for AppState {
    fn db(&self) -> &Db {
        &self.db
    }
    fn db_path(&self) -> &Path {
        &self.db_path
    }
    fn transfers(&self) -> &TransferManager {
        &self.transfers
    }
    async fn store_for(&self, account_id: &str) -> AppResult<Arc<dyn ObjectStore>> {
        AppState::store_for(self, account_id).await
    }
    fn nw_claim(&self, watch_id: &str, rel_path: &str) -> bool {
        AppState::nw_claim(self, watch_id, rel_path)
    }
    fn nw_unclaim(&self, watch_id: &str, rel_path: &str) {
        AppState::nw_unclaim(self, watch_id, rel_path)
    }
    fn nw_scan_claim(&self, watch_id: &str) -> bool {
        AppState::nw_scan_claim(self, watch_id)
    }
    fn nw_scan_unclaim(&self, watch_id: &str) {
        AppState::nw_scan_unclaim(self, watch_id)
    }
    fn invalidate_store(&self, account_id: &str) {
        AppState::invalidate(self, account_id)
    }
}

/// Loop tick only; per-watch cadence comes from `full_scan_secs`.
const TICK_SECS: u64 = 30;

/// Consecutive upload failures before a file is paused for RETRY_PAUSE_SECS.
const MAX_UPLOAD_RETRIES: i64 = 3;
const RETRY_PAUSE_SECS: i64 = 3600;

// transfer_id -> watch_id for uploads this process enqueued, so stop/disable can cancel them.
static UPLOADS: std::sync::LazyLock<dashmap::DashMap<String, String>> =
    std::sync::LazyLock::new(dashmap::DashMap::new);

/// `None` cancels every Night Watcher upload.
pub(crate) fn cancel_uploads(transfers: &TransferManager, watch_id: Option<&str>) {
    cancel_uploads_where(transfers, |w| watch_id.map_or(true, |id| id == w));
}

fn cancel_uploads_where(transfers: &TransferManager, pred: impl Fn(&str) -> bool) {
    // Never hold a DashMap shard lock while a sink might remove entries.
    let ids: Vec<String> = UPLOADS
        .iter()
        .filter(|e| pred(e.value()))
        .map(|e| e.key().clone())
        .collect();
    for id in ids {
        let _ = transfers.cancel(&id);
    }
}

// Desktop only: Android runs the loop in the :nightwatch service; a second loop would race its writes.
#[cfg(not(target_os = "android"))]
pub fn spawn(state: AppState) -> CancellationToken {
    let cancel = CancellationToken::new();
    {
        let token = cancel.clone();
        let st = state.clone();
        tokio::spawn(async move { desktop::run_watchers(st, token).await });
    }

    let token = cancel.clone();
    tokio::spawn(run_loop(state, token));

    cancel
}

/// Generic over [`NwCtx`] so the Android headless service can drive the same reconcile core.
pub async fn run_loop<S: NwCtx>(ctx: S, token: CancellationToken) {
    info!("night watcher started");
    let mut tick = tokio::time::interval(Duration::from_secs(TICK_SECS));
    loop {
        tokio::select! {
            _ = token.cancelled() => {
                info!("night watcher stopped");
                return;
            }
            _ = tick.tick() => {
                if let Err(e) = run_once(&ctx, &token).await {
                    warn!("night watcher tick failed: {e}");
                }
            }
        }
    }
}

/// Releases the scan claim on drop so a panic or early return can't strand it.
struct ScanClaimGuard<S: NwCtx> {
    state: S,
    watch_id: String,
}

impl<S: NwCtx> Drop for ScanClaimGuard<S> {
    fn drop(&mut self) {
        self.state.nw_scan_unclaim(&self.watch_id);
    }
}

/// One guarded task per due watch, never concurrent scans of one watch.
async fn run_once<S: NwCtx>(state: &S, token: &CancellationToken) -> AppResult<()> {
    let watches = state.db().list_enabled_watches().await?;
    let live: std::collections::HashSet<&str> = watches.iter().map(|w| w.id.as_str()).collect();
    cancel_uploads_where(state.transfers(), |w| !live.contains(w));
    // Done can fire before enqueue returns and records the id; drop such leftovers.
    UPLOADS.retain(|id, _| state.transfers().is_live(id));
    let now = Utc::now().timestamp();
    for w in watches {
        if token.is_cancelled() {
            break;
        }
        let due = w
            .last_scan_at
            .map(|t| now - t >= w.full_scan_secs)
            .unwrap_or(true);
        if !due {
            continue;
        }
        if !state.nw_scan_claim(&w.id) {
            continue;
        }
        let _ = crate::saf::nw_wakelock_heartbeat();
        let state = state.clone();
        let task_token = token.clone();
        tokio::spawn(async move {
            let _guard = ScanClaimGuard {
                state: state.clone(),
                watch_id: w.id.clone(),
            };
            if task_token.is_cancelled() {
                return;
            }
            let res = reconcile_watch(&state, &w, &task_token).await;
            let err = res.as_ref().err().map(|e| e.to_wire_string());
            if let Err(ref e) = res {
                warn!(watch = %w.id, "night watcher scan failed: {e}");
            }
            let _ = state
                .db()
                .set_watch_scan_result(&w.id, Utc::now().timestamp(), err)
                .await;
        });
    }
    Ok(())
}

#[cfg(feature = "nw-test-hooks")]
use crate::db::night_watcher::NightWatch;

#[cfg(feature = "nw-test-hooks")]
pub async fn reconcile_file_for_test<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    rel_path: &str,
    abs: &Path,
) -> AppResult<bool> {
    reconcile::reconcile_file(state, watch, rel_path, abs, None)
        .await
        .map(|r| r.enqueued)
}

#[cfg(feature = "nw-test-hooks")]
pub async fn reconcile_watch_for_test<S: NwCtx>(state: &S, watch: &NightWatch) -> AppResult<()> {
    let token = CancellationToken::new();
    reconcile_watch(state, watch, &token).await
}
