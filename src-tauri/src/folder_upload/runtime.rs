//! In-memory side of folder jobs: tokens, runner handle, waiting flag, cached counts, inflight files.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use dashmap::DashMap;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::db::folder_upload::{Drift, ItemCounts};
use crate::transfer::source_guard::FileId;

/// What the runner was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    None,
    Pause,
    Cancel,
}

pub struct RunnerHandle {
    pub stop: watch::Sender<Stop>,
    pub done: watch::Receiver<bool>,
}

#[derive(Debug, Clone)]
pub struct Inflight {
    pub transfer_id: String,
    pub rel_path: String,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    /// Stat at enqueue; a different stat after Done means it changed during upload.
    pub size: i64,
    pub mtime_ms: i64,
    pub drift: Option<Drift>,
    /// Sent with If-None-Match, so a 412 means "appeared after check" (or our own lost write).
    pub if_none_match: bool,
    pub attempts: i64,
    /// Identity of the file at enqueue; the source guard holds the worker to it.
    pub file_id: FileId,
    /// Started again after a network wait: a failure without progress counts an attempt.
    pub net_retry: bool,
    /// At least one part reached the provider in this run.
    pub progressed: bool,
}

#[derive(Debug, Clone)]
pub struct CachedCounts {
    pub counts: ItemCounts,
    pub done_bytes: i64,
    pub at: Instant,
}

pub struct JobRt {
    pub analysis_cancel: Mutex<Option<CancellationToken>>,
    /// true while no walk is running for this job.
    pub analysis_idle: watch::Sender<bool>,
    pub remote_cancel: Mutex<Option<CancellationToken>>,
    pub runner: Mutex<Option<RunnerHandle>>,
    pub waiting: Mutex<Option<&'static str>>,
    pub counts: Mutex<Option<CachedCounts>>,
    pub inflight: Mutex<BTreeMap<i64, Inflight>>,
}

impl JobRt {
    fn new() -> Self {
        Self {
            analysis_cancel: Mutex::new(None),
            analysis_idle: watch::Sender::new(true),
            remote_cancel: Mutex::new(None),
            runner: Mutex::new(None),
            waiting: Mutex::new(None),
            counts: Mutex::new(None),
            inflight: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn invalidate_counts(&self) {
        *self.counts.lock().unwrap() = None;
    }

    pub fn runner_live(&self) -> bool {
        self.runner.lock().unwrap().as_ref().is_some_and(|h| !*h.done.borrow())
    }
}

/// Registry of job runtimes plus the lock that keeps one job running at a time.
#[derive(Clone)]
pub struct FolderJobs {
    jobs: Arc<DashMap<String, Arc<JobRt>>>,
    pub(crate) sched: Arc<tokio::sync::Mutex<()>>,
    /// Unix secs when this registry (this run of the app) began.
    pub started_at: i64,
}

impl Default for FolderJobs {
    fn default() -> Self {
        Self { jobs: Default::default(), sched: Default::default(), started_at: chrono::Utc::now().timestamp() }
    }
}

impl FolderJobs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Jobs touched in this run.
    pub fn ids(&self) -> Vec<String> {
        self.jobs.iter().map(|e| e.key().clone()).collect()
    }

    pub fn rt(&self, job_id: &str) -> Arc<JobRt> {
        self.jobs.entry(job_id.to_string()).or_insert_with(|| Arc::new(JobRt::new())).clone()
    }

    pub fn get(&self, job_id: &str) -> Option<Arc<JobRt>> {
        self.jobs.get(job_id).map(|r| r.clone())
    }

    pub fn forget(&self, job_id: &str) {
        self.jobs.remove(job_id);
    }

    pub fn any_runner_live(&self) -> bool {
        self.jobs.iter().any(|e| e.value().runner_live())
    }
}
