import ExcelJS from "exceljs";
import { readFile, writeFile, mkdir, exists, BaseDirectory } from "@tauri-apps/plugin-fs";
import { appCacheDir, join } from "@tauri-apps/api/path";

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

/** Android SAF returns content:// URIs. Rust upload path requires absolute
 *  filesystem path — copy the URI into app cache dir and return that path.
 *  Non-URI paths pass through unchanged. */
export async function resolveUploadPath(pathOrUri: string): Promise<string> {
  if (!pathOrUri) return "";
  if (!pathOrUri.startsWith("content://") && !pathOrUri.startsWith("file://")) return pathOrUri;

  const nameRaw = pathOrUri.split("/").pop() || "upload";
  const name = (() => { try { return decodeURIComponent(nameRaw); } catch { return nameRaw; } })();
  const uploadsRel = "uploads";
  if (!(await exists(uploadsRel, { baseDir: BaseDirectory.AppCache }))) {
    await mkdir(uploadsRel, { baseDir: BaseDirectory.AppCache, recursive: true });
  }
  const rel = `${uploadsRel}/${Date.now()}-${name}`;
  const bytes = await readFile(pathOrUri);
  await writeFile(rel, bytes, { baseDir: BaseDirectory.AppCache });
  const cache = await appCacheDir();
  return await join(cache, rel);
}

/** Android SAF returns content:// URIs. Rust download requires absolute
 *  filesystem path — redirect any URI target to $APPCACHE/downloads/<name>. */
export async function resolveDownloadPath(pathOrUri: string, fallbackName?: string): Promise<string> {
  if (!pathOrUri) return "";
  const isUri = pathOrUri.startsWith("content://") || pathOrUri.startsWith("file://");
  const isMobileLike = typeof window !== "undefined" && window.matchMedia?.("(max-width: 768px)").matches;
  const isAbsolute = pathOrUri.startsWith("/") || /^[a-zA-Z]:[\\/]/.test(pathOrUri);
  // desktop: absolute filesystem path — pass through unchanged.
  if (!isUri && !isMobileLike && isAbsolute) return pathOrUri;
  // desktop with non-absolute path — return as-is; Rust will surface the error.
  if (!isUri && !isMobileLike) return pathOrUri;

  const tailRaw = pathOrUri.split("/").pop() || fallbackName || "download";
  const name = (() => { try { return decodeURIComponent(tailRaw); } catch { return tailRaw; } })();
  const downloadsRel = "downloads";
  if (!(await exists(downloadsRel, { baseDir: BaseDirectory.AppCache }))) {
    await mkdir(downloadsRel, { baseDir: BaseDirectory.AppCache, recursive: true });
  }
  const cache = await appCacheDir();
  return await join(cache, downloadsRel, name);
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
