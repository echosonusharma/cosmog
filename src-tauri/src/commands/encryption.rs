//! Per-bucket client-side encryption (age): X25519 identity in the OS keychain.
//! Export the secret before disabling, or all uploaded objects become undecryptable.

use serde::Serialize;
use tauri::State;
use zeroize::Zeroizing;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::{crypto, secrets};

#[derive(Debug, Serialize)]
pub struct EncryptionStatus {
    pub enabled: bool,
    pub public_recipient: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EnableResult {
    /// Public bech32 `age1...` recipient; safe to display + persist.
    pub public_recipient: String,
    /// SECRET bech32 key: returned once for immediate backup; never persisted
    /// outside the OS keychain.
    pub secret_identity: String,
}

#[derive(Debug, Serialize)]
pub struct KeyExport {
    pub tool: &'static str,
    pub version: u32,
    pub encryption_format: &'static str,
    pub encryption_algorithm: &'static str,
    /// bech32 `AGE-SECRET-KEY-...`; decrypt with `age -d -i keyfile.txt ciphertext.bin`.
    pub secret_identity: String,
    /// bech32 `age1...`; useful for external re-encryption tooling.
    pub public_recipient: String,
    pub external_decrypt_cmd: &'static str,
}

/// Enables encryption: generates an X25519 identity, stores the secret in the
/// keychain, records the recipient in DB. Rotation invalidates all prior objects.
#[tauri::command]
pub async fn enable_bucket_encryption(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    allow_rotate: Option<bool>,
    confirm_previous_key_saved: Option<bool>,
) -> AppResult<EnableResult> {
    let lock = state.encryption_lock(&account_id, &bucket);
    let _guard = lock.lock().await;

    let already_enabled = state
        .db
        .get_encryption_config(&account_id, &bucket)
        .await?
        .is_some();
    if already_enabled && !allow_rotate.unwrap_or(false) {
        return Err(AppError::InvalidInput(
            "encryption already enabled for this bucket. Pass allow_rotate=true to \
             replace the key (existing encrypted objects become undecryptable)."
                .into(),
        ));
    }
    // Rotation destroys the previous keychain entry; require explicit
    // confirmation so the FE can't skip the user's export step.
    if already_enabled
        && allow_rotate.unwrap_or(false)
        && !confirm_previous_key_saved.unwrap_or(false)
    {
        return Err(AppError::InvalidInput(
            "rotate refused: confirm_previous_key_saved must be true. Export the \
             existing identity first via export_encryption_key so previously \
             encrypted objects remain decryptable."
                .into(),
        ));
    }
    let (secret_str, public_recipient) = crypto::new_identity();
    // Zeroized locally (and in the keychain-write clone); serde unavoidably
    // copies one out via EnableResult — the FE clears its signal on modal close.
    let secret_identity = Zeroizing::new(secret_str);

    tokio::task::spawn_blocking({
        let aid = account_id.clone();
        let bkt = bucket.clone();
        let secret: Zeroizing<String> = Zeroizing::new((*secret_identity).clone());
        move || secrets::set_enc_identity(&aid, &bkt, &secret)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    state
        .db
        .set_encryption_config(&account_id, &bucket, &public_recipient)
        .await?;

    Ok(EnableResult {
        public_recipient,
        secret_identity: (*secret_identity).clone(),
    })
}

/// Disables encryption: removes the keychain identity and DB config. Existing
/// objects stay encrypted and become undecryptable without a re-imported identity.
#[tauri::command]
pub async fn disable_bucket_encryption(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> AppResult<()> {
    let lock = state.encryption_lock(&account_id, &bucket);
    let _guard = lock.lock().await;
    tokio::task::spawn_blocking({
        let aid = account_id.clone();
        let bkt = bucket.clone();
        move || secrets::delete_enc_identity(&aid, &bkt)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    state.db.delete_encryption_config(&account_id, &bucket).await
}

#[tauri::command]
pub async fn get_bucket_encryption_status(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> AppResult<EncryptionStatus> {
    let cfg = state.db.get_encryption_config(&account_id, &bucket).await?;
    Ok(EncryptionStatus {
        enabled: cfg.is_some(),
        public_recipient: cfg.map(|c| c.recipient),
    })
}

/// Returns the sensitive identity export; callers should save it directly
/// (see `save_encryption_key_export`) or hand off to a dialog and discard.
#[tauri::command]
pub async fn export_encryption_key(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> AppResult<KeyExport> {
    let cfg = state
        .db
        .get_encryption_config(&account_id, &bucket)
        .await?
        .ok_or_else(|| AppError::NotFound("encryption not enabled for this bucket".into()))?;

    let aid = account_id.clone();
    let bkt = bucket.clone();
    let raw_secret = tokio::task::spawn_blocking(move || secrets::get_enc_identity(&aid, &bkt))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??
        .ok_or_else(|| AppError::Internal("identity not found in keychain".into()))?;
    let secret_identity = Zeroizing::new(raw_secret);

    Ok(KeyExport {
        tool: "cosmog",
        version: 2,
        encryption_format: crypto::FORMAT_TAG,
        encryption_algorithm: "age (X25519 + ChaCha20-Poly1305)",
        secret_identity: (*secret_identity).clone(),
        public_recipient: cfg.recipient,
        external_decrypt_cmd: "age -d -i cosmog-key.txt <ciphertext> > <plaintext>",
    })
}

/// Writes the identity secret to `dest_path` as plain text compatible with
/// `age -i`: the bech32 string plus newline, no JSON envelope.
#[tauri::command]
pub async fn save_encryption_key_export(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    dest_path: String,
) -> AppResult<()> {
    let export = export_encryption_key(state, account_id, bucket).await?;
    let mut body = String::new();
    body.push_str("# cosmog per-bucket encryption identity (age X25519).\n");
    body.push_str("# Anyone with this file can decrypt every object encrypted for the\n");
    body.push_str("# matching recipient. Store it somewhere safe.\n");
    body.push_str(&format!("# recipient: {}\n", export.public_recipient));
    body.push_str(&format!("# decrypt example: {}\n", export.external_decrypt_cmd));
    body.push_str(&export.secret_identity);
    body.push('\n');
    write_secret_file(&dest_path, &body).await?;
    Ok(())
}

/// Imports a raw bech32 secret or full export file (comment lines skipped)
/// into the keychain, verifying it matches the bucket's recorded recipient.
#[tauri::command]
pub async fn import_encryption_identity(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    identity_text: String,
) -> AppResult<()> {
    let lock = state.encryption_lock(&account_id, &bucket);
    let _guard = lock.lock().await;

    let secret_line = identity_text
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('#') && l.starts_with("AGE-SECRET-KEY-"))
        .ok_or_else(|| AppError::InvalidInput(
            "no AGE-SECRET-KEY-... line found in the provided identity text".into(),
        ))?;
    let secret = Zeroizing::new(secret_line.to_string());

    let identity = crypto::parse_identity(&secret)?;
    let derived_recipient = identity.to_public().to_string();

    // Refuse a mismatched identity: importing the wrong one would silently
    // break every future upload to the recorded recipient.
    match state.db.get_encryption_config(&account_id, &bucket).await? {
        Some(cfg) if cfg.recipient != derived_recipient => {
            return Err(AppError::InvalidInput(format!(
                "identity does not match the recipient recorded for this bucket. \
                 Expected recipient '{}', imported identity derives '{}'.",
                cfg.recipient, derived_recipient,
            )));
        }
        Some(_) => { /* match; nothing to update in DB */ }
        None => {
            state
                .db
                .set_encryption_config(&account_id, &bucket, &derived_recipient)
                .await?;
        }
    }

    tokio::task::spawn_blocking({
        let aid = account_id.clone();
        let bkt = bucket.clone();
        let s: Zeroizing<String> = Zeroizing::new((*secret).clone());
        move || secrets::set_enc_identity(&aid, &bkt, &s)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;
    Ok(())
}

/// Reads an age identity file from disk and imports it; bounded to 64 KiB so
/// a mis-picked huge binary can't OOM us.
#[tauri::command]
pub async fn import_encryption_identity_from_file(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    src_path: String,
) -> AppResult<()> {
    const MAX_IDENTITY_FILE_BYTES: u64 = 64 * 1024;
    let meta = tokio::fs::metadata(&src_path)
        .await
        .map_err(|e| AppError::InvalidInput(format!("open {src_path}: {e}")))?;
    if meta.len() > MAX_IDENTITY_FILE_BYTES {
        return Err(AppError::InvalidInput(format!(
            "identity file too large ({} bytes, max {}). Wrong file?",
            meta.len(),
            MAX_IDENTITY_FILE_BYTES
        )));
    }
    let text = tokio::fs::read_to_string(&src_path)
        .await
        .map_err(|e| AppError::InvalidInput(format!("read {src_path}: {e}")))?;
    import_encryption_identity(state, account_id, bucket, text).await
}

/// Buckets of `account_id` with client-side encryption enabled (FE lock badges).
#[tauri::command]
pub async fn list_encrypted_buckets(
    state: State<'_, AppState>,
    account_id: String,
) -> AppResult<Vec<String>> {
    state.db.list_encrypted_buckets_for_account(&account_id).await
}

/// True iff the keychain holds an identity for this bucket, so the FE can
/// detect the "identity missing" state proactively (fresh install, keychain wipe).
#[tauri::command]
pub async fn has_encryption_identity(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> AppResult<bool> {
    if state.db.get_encryption_config(&account_id, &bucket).await?.is_none() {
        return Ok(false);
    }
    let aid = account_id.clone();
    let bkt = bucket.clone();
    let found = tokio::task::spawn_blocking(move || secrets::get_enc_identity(&aid, &bkt))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??
        .is_some();
    Ok(found)
}

/// Writes `body` owner-only: mode 0600 on Unix; on Windows the default
/// per-user profile ACL already restricts to current user + admins.
async fn write_secret_file(path: &str, body: &str) -> AppResult<()> {
    let path_owned = path.to_string();
    let body_owned = body.to_string();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::Write;
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path_owned)?;
            // mode(0600) is ignored if the file pre-existed with looser bits; chmod explicitly.
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            f.write_all(body_owned.as_bytes())?;
            f.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path_owned)?;
            f.write_all(body_owned.as_bytes())?;
            f.sync_all()?;
        }
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
    .map_err(|e| AppError::Internal(format!("write {path}: {e}")))
}
