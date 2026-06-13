//! Folder-scoped bulk operations: recursive delete, recursive upload,
//! recursive download.
//!
//! These compose the trait-level primitives ([`ObjectStore::list_objects`],
//! [`ObjectStore::delete_objects`], [`TransferManager::enqueue_upload`],
//! [`TransferManager::enqueue_download`]) into single user-facing actions.
//! Each emits [`TransferEvent`]s through a [`ProgressSink`] so the front-end
//! can show one progress bar for the whole job, and each respects a
//! [`CancellationToken`] for mid-flight stop.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::store::{ListOptions, ObjectStore};
use crate::transfer::{ProgressSink, TransferEvent};

#[derive(Debug, Clone, Default, Serialize)]
pub struct BulkDeleteResult {
    pub deleted: u64,
    pub failed: u64,
    pub errors: Vec<String>,
}

/// Recursively delete every object under `prefix` using batched
/// `DeleteObjects` requests (1000 keys per call). Mirrors the deletions into
/// the local cache. Progress events use `bytes_done = deleted count`,
/// `bytes_total = None` (unknown until LIST completes — we delete in batches
/// as we scan).
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

    let mut result = BulkDeleteResult::default();
    let mut continuation: Option<String> = None;
    const BATCH: usize = 1000;
    let mut buffer: Vec<String> = Vec::with_capacity(BATCH);

    let job: AppResult<()> = async {
        loop {
            if cancel.is_cancelled() {
                return Err(AppError::Canceled(format!("delete_folder {prefix}")));
            }
            let page = tokio::select! {
                _ = cancel.cancelled() => return Err(AppError::Canceled(format!("delete_folder {prefix}"))),
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
                buffer.push(obj.key.clone());
                if buffer.len() >= BATCH {
                    flush(
                        &store,
                        db,
                        account_id,
                        bucket,
                        &mut buffer,
                        &mut result,
                        &sink,
                        &transfer_id,
                    )
                    .await?;
                }
            }

            if page.is_truncated {
                continuation = page.continuation;
            } else {
                break;
            }
        }
        if !buffer.is_empty() {
            flush(
                &store,
                db,
                account_id,
                bucket,
                &mut buffer,
                &mut result,
                &sink,
                &transfer_id,
            )
            .await?;
        }
        Ok(())
    }
    .await;

    match job {
        Ok(()) => {
            sink.emit(TransferEvent::Done {
                transfer_id,
                etag: None,
            });
            Ok(result)
        }
        Err(AppError::Canceled(m)) => {
            sink.emit(TransferEvent::Canceled { transfer_id });
            Err(AppError::Canceled(m))
        }
        Err(e) => {
            sink.emit(TransferEvent::Failed {
                transfer_id,
                error: e.to_string(),
            });
            Err(e)
        }
    }
}

