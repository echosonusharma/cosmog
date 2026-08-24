//! Transfer queue types: worker-emitted [`TransferEvent`], type-erased [`ProgressSink`],
//! and per-transfer [`TransferCtx`] threaded through ObjectStore put/get.

pub mod encrypt;
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
    MultipartInitiated {
        transfer_id: String,
        upload_id: String,
    },
    PartCompleted {
        transfer_id: String,
        upload_id: String,
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

/// Fingerprint of an upload source file captured at enqueue; multipart resume state
/// from one file version must never apply to another (part boundaries are byte offsets).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SourceStat {
    pub len: u64,
    pub mtime_secs: i64,
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
    /// Source size when the saved parts were cut; a mismatch means the file
    /// changed and every saved part boundary is wrong.
    #[serde(default)]
    pub source_len: Option<u64>,
    /// Captured alongside `source_len`; older persisted rows without these fields
    /// deserialize as `None` and skip staleness validation.
    #[serde(default)]
    pub source_mtime_secs: Option<i64>,
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
    /// Current upload-source fingerprint (uploads only), compared against [`ResumeState`]
    /// so a resume after the file changed aborts instead of assembling a corrupted object.
    pub source_stat: Option<SourceStat>,
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
            source_stat: None,
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
