import { createSignal, Show } from "solid-js";
import { IconChevronD, IconFolder, IconUpload } from "../../../utils/icons";
import { t } from "../../../i18n";

// Desktop Upload button with a menu for "Upload files" / "Upload folder".
export function UploadSplitButton(props: { onUpload: () => void; onUploadFolder: () => void }) {
  const [open, setOpen] = createSignal(false);
  const run = (fn: () => void) => () => { setOpen(false); fn(); };
  return (
    <div class="fu-split">
      <button class="btn-primary toolbar-btn upload-btn fu-split-main" onClick={props.onUpload}>
        <IconUpload size={14} /> {t("common.upload")}
      </button>
      <button class="btn-primary toolbar-btn fu-split-caret" title={t("folderUpload.entry.more")}
              aria-label={t("folderUpload.entry.more")} aria-expanded={open()} onClick={() => setOpen((v) => !v)}>
        <IconChevronD size={13} />
      </button>
      <Show when={open()}>
        <div class="toolbar-overflow-backdrop" onClick={() => setOpen(false)} />
        <div class="context-menu fu-split-menu" onClick={(e) => e.stopPropagation()}>
          <button class="context-item" onClick={run(props.onUpload)}>
            <span class="context-item-icon"><IconUpload size={14} /></span> {t("folderUpload.entry.uploadFiles")}
          </button>
          <button class="context-item" onClick={run(props.onUploadFolder)}>
            <span class="context-item-icon"><IconFolder size={14} /></span> {t("folderUpload.entry.uploadFolder")}
          </button>
        </div>
      </Show>
    </div>
  );
}
