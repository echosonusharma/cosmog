import { createSignal, Show } from "solid-js";
import { SettingsForm } from "./settings/SettingsForm";
import { AccountsList } from "./settings/AccountsList";
import { clearAppData } from "../api/portable";
import { toast } from "../state/toast";

const CONFIRM_PHRASE = "i understand"; // compared lowercase; input is lowercased before match

function ClearDataModal(props: { onClose: () => void }) {
  const [input, setInput] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const confirmed = () => input().trim().toLowerCase() === CONFIRM_PHRASE;
  const mismatch = () => input().length > 0 && !confirmed();

  async function doDelete() {
    if (!confirmed()) return;
    setBusy(true);
    try {
      await clearAppData();
    } catch (e) {
      toast.err(e);
      setBusy(false);
    }
  }

  return (
    <div class="modal-backdrop" onClick={() => !busy() && props.onClose()}>
      <div class="cd-modal" onClick={(e) => e.stopPropagation()}>
        <div class="cd-modal-header">
          <span class="cd-modal-title">Clear all app data</span>
          <button class="cd-modal-close" onClick={props.onClose} aria-label="Close">✕</button>
        </div>

        <div class="cd-modal-body">
          <div class="cd-warning-box">
            <svg class="cd-warning-icon" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/>
              <line x1="12" y1="9" x2="12" y2="13"/>
              <line x1="12" y1="17" x2="12.01" y2="17"/>
            </svg>
            <div>
              <div class="cd-warning-title">This action is irreversible.</div>
              <div class="cd-warning-desc">All local data will be permanently deleted and the app will close immediately.</div>
            </div>
          </div>

          <ul class="cd-consequence-list">
            <li>Database, transfer history, and search indexes will be deleted</li>
            <li>All credentials will be removed from the OS keychain</li>
            <li>Log files will be deleted</li>
            <li>The app will close automatically once deletion is complete</li>
          </ul>

          <div class="cd-confirm-section">
            <p class="cd-confirm-label">Please type <strong>I understand</strong> to confirm.</p>
            <input
              class="cd-confirm-input"
              classList={{ "cd-confirm-input-error": mismatch() }}
              placeholder=""
              value={input()}
              onInput={(e) => setInput(e.currentTarget.value)}
              disabled={busy()}
              autofocus
            />
          </div>

          <button
            class="cd-delete-btn"
            onClick={doDelete}
            disabled={!confirmed() || busy()}
          >
            {busy() ? "Deleting…" : "I understand, delete all app data"}
          </button>
        </div>
      </div>
    </div>
  );
}

function DangerZone() {
  const [showModal, setShowModal] = createSignal(false);

  return (
    <>
      <div class="settings-section settings-danger-zone">
        <div class="settings-section-title">Danger zone</div>
        <div class="settings-danger-row">
          <div class="settings-danger-text">
            <span class="settings-danger-label">Clear all app data</span>
            <span class="settings-danger-desc">Permanently deletes the local database, transfer history, search indexes, logs, and all credentials from the OS keychain. The app will close immediately. Use this before uninstalling to leave no files behind.</span>
          </div>
          <button class="btn-danger" onClick={() => setShowModal(true)}>Clear all data</button>
        </div>
      </div>
      <Show when={showModal()}>
        <ClearDataModal onClose={() => setShowModal(false)} />
      </Show>
    </>
  );
}

export default function Settings() {
  return (
    <div class="view-container">
      <div class="view-header">
        <span class="section-title">Settings</span>
      </div>
      <div class="settings-body">
        <SettingsForm />
        <AccountsList />
        <DangerZone />
      </div>
    </div>
  );
}
