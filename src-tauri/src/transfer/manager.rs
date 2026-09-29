//! Persistent transfer queue + worker scheduler. Owns every upload/download
//! lifecycle; beyond the cancel map, the DB is the source of truth.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use futures::FutureExt;
use tokio::sync::{Semaphore, Mutex as AsyncMutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use std::time::Duration;

use crate::db::transfers::{Direction, NewTransfer, Transfer, TransferOrigin, TransferStatus};
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::store::{GetOptions, ObjectStore, PutOptions};

use super::{CompletedPart, ProgressSink, ResumeState, SourceStat, TransferCtx, TransferEvent};

/// Returns `true` for transient S3 errors that are safe to retry.
fn is_retriable(err: &AppError) -> bool {
    matches!(err, AppError::RateLimited(_) | AppError::S3(_) | AppError::NetworkUnreachable(_))
}

/// Semaphore with runtime-adjustable limit; `current` tracks true capacity once in-flight permits release.
/// Resize-down records only what it reclaimed, otherwise a later resize-up grows capacity past the limit.
struct ResizableSemaphore {
    sem: Arc<Semaphore>,
    current: Arc<Mutex<usize>>,
}

impl ResizableSemaphore {
    fn new(n: usize) -> Self {
        let n = n.max(1);
        Self {
            sem: Arc::new(Semaphore::new(n)),
            current: Arc::new(Mutex::new(n)),
        }
    }

    async fn acquire(&self) -> Result<tokio::sync::SemaphorePermit<'_>, tokio::sync::AcquireError> {
        self.sem.acquire().await
    }

    #[cfg(test)]
    fn available(&self) -> usize {
        self.sem.available_permits()
    }

    fn resize(&self, new_size: usize) {
        let new_size = new_size.max(1);
        let mut current = self.current.lock().unwrap();
        let old = *current;
        if new_size > old {
            self.sem.add_permits(new_size - old);
            *current = new_size;
        } else if new_size < old {
            let to_remove = old - new_size;
            let mut removed = 0;
            while removed < to_remove {
                match self.sem.try_acquire() {
                    Ok(permit) => {
                        permit.forget();
                        removed += 1;
                    }
                    Err(_) => break,
                }
            }
            *current = old - removed;
        }
    }
}

