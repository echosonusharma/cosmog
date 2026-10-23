use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::db::transfers::TransferOrigin;
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::store::{GetOptions, ObjectStore, PutOptions};

use super::sink::PartsJournal;
use super::{ResumeState, SourceStat, TransferCtx};

const MAX_ATTEMPTS: u32 = 3;

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
pub(super) async fn run_upload(
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
    // Encrypt here: under the permit, and the row keeps the plaintext so a retry can re-encrypt.
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
                let again = e.is_retriable() && attempt + 1 < MAX_ATTEMPTS;
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

/// Writes to `<dest>.cosmog-part`, renamed only on success: resume only continues bytes Cosmog wrote and `dest` is never clobbered early.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_download(
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
                let again = e.is_retriable() && attempt + 1 < MAX_ATTEMPTS;
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
