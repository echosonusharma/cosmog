import type { Transfer, TransferStatus } from "../../types";

export const STATUS_ORDER: TransferStatus[] = ["active", "pending", "failed", "paused", "done", "canceled"];

export function actionVerb(t: Transfer): string {
  const up = t.direction === "upload";
  switch (t.status) {
    case "active":   return up ? "Uploading" : "Downloading";
    case "pending":  return "Queued";
    case "done":     return up ? "Uploaded" : "Downloaded";
    case "failed":   return up ? "Upload failed" : "Download failed";
    case "canceled": return "Canceled";
    case "paused":   return "Paused";
  }
}

export function pct(t: Transfer): number {
  if (!t.bytes_total || t.bytes_total <= 0) return t.status === "done" ? 100 : 0;
  return Math.min(100, (t.bytes_done / t.bytes_total) * 100);
}

export function shortPath(t: Transfer): string {
  const slash = t.key.lastIndexOf("/");
  const dir = slash > 0 ? t.key.slice(0, slash + 1) : "";
  return dir ? `${t.bucket} › ${dir}` : t.bucket;
}
