import ExcelJS from "exceljs";
import { mkdir, exists, BaseDirectory } from "@tauri-apps/plugin-fs";
import { appCacheDir, join } from "@tauri-apps/api/path";
import { invoke } from "@tauri-apps/api/core";

// ── helpers ───────────────────────────────────────────────────────────────────

function hasStringPath(x: unknown): x is { path: string } {
  return typeof x === "object" && x !== null && "path" in x && typeof (x as { path: unknown }).path === "string";
}

export function pathFromDialog(sel: unknown): string {
  if (!sel) return "";
  let s = typeof sel === "string" ? sel : hasStringPath(sel) ? sel.path : "";
  // Tauri on Linux/Wayland may return file:// URIs — unwrap to a plain path.
  if (s.startsWith("file://")) s = decodeURIComponent(s.replace(/^file:\/\//, ""));
  return s;
}

/** Human-readable local timestamp for filename suffixes: `2026-07-18_14-32-05`.
 *  ISO-ish but with `_` between date and time and `-` in the time component so
 *  the whole thing is filename-safe on every filesystem. */
export function humanTimestamp(d: Date = new Date()): string {
  const p = (n: number) => n.toString().padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}_${p(d.getHours())}-${p(d.getMinutes())}-${p(d.getSeconds())}`;
}

/** Insert a human timestamp between a filename's stem and extension so the
 *  extension stays intact: `notes.txt` → `notes-2026-07-18_14-32-05.txt`.
 *  Handles multi-dot names by keeping only the final segment as the ext. */
export function withTimestamp(name: string, when: Date = new Date()): string {
  const ts = humanTimestamp(when);
  const dot = name.lastIndexOf(".");
  if (dot <= 0 || dot === name.length - 1) return `${name}-${ts}`;
  return `${name.slice(0, dot)}-${ts}${name.slice(dot)}`;
}

/** Extract display filename from a path or Android content:// / file:// URI.
 *  SAF URIs look like:
 *    content://com.android.externalstorage.documents/document/primary%3ADownload%2Fnotes.txt
 *    content://com.android.providers.downloads.documents/document/msf%3A1234
 *  Decode, strip everything up to the last colon (SAF <treeId>:<path> form),
 *  then take last "/" segment. Falls back to a synthetic name for opaque doc ids. */
export function displayNameFromUri(pathOrUri: string, fallback = "file"): string {
  if (!pathOrUri) return fallback;
  const noQuery = pathOrUri.split("?")[0];
  let decoded = noQuery;
  try { decoded = decodeURIComponent(noQuery); } catch { /* keep raw */ }
  const afterColon = decoded.includes(":") ? decoded.slice(decoded.lastIndexOf(":") + 1) : decoded;
  const trimmed = afterColon.replace(/[/\\]+$/, "");
  const tail = trimmed.slice(Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\")) + 1);
  // opaque doc id like "1234" or "msf" — no clean name available; hand back
  // the fallback verbatim so S3 keys and UI labels stay stable across retries.
  if (!tail || /^\d+$/.test(tail) || (!/[.]/.test(tail) && tail.length < 3)) {
    return fallback;
  }
  return tail;
}

/** Android SAF returns content:// URIs. Rust upload path requires absolute
 *  filesystem path. For URI inputs, ask the Rust `stage_saf_upload` command
 *  to stream the URI into app cache (constant memory, chunked JNI copy) and
 *  return { path, name } where `name` comes from ContentResolver's
 *  OpenableColumns.DISPLAY_NAME. Non-URI paths pass through with derived
 *  basename. */
export async function resolveUploadPath(pathOrUri: string): Promise<{ path: string; name: string }> {
  if (!pathOrUri) return { path: "", name: "" };
  const isUri = pathOrUri.startsWith("content://") || pathOrUri.startsWith("file://");
  if (!isUri) {
    const trimmed = pathOrUri.endsWith("/") || pathOrUri.endsWith("\\") ? pathOrUri.slice(0, -1) : pathOrUri;
    const lastSep = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
    return { path: pathOrUri, name: trimmed.slice(lastSep + 1) };
  }

  const uploadsRel = "uploads";
  if (!(await exists(uploadsRel, { baseDir: BaseDirectory.AppCache }))) {
    await mkdir(uploadsRel, { baseDir: BaseDirectory.AppCache, recursive: true });
  }
  const cache = await appCacheDir();
  const destDir = await join(cache, uploadsRel);
  const res = await invoke<{ path: string; display_name: string; bytes: number }>(
    "stage_saf_upload",
    { uri: pathOrUri, destDir },
  );
  return { path: res.path, name: res.display_name || displayNameFromUri(pathOrUri, "upload") };
}

/** Android SAF returns content:// URIs. Rust download requires absolute
 *  filesystem path. Redirect any URI target to $APPCACHE/downloads/<name>
 *  and return the picked SAF URI (if any) so the caller can copy bytes
 *  back to the user-facing location after download completes. */
export async function resolveDownloadPath(
  pathOrUri: string,
  fallbackName?: string,
): Promise<{ path: string; safUri: string | null }> {
  if (!pathOrUri) return { path: "", safUri: null };
  const isUri = pathOrUri.startsWith("content://") || pathOrUri.startsWith("file://");
  const isMobileLike = typeof window !== "undefined" && window.matchMedia?.("(max-width: 768px)").matches;
  const isAbsolute = pathOrUri.startsWith("/") || /^[a-zA-Z]:[\\/]/.test(pathOrUri);
  // desktop: absolute filesystem path — pass through unchanged.
  if (!isUri && !isMobileLike && isAbsolute) return { path: pathOrUri, safUri: null };
  // desktop with non-absolute path — return as-is; Rust will surface the error.
  if (!isUri && !isMobileLike) return { path: pathOrUri, safUri: null };

  const name = isUri
    ? displayNameFromUri(pathOrUri, fallbackName || "download")
    : (() => {
        const tail = pathOrUri.split(/[/\\]/).pop() || fallbackName || "download";
        try { return decodeURIComponent(tail); } catch { return tail; }
      })();
  const downloadsRel = "downloads";
  if (!(await exists(downloadsRel, { baseDir: BaseDirectory.AppCache }))) {
    await mkdir(downloadsRel, { baseDir: BaseDirectory.AppCache, recursive: true });
  }
  const cache = await appCacheDir();
  const path = await join(cache, downloadsRel, name);
  return { path, safUri: isUri ? pathOrUri : null };
}

/** After a download finishes on Android, the file is at the cache path we
 *  passed to Rust. saveDialog created a placeholder file at the user-picked
 *  SAF URI (0 bytes). The Rust `finalize_saf_download` command streams the
 *  cache bytes into the URI via ContentResolver.openOutputStream in 1 MB
 *  chunks so multi-GB downloads stay constant-memory, then removes the
 *  cache copy on success. */
export async function finalizeSafDownload(cachePath: string, safUri: string): Promise<void> {
  await invoke("finalize_saf_download", { cachePath, uri: safUri });
}

/** Downloads on Android are staged to $APPCACHE then copied to the SAF URI
 *  the user picked (see finalizeSafDownload). This map holds the pending
 *  copy for each transfer id; the poll loop in MainApp drains it on Done. */
const pendingSafFinalize = new Map<string, { cachePath: string; safUri: string }>();
export function registerSafFinalize(transferId: string, cachePath: string, safUri: string) {
  pendingSafFinalize.set(transferId, { cachePath, safUri });
}
export function takeSafFinalize(transferId: string): { cachePath: string; safUri: string } | null {
  const v = pendingSafFinalize.get(transferId);
  if (v) pendingSafFinalize.delete(transferId);
  return v ?? null;
}
/** Retry creates a NEW transfer id; carry the pending SAF finalize over so the
 *  retried download still lands at the user's originally picked location. */
export function moveSafFinalize(oldId: string, newId: string): void {
  const v = pendingSafFinalize.get(oldId);
  if (!v) return;
  pendingSafFinalize.delete(oldId);
  pendingSafFinalize.set(newId, v);
}

/** Abandon a pending SAF download: drop the finalize entry and delete the
 *  0-byte placeholder document the save dialog pre-created at the user's
 *  chosen location. Called when a transfer is canceled or fails, otherwise
 *  the user finds an empty file where the download would have landed. */
export function discardSafDownload(transferId: string): void {
  const pending = takeSafFinalize(transferId);
  if (pending) invoke("delete_saf_document", { uri: pending.safUri });
}

export const IMAGE_EXTS  = new Set(["jpg","jpeg","png","gif","webp","svg","bmp","ico","avif","tiff","tif"]);
export const TEXT_EXTS   = new Set(["txt","md","json","xml","yaml","yml","toml","log","sh","js","ts","tsx","jsx","css","html","htm","rs","go","py","rb","java","c","cpp","h"]);
export const SHEET_EXTS  = new Set(["xlsx","xls","xlsm","xlsb","ods","csv"]);

export function extOf(name: string) { const i = name.lastIndexOf("."); return i >= 0 ? name.slice(i + 1).toLowerCase() : ""; }

export function parseCsvIntoSheet(csv: string, ws: ExcelJS.Worksheet) {
  csv.trim().split("\n").forEach((line) => {
    const cells: string[] = [];
    let cur = "", inQ = false;
    for (let i = 0; i < line.length; i++) {
      const ch = line[i];
      if (ch === '"') {
        if (inQ && line[i + 1] === '"') { cur += '"'; i++; }
        else inQ = !inQ;
      } else if (ch === "," && !inQ) { cells.push(cur); cur = ""; }
      else cur += ch;
    }
    cells.push(cur);
    ws.addRow(cells);
  });
}

export function worksheetToCsv(ws: ExcelJS.Worksheet): string {
  const lines: string[] = [];
  ws.eachRow({ includeEmpty: false }, (row) => {
    const cols: string[] = [];
    for (let c = 1; c <= (ws.actualColumnCount || 1); c++) {
      const v = String(row.getCell(c).value ?? "");
      cols.push(v.includes(",") || v.includes('"') || v.includes("\n") ? `"${v.replace(/"/g, '""')}"` : v);
    }
    lines.push(cols.join(","));
  });
  return lines.join("\n");
}
