/** Centralized error parsing and formatting for Tauri IPC errors. */

const PREFIXES: Record<string, string> = {
  not_found:            "not found: ",
  invalid_input:        "invalid input: ",
  database:             "database error: ",
  keyring:              "keyring error: ",
  s3:                   "s3 error: ",
  access_denied:        "access denied: ",
  credentials_invalid:  "credentials invalid: ",
  conflict:             "conflict: ",
  rate_limited:         "rate limited: ",
  io:                   "io error: ",
  canceled:             "canceled: ",
  region_redirect:      "region redirect: ",
  network_unreachable:  "network unreachable: ",
  internal:             "internal: ",
};

export interface WireError { code: string; message: string }

/**
 * Parse a Tauri IPC rejection into a structured WireError.
 * Handles up to 2 levels of JSON encoding (Linux/WebKitGTK may double-encode).
 * Strips the Rust Display prefix (e.g. "not found: ") and capitalises.
 */
export function parseWireError(raw: unknown): WireError {
  // WebView2 (Windows) wraps Tauri IPC rejections as JS Error objects whose
  // .message holds the serialized backend error JSON. Unwrap before parsing.
  let obj: unknown = raw instanceof Error ? raw.message : raw;
  for (let i = 0; i < 2 && typeof obj === "string"; i++) {
    try { obj = JSON.parse(obj); } catch { break; }
  }
  const parsed  = typeof obj === "object" && obj !== null ? obj as Record<string, unknown> : {};
  const code    = typeof parsed.code === "string" ? parsed.code : "";
  let   message = (typeof parsed.message === "string" ? parsed.message : null)
    ?? (raw instanceof Error ? raw.message : typeof raw === "string" ? raw : String(raw ?? ""));
  const pfx = PREFIXES[code];
  if (pfx && message.startsWith(pfx)) message = message.slice(pfx.length);
  if (message) message = message.charAt(0).toUpperCase() + message.slice(1);
  return { code, message: message || "An unexpected error occurred" };
}

/** True when the error code indicates a credentials/keychain problem. */
export function isCredentialError(code: string): boolean {
  return code === "not_found" || code === "credentials_invalid" || code === "keyring";
}

/** True when the error is a network-level failure (endpoint down, no internet). */
export function isNetworkError(code: string): boolean {
  return code === "network_unreachable";
}

/** Extract a user-facing string from any Tauri IPC rejection. */
export function errMsg(raw: unknown): string {
  if (raw == null) return "An unexpected error occurred";
  if (raw instanceof Error) return raw.message;
  return parseWireError(raw).message;
}
