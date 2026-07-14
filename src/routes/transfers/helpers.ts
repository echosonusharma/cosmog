import type { Transfer, TransferStatus } from "../../types";

export const STATUS_ORDER: TransferStatus[] = ["active", "pending", "failed", "paused", "done", "canceled"];

export function actionVerb(t: Transfer): string {
  const up = t.direction === "upload";
  switch (t.status) {
    case "active":   return up ? "Uploading" : "Downloading";
    case "pending":  return "Queued";
    case "done":     return up ? "Uploaded" : "Downloaded";
    case "failed":   return up ? "Upload failed" : "Download failed";
    case "canceled": return "canceled";
    case "paused":   return "paused";
  }
}

export function pct(t: Transfer): number {
  if (!t.bytes_total || t.bytes_total <= 0) return t.status === "done" ? 100 : 0;
  return Math.min(100, (t.bytes_done / t.bytes_total) * 100);
}

export function fmtSecs(s: number): string {
  if (!isFinite(s) || s <= 0) return "-";
  if (s < 60) return `${Math.round(s)}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${Math.round(s % 60)}s`;
  return `${Math.floor(s / 3600)}h ${Math.round((s % 3600) / 60)}m`;
}

// Speed tracker — sample bytes_done over time per transfer id.
interface Sample { ts: number; done: number; }
const samples = new Map<string, Sample>();

const TERMINAL: ReadonlySet<string> = new Set(["done", "failed", "canceled"]);

export function recordAndComputeSpeed(t: Transfer): number {
  if (TERMINAL.has(t.status)) { samples.delete(t.id); return 0; }
  const now = Date.now();
  const prev = samples.get(t.id);
  samples.set(t.id, { ts: now, done: t.bytes_done });
  if (!prev) return 0;
  const elapsed = (now - prev.ts) / 1000;
  if (elapsed <= 0) return 0;
  return Math.max(0, (t.bytes_done - prev.done) / elapsed);
}

export function shortPath(t: Transfer): string {
  const slash = t.key.lastIndexOf("/");
  const dir = slash > 0 ? t.key.slice(0, slash + 1) : "";
  return dir ? `${t.bucket} › ${dir}` : t.bucket;
}
