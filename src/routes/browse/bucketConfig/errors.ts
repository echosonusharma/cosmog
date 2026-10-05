import { errCode } from "../../../utils/errors";
import { t } from "../../../i18n";

export type BucketErrorKind = "denied" | "unsupported" | "other";

/** Classify a bucket-config error by its backend code. */
export function classifyBucketError(err: unknown): BucketErrorKind {
  const code = errCode(err);
  if (code === "access_denied") return "denied";
  if (code === "unsupported") return "unsupported";
  return "other";
}

// IAM actions surfaced in "access denied" messages, per tab and operation.
export const IAM_ACTIONS = {
  policy: { get: "s3:GetBucketPolicy", put: "s3:PutBucketPolicy" },
  cors: { get: "s3:GetBucketCORS", put: "s3:PutBucketCORS" },
  versioning: { get: "s3:GetBucketVersioning", put: "s3:PutBucketVersioning" },
} as const;

export type BucketConfigTab = keyof typeof IAM_ACTIONS;

export function deniedMessage(tab: BucketConfigTab, op: "get" | "put"): string {
  return t("bucketconfig.error.denied", { action: IAM_ACTIONS[tab][op] });
}
