import { onCleanup, onMount, Show } from "solid-js";
import { confirmDialog } from "../../../state/confirm";
import { formatBytes, basename } from "../../../utils/fmt";
import { ErrorBody } from "../../../utils/ErrorBody";
import { ScrollFade } from "../../../utils/ScrollFade";
import Spinner from "../../../utils/Spinner";
import { language, t, tp } from "../../../i18n";
import { createFolderFlow } from "./flow";
import { excludedTotal } from "./kinds";
import { SummaryDetails, SummaryHead } from "./AnalysisSummary";
import { ExcludedSection } from "./ExcludedSection";
import { OverwriteSection } from "./OverwriteSection";
import { DestinationField } from "./DestinationField";

// Discarding a quick scan needs no confirm; a longer one does.
const CONFIRM_DISCARD_MS = 3000;

export function FolderUploadModal(props: {
  accountId: string;
  bucket: string;
  localRoot: string;
  prefix: string;
  encrypted: boolean;
  onClose: () => void;
}) {
  const flow = createFolderFlow({
    accountId: props.accountId, bucket: props.bucket, localRoot: props.localRoot,
    initialDest: `${props.prefix}${basename(props.localRoot)}/`,
  });
  onCleanup(flow.discard);

  const num = (n: number) => n.toLocaleString(language());
  const a = () => flow.analysis();
  // Backend-refreshed after each check, so these match what approval uploads.
  const files = () => a()?.files ?? 0;
  const nothing = () => { const x = a(); return !!x && files() === 0 && x.empty_dirs === 0; };
  const exampleRel = () => a()?.largest_files[0]?.rel_path ?? "…";

  const approveLabel = () => {
    if (flow.approving()) return t("folderUpload.approve.starting");
    const x = a();
    if (!x) return t("common.upload");
    const existing = flow.existing();
    const existingSize = formatBytes(flow.check().result?.existing_bytes ?? 0);
    const parts: string[] = [];
    if (existing > 0 && flow.policy() === "skip") {
      parts.push(tp("folderUpload.approve.uploadNew", Math.max(0, files() - existing)));
      parts.push(tp("folderUpload.approve.skipExisting", existing, { size: existingSize }));
    } else {
      parts.push(tp("folderUpload.approve.upload", files(), { size: formatBytes(x.total_bytes) }));
      if (existing > 0 && flow.policy() === "overwrite") {
        parts.push(tp("folderUpload.approve.overwriteExisting", existing, { size: existingSize }));
      }
    }
    const excl = excludedTotal(x.excluded);
    if (excl > 0) parts.push(tp("folderUpload.approve.excluded", excl));
    return parts.reduce((first, second) => t("folderUpload.approve.join", { first, second }));
  };

  const hint = () => {
    if (flow.phase() !== "summary" || nothing()) return "";
    if (!flow.checkCurrent()) return t("folderUpload.approve.waitCheck");
    if (flow.existing() > 0 && !flow.policy()) return t("folderUpload.approve.needPolicy");
    return "";
  };

  let asking = false;
  async function requestClose() {
    if (asking || flow.approving()) return;
    if (flow.phase() !== "error" && flow.elapsedMs() > CONFIRM_DISCARD_MS) {
      asking = true;
      const ok = await confirmDialog({
        title: t("folderUpload.analyze.discardTitle"),
        body: t("folderUpload.analyze.discardBody"),
        confirmLabel: t("folderUpload.analyze.discard"),
        danger: true,
      }).catch(() => false);
      asking = false;
      if (!ok) return;
    }
    flow.discard();
    props.onClose();
  }

  let dialogEl: HTMLDivElement | undefined;
  onMount(() => {
    dialogEl?.focus();
    // Escape closes, unless a confirm dialog on top is handling it.
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape" && !asking) { e.preventDefault(); void requestClose(); } };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
  });

  async function approve() {
    if (await flow.approve()) props.onClose();
  }

  return (
    <div class="modal-backdrop" onClick={() => void requestClose()}>
      <div class="modal fu-modal" role="dialog" aria-modal="true" aria-labelledby="fu-modal-title" tabIndex={-1}
           ref={dialogEl} onClick={(e) => e.stopPropagation()}>
        <div class="modal-title" id="fu-modal-title">{t("folderUpload.title")}</div>
        <div class="modal-label">{t("folderUpload.analyze.source")}</div>
        <div class="modal-sub fu-source" title={props.localRoot}>{props.localRoot}</div>
        <DestinationField flow={flow} bucket={props.bucket} exampleRel={exampleRel()} disabled={flow.approving()} />

        <Show when={flow.phase() === "scanning"}>
          <div class="fu-scan">
            <div class="loading-row"><Spinner /> {t("folderUpload.analyze.scanning")}</div>
            <div class="stats-summary">
              <div class="stats-figure">
                <span class="stats-figure-val">{num(flow.progress().files)}</span>
                <span class="stats-figure-lbl">{t("folderUpload.summary.files")}</span>
              </div>
              <div class="stats-figure">
                <span class="stats-figure-val">{num(flow.progress().dirs)}</span>
                <span class="stats-figure-lbl">{t("folderUpload.summary.folders")}</span>
              </div>
              <div class="stats-figure">
                <span class="stats-figure-val">{formatBytes(flow.progress().bytes)}</span>
                <span class="stats-figure-lbl">{t("folderUpload.summary.totalSize")}</span>
              </div>
            </div>
            <Show when={flow.progress().current_dir}>
              <div class="fu-note faint truncate">{t("folderUpload.analyze.reading", { path: flow.progress().current_dir })}</div>
            </Show>
          </div>
        </Show>

        <Show when={flow.phase() === "error"}>
          <div class="fu-error">
            <div class="fu-card-title">{t("folderUpload.analyze.failedTitle")}</div>
            <ErrorBody error={flow.error()} />
          </div>
        </Show>

        <Show when={flow.phase() === "summary" && a()}>
          {(x) => (
            // Decision items first (what lands, what gets replaced), details after.
            <ScrollFade class="fu-scroll">
              <SummaryHead a={x()} />
              <Show when={!nothing()} fallback={<div class="fu-note">{t("folderUpload.analyze.nothing")}</div>}>
                <OverwriteSection flow={flow} jobId={x().job_id} encrypted={x().encrypted} />
              </Show>
              <ExcludedSection jobId={x().job_id} counts={x().excluded} reload={flow.checkGen()} />
              <SummaryDetails a={x()} />
            </ScrollFade>
          )}
        </Show>

        <Show when={flow.approveError()}>
          <div class="fu-error"><ErrorBody error={flow.approveError()} /></div>
        </Show>
        <Show when={hint()}><div class="fu-note faint">{hint()}</div></Show>

        <div class="btn-row fu-actions">
          <button class="btn-secondary" onClick={() => void requestClose()} disabled={flow.approving()}>
            {flow.phase() === "error" ? t("common.close") : t("common.cancel")}
          </button>
          <Show when={flow.phase() === "summary"}>
            <button class="btn-primary fu-approve" disabled={!flow.canApprove() || nothing()} onClick={() => void approve()}>
              {approveLabel()}
            </button>
          </Show>
        </div>
      </div>
    </div>
  );
}
