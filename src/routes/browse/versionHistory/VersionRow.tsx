import { Show } from "solid-js";
import { IconDownload, IconRefresh, IconTrash } from "../../../utils/icons";
import { formatBytes, formatDate } from "../../../utils/fmt";
import type { ObjectVersion } from "../../../types";

function shortId(id: string | null): string {
  if (!id) return "null";
  return id.length > 8 ? id.slice(0, 8) : id;
}

export function VersionRow(props: {
  version: ObjectVersion;
  busy: boolean;
  onDownload: (v: ObjectVersion) => void;
  onRestore: (v: ObjectVersion) => void;
  onDelete: (v: ObjectVersion) => void;
}) {
  const v = () => props.version;
  const hasVersionId = () => v().version_id !== null;
  // Restore covers non-latest versions plus removing a latest delete marker
  // (which re-exposes the prior version); both need a version id to act on.
  const canRestore = () =>
    hasVersionId() &&
    ((!v().is_latest && !v().is_delete_marker) || (v().is_latest && v().is_delete_marker));

  return (
    <div class="vh-row" classList={{ latest: v().is_latest }}>
      <div class="vh-row-main">
        <div class="vh-row-top">
          <span class="vh-vid" title={v().version_id ?? "no version id"}>
            {shortId(v().version_id)}
          </span>
          <Show when={v().is_latest}>
            <span class="vh-badge latest">Latest</span>
          </Show>
          <Show when={v().is_delete_marker}>
            <span class="vh-badge delete-marker">Delete marker</span>
          </Show>
        </div>
        <div class="vh-meta">
          <span>{formatDate(v().last_modified)}</span>
          <span>{v().size === null ? "-" : formatBytes(v().size!)}</span>
        </div>
      </div>
      <div class="vh-row-actions">
        <Show when={!v().is_delete_marker}>
          <button
            class="icon-btn"
            title="Download this version"
            disabled={props.busy}
            onClick={() => props.onDownload(v())}
          >
            <IconDownload size={14} />
          </button>
        </Show>
        <Show when={canRestore()}>
          <button
            class="icon-btn"
            title="Restore this version"
            disabled={props.busy}
            onClick={() => props.onRestore(v())}
          >
            <IconRefresh size={14} />
          </button>
        </Show>
        <Show when={hasVersionId()}>
          <button
            class="icon-btn danger"
            title="Delete this version permanently"
            disabled={props.busy}
            onClick={() => props.onDelete(v())}
          >
            <IconTrash size={14} />
          </button>
        </Show>
      </div>
    </div>
  );
}
