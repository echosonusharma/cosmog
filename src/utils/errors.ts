/** Centralized error parsing and formatting for Tauri IPC errors. */

const PREFIXES: Record<string, string> = {
  not_found:           "not found: ",
  invalid_input:       "invalid input: ",
  database:            "database error: ",
  keyring:             "keyring error: ",
  s3:                  "s3 error: ",
  access_denied:       "access denied: ",
  credentials_invalid: "credentials invalid: ",
  conflict:            "conflict: ",
  rate_limited:        "rate limited: ",
  io:                  "io error: ",
  canceled:            "canceled: ",
  internal:            "internal: ",
};

export interface WireError { code: string; message: string }

/**
 * Parse a Tauri IPC rejection into a structured WireError.
 * Handles up to 2 levels of JSON encoding (Linux/WebKitGTK may double-encode).
 * Strips the Rust Display prefix (e.g. "not found: ") and capitalises.
 */
export function parseWireError(raw: unknown): WireError {
  let obj: unknown = raw;
  for (let i = 0; i < 2 && typeof obj === "string"; i++) {
    try { obj = JSON.parse(obj); } catch { break; }
  }
  const code    = (obj as any)?.code    ?? "";
  let   message = (obj as any)?.message ?? (typeof raw === "string" ? raw : String(raw ?? ""));
  const pfx = PREFIXES[code];
  if (pfx && message.startsWith(pfx)) message = message.slice(pfx.length);
  if (message) message = message.charAt(0).toUpperCase() + message.slice(1);
  return { code, message: message || "An unexpected error occurred" };
}

/** True when the error code indicates a credentials/keychain problem. */
export function isCredentialError(code: string): boolean {
  return code === "not_found" || code === "credentials_invalid" || code === "keyring";
}

/** Extract a user-facing string from any Tauri IPC rejection. */
export function errMsg(raw: unknown): string {
  if (raw == null) return "An unexpected error occurred";
  if (raw instanceof Error) return raw.message;
  return parseWireError(raw).message;
}
