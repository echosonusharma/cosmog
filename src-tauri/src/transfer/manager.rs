//! Persistent transfer queue + worker scheduler. Owns every upload/download
//! lifecycle; beyond the cancel map, the DB is the source of truth.

use std::path::PathBuf;
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

use super::{CompletedPart, ProgressSink, ResumeState, TransferCtx, TransferEvent};

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

    /// Re-enqueue a failed/canceled/paused transfer as a *new* row, carrying over
    /// upload_id + completed parts so multipart uploads resume where they left off.
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
                let parts: Vec<CompletedPart> = serde_json::from_str(parts_json)
                    .unwrap_or_else(|e| {
                        tracing::warn!(transfer_id = %row.id, "corrupt parts_json, starting fresh: {e}");
                        vec![]
                    });
                // Fingerprints unknown for rows predating them; enqueue() re-stats the
                // file so later attempts still get validated.
                Some(ResumeState {
                    upload_id: upload_id.clone(),
                    completed_parts: parts,
                    source_len: None,
                    source_mtime_secs: None,
                })
            }
            _ => None,
        };

        // Recover original PutOptions/GetOptions from the row so retries reuse the same
        // content-type/ACL/SSE/range; defaults if column missing or JSON bad.
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
                let mut opts = row
                    .options_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<GetOptions>(raw).ok())
                    .unwrap_or_default();
                // Re-validate the stored path on retry; defense-in-depth against
                // tampered DB rows between the original enqueue and this call.
                let local_path = crate::validate::validate_download_dest(&row.local_path).await
                    .map_err(|e| AppError::InvalidInput(format!("retry: invalid local_path: {e}")))?;
                // Encrypted buckets can't range-resume (age needs the full ciphertext
                // to authenticate); restart from byte 0 over any partial file.
                let bucket_encrypted = self
                    .db
                    .get_encryption_config(&row.account_id, &row.bucket)
                    .await
                    .ok()
                    .flatten()
                    .is_some();
                if bucket_encrypted {
                    let _ = tokio::fs::remove_file(&local_path).await;
                    opts.range_start = None;
                    opts.resume = false;
                } else if opts.range_start.is_none() && opts.range_end.is_none() {
                    // Auto-resume applies only to full-object downloads onto a genuinely
                    // partial file; explicit ranges pass through untouched.
                    if let Ok(meta) = tokio::fs::metadata(&local_path).await {
                        let existing = meta.len();
                        if existing > 0 {
                            opts.range_start = Some(existing);
                            // Signals s3 to APPEND instead of truncating.
                            // Never set for a dest that merely exists because
                            // an unrelated file was already there: without
                            // this gate every retry would append onto it.
                            opts.resume = true;
                        }
                    }
                }
                WorkerJob::Download {
                    bucket: row.bucket.clone(),
                    key: row.key.clone(),
                    local_path,
                    opts,
                }
            }
        };

        // Preserve the original origin so a retried night-watch upload stays silent.
        self.enqueue(store, row.account_id, job, external_sink, resume, row.origin)
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
        let path_for_cleanup = path_for_row.clone();
        // SAF staging dir reaped on terminal Done/Canceled; captured before the job is
        // consumed, independent of the encryption temp-file swap elsewhere.
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

        let cancel = CancellationToken::new();
        self.cancels.insert(id.clone(), cancel.clone());

        // Whether a terminal event reached the external sink via the store; the worker
        // emits a fallback terminal only if none did (exactly-once).
        let term_emitted = Arc::new(AtomicBool::new(false));
        // Enqueue-time source fingerprint so multipart resume state is discarded when the
        // file changed between attempts (saved parts are byte offsets into one version).
        let source_stat = match &job {
            WorkerJob::Upload { local_path, .. } => {
                tokio::fs::metadata(local_path).await.ok().map(|m| super::SourceStat {
                    len: m.len(),
                    mtime_secs: m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0),
                })
            }
            WorkerJob::Download { .. } => None,
        };
        let (sink, resume_handle, journal) =
            self.composite_sink(id.clone(), external_sink.clone(), term_emitted.clone(), source_stat);
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
            source_stat,
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
        let term_emitted_for_task = term_emitted;

        tokio::spawn(async move {
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let _ = db
                .update_transfer_status(&id_for_task, TransferStatus::Active, None)
                .await;

            const MAX_ATTEMPTS: u32 = 3;
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
                    // Shadow outer handles: this block consumes its own clones so terminal
                    // handling still works after a body panic.
                    let mut ctx = ctx_job;
                    let resume_handle = resume_handle_job;
                    let store_for_task = store_job;
                    let db = db_job;
                    let account_id_for_cache = account_id_job;
                    let journal = journal_job;
                    match job {
                WorkerJob::Upload {
                    bucket,
                    key,
                    local_path,
                    opts,
                } => {
                    let mut last_err: Option<AppError> = None;
                    let mut outcome: Option<()> = None;
                    for attempt in 0..MAX_ATTEMPTS {
                        if ctx.cancel.is_cancelled() {
                            last_err = Some(AppError::Canceled(format!("transfer {} canceled", ctx.transfer_id)));
                            break;
                        }
                        if attempt > 0 {
                            tokio::time::sleep(Duration::from_secs(1u64 << (attempt - 1))).await;
                            // Make sure every part completed on prior attempts is
                            // persisted before this attempt starts (it may fail
                            // or crash the process).
                            journal.flush(&ctx.transfer_id).await;
                            // Resume the same multipart upload: reuse upload_id + captured parts
                            // so already-uploaded parts are not re-sent.
                            let snapshot = resume_handle.lock().unwrap().clone();
                            if !snapshot.upload_id.is_empty() {
                                ctx.resume = Some(snapshot);
                            }
                        }
                        match store_for_task
                            .put_object(&bucket, &key, local_path.clone(), opts.clone(), ctx.clone())
                            .await
                        {
                            Ok(_) => { outcome = Some(()); break; }
                            Err(e) => {
                                if is_retriable(&e) && attempt + 1 < MAX_ATTEMPTS {
                                    last_err = Some(e);
                                } else {
                                    last_err = Some(e);
                                    break;
                                }
                            }
                        }
                    }
                    // Delete encrypted temp file regardless of outcome.
                    if let Some(p) = &opts.cleanup_path {
                        let _ = tokio::fs::remove_file(p).await;
                    }
                    match outcome {
                        Some(v) => Ok(v),
                        None => Err(last_err.expect("loop always sets last_err before None outcome")),
                    }
                }
                WorkerJob::Download {
                    bucket,
                    key,
                    local_path,
                    opts,
                } => {
                    let mut last_err: Option<AppError> = None;
                    let mut outcome: Option<()> = None;
                    let mut retry_opts = opts.clone();
                    // Encrypted buckets can't range-resume: GCM auth needs the full
                    // ciphertext, so suppress range-resume on retries here.
                    let bucket_encrypted = db
                        .get_encryption_config(&account_id_for_cache, &bucket)
                        .await
                        .ok()
                        .flatten()
                        .is_some();
                    // Auto-resume onto a partial file only for full-object downloads;
                    // explicit ranges retried verbatim (rewriting would clobber).
                    let auto_resumable =
                        !bucket_encrypted && opts.range_start.is_none() && opts.range_end.is_none();
                    for attempt in 0..MAX_ATTEMPTS {
                        if ctx.cancel.is_cancelled() {
                            last_err = Some(AppError::Canceled(format!("transfer {} canceled", ctx.transfer_id)));
                            break;
                        }
                        if attempt > 0 {
                            tokio::time::sleep(Duration::from_secs(1u64 << (attempt - 1))).await;
                            if auto_resumable {
                                // Re-derive each attempt: an earlier attempt may have set a
                                // stale offset and the file has likely grown since.
                                retry_opts.range_start = None;
                                retry_opts.resume = false;
                                if let Ok(meta) = tokio::fs::metadata(&local_path).await {
                                    let existing = meta.len();
                                    if existing > 0 {
                                        retry_opts.range_start = Some(existing);
                                        // s3 appends only under this flag, so unrelated
                                        // pre-existing files are never appended to.
                                        retry_opts.resume = true;
                                    }
                                }
                            }
                        }
                        match store_for_task
                            .get_object(&bucket, &key, local_path.clone(), retry_opts.clone(), ctx.clone())
                            .await
                        {
                            Ok(_) => {
                                // Post-download decryption for Cosmog-marked objects; age streams
                                // chunk-by-chunk to a sibling temp swapped in on success.
                                let dec_result: AppResult<()> = async {
                                    if db.get_encryption_config(&account_id_for_cache, &bucket).await?.is_none() {
                                        return Ok(());
                                    }
                                    // Trust file bytes over user_metadata: skip decrypt unless
                                    // the header magic marks an age payload.
                                    let magic_len = crate::crypto::AGE_MAGIC.len();
                                    let mut header = vec![0u8; magic_len];
                                    let is_age = match tokio::fs::File::open(&local_path).await {
                                        Ok(mut f) => {
                                            use tokio::io::AsyncReadExt;
                                            match f.read_exact(&mut header).await {
                                                Ok(_) => crate::crypto::is_age_ciphertext(&header),
                                                Err(_) => false,
                                            }
                                        }
                                        Err(_) => false,
                                    };
                                    if !is_age {
                                        return Ok(());
                                    }
                                    let aid = account_id_for_cache.clone();
                                    let bkt = bucket.clone();
                                    let secret = tokio::task::spawn_blocking(move || {
                                        crate::secrets::get_enc_identity(&aid, &bkt)
                                    })
                                    .await
                                    .map_err(|e| AppError::Internal(e.to_string()))??
                                    .ok_or_else(|| AppError::EncryptionIdentityMissing(format!(
                                        "identity for bucket '{bucket}' not present in the OS keychain. \
                                         Import a previously exported identity file to decrypt this object."
                                    )))?;
                                    let identity = crate::crypto::parse_identity(&secret)?;
                                    let mut plaintext_path = local_path.clone();
                                    let mut fname = plaintext_path.file_name().unwrap_or_default().to_os_string();
                                    fname.push(".dec");
                                    plaintext_path.set_file_name(&fname);
                                    crate::crypto::decrypt_file(&local_path, &plaintext_path, identity).await?;
                                    tokio::fs::rename(&plaintext_path, &local_path).await?;
                                    Ok(())
                                }
                                .await;
                                if let Err(e) = dec_result {
                                    // Failed decrypt leaves ciphertext under the plaintext filename;
                                    // delete it (plus any partial .dec) so shell handlers can't launch it.
                                    let _ = tokio::fs::remove_file(&local_path).await;
                                    let mut dec_tmp = local_path.clone();
                                    let mut fname = dec_tmp.file_name().unwrap_or_default().to_os_string();
                                    fname.push(".dec");
                                    dec_tmp.set_file_name(&fname);
                                    let _ = tokio::fs::remove_file(&dec_tmp).await;
                                    last_err = Some(e);
                                    break;
                                }
                                outcome = Some(());
                                break;
                            }
                            Err(e) => {
                                if is_retriable(&e) && attempt + 1 < MAX_ATTEMPTS {
                                    last_err = Some(e);
                                } else {
                                    last_err = Some(e);
                                    break;
                                }
                            }
                        }
                    }
                    match outcome {
                        Some(v) => Ok(v),
                        None => Err(last_err.expect("loop always sets last_err before None outcome")),
                    }
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
            // Multipart uploads stay alive across retries for resume; on final give-up/
            // cancel abort best-effort so incomplete-multipart storage isn't leaked.
            if matches!(direction, Direction::Upload)
                && !matches!(terminal, TransferStatus::Done)
            {
                let upload_id = resume_handle.lock().unwrap().upload_id.clone();
                if !upload_id.is_empty() {
                    let _ = store_for_task
                        .abort_multipart_upload(&bucket_for_cache, &key_for_cache, &upload_id)
                        .await;
                }
            }
            // Guarantee exactly-once terminal emission to the external sink.
            // The store emits a terminal only on success paths (put_single /
            // put_multipart Done); it no longer emits terminals on upload error
            // so retries can resume. If no terminal reached the external sink,
            // emit one here so downstream sinks (Night Watcher claim release +
            // stage cleanup) always fire.
            if !term_emitted_for_task.load(Ordering::SeqCst) {
                let event = match &terminal {
                    TransferStatus::Done => TransferEvent::Done {
                        transfer_id: id_for_task.clone(),
                        etag: None,
                    },
                    TransferStatus::Canceled => TransferEvent::Canceled {
                        transfer_id: id_for_task.clone(),
                    },
                    _ => TransferEvent::Failed {
                        transfer_id: id_for_task.clone(),
                        error: result
                            .as_ref()
                            .err()
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "upload failed".into()),
                    },
                };
                external_for_task.emit(event);
            }
            // Canceled downloads delete the partial file; failed ones keep it for
            // range-resume on retry.
            if matches!(terminal, TransferStatus::Canceled)
                && matches!(direction, Direction::Download)
            {
                let _ = tokio::fs::remove_file(&path_for_cleanup).await;
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
            // Flush batched part completions so parts_json matches the terminal row and
            // a crash-restart resume never re-uploads a finished part.
            if matches!(direction, Direction::Upload) {
                journal.flush(&id_for_task).await;
            }
            let err_text = result.err().map(|e| e.to_string());
            let _ = db
                .update_transfer_status(&id_for_task, terminal, err_text)
                .await;
            cancels.remove(&id_for_task);
        });

        Ok(id)
    }

    /// Fan-out sink (FE channel + DB milestone persistence, parts batched in memory).
    /// Returns the shared ResumeState for multipart resume plus the flush journal handle.
    fn composite_sink(
        &self,
        transfer_id: String,
        external: ProgressSink,
        term_emitted: Arc<AtomicBool>,
        source_stat: Option<super::SourceStat>,
    ) -> (
        ProgressSink,
        Arc<Mutex<ResumeState>>,
        Arc<PartsJournal>,
    ) {
        let db = self.db.clone();
        let resume: Arc<Mutex<ResumeState>> = Arc::new(Mutex::new(ResumeState::default()));
        // Seed the enqueue-time fingerprint so every snapshot carries it; s3 discards
        // resume state that doesn't match the file's current stat.
        if let Some(st) = source_stat {
            let mut guard = resume.lock().unwrap();
            guard.source_len = Some(st.len);
            guard.source_mtime_secs = Some(st.mtime_secs);
        }
        let resume_ret = resume.clone();
        // Serialize DB writes for PartCompleted snapshots: concurrent multipart workers
        // could otherwise write stale snapshots over newer ones.
        let parts_db_lock: Arc<AsyncMutex<()>> = Arc::new(AsyncMutex::new(()));
        let journal = Arc::new(PartsJournal {
            db: db.clone(),
            resume: resume.clone(),
            db_lock: parts_db_lock.clone(),
        });
        // Clone for the sink closure; the original goes back to the worker for flushes.
        let journal_for_sink = journal.clone();

        let sink = ProgressSink::from_fn(move |event: TransferEvent| {
            // Record terminal events so the worker knows the store already
            // emitted one and can skip its fallback emission.
            if matches!(
                event,
                TransferEvent::Done { .. }
                    | TransferEvent::Failed { .. }
                    | TransferEvent::Canceled { .. }
            ) {
                term_emitted.store(true, Ordering::SeqCst);
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
                TransferEvent::MultipartInitiated { upload_id, .. } => {
                    // Capture upload_id in memory even with zero completed parts; DB
                    // persistence waits for first PartCompleted (would clobber parts_json).
                    resume.lock().unwrap().upload_id = upload_id;
                }
                TransferEvent::PartCompleted {
                    upload_id, part_number, etag, ..
                } => {
                    // Record the completed part + upload_id so resume (in-worker
                    // retry or after a crash) never re-uploads finished parts.
                    let persist_now = {
                        let mut guard = resume.lock().unwrap();
                        if guard.upload_id.is_empty() {
                            guard.upload_id = upload_id;
                        }
                        guard.completed_parts.push(CompletedPart { part_number, etag });
                        // Batch DB writes: persist only when the part count is a
                        // power of two, so total writes grow O(log n) with
                        // upload size instead of O(n) per part. The tail is
                        // covered by the journal flushes the worker performs
                        // before each retry-attempt snapshot read and before
                        // the terminal status update — after those, at most
                        // log2(n) finished parts can be missing from the DB.
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

/// Shared multipart-progress journal: the in-memory resume snapshot plus the
/// lock serializing its persistence to `transfers.parts_json`.
struct PartsJournal {
    db: crate::db::Db,
    resume: Arc<Mutex<ResumeState>>,
    db_lock: Arc<AsyncMutex<()>>,
}

impl PartsJournal {
    /// Persist the current snapshot; writers re-snapshot under `db_lock` so the last
    /// write always wins and an older queued write can never clobber it.
    async fn flush(&self, transfer_id: &str) {
        let _guard = self.db_lock.lock().await;
        let snapshot = self.resume.lock().unwrap().clone();
        if snapshot.upload_id.is_empty() {
            return;
        }
        let uid = Some(snapshot.upload_id);
        let _ = self
            .db
            .update_transfer_multipart(transfer_id, uid, &snapshot.completed_parts)
            .await;
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
