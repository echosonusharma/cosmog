import { t, type TKey } from "../i18n";
import { isKnownCode } from "./errorCodes";
import { parseWireError, type WireError } from "./wireError";

export { parseWireError, isCredentialError, needsSecretReentry, isNetworkError, isNotFound, type WireError } from "./wireError";

// All user-facing wording (title, description) lives in the i18n `errors.*` strings, keyed by code.
export interface DescribedError extends WireError {
  title: string;
  desc: string;
  /** Description plus the code, for inline messages. */
  text: string;
}

// Generic codes say little on their own, so the raw reason is appended to inline messages.
const SHOW_DETAIL = new Set(["invalid_input", "s3", "internal", "conflict", "io", "database", "keyring", "not_found"]);

export function describeError(raw: unknown): DescribedError {
  const { code, detail } = raw == null ? { code: "", detail: "" } : parseWireError(raw);
  if (!isKnownCode(code)) {
    const desc = detail || t("errors.unknown.desc");
    return { code, detail, title: t("common.error"), desc, text: desc };
  }
  const title = t(`errors.${code}.title` as TKey);
  const desc = t(`errors.${code}.desc` as TKey);
  const extra = SHOW_DETAIL.has(code) && detail ? `: ${detail}` : "";
  return { code, detail, title, desc, text: `${desc} (${code})${extra}` };
}

export const errMsg = (raw: unknown): string => describeError(raw).text;
export const errTitle = (raw: unknown): string => describeError(raw).title;
export const errCode = (raw: unknown): string => (raw == null ? "" : parseWireError(raw).code);
