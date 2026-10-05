import { Show } from "solid-js";
import { IconAlertCircle, IconX } from "./icons";
import { describeError, isCredentialError, isNetworkError } from "./errors";
import { ErrorBody } from "./ErrorBody";
import { t } from "../i18n";
import { setCurrentView } from "../state/app";

/** Dismissable popup; floats over the nearest `position:relative` ancestor (every `.view-container`). */
export function ErrorPopup(props: { error: unknown; onClose: () => void }) {
  const { code, title } = describeError(props.error);
  const credErr = isCredentialError(code);
  const netErr  = isNetworkError(code);

  return (
    <div class="err-popup-backdrop" onClick={props.onClose}>
      <div class="err-popup" onClick={(e) => e.stopPropagation()}>
        <div class="err-popup-header">
          <IconAlertCircle size={16} class="err-popup-icon" />
          <span class="err-popup-title">{title}</span>
          <button class="icon-btn err-popup-close" onClick={props.onClose}>
            <IconX size={15} />
          </button>
        </div>
        <ErrorBody error={props.error} />
        <div class="err-popup-actions">
          <Show when={credErr || netErr}>
            <button class="btn-primary text-xs"
                    onClick={() => { setCurrentView("settings"); props.onClose(); }}>
              {t("shell.error.openSettings")}
            </button>
          </Show>
          <button class="btn-secondary text-xs" onClick={props.onClose}>
            {t("shell.dismiss")}
          </button>
        </div>
      </div>
    </div>
  );
}
