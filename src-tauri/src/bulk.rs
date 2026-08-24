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

#[derive(Debug, Clone, Serialize)]
pub struct BulkTransferResult {
    pub enqueued: Vec<String>,
    pub skipped: Vec<String>,
}

/// Walk a local dir, enqueueing each file as an individual upload (subdirs joined onto
/// `prefix`); encrypted buckets get per-file stream-encryption to enc_tmp, config once/op.
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
    cancel: CancellationToken,
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

    // Resolve encryption once per op (mirrors encrypt_for_bucket_if_needed_with,
    // hoisted out of the loop); None => plaintext.
    let enc_cfg = db.get_encryption_config(account_id, bucket).await?;
    let enc = match &enc_cfg {
        Some(cfg) => {
            let recipient = crate::crypto::parse_recipient(&cfg.recipient)?;
            let tmp_dir = db_path
                .parent()
                .ok_or_else(|| AppError::Internal("db_path has no parent".into()))?
                .join("enc_tmp");
            tokio::fs::create_dir_all(&tmp_dir).await?;
            Some((recipient, tmp_dir))
        }
        None => None,
    };

    while let Some(dir) = stack.pop() {
        if cancel.is_cancelled() {
            return Err(AppError::Canceled("upload_directory canceled".into()));
        }
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
            if cancel.is_cancelled() {
                return Err(AppError::Canceled("upload_directory canceled".into()));
            }

            // Encrypt BEFORE enqueue so plaintext never reaches the worker; failure aborts
            // the op rather than uploading plaintext (staged temps owned via cleanup_path).
            let mut opts = crate::store::PutOptions::default();
            let mut upload_path = path.clone();
            if let Some((recipient, tmp_dir)) = &enc {
                let tmp_path = tmp_dir.join(format!("{}.age", uuid::Uuid::new_v4()));
                if let Err(e) =
                    crate::crypto::encrypt_file(&path, &tmp_path, recipient.clone()).await
                {
                    let _ = tokio::fs::remove_file(&tmp_path).await;
                    return Err(e);
                }
                opts.cleanup_path = Some(tmp_path.clone());
                // Same metadata markers as the single-file path so download +
                // UI detection work identically for bulk-uploaded objects.
                opts.user_metadata
                    .insert("cosmog-encrypted".into(), "1".into());
                opts.user_metadata
                    .insert("cosmog-format".into(), crate::crypto::FORMAT_TAG.into());
                if let Some(cfg) = &enc_cfg {
                    opts.user_metadata
                        .insert("cosmog-recipient".into(), cfg.recipient.clone());
                }
                upload_path = tmp_path;
            }

            let sink = external_sink_factory(&path.to_string_lossy());
            let id = transfers
                .enqueue_upload(
                    store.clone(),
                    account_id.to_string(),
                    bucket.to_string(),
                    key,
                    upload_path,
                    opts,
                    sink,
                    crate::db::transfers::TransferOrigin::User,
                )
                .await?;
            out.enqueued.push(id);
        }
    }

    Ok(out)
}

/// Recursively LIST a remote prefix, enqueuing each object as a download into
/// `local_root` (subpaths preserved). Mid-flight cancellable like the other bulk ops.
pub async fn download_directory(
    transfers: &crate::transfer::TransferManager,
    store: Arc<dyn ObjectStore>,
    account_id: &str,
    bucket: &str,
    prefix: &str,
    local_root: &Path,
    external_sink_factory: impl Fn(&str) -> ProgressSink,
    cancel: CancellationToken,
) -> AppResult<BulkTransferResult> {
    tokio::fs::create_dir_all(local_root).await?;
    let mut out = BulkTransferResult {
        enqueued: Vec::new(),
        skipped: Vec::new(),
    };

    // Canonicalize root once: path-traversal guard so server-controlled keys like
    // "a/../../etc/x" can't write outside local_root.
    let root_canonical = tokio::fs::canonicalize(local_root)
        .await
        .map_err(|e| AppError::Io(format!("canonicalize local_root: {e}")))?;

    // Parents already mkdir'd + escape-checked this run; skips repeated syscalls
    // since thousands of objects often share few dirs.
    let mut validated_parents: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    let mut continuation: Option<String> = None;
    loop {
        if cancel.is_cancelled() {
            return Err(AppError::Canceled(format!("download_directory {prefix}")));
        }
        let page = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(AppError::Canceled(format!("download_directory {prefix}")))
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
                return Err(AppError::Canceled(format!("download_directory {prefix}")));
            }
            let suffix = obj.key.strip_prefix(prefix).unwrap_or(&obj.key);
            let suffix = suffix.trim_start_matches('/');
            if suffix.is_empty() {
                // The prefix itself is a "directory marker" — skip it.
                out.skipped.push(obj.key.clone());
                continue;
            }
            // Reject components that would escape local_root, before touching the FS.
            if !is_safe_relative_suffix(suffix) {
                out.skipped.push(obj.key.clone());
                continue;
            }
            let dest = local_root.join(suffix);
            // Defense in depth: if is_safe_relative_suffix missed something (symlink, OS
            // quirk), the resolved-parent check after mkdir catches it.
            if let Some(parent) = dest.parent() {
                if !validated_parents.contains(parent) {
                    tokio::fs::create_dir_all(parent).await?;
                    let parent_canonical = tokio::fs::canonicalize(parent)
                        .await
                        .map_err(|e| AppError::Io(format!("canonicalize dest parent: {e}")))?;
                    if !parent_canonical.starts_with(&root_canonical) {
                        out.skipped.push(obj.key.clone());
                        continue;
                    }
                    validated_parents.insert(parent.to_path_buf());
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
