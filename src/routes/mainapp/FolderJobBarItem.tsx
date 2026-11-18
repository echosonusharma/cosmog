import { createMemo, createSignal, onCleanup, Show } from "solid-js";
import { language, t, tp, type TKey } from "../../i18n";
import { basename, formatBytes, formatDuration } from "../../utils/fmt";
import { IconFolder, IconPause } from "../../utils/icons";
import { folderUploadPause } from "../../api/folderUpload";
import { refreshFolderJobs } from "../../state/folderJobs";
import { toast } from "../../state/toast";
import { createJobRate, filesDone, jobName, jobPct, jobReason, jobSourceTitle } from "../transfers/folderJobHelpers";
import type { FolderJob } from "../../types";

// One folder job in the bottom bar: progress, files, speed, time left, current file.
export function FolderJobBarItem(props: { job: FolderJob }) {
  const j = () => props.job;
  const rate = createJobRate(j);
  // Ticks every second so a stalled upload's speed falls instead of freezing.
  const [tick, setTick] = createSignal(0);
  const timer = setInterval(() => setTick((n) => n + 1), 1000);
  onCleanup(() => clearInterval(timer));
  const speed = createMemo(() => (tick(), j().bytes_done, rate.speed()));
  const eta = createMemo(() => (speed(), rate.etaSec()));
  const pct = () => Math.floor(jobPct(j()));
  const current = () => j().inflight.map((f) => basename(f.rel_path)).join(", ");
  const reason = () => jobReason(j());

  async function pause(e: MouseEvent) {
    e.stopPropagation();
    try { await folderUploadPause(j().id); }
    catch (err) { toast.err(err); }
    finally { void refreshFolderJobs(); }
  }

  return (
    <div class="active-xfer-item">
      <span class="active-xfer-dir upload"><IconFolder size={12} /></span>
      <div class="active-xfer-item-body">
        <div class="active-xfer-item-line-1">
          <span class="active-xfer-name fu-bar-name truncate" title={jobSourceTitle(j())}>{jobName(j())}</span>
          <span class="fu-bar-dest truncate">{t("folderUpload.job.dest", { bucket: j().bucket, prefix: j().dest_prefix })}</span>
          <Show when={j().state === "queued"}>
            <span class="fu-bar-state">{t(`folderUpload.job.state.${j().state}` as TKey)}</span>
          </Show>
          <span class="fu-bar-pct">{pct()}%</span>
          <Show when={j().state === "running"}>
            <button class="icon-btn fu-bar-btn" title={t("folderUpload.job.actions.pause")}
                    aria-label={t("folderUpload.job.actions.pause")} onClick={pause}><IconPause size={12} /></button>
          </Show>
        </div>
        <div class="progress-track active-xfer-progress">
          <div class="progress-fill" style={{ width: `${jobPct(j())}%` }} />
        </div>
        <div class="active-xfer-item-line-2">
          <span>{t("folderUpload.job.bytes", { done: formatBytes(j().bytes_done), total: formatBytes(j().total_bytes) })}</span>
          <span class="fu-bar-sep">·</span>
          <span>{tp("folderUpload.job.files", j().total_files, { done: filesDone(j()).toLocaleString(language()) })}</span>
          <Show when={speed()}>
            {(s) => <><span class="fu-bar-sep">·</span><span>{t("folderUpload.job.speed", { size: formatBytes(Math.round(s())) })}</span></>}
          </Show>
          <Show when={eta()}>
            {(s) => <><span class="fu-bar-sep">·</span><span>{t("folderUpload.job.eta", { time: formatDuration(s()) })}</span></>}
          </Show>
          <Show when={reason()} fallback={
            <Show when={current()}>
              <span class="fu-bar-current truncate" title={current()}>{t("folderUpload.job.current", { names: current() })}</span>
            </Show>
          }>
            {(r) => <span class="fu-bar-current fu-warn-text truncate">{r().title}</span>}
          </Show>
        </div>
      </div>
    </div>
  );
}
