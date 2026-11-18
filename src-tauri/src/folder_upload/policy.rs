//! Error class of a failed item, from its wire code.

/// How the engine reacts to one failed item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Back to pending (kept upload), counted only without progress after the network returned.
    Network,
    /// Back off 30 s doubling (cap 30 min), 5 attempts.
    Throttle,
    /// Every later item would fail too: pause the job with the code.
    Pause,
    /// 3 attempts, 30 s apart (locks can be transient).
    Io,
    /// 412 on If-None-Match: the key appeared after the check.
    Conflict,
    /// Re-queue at once without counting (row reaped by a restart).
    Requeue,
    Fatal,
}

pub fn classify(code: &str) -> Class {
    match code {
        "network_unreachable" | "dns_failure" | "connection_refused" | "request_timeout" => Class::Network,
        "rate_limited" | "service_unavailable" | "s3" => Class::Throttle,
        "access_denied" | "credentials_invalid" | "credentials_missing" | "access_key_invalid" | "token_expired"
        | "clock_skew" | "quota_exceeded" | "bucket_not_found" | "invalid_region" | "tls_error"
        | "folder_source_unavailable" => Class::Pause,
        "io" => Class::Io,
        "conflict" => Class::Conflict,
        "interrupted" => Class::Requeue,
        _ => Class::Fatal,
    }
}

/// Wire JSON `{code, detail}` to its code; plain text maps to "internal".
pub fn wire_code(wire: &str) -> String {
    serde_json::from_str::<serde_json::Value>(wire)
        .ok()
        .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_string))
        .unwrap_or_else(|| "internal".into())
}

pub const THROTTLE_MAX_ATTEMPTS: i64 = 5;
pub const IO_MAX_ATTEMPTS: i64 = 3;
/// Network failures counted only when online again and the transfer made no progress.
pub const NET_MAX_ATTEMPTS: i64 = 5;

/// Seconds until the next try after `attempts` counted failures.
pub fn throttle_delay(base: i64, cap: i64, attempts: i64) -> i64 {
    (base << attempts.clamp(0, 20)).min(cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_follow_the_design_table() {
        assert_eq!(classify("dns_failure"), Class::Network);
        assert_eq!(classify("rate_limited"), Class::Throttle);
        assert_eq!(classify("token_expired"), Class::Pause);
        // A certificate problem is not transient: pause, never "waiting for network".
        assert_eq!(classify("tls_error"), Class::Pause);
        assert_eq!(classify("request_timeout"), Class::Network);
        assert_eq!(classify("entity_too_large"), Class::Fatal);
        assert_eq!(classify("source_changed"), Class::Fatal);
        assert_eq!(wire_code(r#"{"code":"io","detail":"x"}"#), "io");
        assert_eq!(wire_code("plain"), "internal");
        assert_eq!(throttle_delay(30, 1800, 0), 30);
        assert_eq!(throttle_delay(30, 1800, 3), 240);
        assert_eq!(throttle_delay(30, 1800, 9), 1800);
    }
}