/// Persistent transfer queue + worker scheduler. Cheap to clone (all interior
/// state is `Arc`-shared).
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
        /// Retry of an earlier row: its `.cosmog-part` may be resumed on the first attempt.
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

    /// Adjust the maximum number of concurrent transfers. Takes effect for the
    /// next acquisition; in-flight transfers are not interrupted.
    pub fn set_concurrency(&self, n: usize) {
        self.sem.resize(n);
    }

    /// Enqueue an upload: inserts the transfer row, spawns a concurrency-limited worker.
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
                resume_part: false,
            },
            external_sink,
            None,
            TransferOrigin::User,
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

    /// True while the transfer is queued or running in this process.
    pub fn is_live(&self, transfer_id: &str) -> bool {
        self.cancels.contains_key(transfer_id)
    }

    /// Cancel a transfer; if its process died leaving a ghost active/pending row,
    /// flip the DB row to `canceled`. Live tokens let the worker emit the terminal itself.
    pub async fn cancel_or_reap(&self, transfer_id: &str) -> AppResult<()> {
        if let Some(token) = self.cancels.get(transfer_id) {
            token.cancel();
            return Ok(());
        }
        self.db.mark_canceled_if_active(transfer_id).await?;
        Ok(())
    }

    /// Cancel every active transfer for a deleted account so dangling workers stop
    /// writing to soon-cascade-deleted rows. Returns the count signalled.
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

    /// Cancel every active transfer for a bucket being deleted, stopping workers
    /// before its cache rows are purged and S3 starts returning 404.
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

    /// Re-enqueue a failed/canceled/paused transfer as a *new* row. Uploads carry over a
    /// crash-surviving multipart upload; downloads resume from their own `.cosmog-part`.
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

        // Recover original PutOptions/GetOptions from the row so retries reuse the same
        // content-type/ACL/SSE/range; defaults if column missing or JSON bad.
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
                // Re-validate the stored path on retry; defense-in-depth against
                // tampered DB rows between the original enqueue and this call.
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

    /// A retry follows the bucket's current key: the persisted recipient may have been
    /// rotated away or disabled (its identity destroyed) since the row was queued.
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

    /// Unified worker spawn used by upload, download, and retry paths.
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
        // SAF staging dir reaped on terminal Done/Canceled; captured before the job is consumed.
        let stage_cleanup_dir = match &job {
            WorkerJob::Upload { opts, .. } => opts.stage_cleanup_dir.clone(),
            WorkerJob::Download { .. } => None,
        };

        let account_id_for_cache = account_id.clone();
        // Persist options so a future retry reapplies the same headers/ACL/SSE/range.
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

        let (sink, resume_handle, journal) = self.composite_sink(id.clone(), external_sink.clone(), resume.clone());
        // Per-transfer tunables come from user settings (FE-configurable).
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

            // Panic guard: job runs under catch_unwind and panics map to Internal, so
            // multipart abort, terminal event/status, and cancels.remove still run once.
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
            // and we only flip the cap on Allowed / AccessDenied; other
            // failure classes (network, cancel) don't prove anything.
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
                // Multipart uploads stay alive across in-worker retries; on give-up/cancel abort
                // best-effort. Either way the row stops pointing at it, so a retry starts fresh.
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
            // SAF staging dir is dead weight after Done/Canceled (multi-GB pileup);
            // failed uploads keep it for retry. Desktop uploads never set it.
            if matches!(direction, Direction::Upload)
                && matches!(terminal, TransferStatus::Done | TransferStatus::Canceled)
            {
                if let Some(dir) = &stage_cleanup_dir {
                    let _ = tokio::fs::remove_dir_all(dir).await;
                }
            }
            let err_text = result.as_ref().err().map(|e| e.to_string());
            let _ = db
                .update_transfer_status(&id_for_task, terminal, err_text.clone())
                .await;
            cancels.remove(&id_for_task);
            // The only terminal event for this transfer: store terminals are filtered in
            // composite_sink, so this fires once, after decrypt/rename.
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

    /// Fan-out sink (FE channel + DB milestone persistence, parts batched in memory).
    /// Returns the shared ResumeState for multipart resume plus the flush journal handle.
    fn composite_sink(
        &self,
        transfer_id: String,
        external: ProgressSink,
        seed: Option<ResumeState>,
    ) -> (
        ProgressSink,
        Arc<Mutex<ResumeState>>,
        Arc<PartsJournal>,
    ) {
        let db = self.db.clone();
        // Seeded with carried-over parts so journal flushes never drop them.
        let resume: Arc<Mutex<ResumeState>> = Arc::new(Mutex::new(seed.unwrap_or_default()));
        let resume_ret = resume.clone();
        let journal = Arc::new(PartsJournal {
            db: db.clone(),
            resume: resume.clone(),
            db_lock: Arc::new(AsyncMutex::new(())),
            closed: AtomicBool::new(false),
        });
        let journal_for_sink = journal.clone();

        let sink = ProgressSink::from_fn(move |event: TransferEvent| {
            // Store terminals are premature (retry pending, decrypt not done); the worker
            // emits the one real terminal.
            if matches!(
                event,
                TransferEvent::Done { .. }
                    | TransferEvent::Failed { .. }
                    | TransferEvent::Canceled { .. }
            ) {
                return;
            }
            external.emit(event.clone());

            let db = db.clone();
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
                TransferEvent::MultipartInitiated { upload_id, part_size, .. } => {
                    {
                        let mut guard = resume.lock().unwrap();
                        // A different id means the saved state was discarded; its parts are void.
                        if guard.upload_id != upload_id {
                            guard.completed_parts.clear();
                        }
                        guard.upload_id = upload_id;
                        guard.part_size = Some(part_size);
                    }
                    // Persist right away so a crash before the first part still leaves an
                    // upload_id to abort on clear.
                    let journal = journal_for_sink.clone();
                    tokio::spawn(async move {
                        journal.flush(&tid).await;
                    });
                }
                TransferEvent::PartCompleted {
                    upload_id, part_number, etag, ..
                } => {
                    let persist_now = {
                        let mut guard = resume.lock().unwrap();
                        if guard.upload_id.is_empty() {
                            guard.upload_id = upload_id;
                        }
                        guard.completed_parts.push(CompletedPart { part_number, etag });
                        // Persist at power-of-two part counts (O(log n) writes); the worker
                        // flushes the tail before each retry attempt.
                        guard.completed_parts.len().is_power_of_two()
                    };
                    if persist_now {
                        let journal = journal_for_sink.clone();
                        tokio::spawn(async move {
                            journal.flush(&tid).await;
                        });
                    }
                }
                _ => {}
            }
        });
        (sink, resume_ret, journal)
    }
}

