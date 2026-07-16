//! Thin wrapper over the OS-native credential store.
//!
//! Secrets (S3 access secret keys) live in the OS keyring — Secret Service
//! (Linux), Keychain (macOS), or Credential Manager (Windows) — keyed by
//! account id. The `accounts` SQLite table only stores the public
//! `access_key_id` and endpoint metadata.
//!
//! [`SERVICE`] must match the application's identifier so OS UIs render
//! sensible attribution. If you change it, existing users will lose access to
//! their stored secrets.

use crate::error::{AppError, AppResult};

const SERVICE: &str = "com.sonus.cosmog";

fn entry(account_id: &str) -> AppResult<keyring::Entry> {
    keyring::Entry::new(SERVICE, account_id).map_err(AppError::from)
}

pub fn set_secret(account_id: &str, secret: &str) -> AppResult<()> {
    entry(account_id)?.set_password(secret).map_err(AppError::from)
}

pub fn get_secret(account_id: &str) -> AppResult<String> {
    entry(account_id)?.get_password().map_err(AppError::from)
}

pub fn delete_secret(account_id: &str) -> AppResult<()> {
    match entry(account_id)?.delete_credential() {
        Ok(_) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

// ── per-bucket encryption identities (age X25519 secret keys) ─────────────────

fn enc_entry(account_id: &str, bucket: &str) -> AppResult<keyring::Entry> {
    let id = format!("enc:{account_id}:{bucket}");
    keyring::Entry::new(SERVICE, &id).map_err(AppError::from)
}

/// Store the bech32 `AGE-SECRET-KEY-...` string for a bucket.
pub fn set_enc_identity(account_id: &str, bucket: &str, secret: &str) -> AppResult<()> {
    enc_entry(account_id, bucket)?
        .set_password(secret)
        .map_err(AppError::from)
}

/// Retrieve the bech32 `AGE-SECRET-KEY-...` string, or `None` if the entry is
/// missing. Callers should scrub the returned buffer once done.
pub fn get_enc_identity(account_id: &str, bucket: &str) -> AppResult<Option<String>> {
    match enc_entry(account_id, bucket)?.get_password() {
        Ok(s) => Ok(Some(s)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(AppError::from(e)),
    }
}

pub fn delete_enc_identity(account_id: &str, bucket: &str) -> AppResult<()> {
    match enc_entry(account_id, bucket)?.delete_credential() {
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(AppError::from(e)),
    }
}
