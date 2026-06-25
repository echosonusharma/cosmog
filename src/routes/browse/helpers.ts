import ExcelJS from "exceljs";

// ── helpers ───────────────────────────────────────────────────────────────────

export function pathFromDialog(sel: unknown): string {
  if (!sel) return "";
  let s = typeof sel === "string" ? sel : typeof (sel as any).path === "string" ? (sel as any).path : "";
  // Tauri on Linux/Wayland may return file:// URIs — unwrap to a plain path.
  if (s.startsWith("file://")) s = decodeURIComponent(s.replace(/^file:\/\//, ""));
  return s;
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
