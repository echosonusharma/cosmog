import data from "./providers.json";

export interface ProviderDef {
  id: string;
  label: string;
  color: string;
  iconUrl: string;
  endpoint: string;
  region: string;
  addressing_style: string;
  custom_endpoint: boolean;
  endpoint_placeholder: string | null;
  monochrome_icon: boolean;
  tile_fill: boolean;
  detect: string[];
  caps?: { policy?: Support; cors?: Support; versioning?: Support };
  docs?: { policy?: string; cors?: string; versioning?: string };
}

// Whether a provider supports a given bucket-config S3 API.
//   "yes"     - known to work
//   "no"      - known NOT to be implemented (warn before the user does work)
//   "unknown" - not verified; show a soft warning, let the attempt decide
export type Support = "yes" | "no" | "unknown";

export const PROVIDERS: ProviderDef[] = data.providers as ProviderDef[];

// Onboarding picker shows everything except the generic catch-all "s3" tile.
export const PICKABLE_PROVIDERS: ProviderDef[] =
  PROVIDERS.filter((p) => p.id !== "s3");

export function getProviderById(id: string): ProviderDef | undefined {
  return PROVIDERS.find((p) => p.id === id);
}

const FALLBACK = PROVIDERS.find((p) => p.id === "s3")!;
const AWS      = PROVIDERS.find((p) => p.id === "aws")!;

export function detectProvider(acct: { endpoint?: string | null }): ProviderDef {
  const ep = (acct.endpoint ?? "").toLowerCase();
  if (!ep) return AWS;
  for (const p of PROVIDERS) {
    if (p.id === "s3" || p.id === "aws") continue;
    if (p.detect.some((s) => ep.includes(s))) return p;
  }
  if (ep.includes("amazonaws.com")) return AWS;
  return FALLBACK;
}

export function providerLabel(acct: { endpoint?: string | null }): string {
  return detectProvider(acct).label;
}
