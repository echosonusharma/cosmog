import { openUrl } from "@tauri-apps/plugin-opener";
import { RELEASES_PAGE, type UpdateInfo } from "../../utils/updates";

export function UpdateModal(props: { info: UpdateInfo; onClose: () => void }) {
  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="modal update-modal" onClick={(e) => e.stopPropagation()}>
        <div class="update-modal-header">
          <span class="modal-title">v{props.info.version} available</span>
          <button class="dismiss-btn" onClick={props.onClose}>✕</button>
        </div>
        <pre class="update-modal-changelog">{props.info.changelog}</pre>
        <div class="btn-row">
          <button class="btn-secondary" onClick={props.onClose}>Dismiss</button>
          <button
            class="btn-primary"
            onClick={() => { openUrl(RELEASES_PAGE).catch(() => {}); props.onClose(); }}
          >
            Download
          </button>
        </div>
      </div>
    </div>
  );
}
