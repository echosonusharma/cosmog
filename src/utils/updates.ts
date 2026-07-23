const RELEASES_API = "https://api.github.com/repos/echosonusharma/cosmog/releases/latest";
export const RELEASES_PAGE = "https://github.com/echosonusharma/cosmog/releases/latest";

export type UpdateInfo = { version: string; changelog: string };

function parseVer(v: string): number[] {
  return v.replace(/^v/, "").split(".").map(Number);
}

function isNewer(remote: string, current: string): boolean {
  const r = parseVer(remote);
  const c = parseVer(current);
  for (let i = 0; i < Math.max(r.length, c.length); i++) {
    const rv = r[i] ?? 0;
    const cv = c[i] ?? 0;
    if (rv > cv) return true;
    if (rv < cv) return false;
  }
  return false;
}

// Cached at module level — both Titlebar (desktop) and MobileHeader (mobile)
// read from the same promise so only one fetch happens per session.
let _cached: Promise<UpdateInfo | null> | null = null;

export function checkLatestVersion(currentVersion: string): Promise<UpdateInfo | null> {
  if (!_cached) {
    _cached = (async () => {
      try {
        const res = await fetch(RELEASES_API, { headers: { Accept: "application/vnd.github+json" } });
        if (!res.ok) return null;
        const data = await res.json();
        const tag: string = data.tag_name ?? "";
        if (!isNewer(tag, currentVersion)) return null;
        return { version: tag.replace(/^v/, ""), changelog: data.body ?? "" };
      } catch {
        return null;
      }
    })();
  }
  return _cached;
}
