import { Show } from "solid-js";
import { formatBytes, formatDate } from "../../../utils/fmt";
import type { CachedObjectMeta } from "../../../types";

export function MetaList(props: { obj: CachedObjectMeta }) {
  return (
    <dl class="preview-meta">
      <dt>Key</dt><dd class="mono">{props.obj.key}</dd>
      <dt>Size</dt><dd>{formatBytes(props.obj.size)}</dd>
      <dt>Type</dt><dd>{props.obj.content_type ?? "-"}</dd>
      <Show when={props.obj.last_modified}>
        <dt>Modified</dt><dd>{formatDate(props.obj.last_modified!)}</dd>
      </Show>
      <Show when={props.obj.storage_class}>
        <dt>Class</dt><dd>{props.obj.storage_class}</dd>
      </Show>
      <Show when={props.obj.etag}>
        <dt>ETag</dt><dd class="mono">{props.obj.etag}</dd>
      </Show>
    </dl>
  );
}
