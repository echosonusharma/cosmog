import { Show } from "solid-js";
import { Portal } from "solid-js/web";
import { IconFolder, IconUpload } from "../../../utils/icons";
import { t } from "../../../i18n";
import { useBackHandler } from "../../../utils/androidBack";

// Mobile bottom sheet: pick files or a whole folder.
export function UploadSheet(props: {
  open: boolean;
  onClose: () => void;
  onUpload: () => void;
  onUploadFolder: () => void;
}) {
  useBackHandler(() => props.open, () => { props.onClose(); return true; });
  const run = (fn: () => void) => () => { props.onClose(); fn(); };
  return (
    <Show when={props.open}>
      <Portal mount={document.body}>
        <div class="upload-sheet-backdrop" onClick={props.onClose}>
          <div class="upload-sheet" role="dialog" aria-label={t("common.upload")} onClick={(e) => e.stopPropagation()}>
            <div class="upload-sheet-grip" />
            <div class="upload-sheet-title">{t("common.upload")}</div>
            <button class="upload-sheet-item" onClick={run(props.onUpload)}>
              <span class="upload-sheet-icon"><IconUpload size={20} /></span>
              <span class="upload-sheet-text">
                <span class="upload-sheet-label">{t("folderUpload.sheet.files")}</span>
                <span class="upload-sheet-desc">{t("folderUpload.sheet.filesDesc")}</span>
              </span>
            </button>
            <button class="upload-sheet-item" onClick={run(props.onUploadFolder)}>
              <span class="upload-sheet-icon"><IconFolder size={20} /></span>
              <span class="upload-sheet-text">
                <span class="upload-sheet-label">{t("folderUpload.sheet.folder")}</span>
                <span class="upload-sheet-desc">{t("folderUpload.sheet.folderDesc")}</span>
              </span>
            </button>
            <button class="btn-secondary upload-sheet-cancel" onClick={props.onClose}>{t("common.cancel")}</button>
          </div>
        </div>
      </Portal>
    </Show>
  );
}
