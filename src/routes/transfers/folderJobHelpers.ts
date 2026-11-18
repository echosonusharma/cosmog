import { isAndroid } from "../../utils/platform";
import { isContentUri, sourceDisplayName } from "../../utils/sourceName";
import { describeError, errCode } from "../../utils/errors";
import { isKnownCode } from "../../utils/errorCodes";
import { t, type TKey } from "../../i18n";
import type { FolderJob } from "../../types";

export const jobName = (j: FolderJob) => j.display_name || sourceDisplayName(j.source_root);
/** Tooltip: full path on desktop, folder name for a SAF tree URI. */
export const jobSourceTitle = (j: FolderJob) => (isContentUri(j.source_root) ? jobName(j) : j.source_root);

/** Android: the folder grant is gone. A job resumes via Resume + re-pick; a new upload just re-picks. */
export const sourceGoneHint = (raw: unknown, forJob = false): string =>
  isAndroid && errCode(raw) === "folder_source_unavailable"
    ? t(forJob ? "folderUpload.android.pickAgain" : "folderUpload.android.pickFolderAgain")
    : "";
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
  return { title: t("folderUpload.job.pause.error", { reason: d.title }), desc: sourceGoneHint({ code, detail: "" }, true) || d.desc };
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

// Speed is averaged over this window; ETA shows once a few seconds of samples exist.
const RATE_WINDOW_MS = 10_000;
const RATE_MIN_MS = 2_000;

/** Upload speed (bytes/s) and seconds left for a job, from its bytes_done samples. */
export function createJobRate(job: () => FolderJob) {
  const samples: { t: number; b: number }[] = [];
  let jobId = "";
  const sample = () => {
    const j = job();
    const now = Date.now();
    // A bar row can be handed another job when the list shifts.
    if (j.id !== jobId) { jobId = j.id; samples.length = 0; }
    const last = samples[samples.length - 1];
    // Paused or restarted (bytes went down): start over.
    if (j.state !== "running" || (last && j.bytes_done < last.b)) samples.length = 0;
    else if (!last || last.b !== j.bytes_done || now - last.t >= 1000) samples.push({ t: now, b: j.bytes_done });
    while (samples.length > 2 && now - samples[0].t > RATE_WINDOW_MS) samples.shift();
  };
  const speed = (): number | null => {
    sample();
    if (samples.length < 2) return null;
    const a = samples[0], b = samples[samples.length - 1];
    const dt = b.t - a.t;
    return dt >= RATE_MIN_MS ? ((b.b - a.b) * 1000) / dt : null;
  };
  const etaSec = (): number | null => {
    const s = speed();
    const left = job().total_bytes - job().bytes_done;
    return s && s > 0 && left > 0 ? left / s : null;
  };
  return { speed, etaSec };
}
