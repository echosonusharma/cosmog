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

use std::time::Duration;

use crate::db::transfers::{Direction, NewTransfer, Transfer, TransferStatus};
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::store::{GetOptions, ObjectStore, PutOptions};

use super::{CompletedPart, ProgressSink, ResumeState, TransferCtx, TransferEvent};

/// Returns `true` for transient S3 errors that are safe to retry.
fn is_retriable(err: &AppError) -> bool {
    matches!(err, AppError::RateLimited(_) | AppError::S3(_) | AppError::NetworkUnreachable(_))
}

/// A semaphore whose permit count can be adjusted at runtime.
///
/// Increasing the limit is immediate (new permits are added). Decreasing
/// reclaims immediately-available permits via `try_acquire`+`forget`; any
/// permits currently held by in-flight transfers are returned to a now-
/// reduced pool (they converge to the new limit over time as transfers
/// complete).
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

    fn resize(&self, new_size: usize) {
        let new_size = new_size.max(1);
        let mut current = self.current.lock().unwrap();
        let old = *current;
        if new_size > old {
            self.sem.add_permits(new_size - old);
            *current = new_size;
        } else if new_size < old {
            // Attempt to reclaim immediately-available permits.
            let to_remove = old - new_size;
            let mut removed = 0;
            while removed < to_remove {
                match self.sem.try_acquire() {
                    Ok(permit) => {
                        permit.forget();
                        removed += 1;
                    }
                    // Remaining permits are in-flight; they will complete at the
                    // old limit and the semaphore will hold at old - removed.
                    Err(_) => break,
                }
            }
            // Record the actual effective capacity, not the desired one — in-flight
            // permits that couldn't be reclaimed will return and restore the semaphore
            // to old - removed, not to new_size.
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
            sem: Arc::new(ResizableSemaphore::new(concurrency)),
        }
    }

    /// Adjust the maximum number of concurrent transfers. Takes effect for the
    /// next acquisition; in-flight transfers are not interrupted.
    pub fn set_concurrency(&self, n: usize) {
        self.sem.resize(n);
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

    /// Cancel every active transfer for a specific bucket. Used when the
    /// bucket is being deleted so workers stop before its cache rows are
    /// purged and S3 starts returning 404.
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
                let parts: Vec<CompletedPart> = serde_json::from_str(parts_json)
                    .unwrap_or_else(|e| {
                        tracing::warn!(transfer_id = %row.id, "corrupt parts_json, starting fresh: {e}");
                        vec![]
                    });
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
                let mut opts = row
                    .options_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<GetOptions>(raw).ok())
                    .unwrap_or_default();
                // Re-validate the stored path on retry; defense-in-depth against
                // tampered DB rows between the original enqueue and this call.
                let local_path = crate::validate::validate_download_dest(&row.local_path)
                    .map_err(|e| AppError::InvalidInput(format!("retry: invalid local_path: {e}")))?;
                // Encrypted buckets cannot be range-resumed: age needs the full
                // ciphertext to authenticate the stream. Overwrite any partial
                // file from a previous attempt and always restart from byte 0.
                let bucket_encrypted = self
                    .db
                    .get_encryption_config(&row.account_id, &row.bucket)
                    .await
                    .ok()
                    .flatten()
                    .is_some();
                if bucket_encrypted {
                    let _ = std::fs::remove_file(&local_path);
                    opts.range_start = None;
                } else if let Ok(meta) = std::fs::metadata(&local_path) {
                    // Resume from where the partial file left off.
                    let existing = meta.len();
                    if existing > 0 {
                        opts.range_start = Some(existing);
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
        let path_for_cleanup = path_for_row.clone();

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

            const MAX_ATTEMPTS: u32 = 3;
            let result = match job {
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
                        let _ = std::fs::remove_file(p);
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
                    // Encrypted buckets cannot be range-resumed: whole-object GCM
                    // authentication requires the full ciphertext to be present.
                    // Suppress range-resume on retries for any download from a
                    // bucket with encryption configured.
                    let bucket_encrypted = db
                        .get_encryption_config(&account_id_for_cache, &bucket)
                        .await
                        .ok()
                        .flatten()
                        .is_some();
                    for attempt in 0..MAX_ATTEMPTS {
                        if ctx.cancel.is_cancelled() {
                            last_err = Some(AppError::Canceled(format!("transfer {} canceled", ctx.transfer_id)));
                            break;
                        }
                        if attempt > 0 {
                            tokio::time::sleep(Duration::from_secs(1u64 << (attempt - 1))).await;
                            if !bucket_encrypted {
                                if let Ok(meta) = std::fs::metadata(&local_path) {
                                    let existing = meta.len();
                                    if existing > 0 {
                                        retry_opts.range_start = Some(existing);
                                    }
                                }
                            }
                        }
                        match store_for_task
                            .get_object(&bucket, &key, local_path.clone(), retry_opts.clone(), ctx.clone())
                            .await
                        {
                            Ok(_) => {
                                // Post-download decryption: only decrypt when the server
                                // metadata explicitly marks the object as Cosmog-encrypted.
                                // Streaming path: age reads ciphertext chunk-by-chunk and
                                // writes plaintext to a sibling temp file, then swaps it
                                // into `local_path` on success. Constant RAM.
                                let dec_result: AppResult<()> = async {
                                    if db.get_encryption_config(&account_id_for_cache, &bucket).await?.is_none() {
                                        return Ok(());
                                    }
                                    // Trust the file bytes, not S3 metadata.
                                    // Read the header magic; if the downloaded
                                    // file is not an age payload, skip decrypt
                                    // regardless of what user_metadata claims.
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
                                    // Atomic swap: rename decrypted temp over the ciphertext file.
                                    tokio::fs::rename(&plaintext_path, &local_path).await?;
                                    Ok(())
                                }
                                .await;
                                if let Err(e) = dec_result {
                                    // Decrypt failed: the file at local_path
                                    // still holds raw ciphertext under the
                                    // user-facing plaintext filename. Delete
                                    // it so shell handlers (auto-open,
                                    // thumbnailer, Spotlight) don't index or
                                    // launch a ciphertext blob as if it were
                                    // the real file. Also nuke the .dec temp
                                    // if a partial write happened.
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
            // Canceled downloads leave a partial (often 0-byte) file at the
            // destination; the user explicitly aborted, so remove it. Failed
            // downloads keep the partial so a retry can range-resume.
            if matches!(terminal, TransferStatus::Canceled)
                && matches!(direction, Direction::Download)
            {
                let _ = tokio::fs::remove_file(&path_for_cleanup).await;
            }
            // Android SAF uploads are staged into $APPCACHE/uploads/<uuid>/;
            // once the upload is done or canceled the staged copy is dead
            // weight (multi-GB files pile up fast). Failed uploads keep it so
            // a retry does not need re-staging. The path test keeps this away
            // from real user files: desktop uploads reference the original
            // source path, never a cache/uploads staging dir.
            if matches!(direction, Direction::Upload)
                && matches!(terminal, TransferStatus::Done | TransferStatus::Canceled)
                && path_for_cleanup.replace('\\', "/").contains("/cache/uploads/")
            {
                let p = std::path::Path::new(&path_for_cleanup);
                let _ = tokio::fs::remove_file(p).await;
                if let Some(dir) = p.parent() {
                    // Only succeeds when empty, which is exactly what we want.
                    let _ = tokio::fs::remove_dir(dir).await;
                }
            }
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
                    // Persist every completed part so resume after a crash never
                    // re-uploads finished parts. parts_db_lock serializes writes.
                    {
                        let mut guard = parts.lock().unwrap();
                        guard.push(CompletedPart { part_number, etag });
                    }
                    let parts_ref = parts.clone();
                    let lock = parts_db_lock.clone();
                    tokio::spawn(async move {
                        let _guard = lock.lock().await;
                        let snapshot = parts_ref.lock().unwrap().clone();
                        let _ = db.update_transfer_multipart(&tid, None, &snapshot).await;
                    });
                }
                _ => {}
            }
        })
    }
}