/// Finished rows kept for history; older ones are pruned on each enqueue.
const KEEP_FINISHED_ROWS: i64 = 1000;
const PRUNE_EVERY: u64 = 64;
const MAX_ATTEMPTS: u32 = 3;

/// Rebuild multipart resume state from a row. Legacy rows stored a bare parts array with
/// no fingerprint; those come back stale-by-design so s3 aborts and starts fresh.
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

async fn stat_source(path: &Path) -> Option<SourceStat> {
    let m = tokio::fs::metadata(path).await.ok()?;
    Some(SourceStat {
        len: m.len(),
        mtime_secs: m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_upload(
    store: &Arc<dyn ObjectStore>,
    mut ctx: TransferCtx,
    resume_handle: &Arc<Mutex<ResumeState>>,
    journal: &PartsJournal,
    origin: TransferOrigin,
    bucket: &str,
    key: &str,
    local_path: &Path,
    opts: &PutOptions,
) -> AppResult<Option<String>> {
    // Encrypt inside the worker: runs under the concurrency permit, and the row keeps the
    // plaintext source so a retry can re-encrypt.
    let enc_tmp = match &opts.encrypt {
        Some(spec) => Some(super::encrypt::encrypt_to_temp(spec, origin, local_path, &ctx.cancel).await?),
        None => None,
    };
    let upload_path = enc_tmp.clone().unwrap_or_else(|| local_path.to_path_buf());
    let result = upload_attempts(store, &mut ctx, resume_handle, journal, bucket, key, &upload_path, opts).await;
    if let Some(p) = &enc_tmp {
        let _ = tokio::fs::remove_file(p).await;
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn upload_attempts(
    store: &Arc<dyn ObjectStore>,
    ctx: &mut TransferCtx,
    resume_handle: &Arc<Mutex<ResumeState>>,
    journal: &PartsJournal,
    bucket: &str,
    key: &str,
    upload_path: &Path,
    opts: &PutOptions,
) -> AppResult<Option<String>> {
    // Fingerprint of the bytes actually uploaded; saved parts only resume against it.
    ctx.source_stat = stat_source(upload_path).await;
    {
        let mut guard = resume_handle.lock().unwrap();
        guard.source_len = ctx.source_stat.map(|s| s.len);
        guard.source_mtime_secs = ctx.source_stat.map(|s| s.mtime_secs);
    }
    let mut last_err: Option<AppError> = None;
    for attempt in 0..MAX_ATTEMPTS {
        if ctx.cancel.is_cancelled() {
            return Err(AppError::Canceled(format!("transfer {} canceled", ctx.transfer_id)));
        }
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1u64 << (attempt - 1))).await;
            // Persist every part from prior attempts before this one (it may crash).
            journal.flush(&ctx.transfer_id).await;
            let snapshot = resume_handle.lock().unwrap().clone();
            if !snapshot.upload_id.is_empty() {
                ctx.resume = Some(snapshot);
            }
        }
        match store
            .put_object(bucket, key, upload_path.to_path_buf(), opts.clone(), ctx.clone())
            .await
        {
            Ok(r) => return Ok(r.etag),
            Err(e) => {
                let again = is_retriable(&e) && attempt + 1 < MAX_ATTEMPTS;
                last_err = Some(e);
                if !again {
                    break;
                }
            }
        }
    }
    Err(last_err.expect("loop always sets last_err before giving up"))
}

fn sibling(dest: &Path, suffix: &str) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    dest.with_file_name(name)
}

/// Downloads land in `<dest>.cosmog-part` and are renamed onto `dest` only on success, so
/// resume only ever continues bytes a Cosmog transfer wrote and `dest` is never clobbered early.
#[allow(clippy::too_many_arguments)]
async fn run_download(
    store: &Arc<dyn ObjectStore>,
    ctx: &TransferCtx,
    db: &Db,
    account_id: &str,
    bucket: &str,
    key: &str,
    dest: &Path,
    opts: &GetOptions,
    resume_part: bool,
) -> AppResult<()> {
    let part = sibling(dest, PART_SUFFIX);
    let explicit_range = opts.range_start.is_some() || opts.range_end.is_some();
    let bucket_encrypted = db
        .get_encryption_config(account_id, bucket)
        .await
        .ok()
        .flatten()
        .is_some();
    // Encrypted buckets restart from zero; explicit ranges are retried verbatim.
    let resumable = !bucket_encrypted && !explicit_range;

    if bucket_encrypted && !explicit_range && opts.version_id.is_none() {
        let has_key = load_identity(account_id, bucket).await.ok().flatten().is_some();
        if !has_key {
            if let Ok(meta) = store.head_object(bucket, key).await {
                if is_marked_encrypted(&meta.user_metadata) {
                    return Err(missing_identity(bucket));
                }
            }
        }
    }

    let mut last_err: Option<AppError> = None;
    let mut got = None;
    for attempt in 0..MAX_ATTEMPTS {
        if ctx.cancel.is_cancelled() {
            last_err = Some(AppError::Canceled(format!("transfer {} canceled", ctx.transfer_id)));
            break;
        }
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1u64 << (attempt - 1))).await;
        }
        let mut o = opts.clone();
        o.resume = false;
        // Attempt 0 of a fresh enqueue must not trust a part file some other transfer left.
        if resumable && (attempt > 0 || resume_part) {
            if let Ok(meta) = tokio::fs::metadata(&part).await {
                if meta.len() > 0 {
                    o.range_start = Some(meta.len());
                    o.resume = true;
                    o.resume_unmodified_since = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64);
                }
            }
        }
        match store
            .get_object(bucket, key, part.clone(), o, ctx.clone())
            .await
        {
            Ok(r) => {
                got = Some(r);
                break;
            }
            Err(e) => {
                let again = is_retriable(&e) && attempt + 1 < MAX_ATTEMPTS;
                last_err = Some(e);
                if !again {
                    break;
                }
            }
        }
    }

    let Some(res) = got else {
        let err = last_err.expect("loop always sets last_err before giving up");
        // Failed resumable downloads keep the part for retry; nothing else is worth keeping.
        if matches!(err, AppError::Canceled(_)) || !resumable {
            remove_download_temps(&part).await;
        }
        return Err(err);
    };

    let finish: AppResult<()> = async {
        let marked = is_marked_encrypted(&res.user_metadata);
        if !explicit_range && marked && has_age_magic(&part).await {
            let secret = load_identity(account_id, bucket)
                .await?
                .ok_or_else(|| missing_identity(bucket))?;
            let identity = crate::crypto::parse_identity(&secret)?;
            let dec = sibling(dest, DEC_SUFFIX);
            crate::crypto::decrypt_file(&part, &dec, identity).await?;
            tokio::fs::rename(&dec, dest).await?;
            let _ = tokio::fs::remove_file(&part).await;
        } else {
            if marked && !explicit_range {
                tracing::warn!(key, "object marked encrypted but not age ciphertext; saved raw");
            }
            tokio::fs::rename(&part, dest).await?;
        }
        Ok(())
    }
    .await;
    if finish.is_err() {
        // dest was never touched; drop the ciphertext part and any partial plaintext.
        remove_download_temps(&part).await;
        let _ = tokio::fs::remove_file(sibling(dest, DEC_SUFFIX)).await;
    }
    finish
}

