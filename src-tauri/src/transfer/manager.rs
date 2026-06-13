//! Persistent transfer queue + worker scheduler.
//!
//! [`TransferManager`] owns the lifecycle of every upload and download. It:
//!
//! - Persists each transfer to the `transfers` SQLite table so progress survives
//!   restarts.
//! - Caps concurrent workers via a [`tokio::sync::Semaphore`].
//! - Tracks per-transfer [`CancellationToken`]s so [`TransferManager::cancel`] can
//!   abort an in-flight worker (and any S3 multipart it owns).
//! - Composes a [`ProgressSink`] that fans out to the FE [`tauri::ipc::Channel`]
//!   *and* persists milestone events back to the database.
//!
//! The manager is intentionally stateless beyond the cancel map; the source of
//! truth is the DB.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use tokio::sync::{Semaphore, Mutex as AsyncMutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::transfers::{Direction, NewTransfer, Transfer, TransferStatus};
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::store::{GetOptions, ObjectStore, PutOptions};

use super::{CompletedPart, ProgressSink, ResumeState, TransferCtx, TransferEvent};

/// Persistent transfer queue + worker scheduler. Cheap to clone (all interior
/// state is `Arc`-shared).
#[derive(Clone)]
pub struct TransferManager {
    db: Db,
    cancels: Arc<DashMap<String, CancellationToken>>,
    sem: Arc<Semaphore>,
}

/// What direction-specific work a worker should perform.
enum WorkerJob {
    Upload {
        bucket: String,
        key: String,
        local_path: PathBuf,
        opts: PutOptions,
    },
    Download {
        bucket: String,
        key: String,
        local_path: PathBuf,
        opts: GetOptions,
    },
}

impl WorkerJob {
    fn direction(&self) -> Direction {
        match self {
            WorkerJob::Upload { .. } => Direction::Upload,
            WorkerJob::Download { .. } => Direction::Download,
        }
    }
}

impl TransferManager {
    pub fn new(db: Db, concurrency: usize) -> Self {
        Self {
            db,
            cancels: Arc::new(DashMap::new()),
            sem: Arc::new(Semaphore::new(concurrency.max(1))),
        }
    }

    /// Enqueue a new upload. Inserts the transfer row, returns its id, and
    /// spawns a worker that will respect the global concurrency limit.
    pub async fn enqueue_upload(
        &self,
        store: Arc<dyn ObjectStore>,
        account_id: String,
        bucket: String,
        key: String,
        local_path: PathBuf,
        opts: PutOptions,
        external_sink: ProgressSink,
    ) -> AppResult<String> {
        self.enqueue(
            store,
            account_id,
            WorkerJob::Upload {
                bucket,
                key,
                local_path,
                opts,
            },
            external_sink,
            None,
        )
        .await
    }

    /// Enqueue a new download. Mirrors [`enqueue_upload`].
    pub async fn enqueue_download(
        &self,
        store: Arc<dyn ObjectStore>,
        account_id: String,
        bucket: String,
        key: String,
        local_path: PathBuf,
        opts: GetOptions,
        external_sink: ProgressSink,
    ) -> AppResult<String> {
        self.enqueue(
            store,
            account_id,
            WorkerJob::Download {
                bucket,
                key,
                local_path,
                opts,
            },
            external_sink,
            None,
        )
        .await
    }

    /// Cancel an active transfer. Idempotent — returns `Ok(())` if the transfer
    /// is already terminal (the cancel token has been dropped from the map).
    pub fn cancel(&self, transfer_id: &str) -> AppResult<()> {
        if let Some(token) = self.cancels.get(transfer_id) {
            token.cancel();
        }
        Ok(())
    }

    /// Cancel every active transfer belonging to an account. Used when an
    /// account is deleted so dangling workers don't keep writing to soon-
    /// cascade-deleted DB rows. Returns the number of transfers signalled.
    pub async fn cancel_for_account(&self, account_id: &str) -> AppResult<usize> {
        let active = self
            .db
            .list_transfers(Some(TransferStatus::Active))
            .await?;
        let pending = self
            .db
            .list_transfers(Some(TransferStatus::Pending))
            .await?;
        let mut signaled = 0usize;
        for t in active.iter().chain(pending.iter()) {
            if t.account_id == account_id {
                if let Some(token) = self.cancels.get(&t.id) {
                    token.cancel();
                    signaled += 1;
                }
            }
        }
        Ok(signaled)
    }

    pub async fn list(&self, status: Option<TransferStatus>) -> AppResult<Vec<Transfer>> {
        self.db.list_transfers(status).await
    }

