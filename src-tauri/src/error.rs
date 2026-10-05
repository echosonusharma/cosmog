//! Centralized backend error type. Serializes over Tauri IPC as a JSON string of
//! `{ code, detail }` so the FE branches on the stable `code`, never on text.

use serde::Serialize;

// One row per error: `Variant => "wire_code", "log prefix";`. The wire code is the stable contract
// with the FE, which owns the user-facing title and description for each code (src/i18n errors).
macro_rules! app_errors {
    ($($(#[$doc:meta])* $var:ident => $code:literal, $prefix:literal;)*) => {
        /// Backend-wide error type. Variants carry only a `String` detail (cheap to clone and
        /// serialize); branch via [`AppError::code`] or the `is_*` helpers, never on text.
        #[derive(Debug)]
        pub enum AppError {
            $($(#[$doc])* $var(String),)*
        }

        impl AppError {
            pub fn code(&self) -> &'static str {
                match self { $(AppError::$var(_) => $code,)* }
            }

            /// Raw technical text (SDK message, OS error). Logs and "technical details" only.
            pub fn detail(&self) -> &str {
                match self { $(AppError::$var(m) => m,)* }
            }

            /// Every wire code, so tests can check the FE has a translation for each.
            pub const CODES: &'static [&'static str] = &[$($code,)*];
        }

        impl std::fmt::Display for AppError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self { $(AppError::$var(m) => write!(f, "{}: {}", $prefix, m),)* }
            }
        }
    };
}

app_errors! {
    /// Generic missing item (local data, or a 404 with no more specific S3 code).
    NotFound => "not_found", "not found";
    InvalidInput => "invalid_input", "invalid input";
    Database => "database", "database error";
    /// OS keyring access failure; a missing entry surfaces as `CredentialsMissing`.
    Keyring => "keyring", "keyring error";
    /// Unclassified AWS SDK / S3-protocol failure; detail carries the SDK code + reason verbatim.
    S3 => "s3", "s3 error";
    AccessDenied => "access_denied", "access denied";
    /// Signature mismatch: secret rotated outside Cosmog or mistyped; FE prompts re-entry.
    CredentialsInvalid => "credentials_invalid", "credentials invalid";
    /// No secret in the keychain for this account; FE prompts re-entry.
    CredentialsMissing => "credentials_missing", "credentials missing";
    /// `InvalidAccessKeyId`: the provider does not know this access key.
    AccessKeyInvalid => "access_key_invalid", "access key invalid";
    /// `ExpiredToken` / `InvalidToken`: temporary credentials ran out.
    TokenExpired => "token_expired", "token expired";
    /// `RequestTimeTooSkewed` / `RequestExpired`: device clock is off from the server's.
    ClockSkew => "clock_skew", "clock skew";
    /// State conflict (`PreconditionFailed`, `OperationAborted`, ...); retryable once fixed.
    Conflict => "conflict", "conflict";
    BucketExists => "bucket_exists", "bucket exists";
    BucketNotEmpty => "bucket_not_empty", "bucket not empty";
    BucketNotFound => "bucket_not_found", "bucket not found";
    ObjectNotFound => "object_not_found", "object not found";
    UploadNotFound => "upload_not_found", "upload not found";
    InvalidBucketName => "invalid_bucket_name", "invalid bucket name";
    /// Request signed for the wrong region (`AuthorizationHeaderMalformed`).
    InvalidRegion => "invalid_region", "invalid region";
    QuotaExceeded => "quota_exceeded", "quota exceeded";
    EntityTooLarge => "entity_too_large", "entity too large";
    /// `SlowDown` / `TooManyRequests`; back off before retrying.
    RateLimited => "rate_limited", "rate limited";
    /// 5xx / `InternalError` / `ServiceUnavailable` from the provider.
    ServiceUnavailable => "service_unavailable", "service unavailable";
    Io => "io", "io error";
    /// Cooperative cancellation via `CancellationToken`; a terminal `canceled` status, not a failure.
    Canceled => "canceled", "canceled";
    /// `PermanentRedirect`: bucket is in another region. Backend auto-corrects the stored region
    /// and retries; surfaced only if the retry also fails.
    RegionRedirect => "region_redirect", "region redirect";
    /// Connection failed for a reason we could not narrow down.
    NetworkUnreachable => "network_unreachable", "network unreachable";
    RequestTimeout => "request_timeout", "request timeout";
    DnsFailure => "dns_failure", "dns failure";
    ConnectionRefused => "connection_refused", "connection refused";
    TlsError => "tls_error", "tls error";
    /// Provider doesn't implement the op (`NotImplemented`/HTTP 501, common on B2/R2); FE hides
    /// the feature rather than surfacing a generic error.
    Unsupported => "unsupported", "unsupported";
    /// Bucket is encrypted but no identity is in the OS keychain; FE prompts for import.
    EncryptionIdentityMissing => "encryption_identity_missing", "encryption identity missing";
    /// Ciphertext could not be opened: wrong key, damaged data, or not age format.
    DecryptionFailed => "decryption_failed", "decryption failed";
    /// Archived object (`InvalidObjectState`, e.g. Glacier) must be restored before reading.
    Archived => "archived", "archived";
    Internal => "internal", "internal";
}

