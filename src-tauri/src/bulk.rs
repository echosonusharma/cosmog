//! Folder-scoped bulk ops (recursive delete/upload/download) composing store/transfer
//! primitives into single actions, with ProgressSink reporting and cancellation support.

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

/// Recursively delete under `prefix` via batched DeleteObjects (1000 keys/call),
/// mirroring deletions into the local cache; `bytes_done` reports the deleted count.
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
        Err(e) => TransferEvent::Failed { transfer_id: op_id, error: e.to_string() },
    });
    res
}

/// Walk a local dir, enqueueing each file as an individual upload (subdirs joined onto
/// `prefix`). Encrypted buckets are stamped once; each worker encrypts its own file.
#[allow(clippy::too_many_arguments)]
pub async fn upload_directory(
    transfers: &crate::transfer::TransferManager,
    db: &crate::db::Db,
    db_path: &Path,
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
    if !local_root.is_dir() {
        return Err(AppError::InvalidInput(format!(
            "not a directory: {}",
            local_root.display()
        )));
    }
    run_op(op_id, op_sink, async {
        let mut out = BulkTransferResult::default();
        let enc = crate::transfer::encrypt::encrypt_spec_for_bucket(db, db_path, account_id, bucket).await?;
        let mut stack: Vec<PathBuf> = vec![local_root.to_path_buf()];

        while let Some(dir) = stack.pop() {
            if cancel.is_cancelled() {
                out.canceled = true;
                return Ok(out);
            }
            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(rd) => rd,
                Err(e) if dir.as_path() != local_root => {
                    out.errors.push(format!("{}: {e}", dir.display()));
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            loop {
                let entry = match entries.next_entry().await {
                    Ok(Some(entry)) => entry,
                    Ok(None) => break,
                    Err(e) => {
                        out.errors.push(format!("{}: {e}", dir.display()));
                        break;
                    }
                };
                let path = entry.path();
                let meta = match entry.metadata().await {
                    Ok(m) => m,
                    Err(e) => {
                        out.errors.push(format!("{}: {e}", path.display()));
                        continue;
                    }
                };
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
                if cancel.is_cancelled() {
                    out.canceled = true;
                    return Ok(out);
                }

                let mut opts = crate::store::PutOptions::default();
                if let Some(spec) = &enc {
                    crate::transfer::encrypt::stamp(&mut opts, spec);
                }
                let sink = external_sink_factory(&path.to_string_lossy());
                let id = transfers
                    .enqueue_upload(
                        store.clone(),
                        account_id.to_string(),
                        bucket.to_string(),
                        key,
                        path,
                        opts,
                        sink,
                        crate::db::transfers::TransferOrigin::User,
                    )
                    .await?;
                out.enqueued.push(id);
            }
        }
        Ok(out)
    })
    .await
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
