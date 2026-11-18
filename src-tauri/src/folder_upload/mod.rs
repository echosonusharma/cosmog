//! Folder upload (desktop paths, Android SAF trees): analyze a local tree, check the destination, then upload exactly the
//! approved manifest through a bounded window of job-owned transfers, resumable across restarts.

mod analysis;
mod analyze;
mod control;
mod engine;
mod item;
pub mod keys;
mod outcome;
mod own;
mod policy;
mod remote;
mod resume;
mod runtime;
mod source;
mod view;
mod walk;
mod walk_saf;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::db::folder_upload::JobState;
use crate::db::Db;
use crate::error::AppResult;
use crate::store::ObjectStore;
use crate::transfer::{TransferEvent, TransferManager};

pub use analysis::{FolderAnalysis, FolderAnalyzeEvent};
pub use analyze::analyze;
pub use control::{
    approve, cancel, cancel_jobs_for, clear_finished, kick, pause, remove, resume, retry_failed,
};
pub use remote::{check_remote, FolderRemoteCheck, FolderRemoteEvent};
pub use resume::{resume_all, startup};
pub use runtime::FolderJobs;
pub use source::{pick_done, pick_started, release_pick, release_tree_if_unused, tree_in_use};
pub use view::{get_analysis, get_job, list_items, list_jobs, FolderItem, FolderItemPage, FolderJob};

/// Hard cap on walked entries per job.
pub const MAX_ENTRIES: u64 = 500_000;
/// Single-object S3 limit.
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024 * 1024 * 1024;
pub const WARN_FILES: i64 = 10_000;
pub const WARN_BYTES: i64 = 50 * 1024 * 1024 * 1024;
pub const EVENT: &str = "folder-upload-changed";

/// Backoff knobs; tests shrink them.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub net_backoff_min: Duration,
    pub net_backoff_max: Duration,
    /// Seconds; doubled per attempt up to `retry_cap`.
    pub retry_base: i64,
    pub retry_cap: i64,
    pub io_backoff: i64,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            net_backoff_min: Duration::from_secs(10),
            net_backoff_max: Duration::from_secs(300),
            retry_base: 30,
            retry_cap: 1800,
            io_backoff: 30,
        }
    }
}

/// Slice of `AppState` the engine needs, so tests drive it without a tauri AppHandle.
#[allow(async_fn_in_trait)]
pub trait FolderCtx: Clone + Send + Sync + 'static {
    fn db(&self) -> &Db;
    fn db_path(&self) -> &Path;
    fn transfers(&self) -> &TransferManager;
    fn jobs(&self) -> &FolderJobs;
    fn store_for(&self, account_id: &str) -> impl std::future::Future<Output = AppResult<Arc<dyn ObjectStore>>> + Send;
    /// Job state or waiting flag changed.
    fn notify(&self, _job_id: &str, _state: JobState) {}
    /// Every event of a job-owned transfer (test hook).
    fn on_transfer_event(&self, _job_id: &str, _event: &TransferEvent) {}
    fn timing(&self) -> Timing {
        Timing::default()
    }
}

impl FolderCtx for crate::state::AppState {
    fn db(&self) -> &Db {
        &self.db
    }
    fn db_path(&self) -> &Path {
        &self.db_path
    }
    fn transfers(&self) -> &TransferManager {
        &self.transfers
    }
    fn jobs(&self) -> &FolderJobs {
        &self.folder_jobs
    }
    async fn store_for(&self, account_id: &str) -> AppResult<Arc<dyn ObjectStore>> {
        crate::state::AppState::store_for(self, account_id).await
    }
    fn notify(&self, job_id: &str, state: JobState) {
        use tauri::Emitter;
        let payload = serde_json::json!({ "job_id": job_id, "state": state });
        if let Err(e) = self.app.emit(EVENT, payload) {
            tracing::debug!("emit {EVENT} failed: {e}");
        }
    }
}

/// Final write of a runner: applies only while the job is still `running`, so it can never
/// overwrite a pause or cancel that won the race.
pub(crate) async fn set_state_if<C: FolderCtx>(
    ctx: &C,
    job_id: &str,
    state: JobState,
    pause_reason: Option<String>,
    error: Option<String>,
) -> AppResult<()> {
    if ctx.db().fu_set_state_if(job_id, JobState::Running, state, pause_reason, error).await? {
        if let Some(rt) = ctx.jobs().get(job_id) {
            rt.invalidate_counts();
        }
        ctx.notify(job_id, state);
    }
    Ok(())
}

/// Persists a state change, drops cached counts and tells the UI.
pub(crate) async fn set_state<C: FolderCtx>(
    ctx: &C,
    job_id: &str,
    state: JobState,
    pause_reason: Option<String>,
    error: Option<String>,
) -> AppResult<()> {
    ctx.db().fu_set_state(job_id, state, pause_reason, error).await?;
    if let Some(rt) = ctx.jobs().get(job_id) {
        rt.invalidate_counts();
    }
    ctx.notify(job_id, state);
    Ok(())
}
