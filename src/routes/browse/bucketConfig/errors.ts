import { parseWireError } from "../../../utils/errors";

export type BucketErrorKind = "denied" | "unsupported" | "other";

/**
 * Robustly classify a caught bucket-config error into one of three outcomes.
 *
 * The backend AppError serialization shape is being finalized in parallel, so
 * this inspects several places: the structured wire `code`, then a best-effort
 * substring match on the message. Centralized here so it is easy to tighten
 * once the exact tag/variant shape is known.
 */
export function classifyBucketError(err: unknown): BucketErrorKind {
  const wire = parseWireError(err);
  const code = wire.code.toLowerCase();

  // Structured code / tag / variant checks (whatever the envelope carries).
  if (code === "access_denied" || code === "accessdenied" || code === "denied") {
    return "denied";
  }
  if (
    code === "unsupported" ||
    code === "not_implemented" ||
    code === "notimplemented"
  ) {
    return "unsupported";
  }

  // Some envelopes may nest the tag on the raw object under a different key.
  const raw = extractString(err).toLowerCase();
  const hay = `${wire.message} ${raw}`.toLowerCase();

  if (hay.includes("accessdenied") || hay.includes("access denied") || hay.includes("access_denied")) {
    return "denied";
  }
  if (
    hay.includes("unsupported") ||
    hay.includes("notimplemented") ||
    hay.includes("not implemented") ||
    hay.includes("not supported") ||
    hay.includes("501")
  ) {
    return "unsupported";
  }

  return "other";
}

function extractString(err: unknown): string {
  if (err == null) return "";
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}

// IAM actions surfaced in "access denied" messages, per tab and operation.
export const IAM_ACTIONS = {
  policy: { get: "s3:GetBucketPolicy", put: "s3:PutBucketPolicy" },
  cors: { get: "s3:GetBucketCORS", put: "s3:PutBucketCORS" },
  versioning: { get: "s3:GetBucketVersioning", put: "s3:PutBucketVersioning" },
} as const;

export type BucketConfigTab = keyof typeof IAM_ACTIONS;

/** Human-facing message for an access-denied error naming the IAM action. */
export function deniedMessage(tab: BucketConfigTab, op: "get" | "put"): string {
  return `Access denied: credentials lack ${IAM_ACTIONS[tab][op]}`;
}
