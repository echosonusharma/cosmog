//! Client-side encryption using the `age` file format.
//!
//! Payload is standard age binary (or armored when the caller opts in): header
//! + streaming ChaCha20-Poly1305 chunks of 64 KiB with per-chunk counter
//! nonces + last-chunk marker. External decryption: `age -d -i keyfile.txt`
//! (or `rage`, `pyrage`, Go `age`).
//!
//! Per-bucket key material is a randomly generated X25519 age identity. The
//! secret string (`AGE-SECRET-KEY-...`) lives in the OS keychain; the derived
//! recipient (`age1...`) is what encrypts new uploads.

use std::io::{Read, Write};
use std::path::Path;
use std::str::FromStr;

use age::secrecy::ExposeSecret;
use age::x25519;

use crate::error::{AppError, AppResult};

/// Format tag written to S3 user metadata (`cosmog-format`). Lets a future
/// implementation branch on the payload format without probing bytes.
pub const FORMAT_TAG: &str = "age-v1";

/// Magic prefix of an age v1 header. We probe object bytes for this instead of
/// trusting S3 user metadata, which is attacker-controllable for any principal
/// with PUT rights on the bucket.
pub const AGE_MAGIC: &[u8] = b"age-encryption.org/v1\n";

/// True if the first bytes look like an age v1 ciphertext header.
pub fn is_age_ciphertext(bytes: &[u8]) -> bool {
    bytes.starts_with(AGE_MAGIC)
}

/// Preview / in-app crypt operations must not buffer more than this many bytes
/// of plaintext. Streaming file paths (upload/download) do not use this cap.
pub const MAX_INMEMORY_CRYPT_BYTES: u64 = 512 * 1024 * 1024;

// ── identity lifecycle ────────────────────────────────────────────────────────

/// Generate a fresh X25519 identity. The returned secret string is a bech32
/// `AGE-SECRET-KEY-...` value suitable for storage in the OS keychain and for
/// direct use with the `age` CLI (`age -d -i keyfile.txt`).
pub fn new_identity() -> (String, String) {
    let id = x25519::Identity::generate();
    let secret = id.to_string().expose_secret().to_string();
    let public = id.to_public().to_string();
    (secret, public)
}

/// Parse a `AGE-SECRET-KEY-...` string into an identity object. The input
/// buffer is not scrubbed by this function; callers hold the responsibility
/// for zeroizing it if it lives in their own memory.
pub fn parse_identity(secret: &str) -> AppResult<x25519::Identity> {
    x25519::Identity::from_str(secret)
        .map_err(|e| AppError::Internal(format!("parse identity: {e}")))
}

pub fn parse_recipient(public: &str) -> AppResult<x25519::Recipient> {
    x25519::Recipient::from_str(public)
        .map_err(|e| AppError::Internal(format!("parse recipient: {e}")))
}

// ── in-memory helpers ────────────────────────────────────────────────────────

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

// ── streaming file helpers (constant RAM) ────────────────────────────────────

/// Stream-encrypt `src` to `dst`. Runs on a blocking thread so tokio's runtime
/// stays responsive during file IO + AEAD. Memory usage is O(64 KiB chunk).
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

/// Stream-decrypt `src` to `dst`. See `encrypt_file` for RAM behavior.
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
