//! Maps AWS SDK failures to [`AppError`] codes. The FE owns the wording per code, so the goal
//! here is the most specific stable code, with the raw SDK text kept as detail.

use aws_smithy_runtime_api::client::result::SdkError;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;

use crate::error::AppError;

type HttpResponse = aws_smithy_runtime_api::client::orchestrator::HttpResponse;

/// Flatten an error's `source()` chain into one string; the SDK hides the real
/// cause (DNS, refused, TLS) behind a generic outer Display.
fn error_chain<E: std::error::Error>(e: &E) -> String {
    let mut parts = vec![e.to_string()];
    let mut src = e.source();
    while let Some(s) = src {
        let msg = s.to_string();
        if !parts.contains(&msg) {
            parts.push(msg);
        }
        src = s.source();
    }
    parts.join(": ")
}

/// Map an AWS SDK error to the most specific [`AppError`]; unknown codes fall back to
/// [`AppError::S3`].
pub(super) fn classify_aws<E>(ctx: &str, err: SdkError<E, HttpResponse>) -> AppError
where
    E: ProvideErrorMetadata + std::fmt::Display + std::fmt::Debug,
{
    // Transport failure before any HTTP response; the outer Display collapses to
    // "dispatch failure", so classify from the source chain's real cause.
    if let SdkError::DispatchFailure(ref df) = err {
        if df.is_timeout() {
            return AppError::RequestTimeout(format!("{ctx}: connection timed out"));
        }
        let chain = df.as_connector_error().map(error_chain).unwrap_or_else(|| err.to_string());
        return from_transport(ctx, chain);
    }
    if let SdkError::TimeoutError(_) = err {
        return AppError::RequestTimeout(format!("{ctx}: request timed out"));
    }

    let status = match &err {
        SdkError::ServiceError(se) => Some(se.raw().status().as_u16()),
        _ => None,
    };
    let (code, msg) = match err.as_service_error() {
        Some(se) => (se.code().unwrap_or_default(), se.message().unwrap_or_default()),
        None => ("", ""),
    };
    let display = if code.is_empty() && msg.is_empty() {
        format!("{ctx}: {err}")
    } else {
        format!("{ctx}: {code} {msg}").trim().to_string()
    };
    from_service(code, status, display)
}

/// Classify a connection-level failure from its flattened cause text. Only `chain` is matched,
/// so words in `ctx` cannot skew the result.
pub(super) fn from_transport(ctx: &str, chain: String) -> AppError {
    let l = chain.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| l.contains(n));
    let text = format!("{ctx}: {chain}");
    if has(&["dns error", "failed to lookup", "name or service not known", "no such host", "nodename nor servname", "failed to resolve", "could not resolve"]) {
        AppError::DnsFailure(text)
    } else if has(&["refused"]) {
        AppError::ConnectionRefused(text)
    } else if has(&["timed out", "timeout"]) {
        // Before TLS: "tls handshake timed out" is a flaky link, not a cert problem.
        AppError::RequestTimeout(text)
    } else if has(&["certificate", "unknownissuer", "unknown issuer", "self signed", "self-signed", "x509"]) {
        // Only real cert failures are terminal; a dropped handshake falls through and retries.
        AppError::TlsError(text)
    } else {
        AppError::NetworkUnreachable(text)
    }
}

/// Classify by S3 error code first, then by HTTP status for providers that send no code.
pub(super) fn from_service(code: &str, status: Option<u16>, d: String) -> AppError {
    match code {
        "NoSuchBucket" => AppError::BucketNotFound(d),
        "NoSuchKey" | "NoSuchVersion" => AppError::ObjectNotFound(d),
        "NoSuchUpload" => AppError::UploadNotFound(d),
        "NotFound" | "404" => AppError::NotFound(d),
        "SignatureDoesNotMatch" => AppError::CredentialsInvalid(d),
        "InvalidAccessKeyId" => AppError::AccessKeyInvalid(d),
        "ExpiredToken" | "ExpiredTokenException" | "InvalidToken" | "TokenRefreshRequired" => AppError::TokenExpired(d),
        "RequestTimeTooSkewed" | "RequestExpired" => AppError::ClockSkew(d),
        "AccessDenied" | "AllAccessDisabled" | "UnauthorizedAccess" | "AccountProblem" | "Forbidden" | "403" => {
            AppError::AccessDenied(d)
        }
        "BucketAlreadyExists" | "BucketAlreadyOwnedByYou" => AppError::BucketExists(d),
        "BucketNotEmpty" => AppError::BucketNotEmpty(d),
        "PreconditionFailed" | "OperationAborted" | "ConditionalRequestConflict" | "Conflict" | "409" => {
            AppError::Conflict(d)
        }
        "InvalidBucketName" => AppError::InvalidBucketName(d),
        "AuthorizationHeaderMalformed" | "InvalidLocationConstraint" | "IllegalLocationConstraintException" => {
            AppError::InvalidRegion(d)
        }
        "QuotaExceeded" | "TooManyBuckets" | "StorageLimitExceeded" | "MaxBucketsExceeded" => AppError::QuotaExceeded(d),
        "EntityTooLarge" | "MaxMessageLengthExceeded" => AppError::EntityTooLarge(d),
        "SlowDown" | "TooManyRequests" | "Throttling" | "ThrottlingException" | "RequestLimitExceeded" | "429" => {
            AppError::RateLimited(d)
        }
        "ServiceUnavailable" | "InternalError" | "InternalFailure" | "500" | "502" | "503" | "504" => {
            AppError::ServiceUnavailable(d)
        }
        "RequestTimeout" | "RequestTimeoutException" => AppError::RequestTimeout(d),
        "PermanentRedirect" => AppError::RegionRedirect(d),
        // Op unimplemented on this provider (common on B2/R2).
        "NotImplemented" | "XNotImplemented" | "UnsupportedOperation" | "NotSupported" | "501" => {
            AppError::Unsupported(d)
        }
        // Cold/archive storage class blocks reads until restored.
        "InvalidObjectState" => AppError::Archived(d),
        _ => from_status(status, d),
    }
}

