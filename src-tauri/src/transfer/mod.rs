//! Transfer queue types and lifecycle.
//!
//! - [`TransferEvent`]: wire-format progress/lifecycle event emitted by workers
//! - [`ProgressSink`]: type-erased event consumer (FE channel, DB persistence,
//!   or any fan-out combination)
//! - [`TransferCtx`]: per-transfer config + cooperative cancellation token +
//!   progress sink, threaded through [`crate::store::ObjectStore::put_object`]
//!   and [`crate::store::ObjectStore::get_object`]
//! - [`TransferManager`]: see [`manager`]

pub mod manager;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

pub use manager::TransferManager;

/// Progress / lifecycle event emitted by a transfer worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransferEvent {
    Started {
        transfer_id: String,
        bytes_total: Option<u64>,
    },
    Progress {
        transfer_id: String,
        bytes_done: u64,
        bytes_total: Option<u64>,
    },
    PartCompleted {
        transfer_id: String,
        part_number: i32,
        etag: String,
    },
    Done {
        transfer_id: String,
        etag: Option<String>,
    },
    Failed {
        transfer_id: String,
        error: String,
    },
    Canceled {
        transfer_id: String,
    },
}

/// Cheap clone, type-erased event emitter.
#[derive(Clone)]
pub struct ProgressSink(Arc<dyn Fn(TransferEvent) + Send + Sync>);

impl ProgressSink {
    pub fn noop() -> Self {
        Self(Arc::new(|_| {}))
    }

    pub fn from_fn<F>(f: F) -> Self
    where
        F: Fn(TransferEvent) + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    pub fn emit(&self, event: TransferEvent) {
        (self.0)(event);
    }
}

impl std::fmt::Debug for ProgressSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgressSink").finish()
    }
}

/// Saved per-part state used to resume a previously-failed multipart upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedPart {
    pub part_number: i32,
    pub etag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResumeState {
    pub upload_id: String,
    pub completed_parts: Vec<CompletedPart>,
}

#[derive(Debug, Clone)]
pub struct TransferCtx {
    pub transfer_id: String,
    pub cancel: CancellationToken,
    pub progress: ProgressSink,
    pub part_size: u64,
    pub parallelism: usize,
    pub multipart_threshold: u64,
    pub resume: Option<ResumeState>,
}

impl TransferCtx {
    pub fn new(transfer_id: impl Into<String>) -> Self {
        Self {
            transfer_id: transfer_id.into(),
            cancel: CancellationToken::new(),
            progress: ProgressSink::noop(),
            part_size: 8 * 1024 * 1024,
            parallelism: 4,
            multipart_threshold: 8 * 1024 * 1024,
            resume: None,
        }
    }

    pub fn with_progress(mut self, sink: ProgressSink) -> Self {
        self.progress = sink;
        self
    }

    pub fn with_cancel(mut self, token: CancellationToken) -> Self {
        self.cancel = token;
        self
    }

    pub fn with_resume(mut self, resume: ResumeState) -> Self {
        self.resume = Some(resume);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadResult {
    pub etag: Option<String>,
    pub upload_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResult {
    pub bytes: u64,
}
