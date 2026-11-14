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
        .map_err(|e| AppError::DecryptionFailed(format!("age Decryptor: {e}")))?;
    let mut reader = dec
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|e| AppError::DecryptionFailed(format!("age decrypt: {e}")))?;
    let mut out = Vec::new();
    reader
        .read_to_end(&mut out)
        .map_err(|e| AppError::DecryptionFailed(format!("age read: {e}")))?;
    Ok(out)
}

/// Stream-encrypt on a blocking thread so the runtime stays responsive; O(64 KiB chunk) memory.
pub async fn encrypt_file(
    src: &Path,
    dst: &Path,
    recipient: x25519::Recipient,
) -> AppResult<()> {
    encrypt_file_cancellable(src, dst, recipient, tokio_util::sync::CancellationToken::new()).await
}

struct CancelRead<R> {
    inner: R,
    cancel: tokio_util::sync::CancellationToken,
}

impl<R: Read> Read for CancelRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(std::io::Error::other("canceled"));
        }
        self.inner.read(buf)
    }
}

/// [`encrypt_file`] that stops between chunks once `cancel` fires.
pub async fn encrypt_file_cancellable(
    src: &Path,
    dst: &Path,
    recipient: x25519::Recipient,
    cancel: tokio_util::sync::CancellationToken,
) -> AppResult<()> {
    let f_in = tokio::fs::File::open(src)
        .await
        .map_err(|e| AppError::Internal(format!("open {}: {e}", src.display())))?
        .into_std()
        .await;
    encrypt_open_file_cancellable(f_in, dst, recipient, cancel).await
}

/// [`encrypt_file_cancellable`] from an already opened (and verified) source handle.
pub async fn encrypt_open_file_cancellable(
    f_in: std::fs::File,
    dst: &Path,
    recipient: x25519::Recipient,
    cancel: tokio_util::sync::CancellationToken,
) -> AppResult<()> {
    let dst = dst.to_path_buf();
    let cancel_check = cancel.clone();
    let res = tokio::task::spawn_blocking(move || -> AppResult<()> {
        let mut r = CancelRead {
            inner: std::io::BufReader::new(f_in),
            cancel,
        };
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
        // finish() hands back the BufWriter; dropping it unflushed would swallow write errors.
        let mut w = writer
            .finish()
            .map_err(|e| AppError::Internal(format!("age finish: {e}")))?;
        w.flush()
            .map_err(|e| AppError::Internal(format!("age flush: {e}")))?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    if res.is_err() && cancel_check.is_cancelled() {
        return Err(AppError::Canceled("encryption canceled".into()));
    }
    res
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
            .map_err(|e| AppError::DecryptionFailed(format!("age Decryptor: {e}")))?;
        let mut reader = dec
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .map_err(|e| AppError::DecryptionFailed(format!("age decrypt: {e}")))?;
        let f_out = std::fs::File::create(&dst)
            .map_err(|e| AppError::Internal(format!("create {}: {e}", dst.display())))?;
        let mut w = std::io::BufWriter::new(f_out);
        // Manual copy so read errors (bad or truncated ciphertext, source I/O) are not blamed on dst.
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(match e.kind() {
                        std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => {
                            AppError::DecryptionFailed(format!("age read: {e}"))
                        }
                        _ => AppError::Io(format!("read {}: {e}", src.display())),
                    })
                }
            };
            w.write_all(&buf[..n])
                .map_err(|e| AppError::Io(format!("write {}: {e}", dst.display())))?;
        }
        w.flush()
            .map_err(|e| AppError::Internal(format!("age flush: {e}")))?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
}

const SECRET_PREFIX: &str = "AGE-SECRET-KEY-";

/// Keyfile body for `age -d -i`: `#` comment lines, then the secret and a newline.
pub fn format_keyfile(secret: &str, recipient: &str) -> String {
    format!(
        "# cosmog per-bucket encryption identity (age X25519).\n\
         # Anyone with this file can decrypt every object encrypted for the\n\
         # matching recipient. Store it somewhere safe.\n\
         # public key: {recipient}\n\
         # decrypt example: age -d -i <this-file> <ciphertext> > <plaintext>\n\
         {secret}\n"
    )
}

/// Keyfile bytes to text: UTF-8 (BOM stripped) or UTF-16 with BOM (PowerShell redirects).
pub fn decode_keyfile(bytes: &[u8]) -> AppResult<zeroize::Zeroizing<String>> {
    let utf16 = |le: bool| -> AppResult<String> {
        let units: zeroize::Zeroizing<Vec<u16>> = zeroize::Zeroizing::new(
            bytes[2..]
                .chunks_exact(2)
                .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
                .collect(),
        );
        String::from_utf16(&units).map_err(|_| AppError::InvalidInput("identity file is not valid UTF-16".into()))
    };
    let text = match bytes {
        [0xFF, 0xFE, ..] => utf16(true)?,
        [0xFE, 0xFF, ..] => utf16(false)?,
        _ => std::str::from_utf8(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes))
            .map_err(|_| AppError::InvalidInput("identity file is not a text file".into()))?
            .to_string(),
    };
    Ok(zeroize::Zeroizing::new(text))
}

/// Every `AGE-SECRET-KEY-...` token on non-comment lines, in file order.
pub fn identity_candidates(text: &str) -> Vec<zeroize::Zeroizing<String>> {
    text.lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}'))
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.find(SECRET_PREFIX).map(|i| &l[i..]))
        .map(|s| {
            let end = s
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .unwrap_or(s.len());
            zeroize::Zeroizing::new(s[..end].to_string())
        })
        .collect()
}

/// Picks the identity matching `expected` (or the first valid one) and returns it with its recipient.
pub fn select_identity(
    text: &str,
    expected: Option<&str>,
) -> AppResult<(zeroize::Zeroizing<String>, String)> {
    let candidates = identity_candidates(text);
    if candidates.is_empty() {
        return Err(AppError::InvalidInput(
            "no AGE-SECRET-KEY-... line found in the provided identity text".into(),
        ));
    }
    let mut first_derived: Option<String> = None;
    for secret in candidates {
        let Ok(id) = parse_identity(&secret) else { continue };
        let derived = id.to_public().to_string();
        match expected {
            Some(want) if want != derived => {
                first_derived.get_or_insert(derived);
            }
            _ => return Ok((secret, derived)),
        }
    }
    Err(AppError::InvalidInput(match (expected, first_derived) {
        (Some(want), Some(got)) => format!(
            "identity does not match the recipient recorded for this bucket. \
             Expected recipient '{want}', imported identity derives '{got}'."
        ),
        _ => "AGE-SECRET-KEY-... line found but it is not a valid age identity".into(),
    }))
}
