// Mirrors AppError::CODES in src-tauri/src/error.rs; tests keep both in sync with the i18n strings.
export const ERROR_CODES = [
  "not_found",
  "invalid_input",
  "database",
  "keyring",
  "s3",
  "access_denied",
  "credentials_invalid",
  "credentials_missing",
  "access_key_invalid",
  "token_expired",
  "clock_skew",
  "conflict",
  "bucket_exists",
  "bucket_not_empty",
  "bucket_not_found",
  "object_not_found",
  "upload_not_found",
  "invalid_bucket_name",
  "invalid_region",
  "quota_exceeded",
  "entity_too_large",
  "rate_limited",
  "service_unavailable",
  "request_timeout",
  "canceled",
  "region_redirect",
  "network_unreachable",
  "dns_failure",
  "connection_refused",
  "tls_error",
  "unsupported",
  "encryption_identity_missing",
  "decryption_failed",
  "archived",
  "io",
  "internal",
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

const KNOWN = new Set<string>(ERROR_CODES);
export const isKnownCode = (c: string): c is ErrorCode => KNOWN.has(c);
