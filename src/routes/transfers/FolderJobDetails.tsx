import { createSignal, For, Show } from "solid-js";
import { ItemListPanel } from "../browse/folderUpload/ItemListPanel";
import { formatBytes } from "../../utils/fmt";
import { language, t, tp, type PluralKey, type TKey } from "../../i18n";
import type { FolderItemFilter, FolderJob } from "../../types";
import { filesDone } from "./folderJobHelpers";

type Section = { filter: FolderItemFilter; key: TKey; count: number };

// Final per-job summary lines; zero counts are omitted.
export function FolderJobFinal(props: { job: FolderJob }) {
  const j = () => props.job;
  const lines = () => {
    const c = j().counts;
    const appeared = j().appeared_after_check;
    const rows: [PluralKey, number][] = [
      ["folderUpload.final.skippedExisting", Math.max(0, (c.skipped_exists ?? 0) - appeared)],
      ["folderUpload.final.appeared", appeared],
      ["folderUpload.final.missing", c.skipped_missing ?? 0],
      ["folderUpload.final.changed", j().drift],
      ["folderUpload.final.failed", c.failed ?? 0],
      ["folderUpload.final.excluded", c.excluded ?? 0],
    ];
    return rows.filter(([, n]) => n > 0);
  };
  return (
    <div class="fu-final">
      <div class="fu-final-main">{tp("folderUpload.final.uploaded", filesDone(j()), { size: formatBytes(j().bytes_done) })}</div>
      <For each={lines()}>{([k, n]) => <div class="fu-final-line">{tp(k, n)}</div>}</For>
      <Show when={j().state === "canceled"}><div class="fu-final-line fu-warn-text">{t("folderUpload.final.canceledNote")}</div></Show>
      <div class="fu-final-line faint">{t("folderUpload.final.newFilesNote")}</div>
    </div>
  );
}

// Expanders over the job's item list: failed (with retry), skipped, changed, excluded.
export function FolderJobDetails(props: { job: FolderJob; onRetryFailed?: () => void }) {
  const [open, setOpen] = createSignal<FolderItemFilter | null>(null);
  const sections = (): Section[] => {
    const c = props.job.counts;
    return [
      { filter: "failed" as const, key: "folderUpload.job.expand.failed" as TKey, count: c.failed ?? 0 },
      { filter: "skipped" as const, key: "folderUpload.job.expand.skipped" as TKey, count: (c.skipped_exists ?? 0) + (c.skipped_missing ?? 0) },
      { filter: "drift" as const, key: "folderUpload.job.expand.changed" as TKey, count: props.job.drift },
      { filter: "excluded" as const, key: "folderUpload.job.expand.excluded" as TKey, count: c.excluded ?? 0 },
    ].filter((s) => s.count > 0);
  };
  const toggle = (f: FolderItemFilter) => setOpen((cur) => (cur === f ? null : f));

  return (
    <Show when={sections().length > 0}>
      <div class="fu-expanders">
        <For each={sections()}>
          {(s) => (
            <button class="chip" classList={{ active: open() === s.filter }} aria-expanded={open() === s.filter}
                    onClick={() => toggle(s.filter)}>
              {t(s.key, { count: s.count.toLocaleString(language()) })}
            </button>
          )}
        </For>
      </div>
      <Show when={open()}>
        {(f) => (
          <div class="fu-expander-body">
            <Show when={f() === "failed" && props.onRetryFailed}>
              <button class="btn-secondary fu-retry-btn" onClick={() => props.onRetryFailed?.()}>
                {t("folderUpload.job.actions.retryFailed")}
              </button>
            </Show>
            <ItemListPanel jobId={props.job.id} filter={f()} reload={props.job.updated_at} />
          </div>
        )}
      </Show>
    </Show>
  );
}
