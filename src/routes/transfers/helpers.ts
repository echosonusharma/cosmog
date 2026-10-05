import type { Transfer, TransferStatus } from "../../types";
import { t as tr } from "../../i18n";

export const STATUS_ORDER: TransferStatus[] = ["active", "pending", "failed", "paused", "done", "canceled"];

export function actionVerb(t: Transfer): string {
  const up = t.direction === "upload";
  switch (t.status) {
    case "active":   return up ? tr("transfers.status.uploading") : tr("transfers.status.downloading");
    case "pending":  return tr("transfers.status.queued");
    case "done":     return up ? tr("transfers.status.uploaded") : tr("transfers.status.downloaded");
    case "failed":   return up ? tr("transfers.status.uploadFailed") : tr("transfers.status.downloadFailed");
    case "canceled": return tr("transfers.status.canceled");
    case "paused":   return tr("transfers.status.paused");
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
