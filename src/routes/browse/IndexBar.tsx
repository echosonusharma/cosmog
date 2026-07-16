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
  return (
    <div class="index-bar">
      <Show when={props.indexStatus.loading}>
        <span class="muted index-bar-item">Checking index…</span>
      </Show>
      <Show when={!props.indexStatus.loading && props.indexStatus()}>
        {(st) => (
          <>
            <span class={`index-dot ${st().enabled ? "enabled" : "disabled"}`} />
            <span class="index-bar-item">{st().enabled ? "Indexed" : "Not indexed"}</span>
            <Show when={st().object_count > 0}>
              <span class="dot-sep">·</span>
              <span class="index-bar-item">{st().object_count.toLocaleString()} objects</span>
            </Show>
            <Show when={st().last_full_sync_at}>
              <span class="dot-sep">·</span>
              <span class="index-bar-item faint">synced {formatRelative(st().last_full_sync_at!)}</span>
            </Show>
            <Show when={st().scan_continuation}>
              <span class="dot-sep">·</span>
              <span class="muted index-bar-item">scanning…</span>
              <button class="btn-ghost index-bar-btn" onClick={() => cancelBucketScan(props.accountId, props.bucket).then(props.refetchIndex)}>Cancel</button>
            </Show>
          </>
        )}
      </Show>
      <div class="index-bar-spacer" />
      <Show when={props.indexStatus()?.enabled}>
        <button class="icon-btn" title="Re-index" disabled={props.indexBusy} onClick={props.onReindex}>
          <IconRefresh size={14} />
        </button>
      </Show>
    </div>
  );
}
