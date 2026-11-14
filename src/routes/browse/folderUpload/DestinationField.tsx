import { Show } from "solid-js";
import { KEY_MAX_BYTES, utf8Bytes } from "../../../validation";
import { tp, t } from "../../../i18n";
import type { FolderFlow } from "./flow";

// Editable destination with a live example key and UTF-8 byte-length validation.
export function DestinationField(props: { flow: FolderFlow; bucket: string; exampleRel: string; disabled: boolean }) {
  const f = props.flow;
  const parsed = () => f.destParsed();
  const keyTooLong = () => (f.checkCurrent() ? f.check().result?.key_too_long ?? 0 : 0);
  const example = () => `${props.bucket}/${f.dest() ?? ""}${props.exampleRel}`;

  return (
    <div class="fu-dest">
      <label class="modal-label" for="fu-dest-input">{t("folderUpload.dest.label")}</label>
      <input id="fu-dest-input" class="field fu-dest-input" value={f.destInput()} disabled={props.disabled}
             spellcheck={false} onInput={(e) => f.setDestInput(e.currentTarget.value)} />
      <div class="fu-dest-meta">
        <span class="fu-dest-preview truncate" title={example()}>{t("folderUpload.dest.preview", { key: example() })}</span>
        <span class="fu-dest-bytes">{t("folderUpload.dest.bytes", { bytes: utf8Bytes(f.dest() ?? f.destInput()), max: KEY_MAX_BYTES })}</span>
      </div>
      <Show when={!parsed().success}>
        <div class="status-msg err">{(parsed() as { message: string }).message}</div>
      </Show>
      <Show when={keyTooLong() > 0}>
        <div class="fu-note fu-warn-text">{tp("folderUpload.dest.keyTooLong", keyTooLong())}</div>
      </Show>
    </div>
  );
}
