//! Tauri commands for per-bucket client-side encryption.
//!
//! Backend uses the `age` file format. Enabling generates a fresh X25519
//! identity, stores the secret in the OS keychain, and returns the recipient
//! (public) string. The user must export the secret identity before disabling
//! or losing access to the machine; without it, all uploaded objects become
//! undecryptable.

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
    /// The bech32 `age1...` recipient. Public, safe to display + persist.
    pub public_recipient: String,
    /// The bech32 `AGE-SECRET-KEY-...` string. SECRET. Return once to the FE
    /// so the user can immediately export a backup. Never persisted anywhere
    /// besides the OS keychain.
    pub secret_identity: String,
}

#[derive(Debug, Serialize)]
pub struct KeyExport {
    pub tool: &'static str,
    pub version: u32,
    pub encryption_format: &'static str,
    pub encryption_algorithm: &'static str,
    /// bech32 `AGE-SECRET-KEY-...`. Handles both `age` and `rage` CLIs plus
    /// `pyrage`. Users decrypt with `age -d -i keyfile.txt ciphertext.bin`.
    pub secret_identity: String,
    /// bech32 `age1...`. Public counterpart; not required for decryption but
    /// useful for external re-encryption tooling.
    pub public_recipient: String,
    pub external_decrypt_cmd: &'static str,
}

/// Enable encryption for a bucket. Generates a fresh X25519 identity, stores
/// the secret in the OS keychain, records the public recipient in the DB.
///
/// Refuses if encryption is already configured unless `allow_rotate=true`.
/// Rotation invalidates the previous identity: every object already uploaded
/// becomes undecryptable without a copy of the old identity file.
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
    // Rotation destroys the previous keychain entry. Require the caller to
    // confirm they have exported (or explicitly discarded) the previous key
    // so the FE can never rotate without walking the user through export.
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
    // Zeroize the local copy on drop. The clone handed to keychain persistence
    // (also Zeroizing) is scrubbed after write. The one that leaves via
    // EnableResult is unavoidably serialized into a new String by serde-json;
    // the FE clears its signal on modal close (see EncryptionModal.tsx).
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

/// Disable encryption for a bucket. Removes the identity from the keychain
/// and the recipient from the DB. Existing encrypted objects are NOT
/// decrypted; they remain encrypted on S3 and the app can no longer decrypt
/// them after this without a re-imported identity file.
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

/// Return the identity export payload. Includes the raw secret identity
/// string, which is sensitive: callers should either save it directly (see
/// `save_encryption_key_export`) or hand-off to an OS clipboard/dialog and
/// then discard.
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

/// Write the identity secret directly to `dest_path` as a plain text file
/// compatible with `age -i`. The file is exactly the bech32 secret string
/// followed by a newline — no JSON envelope, so `age` accepts it as-is.
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

/// Import a previously exported age identity file into the OS keychain for
/// `(account_id, bucket)`. Accepts either the raw bech32 secret string or a
/// full export file (comment lines starting with `#` are skipped). Verifies
/// the identity matches the recipient recorded in the DB for the bucket.
#[tauri::command]
pub async fn import_encryption_identity(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    identity_text: String,
) -> AppResult<()> {
    let lock = state.encryption_lock(&account_id, &bucket);
    let _guard = lock.lock().await;

    // Extract the first non-comment line beginning with AGE-SECRET-KEY-.
    let secret_line = identity_text
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('#') && l.starts_with("AGE-SECRET-KEY-"))
        .ok_or_else(|| AppError::InvalidInput(
            "no AGE-SECRET-KEY-... line found in the provided identity text".into(),
        ))?;
    let secret = Zeroizing::new(secret_line.to_string());

    // Verify parseability + derive the public recipient.
    let identity = crypto::parse_identity(&secret)?;
    let derived_recipient = identity.to_public().to_string();

    // Cross-check against the DB-recorded recipient. If the DB doesn't have
    // one yet (fresh bucket), record it. If it has one and it disagrees,
    // refuse: importing the wrong identity would silently break every future
    // upload for objects encrypted to the existing recipient.
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

/// Read an age identity file from disk and import it. Convenience wrapper
/// over `import_encryption_identity` — reads the file (bounded to 64 KiB so a
/// mis-picked huge binary can't OOM us) then delegates.
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

/// Return the list of buckets for `account_id` that have client-side
/// encryption enabled. FE uses this to render lock badges on the bucket grid.
#[tauri::command]
pub async fn list_encrypted_buckets(
    state: State<'_, AppState>,
    account_id: String,
) -> AppResult<Vec<String>> {
    state.db.list_encrypted_buckets_for_account(&account_id).await
}

/// Return `true` iff the OS keychain has an identity stored for this bucket.
/// Used by the FE to detect the "identity missing" state proactively (fresh
/// install, keychain wipe) and prompt for import before an operation fails.
#[tauri::command]
pub async fn has_encryption_identity(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> AppResult<bool> {
    // Only meaningful if the bucket has encryption configured. If no config,
    // there is no identity to be missing.
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

/// Write `body` to `path` with owner-only permissions (0600) on Unix.
/// On Windows we rely on the default per-user ACL of files under the user's
/// profile directory (Downloads etc.), which is already restricted to the
/// current user + admins.
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
            // If the file pre-existed with looser bits, OpenOptions::mode is
            // ignored — chmod explicitly to be safe.
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