// Some providers emit only an HTTP status, or a code we do not know. HEAD carries no XML body, so
// a cross-region 301 is only visible here.
fn from_status(status: Option<u16>, d: String) -> AppError {
    match status {
        Some(301) => AppError::RegionRedirect(d),
        Some(403) => AppError::AccessDenied(d),
        Some(404) => AppError::NotFound(d),
        Some(409) => AppError::Conflict(d),
        Some(413) => AppError::EntityTooLarge(d),
        Some(429) => AppError::RateLimited(d),
        Some(500 | 502 | 503 | 504) => AppError::ServiceUnavailable(d),
        Some(501) => AppError::Unsupported(d),
        _ => AppError::S3(d),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(c: &str, status: Option<u16>) -> &'static str {
        from_service(c, status, "x".into()).code()
    }

    #[test]
    fn s3_codes_map_to_specific_wire_codes() {
        assert_eq!(code("NoSuchBucket", Some(404)), "bucket_not_found");
        assert_eq!(code("NoSuchKey", Some(404)), "object_not_found");
        assert_eq!(code("NoSuchUpload", Some(404)), "upload_not_found");
        assert_eq!(code("SignatureDoesNotMatch", Some(403)), "credentials_invalid");
        assert_eq!(code("InvalidAccessKeyId", Some(403)), "access_key_invalid");
        assert_eq!(code("ExpiredToken", Some(400)), "token_expired");
        assert_eq!(code("AccessDenied", Some(403)), "access_denied");
        assert_eq!(code("BucketAlreadyOwnedByYou", Some(409)), "bucket_exists");
        assert_eq!(code("BucketNotEmpty", Some(409)), "bucket_not_empty");
        assert_eq!(code("SlowDown", Some(503)), "rate_limited");
        assert_eq!(code("InvalidObjectState", Some(403)), "archived");
        assert_eq!(code("PermanentRedirect", Some(301)), "region_redirect");
    }

    #[test]
    fn clock_skew_is_not_reported_as_rate_limited() {
        assert_eq!(code("RequestTimeTooSkewed", Some(403)), "clock_skew");
        assert_eq!(code("RequestExpired", Some(400)), "clock_skew");
    }

    #[test]
    fn slowdown_wins_over_its_503_status() {
        assert_eq!(code("SlowDown", Some(503)), "rate_limited");
        assert_eq!(code("InternalError", Some(500)), "service_unavailable");
    }

    #[test]
    fn unknown_code_falls_back_to_http_status() {
        assert_eq!(code("", Some(403)), "access_denied");
        assert_eq!(code("WeirdProviderThing", Some(404)), "not_found");
        assert_eq!(code("", Some(429)), "rate_limited");
        assert_eq!(code("", Some(503)), "service_unavailable");
        assert_eq!(code("", Some(501)), "unsupported");
        // 405 is S3's answer for GET on a delete marker, not a provider limit.
        assert_eq!(code("MethodNotAllowed", Some(405)), "s3");
        assert_eq!(code("XNotImplemented", Some(400)), "unsupported");
        assert_eq!(code("", Some(301)), "region_redirect");
        assert_eq!(code("WeirdProviderThing", Some(400)), "s3");
        assert_eq!(code("", None), "s3");
    }

    #[test]
    fn classification_keeps_raw_text_as_detail() {
        let e = from_service("AccessDenied", Some(403), "get_object: AccessDenied nope".into());
        assert_eq!(e.detail(), "get_object: AccessDenied nope");
    }

    #[test]
    fn transport_failures_are_told_apart() {
        let c = |s: &str| from_transport("put_object", s.into()).code();
        assert_eq!(c("dns error: failed to lookup address information: Name or service not known"), "dns_failure");
        assert_eq!(c("tcp connect error: Connection refused (os error 111)"), "connection_refused");
        assert_eq!(c("invalid peer certificate: UnknownIssuer"), "tls_error");
        assert_eq!(c("operation timed out"), "request_timeout");
        assert_eq!(c("network is unreachable"), "network_unreachable");
    }

    #[test]
    fn flaky_tls_handshakes_stay_retriable() {
        let e = |s: &str| from_transport("put_object", s.into());
        assert_eq!(e("tls handshake timed out").code(), "request_timeout");
        assert!(e("TLS handshake eof").is_retriable());
        assert!(!e("invalid peer certificate: UnknownIssuer").is_retriable());
    }

    #[test]
    fn resolve_in_non_dns_text_is_not_dns_failure() {
        assert_ne!(from_transport("x", "unable to resolve certificate chain".into()).code(), "dns_failure");
        assert_eq!(from_transport("resolve_ctx", "connection reset".into()).code(), "network_unreachable");
    }
}
