import { Show } from "solid-js";
import type { Resource } from "solid-js";
import { IconRefresh } from "../../utils/icons";
import { cancelBucketScan } from "../../api/search";
import { formatRelative } from "../../utils/fmt";
import type { BucketIndexStatus } from "../../types";
import { t, tp } from "../../i18n";

export function IndexBar(props: {
  accountId: string;
  bucket: string;
  indexStatus: Resource<BucketIndexStatus | undefined>;
  indexBusy: boolean;
  refetchIndex: () => void;
  onReindex: () => void;
}) {
  // Prefer the in-flight resource value; do NOT fall back to `.latest` while
  // loading — after a bucket switch it's still the previous bucket's stats.
  const st = () => props.indexStatus() ?? (!props.indexStatus.loading ? props.indexStatus.latest : undefined);

  return (
    <div class="index-bar">
      <Show when={props.indexStatus.loading && st() == null}>
        <span class="muted index-bar-item">{t("browse.index.checking")}</span>
      </Show>
      <Show when={st()}>
        {(s) => (
          <>
            <span class={`index-dot ${s().enabled ? "enabled" : "disabled"}`} />
            <span class="index-bar-item">{s().enabled ? t("browse.index.on") : t("browse.index.off")}</span>
            <Show when={s().object_count > 0}>
              <span class="dot-sep">·</span>
              <span class="index-bar-item">{tp("browse.index.objects", s().object_count)}</span>
            </Show>
            <Show when={s().last_full_sync_at}>
              <span class="dot-sep">·</span>
              <span class="index-bar-item faint">{t("browse.index.synced", { when: formatRelative(s().last_full_sync_at!) })}</span>
            </Show>
            <Show when={s().scan_continuation}>
              <span class="dot-sep">·</span>
              <span class="muted index-bar-item">{t("browse.index.scanning")}</span>
              <button class="btn-ghost index-bar-btn" onClick={() => cancelBucketScan(props.accountId, props.bucket).then(props.refetchIndex)}>{t("common.cancel")}</button>
            </Show>
          </>
        )}
      </Show>
      <div class="index-bar-spacer" />
      <Show when={st()?.enabled}>
        <button class="icon-btn" title={t("browse.index.reindex")} disabled={props.indexBusy} onClick={props.onReindex}>
          <IconRefresh size={14} />
        </button>
      </Show>
    </div>
  );
}
