import { createSignal, onMount, Show } from "solid-js";
import { Select } from "../../utils/Select";
import { setTheme, themePref, type Theme } from "../../state/theme";
import { appSettings, loadSettings, saveSettings, resetAllSettings } from "../../state/settings";
import { editorHighlightTheme, setEditorHighlightTheme, EDITOR_HIGHLIGHT_THEMES, type EditorHighlightThemeId } from "../../state/editorTheme";
import { toast } from "../../state/toast";
import { confirmDialog } from "../../state/confirm";
import { parseSchema, settingsPatchSchema } from "../../validation";
import type { AppSettings } from "../../types";
import Spinner from "../../utils/Spinner";

type NumKey = { [K in keyof AppSettings]: AppSettings[K] extends number ? K : never }[keyof AppSettings];

export function SettingsForm() {
  const [loading, setLoading] = createSignal(!appSettings());
  const [busy, setBusy] = createSignal(false);
  const [form, setForm] = createSignal<Partial<AppSettings>>({});

  onMount(() => { loadSettings().finally(() => setLoading(false)); });

  function field<K extends keyof AppSettings>(key: K): AppSettings[K] | undefined {
    const over = form() as Partial<AppSettings>;
    if (key in over) return over[key] as AppSettings[K];
    return appSettings()?.[key];
  }

  function patch<K extends keyof AppSettings>(key: K, val: AppSettings[K]) {
    setForm((p) => ({ ...p, [key]: val }));
  }

  // Clamp on commit, not per keystroke; rewrite the text so it shows the clamped value.
  function commitNum(key: NumKey, e: Event & { currentTarget: HTMLInputElement }, min: number, max: number, fallback: number, scale = 1) {
    const n = parseInt(e.currentTarget.value);
    const v = Math.min(max, Math.max(min, Number.isNaN(n) ? fallback : n));
    e.currentTarget.value = String(v);
    patch(key, v * scale);
  }

  // Persist immediately, matching the titlebar toggle.
  async function chooseTheme(t: Theme) {
    setTheme(t);
    try { await saveSettings({ theme: t }); } catch (e) { toast.err(e); }
  }

  async function save() {
    // Number inputs commit on change (blur); flush a still-focused one first.
    (document.activeElement as HTMLElement | null)?.blur?.();
    const patch = form();
    const result = parseSchema(settingsPatchSchema, patch);
    if (!result.success) {
      toast.err(result.message);
      return;
    }
    setBusy(true);
    try {
      await saveSettings(patch);
      setForm({});
      toast.ok("Settings saved", "Your preferences were updated");
    } catch (e) { toast.err(e); }
    finally { setBusy(false); }
  }

  async function doReset() {
    const ok = await confirmDialog({
      title: "Reset all settings?",
      body: "Returns every preference to default.",
      confirmLabel: "Reset",
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    try {
      const s = await resetAllSettings();
      setTheme(s.theme ?? "system");
      setForm({});
      toast.ok("Defaults restored", "Every preference was reset to its default");
    } catch (e) { toast.err(e); }
    finally { setBusy(false); }
  }

  const dirty = () => Object.keys(form()).length > 0;

  return (
    <div class="settings-section">
      <div class="settings-section-title">General</div>
      <Show when={loading() && !appSettings()}>
        <div class="loading-row"><Spinner /> Loading settings…</div>
      </Show>
      <Show when={appSettings()}>
        <div class="settings-grid">
          <label class="settings-label">Theme</label>
          <Select
            value={themePref()}
            options={[
              { value: "system", label: "System" },
              { value: "dark", label: "Dark" },
              { value: "light", label: "Light" },
            ]}
            onChange={(v) => chooseTheme(v as Theme)}
          />

          <label class="settings-label">Editor highlight theme</label>
          <Select
            value={editorHighlightTheme()}
            options={EDITOR_HIGHLIGHT_THEMES.map((t) => ({ value: t.id, label: t.label }))}
            onChange={(v) => setEditorHighlightTheme(v as EditorHighlightThemeId)}
          />

          <label class="settings-label">Default download directory</label>
          <input class="field" placeholder="~/Downloads"
                 value={field("default_download_dir") ?? ""}
                 onInput={(e) => patch("default_download_dir", (e.currentTarget.value.trim() || null) as string | null)} />

          <label class="settings-label">Transfer concurrency</label>
          <div class="num-field">
            <input type="number" min={1} max={16}
                   value={field("transfer_concurrency") ?? 3}
                   onChange={(e) => commitNum("transfer_concurrency", e, 1, 16, 1)} />
            <button type="button" class="num-field-btn" onClick={() => patch("transfer_concurrency", Math.max(1, (field("transfer_concurrency") ?? 3) - 1))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("transfer_concurrency", Math.min(16, (field("transfer_concurrency") ?? 3) + 1))}>+</button>
          </div>

          <label class="settings-label">Multipart parallelism</label>
          <div class="num-field">
            <input type="number" min={1} max={16}
                   value={field("multipart_parallelism") ?? 4}
                   onChange={(e) => commitNum("multipart_parallelism", e, 1, 16, 1)} />
            <button type="button" class="num-field-btn" onClick={() => patch("multipart_parallelism", Math.max(1, (field("multipart_parallelism") ?? 4) - 1))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("multipart_parallelism", Math.min(16, (field("multipart_parallelism") ?? 4) + 1))}>+</button>
          </div>

          <label class="settings-label">Multipart threshold (MB)</label>
          <div class="num-field">
            <input type="number" min={5}
                   value={Math.round((field("multipart_threshold_bytes") ?? 8388608) / 1048576)}
                   onChange={(e) => commitNum("multipart_threshold_bytes", e, 5, Infinity, 8, 1048576)} />
            <button type="button" class="num-field-btn" onClick={() => patch("multipart_threshold_bytes", Math.max(5 * 1048576, (field("multipart_threshold_bytes") ?? 8388608) - 1048576))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("multipart_threshold_bytes", (field("multipart_threshold_bytes") ?? 8388608) + 1048576)}>+</button>
          </div>

          <label class="settings-label">Part size (MB)</label>
          <div class="num-field">
            <input type="number" min={5}
                   value={Math.round((field("part_size_bytes") ?? 8388608) / 1048576)}
                   onChange={(e) => commitNum("part_size_bytes", e, 5, Infinity, 8, 1048576)} />
            <button type="button" class="num-field-btn" onClick={() => patch("part_size_bytes", Math.max(5 * 1048576, (field("part_size_bytes") ?? 8388608) - 1048576))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("part_size_bytes", (field("part_size_bytes") ?? 8388608) + 1048576)}>+</button>
          </div>

          <label class="settings-label">Presign expires (seconds)</label>
          <div class="num-field">
            <input type="number" min={60} max={604800}
                   value={field("presign_default_expires_secs") ?? 3600}
                   onChange={(e) => commitNum("presign_default_expires_secs", e, 60, 604800, 60)} />
            <button type="button" class="num-field-btn" onClick={() => patch("presign_default_expires_secs", Math.max(60, (field("presign_default_expires_secs") ?? 3600) - 60))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("presign_default_expires_secs", Math.min(604800, (field("presign_default_expires_secs") ?? 3600) + 60))}>+</button>
          </div>

          <label class="settings-label">HTTP proxy</label>
          <input class="field" placeholder="http://host:port (optional)"
                 value={field("http_proxy") ?? ""}
                 onInput={(e) => patch("http_proxy", (e.currentTarget.value.trim() || null) as string | null)} />

          <label class="settings-label">Custom CA cert path</label>
          <input class="field" placeholder="/path/to/cert.pem (optional)"
                 value={field("custom_ca_path") ?? ""}
                 onInput={(e) => patch("custom_ca_path", (e.currentTarget.value.trim() || null) as string | null)} />

          <label class="settings-label">Request log retention (days)</label>
          <div class="num-field">
            <input type="number" min={1} max={365}
                   value={field("request_log_ttl_days") ?? 30}
                   onChange={(e) => commitNum("request_log_ttl_days", e, 1, 365, 30)} />
            <button type="button" class="num-field-btn" onClick={() => patch("request_log_ttl_days", Math.max(1, (field("request_log_ttl_days") ?? 30) - 1))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("request_log_ttl_days", Math.min(365, (field("request_log_ttl_days") ?? 30) + 1))}>+</button>
          </div>

          <label class="settings-label">Show hidden files</label>
          <div><input type="checkbox" checked={field("show_hidden") ?? false}
                       onChange={(e) => patch("show_hidden", e.currentTarget.checked)} /></div>

          <label class="settings-label">Confirm destructive ops</label>
          <div><input type="checkbox" checked={field("confirm_destructive") ?? true}
                       onChange={(e) => patch("confirm_destructive", e.currentTarget.checked)} /></div>

          <label class="settings-label">Auto preview images and text</label>
          <div><input type="checkbox" checked={field("auto_preview") ?? false}
                       onChange={(e) => patch("auto_preview", e.currentTarget.checked)} /></div>
        </div>

        <div class="btn-row mt-4">
          <button class="btn-secondary" onClick={doReset} disabled={busy()}>Reset defaults</button>
          <button class="btn-primary" onClick={save} disabled={busy() || !dirty()}>
            {busy() ? "Saving…" : "Save changes"}
          </button>
        </div>
      </Show>
    </div>
  );
}
