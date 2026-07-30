//! Shared "encrypt-before-upload" helper.
//!
//! Both the interactive `enqueue_upload` command and the Night Watcher use
//! this so a synced file lands encrypted for an encrypted bucket in exactly
//! the same way. The transfer worker deletes the temp ciphertext via
//! `opts.cleanup_path` once the upload finishes (success or failure).

use std::path::{Path, PathBuf};

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store::PutOptions;

/// If `bucket` has encryption enabled, stream-encrypt `src` to a temp `.age`
/// file, stamp `opts` with the cleanup path and cosmog metadata markers, and
/// return `(upload_path, cleanup_on_err)` where `upload_path` is the temp file.
/// Otherwise returns `(src, None)` unchanged.
pub async fn encrypt_for_bucket_if_needed(
    state: &AppState,
    account_id: &str,
    bucket: &str,
    src: &Path,
    opts: &mut PutOptions,
) -> AppResult<(PathBuf, Option<PathBuf>)> {
    encrypt_for_bucket_if_needed_with(&state.db, &state.db_path, account_id, bucket, src, opts).await
}

/// Same as [`encrypt_for_bucket_if_needed`] but takes only the pieces it
/// actually uses (`db` + `db_path`), so the reconcile core can be driven with
/// an injected `Db` in tests without a full `AppState`. Behavior is identical.
pub async fn encrypt_for_bucket_if_needed_with(
    db: &Db,
    db_path: &Path,
    account_id: &str,
    bucket: &str,
    src: &Path,
    opts: &mut PutOptions,
) -> AppResult<(PathBuf, Option<PathBuf>)> {
    let Some(enc_cfg) = db.get_encryption_config(account_id, bucket).await? else {
        return Ok((src.to_path_buf(), None));
    };

    // Stream-encrypt to a temp path using the bucket's age recipient.
    // Constant-memory: age streams 64 KiB chunks with per-chunk nonces.
    let recipient = crate::crypto::parse_recipient(&enc_cfg.recipient)?;

    let tmp_dir = db_path
        .parent()
        .ok_or_else(|| AppError::Internal("db_path has no parent".into()))?
        .join("enc_tmp");
    tokio::fs::create_dir_all(&tmp_dir).await?;
    let tmp_path = tmp_dir.join(format!("{}.age", uuid::Uuid::new_v4()));

    // On a mid-stream encrypt failure, remove any partial ciphertext we wrote
    // so the enc_tmp scratch dir doesn't leak orphaned files.
    if let Err(e) = crate::crypto::encrypt_file(src, &tmp_path, recipient).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(e);
    }

    opts.cleanup_path = Some(tmp_path.clone());
    // Mark the object so download + UI know it's client-encrypted, and record
    // the payload format so future format changes stay unambiguous.
    opts.user_metadata.insert("cosmog-encrypted".into(), "1".into());
    opts.user_metadata
        .insert("cosmog-format".into(), crate::crypto::FORMAT_TAG.into());
    opts.user_metadata
        .insert("cosmog-recipient".into(), enc_cfg.recipient);

    Ok((tmp_path.clone(), Some(tmp_path)))
}
