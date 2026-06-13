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
