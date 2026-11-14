import { createSignal, Show } from "solid-js";
import { formatBytes } from "../../../utils/fmt";
import { errMsg } from "../../../utils/errors";
import Spinner from "../../../utils/Spinner";
import { t, tp, type TKey } from "../../../i18n";
import type { FolderFlow } from "./flow";
import { ItemListPanel } from "./ItemListPanel";

// Destination check progress, then existing count + required Skip/Overwrite choice (no default).
export function OverwriteSection(props: { flow: FolderFlow; jobId: string; encrypted: boolean }) {
  const f = props.flow;
  const [review, setReview] = createSignal(false);
  const res = () => (f.checkCurrent() ? f.check().result : null);
  const checking = () => f.check().status === "checking" || (f.check().status === "done" && !f.checkCurrent());

  return (
    <div class="fu-card">
      <Show when={checking()}>
        <div class="loading-row fu-check-row" role="status">
          <Spinner /> {t("folderUpload.overwrite.checking")}
          <span class="faint">{tp("folderUpload.overwrite.listed", f.check().listed)}</span>
        </div>
      </Show>
      <Show when={f.check().status === "error"}>
        <div class="status-msg err">
          <div>{t("folderUpload.overwrite.failed")}</div>
          <div>{errMsg(f.check().error)}</div>
        </div>
        <button class="btn-secondary fu-retry-btn" onClick={f.retryCheck}>{t("common.retry")}</button>
      </Show>
      <Show when={res()}>
        {(r) => (
          <Show when={r().existing > 0} fallback={<div class="fu-note fu-ok">{t("folderUpload.overwrite.none")}</div>}>
            <div class="fu-card-head">
              <span class="fu-card-title fu-warn-text">
                {tp("folderUpload.overwrite.found", r().existing, { size: formatBytes(r().existing_bytes) })}
              </span>
              <button class="btn-ghost fu-link-btn" onClick={() => setReview((v) => !v)}>
                {review() ? t("folderUpload.list.hide") : t("folderUpload.list.review")}
              </button>
            </div>
            <div class="fu-note">{t(`folderUpload.versioning.${r().versioning}` as TKey)}</div>
            <Show when={props.encrypted}>
              <div class="fu-note faint">{t("folderUpload.overwrite.ciphertext")}</div>
            </Show>
            <Show when={review()}>
              <ItemListPanel jobId={props.jobId} filter="existing" reload={f.checkGen()} />
            </Show>
            <div class="fu-note" id="fu-policy-label">{t("folderUpload.overwrite.choose")}</div>
            <div class="fu-choices" role="radiogroup" aria-labelledby="fu-policy-label">
              <label class="fu-choice" classList={{ active: f.policy() === "skip" }}>
                <input type="radio" name="fu-policy" checked={f.policy() === "skip"} onChange={() => f.setPolicy("skip")} />
                <span class="fu-choice-body">
                  <span class="fu-choice-title">{t("folderUpload.overwrite.skip")}</span>
                  <span class="fu-choice-hint">{t("folderUpload.overwrite.skipHint")}</span>
                </span>
              </label>
              <label class="fu-choice" classList={{ active: f.policy() === "overwrite" }}>
                <input type="radio" name="fu-policy" checked={f.policy() === "overwrite"} onChange={() => f.setPolicy("overwrite")} />
                <span class="fu-choice-body">
                  <span class="fu-choice-title">{t("folderUpload.overwrite.overwrite")}</span>
                  <span class="fu-choice-hint">{t("folderUpload.overwrite.overwriteHint")}</span>
                </span>
              </label>
            </div>
          </Show>
        )}
      </Show>
    </div>
  );
}