impl std::error::Error for AppError {}

impl AppError {
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            AppError::NotFound(_)
                | AppError::BucketNotFound(_)
                | AppError::ObjectNotFound(_)
                | AppError::UploadNotFound(_)
        )
    }

    pub fn is_access_denied(&self) -> bool {
        matches!(self, AppError::AccessDenied(_))
    }

    pub fn is_credentials_missing(&self) -> bool {
        matches!(self, AppError::CredentialsMissing(_))
    }

    /// Transient failures worth retrying with backoff.
    pub fn is_retriable(&self) -> bool {
        matches!(
            self,
            AppError::RateLimited(_)
                | AppError::S3(_)
                | AppError::NetworkUnreachable(_)
                | AppError::ServiceUnavailable(_)
                | AppError::RequestTimeout(_)
                | AppError::DnsFailure(_)
                | AppError::ConnectionRefused(_)
        )
    }

    /// The wire JSON string. Also what is persisted for failed transfers and watch scans, so the
    /// FE can translate them later from the code.
    pub fn to_wire_string(&self) -> String {
        serde_json::to_string(&WireError::from(self)).unwrap_or_else(|_| self.to_string())
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
            keyring::Error::NoEntry => AppError::CredentialsMissing("no secret in system keychain".into()),
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

/// Wire-format error returned to the FE: serializes as `{ "code": "...", "detail": "..." }`.
/// The FE renders a translated title and description from `code`; `detail` is technical only.
#[derive(Debug, Serialize)]
pub struct WireError {
    pub code: &'static str,
    pub detail: String,
}

impl From<&AppError> for WireError {
    fn from(err: &AppError) -> Self {
        WireError {
            code: err.code(),
            detail: err.detail().to_string(),
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
        serializer.serialize_str(&self.to_wire_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn wire_codes_are_unique_lowercase() {
        let mut seen = HashSet::new();
        for c in AppError::CODES {
            assert!(seen.insert(*c), "duplicate code {c}");
            assert!(
                !c.is_empty() && c.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'),
                "bad code {c}"
            );
        }
    }

    #[test]
    fn wire_string_carries_code_and_raw_detail_only() {
        let e = AppError::ClockSkew("get_object: RequestTimeTooSkewed boom".into());
        let v: serde_json::Value = serde_json::from_str(&e.to_wire_string()).unwrap();
        assert_eq!(v["code"], "clock_skew");
        assert_eq!(v["detail"], "get_object: RequestTimeTooSkewed boom");
        assert_eq!(v.as_object().unwrap().len(), 2, "no English message field on the wire");
    }

    #[test]
    fn ipc_serialization_is_that_same_json_string() {
        let e = AppError::AccessDenied("nope".into());
        let ipc = serde_json::to_value(&e).unwrap();
        assert_eq!(ipc.as_str().unwrap(), e.to_wire_string());
    }

    #[test]
    fn display_keeps_a_readable_prefix_for_logs() {
        assert_eq!(AppError::BucketNotFound("b".into()).to_string(), "bucket not found: b");
    }

    #[test]
    fn not_found_family_is_matched_by_the_predicate() {
        for e in [
            AppError::NotFound(String::new()),
            AppError::BucketNotFound(String::new()),
            AppError::ObjectNotFound(String::new()),
            AppError::UploadNotFound(String::new()),
        ] {
            assert!(e.is_not_found(), "{}", e.code());
        }
        assert!(!AppError::AccessDenied(String::new()).is_not_found());
    }

    #[test]
    fn only_transient_failures_retry() {
        for e in [
            AppError::RateLimited(String::new()),
            AppError::ServiceUnavailable(String::new()),
            AppError::RequestTimeout(String::new()),
            AppError::DnsFailure(String::new()),
            AppError::ConnectionRefused(String::new()),
            AppError::NetworkUnreachable(String::new()),
        ] {
            assert!(e.is_retriable(), "{} should retry", e.code());
        }
        for e in [
            AppError::ClockSkew(String::new()),
            AppError::AccessDenied(String::new()),
            AppError::CredentialsInvalid(String::new()),
            AppError::TlsError(String::new()),
            AppError::Canceled(String::new()),
        ] {
            assert!(!e.is_retriable(), "{} must not retry", e.code());
        }
    }
}
