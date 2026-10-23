//! Beyond the cancel map, the DB is the source of truth for transfer state.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use dashmap::DashMap;
use futures::FutureExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::transfers::{Direction, NewTransfer, Transfer, TransferOrigin, TransferStatus};
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::store::{GetOptions, ObjectStore, PutOptions};

use super::semaphore::ResizableSemaphore;
use super::sink::composite_sink;
use super::worker::{run_download, run_upload};
use super::{CompletedPart, ProgressSink, ResumeState, TransferCtx, TransferEvent};

#[derive(Clone)]
pub struct TransferManager {
    db: Db,
    cancels: Arc<DashMap<String, CancellationToken>>,
    sem: Arc<ResizableSemaphore>,
    enqueued: Arc<std::sync::atomic::AtomicU64>,
}

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
        /// Retry of an earlier row: its `.cosmog-part` may be resumed on attempt 0.
        resume_part: bool,
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
            sem: Arc::new(ResizableSemaphore::new(concurrency)),
            enqueued: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Applies to the next acquisition; in-flight transfers are not interrupted.
    pub fn set_concurrency(&self, n: usize) {
        self.sem.resize(n);
    }

    pub async fn enqueue_upload(
        &self,
        store: Arc<dyn ObjectStore>,
        account_id: String,
        bucket: String,
        key: String,
        local_path: PathBuf,
        opts: PutOptions,
        external_sink: ProgressSink,
        origin: TransferOrigin,
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
            origin,
        )
        .await
    }

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
                resume_part: false,
            },
            external_sink,
            None,
            TransferOrigin::User,
        )
        .await
    }

    /// Idempotent: `Ok(())` if the transfer is already terminal.
    pub fn cancel(&self, transfer_id: &str) -> AppResult<()> {
        if let Some(token) = self.cancels.get(transfer_id) {
            token.cancel();
        }
        Ok(())
    }

    pub fn is_live(&self, transfer_id: &str) -> bool {
        self.cancels.contains_key(transfer_id)
    }

    /// Ghost rows from a dead process are flipped to `canceled`; live workers emit their own terminal.
    pub async fn cancel_or_reap(&self, transfer_id: &str) -> AppResult<()> {
        if let Some(token) = self.cancels.get(transfer_id) {
            token.cancel();
            return Ok(());
        }
        self.db.mark_canceled_if_active(transfer_id).await?;
        Ok(())
    }

    /// Stops dangling workers writing to soon-cascade-deleted rows.
    pub async fn cancel_for_account(&self, account_id: &str) -> AppResult<usize> {
        let ids = self.db.list_cancellable_ids_for_account(account_id).await?;
        let mut signaled = 0usize;
        for id in &ids {
            if let Some(token) = self.cancels.get(id) {
                token.cancel();
                signaled += 1;
            }
        }
        Ok(signaled)
    }

    /// Stops workers before the bucket's cache rows are purged and S3 starts returning 404.
    pub async fn cancel_for_bucket(&self, account_id: &str, bucket: &str) -> AppResult<usize> {
        let ids = self
            .db
            .list_cancellable_ids_for_bucket(account_id, bucket)
            .await?;
        let mut signaled = 0usize;
        for id in &ids {
            if let Some(token) = self.cancels.get(id) {
                token.cancel();
                signaled += 1;
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

    pub async fn delete_one(&self, id: &str) -> AppResult<()> {
        self.db.delete_transfer(id).await
    }

    /// New row; uploads carry over the multipart upload, downloads resume their `.cosmog-part`.
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

        let mut resume = match (row.direction, row.upload_id.as_ref()) {
            (Direction::Upload, Some(upload_id)) => {
                Some(parse_resume(&row.id, upload_id, row.parts_json.as_deref()))
            }
            _ => None,
        };

        // Reuse the original options; defaults if the column is missing or JSON is bad.
        let job = match row.direction {
            Direction::Upload => {
                let mut opts = row
                    .options_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<PutOptions>(raw).ok())
                    .unwrap_or_default();
                self.refresh_encryption(&row, &mut opts).await?;
                // Re-encryption yields different ciphertext, so saved parts are useless.
                if opts.encrypt.is_some() {
                    if let Some(r) = resume.take() {
                        let _ = store
                            .abort_multipart_upload(&row.bucket, &row.key, &r.upload_id)
                            .await;
                    }
                }
                WorkerJob::Upload {
                    bucket: row.bucket.clone(),
                    key: row.key.clone(),
                    local_path: PathBuf::from(&row.local_path),
                    opts,
                }
            }
            Direction::Download => {
                let mut opts = row
                    .options_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<GetOptions>(raw).ok())
                    .unwrap_or_default();
                // Older builds persisted their auto-resume offset as if it were an explicit range.
                if opts.resume {
                    opts.resume = false;
                    opts.range_start = None;
                }
                // Defense-in-depth against tampered DB rows.
                let local_path = crate::validate::validate_download_dest(&row.local_path).await
                    .map_err(|e| AppError::InvalidInput(format!("retry: invalid local_path: {e}")))?;
                WorkerJob::Download {
                    bucket: row.bucket.clone(),
                    key: row.key.clone(),
                    local_path,
                    opts,
                    resume_part: true,
                }
            }
        };

        // Preserve the original origin so a retried night-watch upload stays silent.
        let id = self
            .enqueue(store, row.account_id, job, external_sink, resume, row.origin)
            .await?;
        // The new row owns the multipart upload now; clearing the old row must not abort it.
        if row.upload_id.is_some() {
            let _ = self.db.clear_transfer_multipart(&row.id).await;
        }
        Ok(id)
    }

    /// Retry uses the bucket's current key: the persisted recipient may since be rotated or disabled.
    async fn refresh_encryption(&self, row: &Transfer, opts: &mut PutOptions) -> AppResult<()> {
        let cfg = self.db.get_encryption_config(&row.account_id, &row.bucket).await?;
        // Legacy rows queued pre-encrypted ciphertext: marker set, no spec.
        let legacy_ciphertext = opts.encrypt.is_none() && opts.user_metadata.contains_key("cosmog-encrypted");
        match (cfg, opts.encrypt.clone()) {
            (Some(cfg), Some(mut spec)) => {
                if spec.recipient != cfg.recipient {
                    spec.recipient = cfg.recipient;
                    super::encrypt::stamp(opts, &spec);
                }
                Ok(())
            }
            (None, None) => Ok(()),
            (Some(_), None) if legacy_ciphertext => Ok(()),
            _ => Err(AppError::InvalidInput(
                "bucket encryption changed since this upload was queued; upload the file again".into(),
            )),
        }
    }

    async fn enqueue(
        &self,
        store: Arc<dyn ObjectStore>,
        account_id: String,
        job: WorkerJob,
        external_sink: ProgressSink,
        resume: Option<ResumeState>,
        origin: TransferOrigin,
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
        let stage_cleanup_dir = match &job {
            WorkerJob::Upload { opts, .. } => opts.stage_cleanup_dir.clone(),
            WorkerJob::Download { .. } => None,
        };

        let account_id_for_cache = account_id.clone();
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
                origin,
            })
            .await?;
        // Throttled: bulk enqueues would otherwise run the sort-and-delete per file.
        if self.enqueued.fetch_add(1, Ordering::Relaxed) % PRUNE_EVERY == 0 {
            if let Err(e) = self.db.prune_finished_transfers(KEEP_FINISHED_ROWS).await {
                tracing::warn!("prune finished transfers failed: {e}");
            }
        }

        let cancel = CancellationToken::new();
        self.cancels.insert(id.clone(), cancel.clone());

        let (sink, resume_handle, journal) = composite_sink(self.db.clone(), id.clone(), external_sink.clone(), resume.clone());
        let settings = self.db.settings_load().await?;
        let mut ctx = TransferCtx {
            transfer_id: id.clone(),
            cancel: cancel.clone(),
            progress: sink,
            part_size: settings.part_size_bytes,
            parallelism: settings.multipart_parallelism as usize,
            multipart_threshold: settings.multipart_threshold_bytes,
            resume: None,
            source_stat: None,
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
        let external_for_task = external_sink;

        tokio::spawn(async move {
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let _ = db
                .update_transfer_status(&id_for_task, TransferStatus::Active, None)
                .await;

            // catch_unwind so abort, terminal event/status and cancels.remove still run once on panic.
            let result = {
                let store_job = store_for_task.clone();
                let db_job = db.clone();
                let ctx_job = ctx.clone();
                let resume_handle_job = resume_handle.clone();
                let account_id_job = account_id_for_cache.clone();
                let journal_job = journal.clone();
                std::panic::AssertUnwindSafe(async move {
                    match job {
                        WorkerJob::Upload { bucket, key, local_path, opts } => {
                            run_upload(
                                &store_job,
                                ctx_job,
                                &resume_handle_job,
                                &journal_job,
                                origin,
                                &bucket,
                                &key,
                                &local_path,
                                &opts,
                            )
                            .await
                        }
                        WorkerJob::Download { bucket, key, local_path, opts, resume_part } => {
                            run_download(
                                &store_job,
                                &ctx_job,
                                &db_job,
                                &account_id_job,
                                &bucket,
                                &key,
                                &local_path,
                                &opts,
                                resume_part,
                            )
                            .await
                            .map(|()| None)
                        }
                    }
                })
                .catch_unwind()
                .await
                .unwrap_or_else(|payload| {
                    let msg = payload
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string panic payload".into());
                    tracing::error!(transfer_id = %id_for_task, "transfer worker panicked: {msg}");
                    Err(AppError::Internal(format!(
                        "transfer worker panicked: {msg}"
                    )))
                })
            };

            // HEAD for authoritative metadata before the cache write-through.
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

            // Only Allowed/AccessDenied flip the put cap; network or cancel failures prove nothing.
            if matches!(direction, Direction::Upload) {
                use crate::db::capabilities::{CapState, WriteOp};
                let cap = match &result {
                    Ok(_) => Some(CapState::Allowed),
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

            let terminal = match &result {
                Ok(_) => TransferStatus::Done,
                Err(AppError::Canceled(_)) => TransferStatus::Canceled,
                Err(_) => TransferStatus::Failed,
            };
            if matches!(direction, Direction::Upload) {
                // Best-effort abort on give-up/cancel; the row drops the upload_id so a retry starts fresh.
                if !matches!(terminal, TransferStatus::Done) {
                    let upload_id = resume_handle.lock().unwrap().upload_id.clone();
                    if !upload_id.is_empty() {
                        let _ = store_for_task
                            .abort_multipart_upload(&bucket_for_cache, &key_for_cache, &upload_id)
                            .await;
                    }
                }
                journal.close_and_clear(&id_for_task).await;
            }
            // SAF staging dir piles up multi-GB if kept; failed uploads keep it for retry.
            if matches!(direction, Direction::Upload)
                && matches!(terminal, TransferStatus::Done | TransferStatus::Canceled)
            {
                if let Some(dir) = &stage_cleanup_dir {
                    let _ = tokio::fs::remove_dir_all(dir).await;
                }
            }
            let err_text = result.as_ref().err().map(|e| e.to_wire_string());
            let _ = db
                .update_transfer_status(&id_for_task, terminal, err_text.clone())
                .await;
            cancels.remove(&id_for_task);
            // The only terminal event: composite_sink filters store terminals, so this fires once.
            let event = match terminal {
                TransferStatus::Done => TransferEvent::Done {
                    transfer_id: id_for_task.clone(),
                    etag: result.ok().flatten(),
                },
                TransferStatus::Canceled => TransferEvent::Canceled {
                    transfer_id: id_for_task.clone(),
                },
                _ => TransferEvent::Failed {
                    transfer_id: id_for_task.clone(),
                    error: err_text.unwrap_or_else(|| "transfer failed".into()),
                },
            };
            external_for_task.emit(event);
        });

        Ok(id)
    }
}

const KEEP_FINISHED_ROWS: i64 = 1000;
const PRUNE_EVERY: u64 = 64;

/// Legacy rows (bare parts array, no fingerprint) come back stale so s3 aborts and starts fresh.
fn parse_resume(row_id: &str, upload_id: &str, parts_json: Option<&str>) -> ResumeState {
    let raw = parts_json.unwrap_or("");
    let mut state = serde_json::from_str::<ResumeState>(raw)
        .ok()
        .or_else(|| {
            serde_json::from_str::<Vec<CompletedPart>>(raw)
                .ok()
                .map(|parts| ResumeState { completed_parts: parts, ..Default::default() })
        })
        .unwrap_or_else(|| {
            tracing::warn!(transfer_id = %row_id, "unreadable parts_json, starting fresh");
            ResumeState::default()
        });
    state.upload_id = upload_id.to_string();
    state
}