async fn flush(
    store: &Arc<dyn ObjectStore>,
    db: &Db,
    account_id: &str,
    bucket: &str,
    buffer: &mut Vec<String>,
    result: &mut BulkDeleteResult,
    sink: &ProgressSink,
    transfer_id: &str,
) -> AppResult<()> {
    let keys = std::mem::take(buffer);
    let outcome = store.delete_objects(bucket, &keys).await?;
    for k in &outcome.deleted {
        let _ = db.cache_remove_object(account_id, bucket, k).await;
        result.deleted += 1;
    }
    for e in &outcome.errors {
        result.failed += 1;
        result.errors.push(format!(
            "{}: {}",
            e.key,
            e.message.as_deref().unwrap_or("unknown")
        ));
    }
    sink.emit(TransferEvent::Progress {
        transfer_id: transfer_id.to_string(),
        bytes_done: result.deleted,
        bytes_total: None,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Recursive upload / download — delegate to TransferManager so each file flows
// through the same queue (concurrency, cancel-per-file, cache write-through).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct BulkTransferResult {
    pub enqueued: Vec<String>,
    pub skipped: Vec<String>,
}

/// Walk a local directory and enqueue every file as an individual upload.
/// Returns the list of transfer ids created. The FE listens on those.
/// Subdirectories are joined onto `prefix` using `/`.
pub async fn upload_directory(
    transfers: &crate::transfer::TransferManager,
    store: Arc<dyn ObjectStore>,
    account_id: &str,
    bucket: &str,
    prefix: &str,
    local_root: &Path,
    external_sink_factory: impl Fn(&str) -> ProgressSink,
) -> AppResult<BulkTransferResult> {
    if !local_root.is_dir() {
        return Err(AppError::InvalidInput(format!(
            "not a directory: {}",
            local_root.display()
        )));
    }
    let mut out = BulkTransferResult {
        enqueued: Vec::new(),
        skipped: Vec::new(),
    };
    let mut stack: Vec<PathBuf> = vec![local_root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let meta = entry.metadata().await?;
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() {
                out.skipped.push(path.to_string_lossy().to_string());
                continue;
            }
            let rel = path
                .strip_prefix(local_root)
                .map_err(|e| AppError::Internal(e.to_string()))?;
            let key = join_key(prefix, rel);
            let sink = external_sink_factory(&path.to_string_lossy());
            let id = transfers
                .enqueue_upload(
                    store.clone(),
                    account_id.to_string(),
                    bucket.to_string(),
                    key,
                    path.clone(),
                    crate::store::PutOptions::default(),
                    sink,
                )
                .await?;
            out.enqueued.push(id);
        }
    }

    Ok(out)
}

/// Walk a remote prefix (recursive LIST) and enqueue every object as an
/// individual download into `local_root`, preserving subpath structure.
pub async fn download_directory(
    transfers: &crate::transfer::TransferManager,
    store: Arc<dyn ObjectStore>,
    account_id: &str,
    bucket: &str,
    prefix: &str,
    local_root: &Path,
    external_sink_factory: impl Fn(&str) -> ProgressSink,
) -> AppResult<BulkTransferResult> {
    tokio::fs::create_dir_all(local_root).await?;
    let mut out = BulkTransferResult {
        enqueued: Vec::new(),
        skipped: Vec::new(),
    };

    // Canonicalize the root once so we can verify every destination is
    // contained within it. This is the path-traversal guard: a server-
    // controlled key like "a/../../etc/x" would otherwise write outside
    // local_root.
    let root_canonical = tokio::fs::canonicalize(local_root)
        .await
        .map_err(|e| AppError::Io(format!("canonicalize local_root: {e}")))?;

    let mut continuation: Option<String> = None;
    loop {
        let page = store
            .list_objects(
                bucket,
                ListOptions {
                    prefix: Some(prefix.to_string()),
                    delimiter: None,
                    continuation: continuation.clone(),
                    max_keys: Some(1000),
                },
            )
            .await?;

        for obj in &page.objects {
            // Strip the leading prefix so the local layout starts at root.
            let suffix = obj.key.strip_prefix(prefix).unwrap_or(&obj.key);
            let suffix = suffix.trim_start_matches('/');
            if suffix.is_empty() {
                // The prefix itself is a "directory marker" — skip it.
                out.skipped.push(obj.key.clone());
                continue;
            }
            // Reject any key component that would escape local_root. We check
            // before joining so we can refuse without touching the FS.
            if !is_safe_relative_suffix(suffix) {
                out.skipped.push(obj.key.clone());
                continue;
            }
            let dest = local_root.join(suffix);
            // Defense in depth: even if is_safe_relative_suffix missed
            // something (symlink, OS-specific quirk), check the resolved
            // parent escape after mkdir.
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
                let parent_canonical = tokio::fs::canonicalize(parent)
                    .await
                    .map_err(|e| AppError::Io(format!("canonicalize dest parent: {e}")))?;
                if !parent_canonical.starts_with(&root_canonical) {
                    out.skipped.push(obj.key.clone());
                    continue;
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
        } else {
            break;
        }
    }
    Ok(out)
}

/// Return `true` when an object-key suffix can be safely joined onto a
/// download root without escaping it. Rejects:
///
/// - Empty segments
/// - `.` or `..`
/// - Absolute path prefixes (Unix `/`, Windows drive letter)
/// - Windows reserved chars and backslash separators that would alter the
///   directory structure on translation
fn is_safe_relative_suffix(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // Drive letters like "C:\..." or "C:..." — refuse on every platform so
    // backups generated on Windows can't escape on macOS or Linux either.
    if s.chars().nth(1) == Some(':') {
        return false;
    }
    // Backslashes are treated as separators on Windows; treat them as keys
    // for cross-platform safety so a suffix can't smuggle path separators.
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

/// Compose `prefix + rel_path` into an S3 key. Always uses forward slashes
/// even on Windows.
fn join_key(prefix: &str, rel: &Path) -> String {
    let rel_str: String = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let cleaned_prefix = prefix.trim_end_matches('/');
    if cleaned_prefix.is_empty() {
        rel_str
    } else {
        format!("{cleaned_prefix}/{rel_str}")
    }
}
