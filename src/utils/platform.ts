export type DesktopOs = "macos" | "windows" | "linux";

const isOs = (v: unknown): v is DesktopOs => v === "macos" || v === "windows" || v === "linux";

// Dev override to preview another OS's titlebar: localStorage "cosmog.titlebarOs" = "macos" | "windows" | "linux".
function override(): DesktopOs | null {
  try {
    const v = localStorage.getItem("cosmog.titlebarOs");
    return isOs(v) ? v : null;
  } catch {
    return null;
  }
}

function detect(): DesktopOs {
  const ua = navigator.userAgent;
  if (/Macintosh|Mac OS X/.test(ua)) return "macos";
  if (/Windows/.test(ua)) return "windows";
  return "linux";
}

export const desktopOs: DesktopOs = override() ?? detect();

// Android UA also says "Linux"; tablets are not "mobile" by width but have no window controls.
export const isAndroid = /Android/.test(navigator.userAgent);
