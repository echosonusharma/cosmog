//! Shared encrypt-before-upload helper (interactive upload + Night Watcher).
//! The transfer worker deletes the temp ciphertext via `opts.cleanup_path` after settle.

use std::path::{Path, PathBuf};

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store::PutOptions;

/// If `bucket` has encryption enabled, stream-encrypt `src` to a temp `.age` file and
/// stamp `opts` (cleanup path + cosmog markers); returns the temp path. Else `(src, None)`.
pub async fn encrypt_for_bucket_if_needed(
    state: &AppState,
    account_id: &str,
    bucket: &str,
    src: &Path,
    opts: &mut PutOptions,
) -> AppResult<(PathBuf, Option<PathBuf>)> {
    encrypt_for_bucket_if_needed_with(&state.db, &state.db_path, account_id, bucket, src, opts).await
}

/// Test-friendly variant taking `db` + `db_path` directly so tests can inject a `Db`
/// without a full `AppState`. Behavior identical.
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

    // Constant-memory stream-encrypt with the bucket's age recipient (64 KiB chunks).
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
    // Mark client-encrypted + record payload format so future format changes stay unambiguous.
    opts.user_metadata.insert("cosmog-encrypted".into(), "1".into());
    opts.user_metadata
        .insert("cosmog-format".into(), crate::crypto::FORMAT_TAG.into());
    opts.user_metadata
        .insert("cosmog-recipient".into(), enc_cfg.recipient);

    Ok((tmp_path.clone(), Some(tmp_path)))
}