const PART_SUFFIX: &str = ".cosmog-part";
const DEC_SUFFIX: &str = ".cosmog-dec";

async fn remove_download_temps(part: &Path) {
    let _ = tokio::fs::remove_file(part).await;
    // Parallel GETs preallocate under this sibling name (see s3 get_object_parallel).
    let _ = tokio::fs::remove_file(sibling(part, ".sparse")).await;
}

fn is_marked_encrypted(meta: &std::collections::HashMap<String, String>) -> bool {
    meta.get("cosmog-encrypted").is_some_and(|v| v == "1")
}

async fn has_age_magic(path: &Path) -> bool {
    use tokio::io::AsyncReadExt;
    let mut header = vec![0u8; crate::crypto::AGE_MAGIC.len()];
    match tokio::fs::File::open(path).await {
        Ok(mut f) => f.read_exact(&mut header).await.is_ok() && crate::crypto::is_age_ciphertext(&header),
        Err(_) => false,
    }
}

async fn load_identity(account_id: &str, bucket: &str) -> AppResult<Option<String>> {
    let aid = account_id.to_string();
    let bkt = bucket.to_string();
    tokio::task::spawn_blocking(move || crate::secrets::get_enc_identity(&aid, &bkt))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
}

fn missing_identity(bucket: &str) -> AppError {
    AppError::EncryptionIdentityMissing(format!(
        "identity for bucket '{bucket}' not present in the OS keychain. \
         Import a previously exported identity file to decrypt this object."
    ))
}

