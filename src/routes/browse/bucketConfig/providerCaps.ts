import { getProviderById } from "../../../providers";
import type { Support } from "../../../providers";
import type { BucketConfigTab } from "./errors";

// Capability hints live in providers.json (`caps` per provider). This is only a
// preemptive hint. The backend's `Unsupported` error remains the source of
// truth, so a stale/missing entry never blocks a genuinely-supported op.
// Missing provider or missing tab entry falls through to "unknown".
function capFor(providerId: string, tab: BucketConfigTab): Support {
  return getProviderById(providerId)?.caps?.[tab] ?? "unknown";
}

/**
 * Documentation URL for a config type. Sourced from providers.json: the
 * provider's own `docs` entry when present, else the canonical AWS S3 docs
 * (the concepts carry across S3-compatibles).
 */
export function docUrl(providerId: string, tab: BucketConfigTab): string {
  const own = getProviderById(providerId)?.docs?.[tab];
  if (own) return own;
  return getProviderById("aws")!.docs![tab]!;
}

const TAB_LABEL: Record<BucketConfigTab, string> = {
  policy: "Bucket policy",
  cors: "CORS configuration",
  versioning: "Versioning",
};

/** Warning text for a non-supported/unknown capability, or "" when supported. */
export function capWarning(providerId: string, providerLabel: string, tab: BucketConfigTab): string {
  const s = capFor(providerId, tab);
  if (s === "yes") return "";
  const feature = TAB_LABEL[tab];
  const who = providerLabel || "this provider";
  if (s === "no") {
    return `${feature} is not supported by ${who}. Saving will likely fail.`;
  }
  return `${feature} support is unverified for ${who}. It may not work.`;
}
