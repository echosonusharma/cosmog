//! Shared encrypt-before-upload helpers (interactive upload, bulk, Night Watcher).
//! Enqueue only stamps `PutOptions`; the transfer worker encrypts to enc_tmp and cleans up.

use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use crate::db::transfers::TransferOrigin;
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store::PutOptions;

use super::EncryptSpec;

/// If `bucket` has encryption enabled, stamp `opts` so the worker encrypts `src`.
/// Returns `(src, None)`; the tuple shape is kept for existing callers.
pub async fn encrypt_for_bucket_if_needed(
    state: &AppState,
    account_id: &str,
    bucket: &str,
    src: &Path,
    opts: &mut PutOptions,
) -> AppResult<(PathBuf, Option<PathBuf>)> {
    encrypt_for_bucket_if_needed_with(&state.db, &state.db_path, account_id, bucket, src, opts).await
}

pub async fn encrypt_for_bucket_if_needed_with(
    db: &Db,
    db_path: &Path,
    account_id: &str,
    bucket: &str,
    src: &Path,
    opts: &mut PutOptions,
) -> AppResult<(PathBuf, Option<PathBuf>)> {
    if let Some(spec) = encrypt_spec_for_bucket(db, db_path, account_id, bucket).await? {
        stamp(opts, &spec);
    } else {
        opts.encrypt = None;
    }
    Ok((src.to_path_buf(), None))
}

/// Resolve the bucket's encryption spec once; `None` = plaintext bucket.
pub async fn encrypt_spec_for_bucket(
    db: &Db,
    db_path: &Path,
    account_id: &str,
    bucket: &str,
) -> AppResult<Option<EncryptSpec>> {
    let Some(enc_cfg) = db.get_encryption_config(account_id, bucket).await? else {
        return Ok(None);
    };
    // Fail at enqueue on a bad recipient rather than inside the worker.
    crate::crypto::parse_recipient(&enc_cfg.recipient)?;
    let tmp_dir = db_path
        .parent()
        .ok_or_else(|| AppError::Internal("db_path has no parent".into()))?
        .join("enc_tmp");
    Ok(Some(EncryptSpec {
        recipient: enc_cfg.recipient,
        tmp_dir,
    }))
}

pub fn stamp(opts: &mut PutOptions, spec: &EncryptSpec) {
    opts.encrypt = Some(spec.clone());
    // Format marker keeps future payload changes unambiguous.
    opts.user_metadata.insert("cosmog-encrypted".into(), "1".into());
    opts.user_metadata
        .insert("cosmog-format".into(), crate::crypto::FORMAT_TAG.into());
    opts.user_metadata
        .insert("cosmog-recipient".into(), spec.recipient.clone());
}

/// Per-origin subdir: on Android the main process sweeps only `user/` while the
/// :nightwatch process may still be encrypting into `nightwatch/`.
pub fn tmp_dir_for(root: &Path, origin: TransferOrigin) -> PathBuf {
    root.join(origin.as_str())
}

/// Stream-encrypt `src` into a fresh temp file; removes the partial on failure.
pub async fn encrypt_to_temp(
    spec: &EncryptSpec,
    origin: TransferOrigin,
    src: &Path,
    cancel: &CancellationToken,
) -> AppResult<PathBuf> {
    let recipient = crate::crypto::parse_recipient(&spec.recipient)?;
    let dir = tmp_dir_for(&spec.tmp_dir, origin);
    tokio::fs::create_dir_all(&dir).await?;
    let tmp_path = dir.join(format!("{}.age", uuid::Uuid::new_v4()));
    if let Err(e) =
        crate::crypto::encrypt_file_cancellable(src, &tmp_path, recipient, cancel.clone()).await
    {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(e);
    }
    Ok(tmp_path)
}

/// Startup sweep of crash-leftover ciphertext. On Android `nightwatch/` belongs to the
/// :nightwatch process; fresh files are skipped so a just-started upload survives.
pub fn sweep_enc_tmp(root: &Path) {
    let owned: &[TransferOrigin] = if cfg!(target_os = "android") {
        &[TransferOrigin::User]
    } else {
        &[TransferOrigin::User, TransferOrigin::NightWatch]
    };
    // Root-level files predate per-origin dirs.
    let mut dirs = vec![root.to_path_buf()];
    dirs.extend(owned.iter().map(|o| tmp_dir_for(root, *o)));
    sweep_dirs(root, dirs);
}

/// Android `:nightwatch` process sweeps only its own origin dir.
pub fn sweep_enc_tmp_origin(root: &Path, origin: TransferOrigin) {
    sweep_dirs(root, vec![tmp_dir_for(root, origin)]);
}

fn sweep_dirs(root: &Path, dirs: Vec<PathBuf>) {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    let mut removed = 0usize;
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            let stale = meta.modified().map(|m| m < cutoff).unwrap_or(true);
            if meta.is_file() && stale && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    if removed > 0 {
        tracing::info!("swept {removed} stale file(s) from {}", root.display());
    }
}
