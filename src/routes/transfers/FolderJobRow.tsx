import { Show } from "solid-js";
import {
  folderUploadCancel, folderUploadPause, folderUploadRemove, folderUploadResume, folderUploadRetryFailed,
} from "../../api/folderUpload";
import { refreshFolderJobs, isJobFinished } from "../../state/folderJobs";
import { setBrowseState, setCurrentView } from "../../state/app";
import { confirmDialog } from "../../state/confirm";
import { toast } from "../../state/toast";
import { errMsg } from "../../utils/errors";
import { formatBytes, formatRelative, basename } from "../../utils/fmt";
import { IconFolder, IconPause, IconPlay, IconRefresh, IconX } from "../../utils/icons";
import { language, t, tp, type TKey } from "../../i18n";
import type { FolderJob } from "../../types";
import { badgeClass, canRetryFailed, filesDone, jobName, jobPct, jobSourceTitle, jobReason, needsEncryptionConfirm } from "./folderJobHelpers";
import { FolderJobDetails, FolderJobFinal } from "./FolderJobDetails";
import { needsRepick, repickAndResume } from "./repickSource";

async function act(fn: () => Promise<unknown>) {
  try { await fn(); } catch (e) { toast.err(e); }
  void refreshFolderJobs();
}

// One row per folder job: progress, controls, and the final summary once finished.
export function FolderJobRow(props: { job: FolderJob }) {
  const j = () => props.job;
  const finished = () => isJobFinished(j());
  const moving = () => j().state === "running" || j().state === "queued";

  async function resume() {
    if (needsRepick(j())) {
      try { await repickAndResume(j()); } catch (e) { toast.err(e); }
      return;
    }
    // The backend treats this resume as the user's confirmation.
    if (needsEncryptionConfirm(j())) {
      const ok = await confirmDialog({
        title: t("folderUpload.job.encChangedTitle"),
        body: t("folderUpload.job.encChangedBody"),
        confirmLabel: t("folderUpload.job.encChangedConfirm"),
      });
      if (!ok) return;
    }
    await act(() => folderUploadResume(j().id));
  }
  async function cancel() {
    const ok = await confirmDialog({
      title: t("folderUpload.job.cancelTitle"),
      body: t("folderUpload.job.cancelBody"),
      confirmLabel: t("folderUpload.job.cancelConfirm"),
      cancelLabel: t("folderUpload.job.keep"),
      danger: true,
    });
    if (ok) await act(() => folderUploadCancel(j().id));
  }
  const retryFailed = () => act(() => folderUploadRetryFailed(j().id));
  const openDest = () => {
    setBrowseState({ accountId: j().account_id, bucket: j().bucket, prefix: j().dest_prefix });
    setCurrentView("browse");
  };
  const current = () => j().inflight.map((f) => basename(f.rel_path)).join(", ");

  return (
    <div class="transfer-row fu-job">
      <span class="transfer-dir upload"><IconFolder size={16} /></span>
      <div class="transfer-info">
        <div class="transfer-line-1">
          <span class="transfer-filename" title={jobSourceTitle(j())}>{jobName(j())}</span>
          <span class={`transfer-action-badge ${badgeClass(j())}`}>{t(`folderUpload.job.state.${j().state}` as TKey)}</span>
          <div class="transfer-btns">
            <Show when={moving()}>
              <button class="icon-btn" title={t("folderUpload.job.actions.pause")} aria-label={t("folderUpload.job.actions.pause")}
                      onClick={() => act(() => folderUploadPause(j().id))}><IconPause size={13} /></button>
            </Show>
            <Show when={j().state === "paused"}>
              <button class="icon-btn" title={t("folderUpload.job.actions.resume")} aria-label={t("folderUpload.job.actions.resume")}
                      onClick={resume}><IconPlay size={13} /></button>
            </Show>
            <Show when={!finished()}>
              <button class="icon-btn danger" title={t("common.cancel")} aria-label={t("common.cancel")} onClick={cancel}><IconX size={13} /></button>
            </Show>
            <Show when={finished() && canRetryFailed(j())}>
              <button class="icon-btn" title={t("folderUpload.job.actions.retryFailed")} aria-label={t("folderUpload.job.actions.retryFailed")}
                      onClick={retryFailed}><IconRefresh size={13} /></button>
            </Show>
            <Show when={finished()}>
              <button class="icon-btn" title={t("common.remove")} aria-label={t("common.remove")}
                      onClick={() => act(() => folderUploadRemove(j().id))}><IconX size={13} /></button>
            </Show>
          </div>
        </div>
        <div class="transfer-line-2">{t("folderUpload.job.dest", { bucket: j().bucket, prefix: j().dest_prefix })}</div>

        <Show when={!finished()}>
          <div class="progress-track"><div class="progress-fill" style={{ width: `${jobPct(j())}%` }} /></div>
          <div class="transfer-stats-row">
            <span class="transfer-pct">{Math.floor(jobPct(j()))}%</span>
            <span>{tp("folderUpload.job.files", j().total_files, { done: filesDone(j()).toLocaleString(language()) })}</span>
            <span class="flex-1" />
            <span class="transfer-stat-size">
              {t("folderUpload.job.bytes", { done: formatBytes(j().bytes_done), total: formatBytes(j().total_bytes) })}
            </span>
          </div>
          <Show when={jobReason(j())}>
            {(r) => (<>
              <div class="fu-job-reason">{r().title}</div>
              <Show when={r().desc}><div class="fu-job-reason-desc">{r().desc}</div></Show>
            </>)}
          </Show>
          <Show when={j().state === "running" && current()}>
            <div class="fu-job-current truncate" title={current()}>{t("folderUpload.job.current", { names: current() })}</div>
          </Show>
        </Show>

        <Show when={finished()}>
          <FolderJobFinal job={j()} />
          <Show when={j().error}><div class="transfer-error">{errMsg(j().error)}</div></Show>
          <div class="fu-job-actions">
            <button class="btn-secondary fu-small-btn" onClick={openDest}>{t("folderUpload.job.actions.openDest")}</button>
            <span class="flex-1" />
            <span class="transfer-stat-time">{formatRelative(j().finished_at ?? j().updated_at)}</span>
          </div>
        </Show>

        <FolderJobDetails job={j()} onRetryFailed={canRetryFailed(j()) ? retryFailed : undefined} />
      </div>
    </div>
  );
}
