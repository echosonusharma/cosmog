//! Centralized error type for backend operations.
//!
//! Every fallible API in the backend returns [`AppResult<T>`]. Variants are
//! shaped so that the front-end can branch on a stable `code` string when an
//! error is serialized over Tauri's IPC. Specifically, [`AppError`] serializes
//! into a [`WireError`] (`{ code, message }`) so the FE does not have to parse
//! free-form `Display` output.
//!
//! When adding a new variant, also update [`AppError::code`] and the
//! `From<…>` implementations as needed.

use serde::Serialize;
use thiserror::Error;

/// Backend-wide error type.
///
/// Variants intentionally avoid carrying typed payloads (only `String` messages)
/// to keep the type cheap to clone, format, and serialize. If a caller needs to
/// branch on a specific error condition, use [`AppError::code`] instead of
/// matching on the message text.
#[derive(Debug, Error)]
pub enum AppError {
    /// The requested entity was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// Caller supplied invalid arguments (empty key, bad protocol, etc.).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// SQLite or migration layer failure.
    #[error("database error: {0}")]
    Database(String),

    /// OS keyring access failure (missing entry surfaces as `NotFound`).
    #[error("keyring error: {0}")]
    Keyring(String),

    /// AWS SDK / S3-protocol failure not classified into a more specific
    /// variant. Message contains the SDK's error code + reason verbatim.
    #[error("s3 error: {0}")]
    S3(String),

    /// Caller is authenticated but not authorized for this operation.
    #[error("access denied: {0}")]
    AccessDenied(String),

    /// Credentials don't match the server's expected signature. Almost always
    /// means the secret was rotated outside Cosmog or the user typed it
    /// wrong. UI should prompt for re-entry.
    #[error("credentials invalid: {0}")]
    CredentialsInvalid(String),

    /// Operation conflicts with current resource state (`PreconditionFailed`,
    /// `BucketAlreadyExists`, etc.). Usually retryable only after the caller
    /// fixes the precondition.
    #[error("conflict: {0}")]
    Conflict(String),

    /// Server signalled `SlowDown` / `TooManyRequests`. Caller should back off
    /// before retrying.
    #[error("rate limited: {0}")]
    RateLimited(String),

    /// Local I/O failure (file read, write, mkdir).
    #[error("io error: {0}")]
    Io(String),

    /// Transfer was cooperatively canceled via its `CancellationToken`.
    ///
    /// Returned by stream/multipart workers when [`tokio_util::sync::CancellationToken::cancelled`]
    /// fires. Not an error in the usual sense — callers typically treat this as
    /// a terminal `canceled` status, not a failure.
    #[error("canceled: {0}")]
    Canceled(String),

    /// Catch-all for unexpected internal failures.
    #[error("internal: {0}")]
    Internal(String),
}

impl AppError {
    /// Stable machine-readable tag for this error. Front-end can match on this
    /// to render localized messages or branch on error class.
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

/// Wire-format error returned to the front-end. Always serializes as
/// `{ "code": "...", "message": "..." }`.
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
        // Serialize as a JSON string rather than an object so the payload
        // survives Linux/WebKitGTK IPC, which silently drops JSON error objects
        // and replaces them with the literal "Unknown error" string.
        // The frontend errMsg() already handles JSON-string → object parsing.
        let wire = WireError::from(self);
        let s = serde_json::to_string(&wire)
            .unwrap_or_else(|_| self.to_string());
        serializer.serialize_str(&s)
    }
}

/// Convenience alias for `Result<T, AppError>`.
pub type AppResult<T> = Result<T, AppError>;
