//! Cache synchronisation: replicate remote object listings into the local
//! [`crate::db::cache`] tables.
//!
//! Two flavours:
//!
//! - **Prefix sync** — single LIST traversal (delimiter `/` for direct
//!   children, omitted for recursive). Cheap. Used on navigation + manual
//!   refresh. Atomic mark-unseen → upsert → sweep-unseen.
//! - **Full-bucket scan** — recursive LIST through every page. Cancellable
//!   and resumable: the continuation token is persisted to `bucket_index`
//!   after every page, so a cancel (or app crash) can resume from the last
//!   completed page on the next call.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::db::cache::SyncScope;
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::store::{ListOptions, ObjectStore};
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

        if page.is_truncated {
            continuation = page.continuation;
        } else {
            break;
        }
    }

    stats.removed = db.cache_sweep_unseen(account_id, bucket, scope).await? as u64;
    stats.completed = true;
    db.prefix_sync_set(account_id, bucket, prefix).await?;
    Ok(stats)
}

/// Full bucket scan. Cancellable, resumable.
///
/// If a previous scan was interrupted, `scan_continuation` in `bucket_index`
/// is non-null and the scan resumes from that token (without re-marking
/// already-seen rows). On fresh starts, marks every cached row in the bucket
/// as unseen before walking; rows still unseen at the end represent remote
/// deletions and get swept.
///
/// Progress events use `bytes_done = number of objects observed since the
/// scan started`. `bytes_total` is always `None` because S3 LIST does not
/// surface a total up-front.
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

    // Detect resume: existing scan_continuation means we crashed or canceled
    // mid-walk. Pick up from there without re-marking rows.
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
