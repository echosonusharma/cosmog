import { Show } from "solid-js";
import { IconAlertCircle, IconX } from "./icons";
import { parseWireError, isCredentialError } from "./errors";
import { setCurrentView } from "../state/app";

/**
 * Dismissable error popup — floats centered over the nearest `position:relative`
 * ancestor (every `.view-container` qualifies). Click backdrop or X to close.
 */
export function ErrorPopup(props: { error: unknown; onClose: () => void }) {
  const { code, message } = parseWireError(props.error);
  const credErr = isCredentialError(code);
  const title = credErr ? "Credentials not found" : "Something went wrong";

  return (
    <div class="err-popup-backdrop" onClick={props.onClose}>
      <div class="err-popup" onClick={(e) => e.stopPropagation()}>
        <div class="err-popup-header">
          <IconAlertCircle size={16} style="color:var(--err);flex-shrink:0" />
          <span class="err-popup-title">{title}</span>
          <button class="icon-btn" style="margin-left:auto" onClick={props.onClose}>
            <IconX size={15} />
          </button>
        </div>
        <p class="err-popup-msg">{message}</p>
        <div class="err-popup-actions">
          <Show when={credErr}>
            <button class="btn-primary" style="font-size:12px"
                    onClick={() => { setCurrentView("settings"); props.onClose(); }}>
              Open Settings
            </button>
          </Show>
          <button class="btn-secondary" style="font-size:12px" onClick={props.onClose}>
            Dismiss
          </button>
        </div>
      </div>
    </div>
  );
}
