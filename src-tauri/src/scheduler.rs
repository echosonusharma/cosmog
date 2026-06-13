//! Background re-index scheduler.
//!
//! A single long-lived tokio task wakes up periodically, lists every bucket
//! with `auto_reindex_secs` set, and triggers a fresh `full_bucket_scan` for
//! any whose `last_full_sync_at + auto_reindex_secs` is in the past.
//!
//! Design notes:
//! - One task for the whole process; we don't fan out per bucket because the
//!   scan itself owns its own semaphore + concurrency boundaries.
//! - Polling every 60 s is plenty given sync windows are typically hours.
//! - Failures are logged + swallowed; a transient network glitch shouldn't
//!   poison the scheduler.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::state::AppState;
use crate::sync::full_bucket_scan;
use crate::transfer::ProgressSink;

/// Spawn the scheduler. Returns immediately. The caller keeps the
/// [`CancellationToken`] if it wants to stop the scheduler (e.g. for tests).
pub fn spawn(state: AppState) -> CancellationToken {
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    tokio::spawn(async move {
        info!("scheduler started");
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = token.cancelled() => {
                    info!("scheduler stopped");
                    return;
                }
                _ = tick.tick() => {
                    if let Err(e) = run_once(&state).await {
                        warn!("scheduler iteration failed: {e}");
                    }
                }
            }
        }
    });
    cancel
}

async fn run_once(state: &AppState) -> crate::error::AppResult<()> {
    let due = state.db.bucket_index_due_list().await?;
    let now = Utc::now().timestamp();
    for (account_id, bucket, next_due) in due {
        if next_due > now {
            continue;
        }
        // Skip buckets that already have an in-flight scan.
        if state.scan_in_flight(&account_id, &bucket) {
            continue;
        }
        info!(account_id, bucket, "scheduler: triggering auto re-index");
        let store = match state.store_for(&account_id).await {
            Ok(s) => s,
            Err(e) => {
                warn!("scheduler store_for failed for {account_id}: {e}");
                continue;
            }
        };
        let cancel = state.register_scan(&account_id, &bucket);
        let scan_id = uuid::Uuid::new_v4().to_string();
        let state_for_task = state.clone();
        let acc = account_id.clone();
        let buck = bucket.clone();
        tokio::spawn(async move {
            let res = full_bucket_scan(
                &state_for_task.db,
                Arc::clone(&store),
                &acc,
                &buck,
                ProgressSink::noop(),
                scan_id,
                cancel,
            )
            .await;
            state_for_task.unregister_scan(&acc, &buck);
            if let Err(e) = res {
                warn!("scheduler scan {acc}/{buck} failed: {e}");
            }
        });
    }
    Ok(())
}
