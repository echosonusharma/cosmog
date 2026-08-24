//! Centralized backend error type. Serializes over Tauri IPC as a JSON string of
//! `{ code, message }` so the FE branches on the stable `code`, never Display text.

use serde::Serialize;
use thiserror::Error;

/// Backend-wide error type. Variants carry only `String` messages (cheap to clone/serialize);
/// branch via [`AppError::code`], not message text.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("database error: {0}")]
    Database(String),

    /// OS keyring access failure; a missing entry surfaces as `NotFound`.
    #[error("keyring error: {0}")]
    Keyring(String),

    /// Catch-all AWS SDK / S3-protocol failure; message carries the SDK error code + reason verbatim.
    #[error("s3 error: {0}")]
    S3(String),

    #[error("access denied: {0}")]
    AccessDenied(String),

    /// Signature mismatch: secret rotated outside Cosmog or mistyped; FE should prompt re-entry.
    #[error("credentials invalid: {0}")]
    CredentialsInvalid(String),

    /// State conflict (`PreconditionFailed`, `BucketAlreadyExists`, ...); retryable once fixed.
    #[error("conflict: {0}")]
    Conflict(String),

    /// Server signalled `SlowDown`/`TooManyRequests`; back off before retrying.
    #[error("rate limited: {0}")]
    RateLimited(String),

    #[error("io error: {0}")]
    Io(String),

    /// Cooperative cancellation via `CancellationToken`; callers treat this as a terminal
    /// `canceled` status, not a failure.
    #[error("canceled: {0}")]
    Canceled(String),

    /// `PermanentRedirect`: bucket is in another region. Backend auto-corrects the stored region
    /// and retries; surfaced only if the retry also fails.
    #[error("region redirect: {0}")]
    RegionRedirect(String),

    /// Connection refused, DNS failure, TCP timeout, or TLS error; endpoint down or misconfigured.
    #[error("network unreachable: {0}")]
    NetworkUnreachable(String),

    /// Provider doesn't implement the op (`NotImplemented`/HTTP 501, common on B2/R2); FE should
    /// hide the feature rather than surface a generic error.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// Bucket is encrypted but no identity is in the OS keychain; FE should prompt for import.
    #[error("encryption identity missing: {0}")]
    EncryptionIdentityMissing(String),

    /// Archived object (`InvalidObjectState`, e.g. Glacier) must be restored before reading;
    /// FE should explain rather than show a raw error.
    #[error("archived: {0}")]
    Archived(String),

    #[error("internal: {0}")]
    Internal(String),
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            AppError::NotFound(_) => "not_found",
            AppError::InvalidInput(_) => "invalid_input",
            AppError::Database(_) => "database",
            AppError::Keyring(_) => "keyring",
            AppError::S3(_) => "s3",
            AppError::AccessDenied(_) => "access_denied",
            AppError::CredentialsInvalid(_) => "credentials_invalid",
            AppError::Conflict(_) => "conflict",
            AppError::RateLimited(_) => "rate_limited",
            AppError::Io(_) => "io",
            AppError::Canceled(_) => "canceled",
            AppError::RegionRedirect(_) => "region_redirect",
            AppError::NetworkUnreachable(_) => "network_unreachable",
            AppError::Unsupported(_) => "unsupported",
            AppError::EncryptionIdentityMissing(_) => "encryption_identity_missing",
            AppError::Archived(_) => "archived",
            AppError::Internal(_) => "internal",
        }
    }
}

impl From<tokio_rusqlite::Error> for AppError {
    fn from(value: tokio_rusqlite::Error) -> Self {
        AppError::Database(value.to_string())
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(value: rusqlite::Error) -> Self {
        AppError::Database(value.to_string())
    }
}

#[cfg(not(target_os = "android"))]
impl From<keyring::Error> for AppError {
    fn from(value: keyring::Error) -> Self {
        match value {
            keyring::Error::NoEntry => AppError::NotFound("credentials not found in system keychain. Please re-add this account in Settings.".into()),
            other => AppError::Keyring(other.to_string()),
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(value: std::io::Error) -> Self {
        AppError::Io(value.to_string())
    }
}

impl From<anyhow::Error> for AppError {
    fn from(value: anyhow::Error) -> Self {
        AppError::Internal(value.to_string())
    }
}

/// Wire-format error returned to the FE: serializes as `{ "code": "...", "message": "..." }`.
#[derive(Debug, Serialize)]
pub struct WireError {
    pub code: &'static str,
    pub message: String,
}

impl From<&AppError> for WireError {
    fn from(err: &AppError) -> Self {
        WireError {
            code: err.code(),
            message: err.to_string(),
        }
    }
}

impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Serialized as a JSON *string*: Linux/WebKitGTK IPC silently drops JSON error objects and
        // replaces them with the literal "Unknown error" string. The FE errMsg() parses it back.
        let wire = WireError::from(self);
        let s = serde_json::to_string(&wire)
            .unwrap_or_else(|_| self.to_string());
        serializer.serialize_str(&s)
    }
}

pub type AppResult<T> = Result<T, AppError>;
