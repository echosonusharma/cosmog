import type ExcelJS from "exceljs";
import { mkdir, exists, BaseDirectory } from "@tauri-apps/plugin-fs";
import { appCacheDir, join } from "@tauri-apps/api/path";
import { invoke } from "@tauri-apps/api/core";

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

/** Filename-safe timestamp for suffixes: `2026-07-18_14-32-05` (no colons/slashes). */
export function humanTimestamp(d: Date = new Date()): string {
  const p = (n: number) => n.toString().padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}_${p(d.getHours())}-${p(d.getMinutes())}-${p(d.getSeconds())}`;
}

/** Insert a timestamp before the extension: `notes.txt` → `notes-<ts>.txt`.
 *  Only the final dot counts as the ext, so multi-dot names stay intact. */
export function withTimestamp(name: string, when: Date = new Date()): string {
  const ts = humanTimestamp(when);
  const dot = name.lastIndexOf(".");
  if (dot <= 0 || dot === name.length - 1) return `${name}-${ts}`;
  return `${name.slice(0, dot)}-${ts}${name.slice(dot)}`;
}

/** Extract display filename from a path or Android content:// / file:// URI:
 *  decode, strip up to the last colon (SAF <treeId>:<path>), take last "/" segment.
 *  Falls back to `fallback` for opaque doc ids. */
export function displayNameFromUri(pathOrUri: string, fallback = "file"): string {
  if (!pathOrUri) return fallback;
  const noQuery = pathOrUri.split("?")[0];
  let decoded = noQuery;
  try { decoded = decodeURIComponent(noQuery); } catch { /* keep raw */ }
  const afterColon = decoded.includes(":") ? decoded.slice(decoded.lastIndexOf(":") + 1) : decoded;
  const trimmed = afterColon.replace(/[/\\]+$/, "");
  const tail = trimmed.slice(Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\")) + 1);
  if (!tail || /^\d+$/.test(tail) || (!/[.]/.test(tail) && tail.length < 3)) {
    return fallback;
  }
  return tail;
}

/** Android SAF returns content:// URIs but Rust uploads need a real filesystem
 *  path: URI inputs are streamed into app cache by `stage_saf_upload` (chunked,
 *  constant memory); plain paths pass through with a derived basename. */
export async function resolveUploadPath(pathOrUri: string): Promise<{ path: string; name: string; stageDir: string | null }> {
  if (!pathOrUri) return { path: "", name: "", stageDir: null };
  const isUri = pathOrUri.startsWith("content://") || pathOrUri.startsWith("file://");
  if (!isUri) {
    const trimmed = pathOrUri.endsWith("/") || pathOrUri.endsWith("\\") ? pathOrUri.slice(0, -1) : pathOrUri;
    const lastSep = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
    // Desktop source is a real user file: never hand back a staging dir.
    return { path: pathOrUri, name: trimmed.slice(lastSep + 1), stageDir: null };
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
  // Rust stages into <destDir>/<uuid>/<name>; the <uuid> dir is what the
  // transfer worker reaps once the upload settles.
  const sep = Math.max(res.path.lastIndexOf("/"), res.path.lastIndexOf("\\"));
  const stageDir = sep > 0 ? res.path.slice(0, sep) : null;
  return { path: res.path, name: res.display_name || displayNameFromUri(pathOrUri, "upload"), stageDir };
}

/** Redirect URI targets to $APPCACHE/downloads/<name> and return the picked
 *  SAF URI (if any) so the caller can copy bytes back to the user's location. */
export async function resolveDownloadPath(
  pathOrUri: string,
  fallbackName?: string,
): Promise<{ path: string; safUri: string | null }> {
  if (!pathOrUri) return { path: "", safUri: null };
  const isUri = pathOrUri.startsWith("content://") || pathOrUri.startsWith("file://");
  const isMobileLike = typeof window !== "undefined" && window.matchMedia?.("(max-width: 768px)").matches;
  const isAbsolute = pathOrUri.startsWith("/") || /^[a-zA-Z]:[\\/]/.test(pathOrUri);
  if (!isUri && !isMobileLike && isAbsolute) return { path: pathOrUri, safUri: null };
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

/** Streams cache bytes into the user-picked SAF URI (saveDialog left a 0-byte
 *  placeholder there) in 1 MB chunks via ContentResolver, then removes the
 *  cache copy. */
export async function finalizeSafDownload(cachePath: string, safUri: string): Promise<void> {
  await invoke("finalize_saf_download", { cachePath, uri: safUri });
}

/** Pending SAF finalize copies per transfer id, drained by MainApp's poll loop
 *  on Done (downloads stage to $APPCACHE then copy to the picked SAF URI). */
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

/** Abandon a pending SAF download: delete the 0-byte placeholder the save
 *  dialog pre-created, else the user finds an empty file at that location. */
export function discardSafDownload(transferId: string): void {
  const pending = takeSafFinalize(transferId);
  if (pending) invoke("delete_saf_document", { uri: pending.safUri });
}

export const IMAGE_EXTS  = new Set(["jpg","jpeg","png","gif","webp","svg","bmp","ico","avif","tiff","tif"]);
export const TEXT_EXTS   = new Set(["txt","md","json","xml","yaml","yml","toml","log","sh","js","ts","tsx","jsx","css","html","htm","rs","go","py","rb","java","c","cpp","h","sql","env","ini","conf","cfg","properties","dockerfile"]);
export const SHEET_EXTS  = new Set(["xlsx","xls","xlsm","xlsb","ods","csv"]);
export const PDF_EXTS    = new Set(["pdf"]);
export const AUDIO_EXTS  = new Set(["mp3","wav","ogg","oga","m4a","aac","flac","opus","weba"]);

export function extOf(name: string) { const i = name.lastIndexOf("."); return i >= 0 ? name.slice(i + 1).toLowerCase() : ""; }

/** `.env`, `.env.local`, `.env.example`, `foo.env` — last-segment extOf alone misses these. */
export function isDotEnvName(name: string): boolean {
  const base = name.toLowerCase().split("/").pop() ?? "";
  return base === ".env" || base.startsWith(".env.") || base.endsWith(".env");
}

/** Extension passed to CodeEditor (dotenv → env mode even when basename is `.env.example`). */
export function editorExtOf(name: string): string {
  if (isDotEnvName(name)) return "env";
  const base = name.toLowerCase().split("/").pop() ?? "";
  if (base === "dockerfile" || base.endsWith(".dockerfile")) return "dockerfile";
  if (base === "nginx.conf") return "nginx";
  return extOf(name);
}

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
