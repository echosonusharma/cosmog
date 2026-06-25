import { SettingsForm } from "./settings/SettingsForm";
import { AccountsList } from "./settings/AccountsList";

// ── root ──────────────────────────────────────────────────────────────────────

export default function Settings() {
  return (
    <div class="view-container">
      <div class="view-header">
        <span class="section-title">Settings</span>
      </div>
      <div class="settings-body">
        <SettingsForm />
        <AccountsList />
      </div>
    </div>
  );
}
