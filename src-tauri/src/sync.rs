//! Cache sync: replicate remote listings into local cache tables. Prefix sync is one
//! atomic LIST traversal; full-bucket scans are cancellable/resumable via saved tokens.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::db::cache::SyncScope;
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::store::{ListOptions, ObjectMeta, ObjectStore};
use crate::transfer::{ProgressSink, TransferEvent};

/// Numbers reported back to the caller after a sync completes.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncStats {
    pub upserted: u64,
    pub removed: u64,
    pub pages: u64,
    /// True when the scan finished all remaining pages. False when it
    /// returned early due to cancellation — caller may resume later.
    pub completed: bool,
}

/// Sync the direct children of a prefix (one logical LIST traversal,
/// delimiter `/`).
pub async fn sync_prefix_direct(
    db: &Db,
    store: Arc<dyn ObjectStore>,
    account_id: &str,
    bucket: &str,
    prefix: &str,
) -> AppResult<SyncStats> {
    let scope = SyncScope::PrefixDirect {
        prefix: prefix.to_string(),
    };
    sync_prefix_impl(db, store, account_id, bucket, prefix, Some("/"), scope).await
}

/// Sync everything under a prefix recursively.
pub async fn sync_prefix_recursive(
    db: &Db,
    store: Arc<dyn ObjectStore>,
    account_id: &str,
    bucket: &str,
    prefix: &str,
) -> AppResult<SyncStats> {
    let scope = SyncScope::PrefixRecursive {
        prefix: prefix.to_string(),
    };
    sync_prefix_impl(db, store, account_id, bucket, prefix, None, scope).await
}

async fn sync_prefix_impl(
    db: &Db,
    store: Arc<dyn ObjectStore>,
    account_id: &str,
    bucket: &str,
    prefix: &str,
    delimiter: Option<&str>,
    scope: SyncScope,
) -> AppResult<SyncStats> {
    db.cache_mark_unseen(account_id, bucket, scope.clone()).await?;

    let mut stats = SyncStats::default();
    let mut continuation: Option<String> = None;
    // CommonPrefixes seen across pages — used to upsert folder markers into
    // the index and drop markers for folders that disappeared remotely.
    let mut seen_prefixes: Vec<String> = Vec::new();
    let delimited = delimiter == Some("/");

    loop {
        let page = store
            .list_objects(
                bucket,
                ListOptions {
                    prefix: if prefix.is_empty() {
                        None
                    } else {
                        Some(prefix.to_string())
                    },
                    delimiter: delimiter.map(String::from),
                    continuation: continuation.clone(),
                    max_keys: Some(1000),
                },
            )
            .await?;

        let upserted = db.cache_upsert_objects_batch(account_id, bucket, &page.objects).await?;
        stats.upserted += upserted as u64;
        stats.pages += 1;

        if delimited {
            // Empty folder markers often appear only in Contents, not CommonPrefixes;
            // keep those keys so reconcile won't delete our synthetic rows.
            let mut content_markers = std::collections::HashSet::new();
            for obj in &page.objects {
                if obj.key.ends_with('/') {
                    content_markers.insert(obj.key.clone());
                    seen_prefixes.push(obj.key.clone());
                }
            }
            // Only synthesize markers for CommonPrefixes that weren't already
            // present as real trailing-slash objects on this page.
            let synthetic: Vec<ObjectMeta> = page
                .prefixes
                .iter()
                .filter(|p| !content_markers.contains(*p))
                .map(|p| ObjectMeta {
                    key: p.clone(),
                    size: 0,
                    etag: None,
                    last_modified: None,
                    storage_class: None,
                    content_type: Some("application/x-directory".into()),
                    version_id: None,
                    user_metadata: Default::default(),
                })
                .collect();
            if !synthetic.is_empty() {
                let n = db
                    .cache_upsert_objects_batch(account_id, bucket, &synthetic)
                    .await?;
                stats.upserted += n as u64;
            }
            seen_prefixes.extend(page.prefixes);
        }

        if page.is_truncated {
            continuation = page.continuation;
        } else {
            break;
        }
    }

    stats.removed = db.cache_sweep_unseen(account_id, bucket, scope).await? as u64;
    if delimited {
        stats.removed += db
            .cache_reconcile_dir_markers(account_id, bucket, prefix, &seen_prefixes)
            .await? as u64;
    }
    stats.completed = true;
    db.prefix_sync_set(account_id, bucket, prefix).await?;
    Ok(stats)
}

/// Full-bucket scan; cancellable and resumable via `scan_continuation` persisted in
/// `bucket_index` after every page. Fresh starts mark rows unseen, then sweep survivors.
pub async fn full_bucket_scan(
    db: &Db,
    store: Arc<dyn ObjectStore>,
    account_id: &str,
    bucket: &str,
    sink: ProgressSink,
    transfer_id: String,
    cancel: CancellationToken,
) -> AppResult<SyncStats> {
    sink.emit(TransferEvent::Started {
        transfer_id: transfer_id.clone(),
        bytes_total: None,
    });

    // Existing scan_continuation => crashed/canceled mid-walk; resume without re-marking.
    let status = db.bucket_index_get(account_id, bucket).await?;
    let mut continuation = status.scan_continuation.clone();
    let resuming = continuation.is_some();

    if !resuming {
        db.bucket_scan_begin(account_id, bucket).await?;
        db.cache_mark_unseen(account_id, bucket, SyncScope::Bucket)
            .await?;
    }

    let mut stats = SyncStats::default();
    let mut seen_total: u64 = 0;

    let result: AppResult<()> = async {
        loop {
            if cancel.is_cancelled() {
                return Err(AppError::Canceled(format!(
                    "bucket scan {bucket} canceled"
                )));
            }

            let page = tokio::select! {
                _ = cancel.cancelled() => return Err(AppError::Canceled(format!(
                    "bucket scan {bucket} canceled"
                ))),
                p = store.list_objects(
                    bucket,
                    ListOptions {
                        prefix: None,
                        delimiter: None,
                        continuation: continuation.clone(),
                        max_keys: Some(1000),
                    },
                ) => p?,
            };

            let batch_count = db.cache_upsert_objects_batch(account_id, bucket, &page.objects).await?;
            stats.upserted += batch_count as u64;
            seen_total += batch_count as u64;
            stats.pages += 1;

            sink.emit(TransferEvent::Progress {
                transfer_id: transfer_id.clone(),
                bytes_done: seen_total,
                bytes_total: None,
            });

            if page.is_truncated {
                continuation = page.continuation.clone();
                // Persist so we can resume from this point on next call.
                db.bucket_scan_progress(account_id, bucket, continuation.clone())
                    .await?;
            } else {
                break;
            }
        }
        Ok(())
    }
    .await;

    match result {
        Ok(()) => {
            stats.removed = db
                .cache_sweep_unseen(account_id, bucket, SyncScope::Bucket)
                .await? as u64;
            stats.completed = true;
            db.bucket_index_finalize(account_id, bucket).await?;
            db.bucket_scan_clear(account_id, bucket).await?;
            sink.emit(TransferEvent::Done {
                transfer_id,
                etag: None,
            });
            Ok(stats)
        }
        Err(AppError::Canceled(msg)) => {
            // Continuation token is already saved; leave seen=0 rows in place
            // so the next resume + completion can sweep them.
            sink.emit(TransferEvent::Canceled { transfer_id });
            Err(AppError::Canceled(msg))
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
