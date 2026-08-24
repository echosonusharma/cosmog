//! Client-side encryption in the standard `age` format: header + streaming ChaCha20-Poly1305 chunks
//! (64 KiB, per-chunk counter nonces, last-chunk marker; decryptable by any age tool). Keys are X25519 identities, secrets in the OS keychain.

use std::io::{Read, Write};
use std::path::Path;
use std::str::FromStr;

use age::secrecy::ExposeSecret;
use age::x25519;

use crate::error::{AppError, AppResult};

/// Written to S3 user metadata (`cosmog-format`) so a future impl can branch without probing bytes.
pub const FORMAT_TAG: &str = "age-v1";

/// Probed from object bytes instead of trusting S3 user metadata, which is attacker-controllable
/// for any principal with PUT rights on the bucket.
pub const AGE_MAGIC: &[u8] = b"age-encryption.org/v1\n";

pub fn is_age_ciphertext(bytes: &[u8]) -> bool {
    bytes.starts_with(AGE_MAGIC)
}

/// Cap for preview/in-app crypt buffering only; streaming upload/download paths bypass it.
pub const MAX_INMEMORY_CRYPT_BYTES: u64 = 512 * 1024 * 1024;

/// Fresh X25519 identity; the secret is bech32, usable directly with the `age` CLI.
pub fn new_identity() -> (String, String) {
    let id = x25519::Identity::generate();
    let secret = id.to_string().expose_secret().to_string();
    let public = id.to_public().to_string();
    (secret, public)
}

/// Parses an `AGE-SECRET-KEY-...` string; the input buffer is NOT scrubbed here — callers own zeroizing it.
pub fn parse_identity(secret: &str) -> AppResult<x25519::Identity> {
    x25519::Identity::from_str(secret)
        .map_err(|e| AppError::Internal(format!("parse identity: {e}")))
}

pub fn parse_recipient(public: &str) -> AppResult<x25519::Recipient> {
    x25519::Recipient::from_str(public)
        .map_err(|e| AppError::Internal(format!("parse recipient: {e}")))
}

pub fn encrypt_bytes(recipient: &x25519::Recipient, plaintext: &[u8]) -> AppResult<Vec<u8>> {
    let enc = age::Encryptor::with_recipients(std::iter::once(recipient as &dyn age::Recipient))
        .map_err(|e| AppError::Internal(format!("age Encryptor: {e}")))?;
    let mut out = Vec::with_capacity(plaintext.len() + 256);
    let mut writer = enc
        .wrap_output(&mut out)
        .map_err(|e| AppError::Internal(format!("age wrap_output: {e}")))?;
    writer
        .write_all(plaintext)
        .map_err(|e| AppError::Internal(format!("age write: {e}")))?;
    writer
        .finish()
        .map_err(|e| AppError::Internal(format!("age finish: {e}")))?;
    Ok(out)
}

pub fn decrypt_bytes(identity: &x25519::Identity, ciphertext: &[u8]) -> AppResult<Vec<u8>> {
    let dec = age::Decryptor::new(ciphertext)
        .map_err(|e| AppError::Internal(format!("age Decryptor: {e}")))?;
    let mut reader = dec
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|e| AppError::Internal(format!("age decrypt: {e}")))?;
    let mut out = Vec::new();
    reader
        .read_to_end(&mut out)
        .map_err(|e| AppError::Internal(format!("age read: {e}")))?;
    Ok(out)
}

/// Stream-encrypt on a blocking thread so the runtime stays responsive; O(64 KiB chunk) memory.
pub async fn encrypt_file(
    src: &Path,
    dst: &Path,
    recipient: x25519::Recipient,
) -> AppResult<()> {
    let src = src.to_path_buf();
    let dst = dst.to_path_buf();
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        let f_in = std::fs::File::open(&src)
            .map_err(|e| AppError::Internal(format!("open {}: {e}", src.display())))?;
        let mut r = std::io::BufReader::new(f_in);
        let f_out = std::fs::File::create(&dst)
            .map_err(|e| AppError::Internal(format!("create {}: {e}", dst.display())))?;
        let w = std::io::BufWriter::new(f_out);
        let enc = age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .map_err(|e| AppError::Internal(format!("age Encryptor: {e}")))?;
        let mut writer = enc
            .wrap_output(w)
            .map_err(|e| AppError::Internal(format!("age wrap_output: {e}")))?;
        std::io::copy(&mut r, &mut writer)
            .map_err(|e| AppError::Internal(format!("age copy: {e}")))?;
        writer
            .finish()
            .map_err(|e| AppError::Internal(format!("age finish: {e}")))?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
}

/// Stream-decrypt on a blocking thread; same chunked memory profile as [`encrypt_file`].
pub async fn decrypt_file(
    src: &Path,
    dst: &Path,
    identity: x25519::Identity,
) -> AppResult<()> {
    let src = src.to_path_buf();
    let dst = dst.to_path_buf();
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        let f_in = std::fs::File::open(&src)
            .map_err(|e| AppError::Internal(format!("open {}: {e}", src.display())))?;
        let r = std::io::BufReader::new(f_in);
        let dec = age::Decryptor::new_buffered(r)
            .map_err(|e| AppError::Internal(format!("age Decryptor: {e}")))?;
        let mut reader = dec
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .map_err(|e| AppError::Internal(format!("age decrypt: {e}")))?;
        let f_out = std::fs::File::create(&dst)
            .map_err(|e| AppError::Internal(format!("create {}: {e}", dst.display())))?;
        let mut w = std::io::BufWriter::new(f_out);
        std::io::copy(&mut reader, &mut w)
            .map_err(|e| AppError::Internal(format!("age copy: {e}")))?;
        w.flush()
            .map_err(|e| AppError::Internal(format!("age flush: {e}")))?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
}
