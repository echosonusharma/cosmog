import { basename } from "../../utils/fmt";
import { describeError, errCode } from "../../utils/errors";
import { isKnownCode } from "../../utils/errorCodes";
import { t, type TKey } from "../../i18n";
import type { FolderJob } from "../../types";

export const jobName = (j: FolderJob) => basename(j.source_root) || j.source_root;
/** Uploaded files; `counts.done` excludes folder markers, so it lines up with total_files. */
export const filesDone = (j: FolderJob) => Math.max(0, j.counts.done ?? 0);

const clampPct = (n: number) => (Number.isFinite(n) ? Math.max(0, Math.min(100, n)) : 0);

// Approximate; always within 0..100.
export function jobPct(j: FolderJob): number {
  if (j.state === "completed") return 100;
  if (j.total_bytes <= 0) return j.total_files > 0 ? clampPct((filesDone(j) / j.total_files) * 100) : 0;
  return clampPct((j.bytes_done / j.total_bytes) * 100);
}

export interface JobReason { title: string; desc?: string }

/** Waiting sub-status of a running job, or why a job is paused. */
export function jobReason(j: FolderJob): JobReason | null {
  if (j.state === "running" && j.waiting) return { title: t(`folderUpload.job.waiting.${j.waiting}` as TKey) };
  if (j.state !== "paused") return null;
  const code = j.pause_reason;
  if (!code) return { title: t("folderUpload.job.pause.generic") };
  if (code === "user") return { title: t("folderUpload.job.pause.user") };
  if (!isKnownCode(code)) return { title: t("folderUpload.job.pause.generic") };
  // pause_reason is a plain code; `error` holds the wire JSON with any detail.
  const d = describeError(errCode(j.error) === code ? j.error : { code, detail: "" });
  return { title: t("folderUpload.job.pause.error", { reason: d.title }), desc: d.desc };
}

export const needsEncryptionConfirm = (j: FolderJob) =>
  j.state === "paused" && j.pause_reason === "folder_encryption_changed";

// States retry_failed accepts; canceled and failed are rejected.
const RETRYABLE: FolderJob["state"][] = ["queued", "running", "paused", "completed", "completed_with_errors"];
export const canRetryFailed = (j: FolderJob) => RETRYABLE.includes(j.state) && (j.counts.failed ?? 0) > 0;

export type TransferFilter = "all" | "active" | "done" | "failed" | "canceled";

export function jobMatches(j: FolderJob, f: TransferFilter): boolean {
  switch (f) {
    case "all": return true;
    case "active": return j.state === "queued" || j.state === "running" || j.state === "paused";
    case "done": return j.state === "completed";
    case "failed": return j.state === "completed_with_errors" || j.state === "failed";
    case "canceled": return j.state === "canceled";
  }
}

export function badgeClass(j: FolderJob): string {
  switch (j.state) {
    case "running": return "active";
    case "queued": return "pending";
    case "completed": return "done";
    case "completed_with_errors": case "failed": return "failed";
    default: return j.state;
  }
}