/// Shared multipart-progress journal: the in-memory resume snapshot plus the
/// lock serializing its persistence to `transfers.parts_json`.
struct PartsJournal {
    db: crate::db::Db,
    resume: Arc<Mutex<ResumeState>>,
    db_lock: Arc<AsyncMutex<()>>,
    /// Set at terminal; late spawned flushes must not resurrect a cleared upload_id.
    closed: AtomicBool,
}

impl PartsJournal {
    /// Persist the current snapshot; writers re-snapshot under `db_lock` so the last
    /// write always wins and an older queued write can never clobber it.
    async fn flush(&self, transfer_id: &str) {
        let _guard = self.db_lock.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let snapshot = self.resume.lock().unwrap().clone();
        if snapshot.upload_id.is_empty() {
            return;
        }
        let uid = Some(snapshot.upload_id.clone());
        let _ = self
            .db
            .update_transfer_multipart(transfer_id, uid, &snapshot)
            .await;
    }

    async fn close_and_clear(&self, transfer_id: &str) {
        let _guard = self.db_lock.lock().await;
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.db.clear_transfer_multipart(transfer_id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: resize-down recording the *target* while in-flight permits were
    /// unreclaimable let later resize-ups exceed the configured limit.
    #[tokio::test]
    async fn resize_down_then_up_never_exceeds_limit() {
        let sem = ResizableSemaphore::new(4);
        let p1 = sem.acquire().await.unwrap();
        let p2 = sem.acquire().await.unwrap();
        let p3 = sem.acquire().await.unwrap();
        let p4 = sem.acquire().await.unwrap();
        assert_eq!(sem.available(), 0);

        // All permits in flight: nothing reclaimable, but capacity must not shrink.
        sem.resize(2);
        assert_eq!(sem.current.lock().unwrap().clone(), 4);

        drop((p1, p2, p3, p4));
        // Permits are back; resize-up must not add any on top of them.
        sem.resize(4);
        assert_eq!(sem.available(), 4);

        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(sem.acquire().await.unwrap());
        }
        assert!(
            sem.sem.try_acquire().is_err(),
            "capacity exceeded the limit after resize down+up"
        );

        drop(held);
        sem.resize(2);
        assert_eq!(sem.available(), 2);
    }
}