    pub async fn get(&self, id: &str) -> AppResult<Transfer> {
        self.db.get_transfer(id).await
    }

    pub async fn clear_completed(&self) -> AppResult<usize> {
        self.db.clear_completed_transfers().await
    }

    /// Re-enqueue a failed/canceled/paused transfer as a *new* row. Carries
    /// over the upload_id + completed parts so multipart uploads resume where
    /// they left off.
    pub async fn retry(
        &self,
        store: Arc<dyn ObjectStore>,
        transfer_id: &str,
        external_sink: ProgressSink,
    ) -> AppResult<String> {
        let row = self.db.get_transfer(transfer_id).await?;
        if !matches!(
            row.status,
            TransferStatus::Failed | TransferStatus::Canceled | TransferStatus::Paused
        ) {
            return Err(AppError::InvalidInput(
                "transfer not in retriable state".into(),
            ));
        }

        let resume = match (row.upload_id.as_ref(), row.parts_json.as_ref()) {
            (Some(upload_id), Some(parts_json)) => {
                let parts: Vec<CompletedPart> = serde_json::from_str(parts_json).unwrap_or_default();
                Some(ResumeState {
                    upload_id: upload_id.clone(),
                    completed_parts: parts,
                })
            }
            _ => None,
        };

        // Recover the original PutOptions / GetOptions from the row so the
        // retry uses the same content-type, ACL, SSE, range, etc. as the
        // original enqueue. Falls back to defaults if the row predates the
        // options_json column or has bad JSON.
        let job = match row.direction {
            Direction::Upload => {
                let opts = row
                    .options_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<PutOptions>(raw).ok())
                    .unwrap_or_default();
                WorkerJob::Upload {
                    bucket: row.bucket.clone(),
                    key: row.key.clone(),
                    local_path: PathBuf::from(&row.local_path),
                    opts,
                }
            }
            Direction::Download => {
                let opts = row
                    .options_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<GetOptions>(raw).ok())
                    .unwrap_or_default();
                WorkerJob::Download {
                    bucket: row.bucket.clone(),
                    key: row.key.clone(),
                    local_path: PathBuf::from(&row.local_path),
                    opts,
                }
            }
        };

        self.enqueue(store, row.account_id, job, external_sink, resume)
            .await
    }

    /// Unified worker spawn used by upload, download, and retry paths.
    async fn enqueue(
        &self,
        store: Arc<dyn ObjectStore>,
        account_id: String,
        job: WorkerJob,
        external_sink: ProgressSink,
        resume: Option<ResumeState>,
    ) -> AppResult<String> {
        let id = Uuid::new_v4().to_string();
        let direction = job.direction();
        let (bucket_for_row, key_for_row, path_for_row) = match &job {
            WorkerJob::Upload {
                bucket,
                key,
                local_path,
                ..
            }
            | WorkerJob::Download {
                bucket,
                key,
                local_path,
                ..
            } => (bucket.clone(), key.clone(), local_path.to_string_lossy().to_string()),
        };

        let account_id_for_cache = account_id.clone();
        // Capture the options blob so a future `retry_transfer` can reapply
        // the same headers / ACL / SSE / range.
        let options_json = match &job {
            WorkerJob::Upload { opts, .. } => serde_json::to_string(opts).ok(),
            WorkerJob::Download { opts, .. } => serde_json::to_string(opts).ok(),
        };
        self.db
            .insert_transfer(NewTransfer {
                id: id.clone(),
                account_id,
                bucket: bucket_for_row.clone(),
                key: key_for_row.clone(),
                direction,
                local_path: path_for_row,
                options_json,
            })
            .await?;

        let cancel = CancellationToken::new();
        self.cancels.insert(id.clone(), cancel.clone());

        let sink = self.composite_sink(id.clone(), external_sink);
        // Pull per-transfer tunables from user settings so the FE can
        // influence them without touching backend code.
        let settings = self.db.settings_load().await?;
        let mut ctx = TransferCtx {
            transfer_id: id.clone(),
            cancel: cancel.clone(),
            progress: sink,
            part_size: settings.part_size_bytes,
            parallelism: settings.multipart_parallelism as usize,
            multipart_threshold: settings.multipart_threshold_bytes,
            resume: None,
        };
        if let Some(r) = resume {
            ctx = ctx.with_resume(r);
        }

        let db = self.db.clone();
        let cancels = self.cancels.clone();
        let sem = self.sem.clone();
        let id_for_task = id.clone();
        let store_for_task = store.clone();
        let bucket_for_cache = bucket_for_row;
        let key_for_cache = key_for_row;

        tokio::spawn(async move {
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let _ = db
                .update_transfer_status(&id_for_task, TransferStatus::Active, None)
                .await;

            let result = match job {
                WorkerJob::Upload {
                    bucket,
                    key,
                    local_path,
                    opts,
                } => store_for_task
                    .put_object(&bucket, &key, local_path, opts, ctx)
                    .await
                    .map(|_| ()),
                WorkerJob::Download {
                    bucket,
                    key,
                    local_path,
                    opts,
                } => store_for_task
                    .get_object(&bucket, &key, local_path, opts, ctx)
                    .await
                    .map(|_| ()),
            };

            // Cache write-through on successful upload: HEAD the freshly-written
            // object to get authoritative metadata, then upsert into the cache.
            if matches!(direction, Direction::Upload) && result.is_ok() {
                if let Ok(meta) = store_for_task
                    .head_object(&bucket_for_cache, &key_for_cache)
                    .await
                {
                    let _ = db
                        .cache_upsert_object(&account_id_for_cache, &bucket_for_cache, &meta)
                        .await;
                }
            }

            // Capability tracking: only uploads contribute to `last_put_result`
            // and we only flip the cap on Allowed / AccessDenied — other
            // failure classes (network, cancel) don't prove anything.
            if matches!(direction, Direction::Upload) {
                use crate::db::capabilities::{CapState, WriteOp};
                let cap = match &result {
                    Ok(()) => Some(CapState::Allowed),
                    Err(crate::error::AppError::AccessDenied(_)) => Some(CapState::Denied),
                    _ => None,
                };
                if let Some(cap) = cap {
                    let _ = db
                        .capability_record_write(
                            &account_id_for_cache,
                            &bucket_for_cache,
                            WriteOp::Put,
                            cap,
                        )
                        .await;
                }
            }

            let terminal = match result {
                Ok(()) => TransferStatus::Done,
                Err(AppError::Canceled(_)) => TransferStatus::Canceled,
                Err(_) => TransferStatus::Failed,
            };
            let err_text = result.err().map(|e| e.to_string());
            let _ = db
                .update_transfer_status(&id_for_task, terminal, err_text)
                .await;
            cancels.remove(&id_for_task);
        });

        Ok(id)
    }

    /// Compose a sink that fans out to the FE channel AND persists milestone
    /// progress to the database. The DB-persistence half intentionally batches
    /// the parts list in an in-memory buffer to avoid touching SQLite on every
    /// chunk.
    fn composite_sink(&self, transfer_id: String, external: ProgressSink) -> ProgressSink {
        let db = self.db.clone();
        let parts: Arc<Mutex<Vec<CompletedPart>>> = Arc::new(Mutex::new(Vec::new()));
        // Serialize DB writes for this transfer's PartCompleted snapshots.
        // Concurrent multipart workers fire emits in any order; without this
        // lock the spawned `update_transfer_multipart` tasks could write
        // out-of-date snapshots over newer ones.
        let parts_db_lock: Arc<AsyncMutex<()>> = Arc::new(AsyncMutex::new(()));

        ProgressSink::from_fn(move |event: TransferEvent| {
            external.emit(event.clone());

            let db = db.clone();
            let parts = parts.clone();
            let tid = transfer_id.clone();

            match event {
                TransferEvent::Started { bytes_total, .. } => {
                    tokio::spawn(async move {
                        let _ = db
                            .update_transfer_bytes(&tid, 0, bytes_total.map(|n| n as i64))
                            .await;
                    });
                }
                TransferEvent::Progress {
                    bytes_done,
                    bytes_total,
                    ..
                } => {
                    tokio::spawn(async move {
                        let _ = db
                            .update_transfer_bytes(
                                &tid,
                                bytes_done as i64,
                                bytes_total.map(|n| n as i64),
                            )
                            .await;
                    });
                }
                TransferEvent::PartCompleted {
                    part_number, etag, ..
                } => {
                    {
                        let mut guard = parts.lock().unwrap();
                        guard.push(CompletedPart { part_number, etag });
                    }
                    let parts_for_write = parts.clone();
                    let lock = parts_db_lock.clone();
                    tokio::spawn(async move {
                        // Take the async mutex so only one DB write for this
                        // transfer is in flight at a time; the snapshot read
                        // happens *inside* the critical section so the write
                        // always reflects the latest state.
                        let _guard = lock.lock().await;
                        let snapshot = parts_for_write.lock().unwrap().clone();
                        let _ = db.update_transfer_multipart(&tid, None, &snapshot).await;
                    });
                }
                _ => {}
            }
        })
    }
}
