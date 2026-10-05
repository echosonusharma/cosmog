import { Show } from "solid-js";
import { formatBytes, formatDate } from "../../../utils/fmt";
import { t } from "../../../i18n";
import type { CachedObjectMeta } from "../../../types";

export function MetaList(props: { obj: CachedObjectMeta }) {
  return (
    <dl class="preview-meta">
      <dt>{t("preview.meta.key")}</dt><dd class="mono">{props.obj.key}</dd>
      <dt>{t("common.size")}</dt><dd>{formatBytes(props.obj.size)}</dd>
      <dt>{t("common.type")}</dt><dd>{props.obj.content_type ?? "-"}</dd>
      <Show when={props.obj.last_modified}>
        <dt>{t("common.modified")}</dt><dd>{formatDate(props.obj.last_modified!)}</dd>
      </Show>
      <Show when={props.obj.storage_class}>
        <dt>{t("preview.meta.class")}</dt><dd>{props.obj.storage_class}</dd>
      </Show>
      <Show when={props.obj.etag}>
        <dt>ETag</dt><dd class="mono">{props.obj.etag}</dd>
      </Show>
    </dl>
  );
}
