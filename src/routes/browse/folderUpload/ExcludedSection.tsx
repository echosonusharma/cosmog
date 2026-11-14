import { createSignal, For, Show } from "solid-js";
import { language, t, tp, type TKey } from "../../../i18n";
import type { ExcludeReason } from "../../../types";
import { EXCLUDE_REASONS, excludedTotal } from "./kinds";
import { ItemListPanel } from "./ItemListPanel";

// Per-reason counts of items that will not be uploaded, with a browsable list.
export function ExcludedSection(props: { jobId: string; counts: Record<ExcludeReason, number>; reload: number }) {
  const [open, setOpen] = createSignal(false);
  const total = () => excludedTotal(props.counts);
  const reasons = () => EXCLUDE_REASONS.filter((r) => (props.counts[r] ?? 0) > 0);

  return (
    <Show when={total() > 0}>
      <div class="fu-card">
        <div class="fu-card-head">
          <span class="fu-card-title">{t("folderUpload.excluded.title")}</span>
          <button class="btn-ghost fu-link-btn" onClick={() => setOpen((v) => !v)}>
            {open() ? t("folderUpload.list.hide") : t("folderUpload.list.review")}
          </button>
        </div>
        <div class="fu-note">{tp("folderUpload.excluded.intro", total())}</div>
        <div class="fu-reasons">
          <For each={reasons()}>
            {(r) => (
              <div class="fu-reason-row">
                <span class="truncate">{t(`folderUpload.excluded.${r}` as TKey)}</span>
                <span class="fu-reason-count">{props.counts[r].toLocaleString(language())}</span>
              </div>
            )}
          </For>
        </div>
        <Show when={open()}>
          <ItemListPanel jobId={props.jobId} filter="excluded" reload={props.reload} />
        </Show>
      </div>
    </Show>
  );
}
