import type { ProviderDef } from "../providers";

/** Derive the signing region from a custom endpoint URL (B2/Wasabi/Spaces embed it in the
 *  subdomain); falls back to the provider's JSON default. */
export function regionFromEndpoint(provider: ProviderDef, endpoint: string): string | undefined {
  if (!provider.custom_endpoint) return undefined;
  if (provider.id === "r2") return "auto";

  if (endpoint) {
    try {
      const host = new URL(endpoint).hostname;

      const b2 = host.match(/^s3\.([^.]+)\.backblazeb2\.com$/);
      if (b2) return b2[1];

      const wasabi = host.match(/^s3\.([^.]+)\.wasabisys\.com$/);
      if (wasabi) return wasabi[1];

      const doSpaces = host.match(/^([^.]+)\.digitaloceanspaces\.com$/);
      if (doSpaces) return doSpaces[1];
    } catch {}
  }

  return provider.region || undefined;
}
