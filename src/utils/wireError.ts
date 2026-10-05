import { isKnownCode } from "./errorCodes.ts";

// The backend sends only a stable `code` plus raw technical `detail`.
export interface WireError { code: string; detail: string }

const NAMED_PREFIX: Record<string, string> = {
  database: "database error",
  keyring: "keyring error",
  s3: "s3 error",
  io: "io error",
};

// Rows stored before the code-based format hold the old Display text ("access denied: ...").
function legacyCode(text: string): { code: string; detail: string } | null {
  const m = /^([a-z0-9 ]+): ([\s\S]*)$/.exec(text);
  if (!m) return null;
  for (const [named, prefix] of Object.entries(NAMED_PREFIX)) if (m[1] === prefix) return { code: named, detail: m[2] };
  const code = m[1].replace(/ /g, "_");
  return isKnownCode(code) ? { code, detail: m[2] } : null;
}

/** Parse a Tauri IPC rejection into a WireError; handles up to 2 levels of JSON encoding
 *  (Linux/WebKitGTK may double-encode). */
export function parseWireError(raw: unknown): WireError {
  // WebView2 (Windows) wraps Tauri IPC rejections as JS Error objects whose
  // .message holds the serialized backend error JSON. Unwrap before parsing.
  let obj: unknown = raw instanceof Error ? raw.message : raw;
  for (let i = 0; i < 2 && typeof obj === "string"; i++) {
    try { obj = JSON.parse(obj); } catch { break; }
  }
  const parsed = typeof obj === "object" && obj !== null ? (obj as Record<string, unknown>) : {};
  const code = typeof parsed.code === "string" ? parsed.code : "";
  const fallback = raw instanceof Error ? raw.message : typeof raw === "string" ? raw : String(raw ?? "");
  const detail = typeof parsed.detail === "string" ? parsed.detail
    : typeof parsed.message === "string" ? parsed.message
    : fallback;
  if (code) return { code, detail };
  return legacyCode(detail) ?? { code: "", detail };
}

const CREDENTIAL_CODES = new Set(["credentials_missing", "credentials_invalid", "access_key_invalid", "token_expired", "keyring"]);
const NETWORK_CODES = new Set(["network_unreachable", "dns_failure", "connection_refused", "tls_error", "request_timeout"]);

export const isCredentialError = (code: string) => CREDENTIAL_CODES.has(code);
export const isNetworkError = (code: string) => NETWORK_CODES.has(code);
export const isNotFound = (code: string) =>
  code === "not_found" || code === "bucket_not_found" || code === "object_not_found" || code === "upload_not_found";
