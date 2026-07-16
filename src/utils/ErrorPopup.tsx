import { Show } from "solid-js";
import { IconAlertCircle, IconX } from "./icons";
import { parseWireError, isCredentialError, isNetworkError } from "./errors";
import { setCurrentView } from "../state/app";

/**
 * Dismissable error popup — floats centered over the nearest `position:relative`
 * ancestor (every `.view-container` qualifies). Click backdrop or X to close.
 */
export function ErrorPopup(props: { error: unknown; onClose: () => void }) {
  const { code, message } = parseWireError(props.error);
  const credErr = isCredentialError(code);
  const netErr  = isNetworkError(code);
  const title = credErr ? "Credentials not found"
              : netErr  ? "Service unreachable"
              : "Something went wrong";

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
        <p class="err-popup-msg">{message}</p>
        <Show when={netErr}>
          <p class="err-popup-msg err-popup-hint">
            Check that the endpoint is running and reachable, then try again.
          </p>
        </Show>
        <div class="err-popup-actions">
          <Show when={credErr || netErr}>
            <button class="btn-primary text-xs"
                    onClick={() => { setCurrentView("settings"); props.onClose(); }}>
              Open Settings
            </button>
          </Show>
          <button class="btn-secondary text-xs" onClick={props.onClose}>
            Dismiss
          </button>
        </div>
      </div>
    </div>
  );
}
