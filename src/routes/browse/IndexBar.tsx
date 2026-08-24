import { Show } from "solid-js";
import type { Resource } from "solid-js";
import { IconRefresh } from "../../utils/icons";
import { cancelBucketScan } from "../../api/search";
import { formatRelative } from "../../utils/fmt";
import type { BucketIndexStatus } from "../../types";

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
        <span class="muted index-bar-item">Checking index…</span>
      </Show>
      <Show when={st()}>
        {(s) => (
          <>
            <span class={`index-dot ${s().enabled ? "enabled" : "disabled"}`} />
            <span class="index-bar-item">{s().enabled ? "Indexed" : "Not indexed"}</span>
            <Show when={s().object_count > 0}>
              <span class="dot-sep">·</span>
              <span class="index-bar-item">{s().object_count.toLocaleString()} objects</span>
            </Show>
            <Show when={s().last_full_sync_at}>
              <span class="dot-sep">·</span>
              <span class="index-bar-item faint">synced {formatRelative(s().last_full_sync_at!)}</span>
            </Show>
            <Show when={s().scan_continuation}>
              <span class="dot-sep">·</span>
              <span class="muted index-bar-item">scanning…</span>
              <button class="btn-ghost index-bar-btn" onClick={() => cancelBucketScan(props.accountId, props.bucket).then(props.refetchIndex)}>Cancel</button>
            </Show>
          </>
        )}
      </Show>
      <div class="index-bar-spacer" />
      <Show when={st()?.enabled}>
        <button class="icon-btn" title="Re-index" disabled={props.indexBusy} onClick={props.onReindex}>
          <IconRefresh size={14} />
        </button>
      </Show>
    </div>
  );
}
