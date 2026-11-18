//! Folder-scoped bulk ops (recursive delete/download) composing store/transfer
//! primitives into single actions, with ProgressSink reporting and cancellation support.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::store::{ListOptions, ObjectStore};
use crate::transfer::{ProgressSink, TransferEvent};

/// Per-key errors kept in a result; `failed` still counts every one.
pub const MAX_DELETE_ERRORS: usize = 100;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BulkDeleteError {
    pub key: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BulkDeleteResult {
    pub deleted: u64,
    pub failed: u64,
    /// Keys listed so far.
    pub found: u64,
    /// First `MAX_DELETE_ERRORS` per-key failures.
    pub errors: Vec<BulkDeleteError>,
    pub canceled: bool,
    /// Wire error of a hard failure (listing or a whole batch); counts cover the work before it.
    pub error: Option<String>,
}

const DELETE_BATCH: usize = 1000;

struct DeleteRun<'a> {
    db: &'a Db,
    store: Arc<dyn ObjectStore>,
    account_id: &'a str,
    bucket: &'a str,
    sink: ProgressSink,
    transfer_id: String,
    cancel: CancellationToken,
    result: BulkDeleteResult,
}

impl DeleteRun<'_> {
    fn canceled(&self) -> AppResult<()> {
        if self.cancel.is_cancelled() {
            return Err(AppError::Canceled("delete_folder".into()));
        }
        Ok(())
    }

    /// Progress: `bytes_done` = deleted, `bytes_total` = keys found so far (not final).
    fn progress(&self) {
        self.sink.emit(TransferEvent::Progress {
            transfer_id: self.transfer_id.clone(),
            bytes_done: self.result.deleted,
            bytes_total: Some(self.result.found),
        });
    }

    async fn flush(&mut self, buffer: &mut Vec<String>) -> AppResult<()> {
        if buffer.is_empty() {
            return Ok(());
        }
        self.canceled()?;
        let keys = std::mem::take(buffer);
        let outcome = self.store.delete_objects(self.bucket, &keys).await?;
        for k in &outcome.deleted {
            let _ = self.db.cache_remove_object(self.account_id, self.bucket, k).await;
            self.result.deleted += 1;
        }
        for e in outcome.errors {
            self.result.failed += 1;
            if self.result.errors.len() < MAX_DELETE_ERRORS {
                self.result.errors.push(BulkDeleteError {
                    key: e.key,
                    message: e.message.or(e.code).unwrap_or_else(|| "unknown".into()),
                });
            }
        }
        self.progress();
        Ok(())
    }

    async fn run(&mut self, prefix: &str) -> AppResult<()> {
        let mut continuation: Option<String> = None;
        let mut buffer: Vec<String> = Vec::with_capacity(DELETE_BATCH);
        let mut deleted_at_restart: u64 = 0;
        loop {
            self.canceled()?;
            let page = tokio::select! {
                _ = self.cancel.cancelled() => return Err(AppError::Canceled("delete_folder".into())),
                p = self.store.list_objects(
                    self.bucket,
                    ListOptions {
                        prefix: Some(prefix.to_string()),
                        delimiter: None,
                        continuation: continuation.clone(),
                        max_keys: Some(1000),
                    },
                ) => p?,
            };
            self.result.found += page.objects.len() as u64;
            self.progress();

            for obj in page.objects {
                buffer.push(obj.key);
                if buffer.len() >= DELETE_BATCH {
                    self.flush(&mut buffer).await?;
                }
            }

            if !page.is_truncated {
                break;
            }
            continuation = page.continuation;
            if continuation.is_none() {
                // No token: relisting from the top only advances while keys keep
                // disappearing. Stop once a round deletes nothing.
                self.flush(&mut buffer).await?;
                if self.result.deleted == deleted_at_restart {
                    return Err(AppError::Internal(
                        "listing is truncated without a continuation token and no further keys could be deleted".into(),
                    ));
                }
                deleted_at_restart = self.result.deleted;
            }
        }
        self.flush(&mut buffer).await
    }
}

