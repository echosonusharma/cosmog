import { getProviderById } from "../../../providers";
import type { Support } from "../../../providers";
import type { BucketConfigTab } from "./errors";

// Preemptive hint from providers.json `caps`; the backend's `Unsupported` error
// stays source of truth, so a stale/missing entry never blocks a supported op.
function capFor(providerId: string, tab: BucketConfigTab): Support {
  return getProviderById(providerId)?.caps?.[tab] ?? "unknown";
}

/** Doc URL for a config tab: provider's own `docs` entry, else canonical AWS
 *  S3 docs (concepts carry across S3-compatibles). */
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