/// Recursively delete under `prefix` via batched DeleteObjects (1000 keys/call),
/// mirroring deletions into the local cache. Cancel and hard failures still return
/// Ok with partial counts (`canceled` / `error`); the sink gets the matching terminal.
#[allow(clippy::too_many_arguments)]
pub async fn delete_folder(
    db: &Db,
    store: Arc<dyn ObjectStore>,
    account_id: &str,
    bucket: &str,
    prefix: &str,
    sink: ProgressSink,
    transfer_id: String,
    cancel: CancellationToken,
) -> AppResult<BulkDeleteResult> {
    sink.emit(TransferEvent::Started {
        transfer_id: transfer_id.clone(),
        bytes_total: None,
    });
    let mut run = DeleteRun {
        db,
        store,
        account_id,
        bucket,
        sink,
        transfer_id,
        cancel,
        result: BulkDeleteResult::default(),
    };
    let job = run.run(prefix).await;
    let DeleteRun { sink, transfer_id, mut result, .. } = run;
    match job {
        Ok(()) => sink.emit(TransferEvent::Done { transfer_id, etag: None }),
        Err(AppError::Canceled(_)) => {
            result.canceled = true;
            sink.emit(TransferEvent::Canceled { transfer_id });
        }
        Err(e) => {
            let wire = e.to_wire_string();
            result.error = Some(wire.clone());
            sink.emit(TransferEvent::Failed { transfer_id, error: wire });
        }
    }
    Ok(result)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BulkTransferResult {
    pub enqueued: Vec<String>,
    pub skipped: Vec<String>,
    /// Unreadable dirs/entries as `path: error`; the walk continues past them.
    pub errors: Vec<String>,
    /// Canceled mid-walk; `enqueued` still lists what was already queued.
    pub canceled: bool,
}

/// Emits the op's Started, then exactly one terminal matching `res`.
async fn run_op<F>(op_id: String, op_sink: ProgressSink, body: F) -> AppResult<BulkTransferResult>
where
    F: std::future::Future<Output = AppResult<BulkTransferResult>>,
{
    op_sink.emit(TransferEvent::Started {
        transfer_id: op_id.clone(),
        bytes_total: None,
    });
    let res = body.await;
    op_sink.emit(match &res {
        Ok(r) if r.canceled => TransferEvent::Canceled { transfer_id: op_id },
        Ok(_) => TransferEvent::Done { transfer_id: op_id, etag: None },
        Err(e) => TransferEvent::Failed { transfer_id: op_id, error: e.to_wire_string() },
    });
    res
}

/// Recursively LIST a remote prefix, enqueuing each object as a download into
/// `local_root` (subpaths preserved). Mid-flight cancellable like the other bulk ops.
#[allow(clippy::too_many_arguments)]
pub async fn download_directory(
    transfers: &crate::transfer::TransferManager,
    store: Arc<dyn ObjectStore>,
    account_id: &str,
    bucket: &str,
    prefix: &str,
    local_root: &Path,
    external_sink_factory: impl Fn(&str) -> ProgressSink,
    op_id: String,
    op_sink: ProgressSink,
    cancel: CancellationToken,
) -> AppResult<BulkTransferResult> {
    tokio::fs::create_dir_all(local_root).await?;
    // Canonicalize root once: path-traversal guard so server-controlled keys like
    // "a/../../etc/x" can't write outside local_root.
    let root_canonical = tokio::fs::canonicalize(local_root)
        .await
        .map_err(|e| AppError::Io(format!("canonicalize local_root: {e}")))?;

    run_op(op_id, op_sink, async {
        let mut out = BulkTransferResult::default();
        // Parents already mkdir'd + escape-checked this run; thousands of objects often share few dirs.
        let mut validated_parents: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

        let mut continuation: Option<String> = None;
        loop {
            if cancel.is_cancelled() {
                out.canceled = true;
                return Ok(out);
            }
            let page = tokio::select! {
                _ = cancel.cancelled() => {
                    out.canceled = true;
                    return Ok(out);
                }
                p = store.list_objects(
                    bucket,
                    ListOptions {
                        prefix: Some(prefix.to_string()),
                        delimiter: None,
                        continuation: continuation.clone(),
                        max_keys: Some(1000),
                    },
                ) => p?,
            };

            for obj in &page.objects {
                if cancel.is_cancelled() {
                    out.canceled = true;
                    return Ok(out);
                }
                let suffix = obj.key.strip_prefix(prefix).unwrap_or(&obj.key);
                let suffix = suffix.trim_start_matches('/');
                // Empty suffix is the prefix's own "directory marker".
                if suffix.is_empty() || !is_safe_relative_suffix(suffix) {
                    out.skipped.push(obj.key.clone());
                    continue;
                }
                let dest = local_root.join(suffix);
                // Defense in depth: if is_safe_relative_suffix missed something (symlink, OS
                // quirk), the resolved-parent check after mkdir catches it.
                if let Some(parent) = dest.parent() {
                    if !validated_parents.contains(parent) {
                        let checked = async {
                            tokio::fs::create_dir_all(parent).await?;
                            tokio::fs::canonicalize(parent).await
                        }
                        .await;
                        match checked {
                            Ok(c) if c.starts_with(&root_canonical) => {
                                validated_parents.insert(parent.to_path_buf());
                            }
                            Ok(_) => {
                                out.skipped.push(obj.key.clone());
                                continue;
                            }
                            Err(e) => {
                                out.errors.push(format!("{}: {e}", parent.display()));
                                continue;
                            }
                        }
                    }
                }
                let sink = external_sink_factory(&obj.key);
                let id = transfers
                    .enqueue_download(
                        store.clone(),
                        account_id.to_string(),
                        bucket.to_string(),
                        obj.key.clone(),
                        dest,
                        crate::store::GetOptions::default(),
                        sink,
                    )
                    .await?;
                out.enqueued.push(id);
            }

            if page.is_truncated {
                continuation = page.continuation;
                if continuation.is_none() {
                    // Truncated without a token would re-list page one forever.
                    tracing::warn!(bucket, "download_directory: listing truncated without a continuation token; stopping after this page");
                    break;
                }
            } else {
                break;
            }
        }
        Ok(out)
    })
    .await
}

/// True when a key suffix can safely join a download root: rejects empty segments,
/// `.`/`..`, absolute or drive-letter prefixes, and backslash separators.
fn is_safe_relative_suffix(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // Refuse drive letters on every platform so Windows-made backups can't escape elsewhere.
    if s.chars().nth(1) == Some(':') {
        return false;
    }
    // Treat backslashes as smuggled path separators cross-platform.
    for raw in s.split(|c| c == '/' || c == '\\') {
        if raw.is_empty() {
            return false;
        }
        if raw == "." || raw == ".." {
            return false;
        }
        if raw.starts_with('/') {
            return false;
        }
    }
    true
}
