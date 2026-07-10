import { createSignal, createResource, Show } from "solid-js";
import { Select } from "../../utils/Select";
import { getSettings, updateSettings, resetSettings } from "../../api/settings";
import { setTheme } from "../../state/theme";
import { toast } from "../../state/toast";
import { confirmDialog } from "../../state/confirm";
import type { AppSettings } from "../../types";

// ── general ────────────────────────────────────────────────────────────────────

export function SettingsForm() {
  const [settings, { refetch }] = createResource(getSettings);
  const [busy, setBusy] = createSignal(false);
  const [form, setForm] = createSignal<Partial<AppSettings>>({});

  function field<K extends keyof AppSettings>(key: K): AppSettings[K] | undefined {
    const over = form() as Partial<AppSettings>;
    if (key in over) return over[key] as AppSettings[K];
    return settings()?.[key];
  }

  function patch<K extends keyof AppSettings>(key: K, val: AppSettings[K]) {
    setForm((p) => ({ ...p, [key]: val }));
    if (key === "theme") setTheme(val as "light" | "dark" | "system");
  }

  async function save() {
    setBusy(true);
    try {
      await updateSettings(form());
      setForm({});
      await refetch();
      toast.ok("Settings saved");
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
      const s = await resetSettings();
      setTheme(s.theme ?? "system");
      setForm({});
      await refetch();
      toast.ok("Defaults restored");
    } catch (e) { toast.err(e); }
    finally { setBusy(false); }
  }

  const dirty = () => Object.keys(form()).length > 0;

  return (
    <div class="settings-section">
      <div class="settings-section-title">General</div>
      <Show when={settings.loading}>
        <div class="loading-row"><span class="spinner" /> Loading settings…</div>
      </Show>
      <Show when={!settings.loading && settings()}>
        <div class="settings-grid">
          <label class="settings-label">Theme</label>
          <Select
            value={field("theme") ?? "system"}
            options={[
              { value: "system", label: "System" },
              { value: "dark", label: "Dark" },
              { value: "light", label: "Light" },
            ]}
            onChange={(v) => patch("theme", v as "light" | "dark" | "system")}
          />

          <label class="settings-label">Default download directory</label>
          <input class="field" placeholder="~/Downloads"
                 value={field("default_download_dir") ?? ""}
                 onInput={(e) => patch("default_download_dir", (e.currentTarget.value.trim() || null) as string | null)} />

          <label class="settings-label">Transfer concurrency</label>
          <div class="num-field">
            <input type="number" min={1} max={16}
                   value={field("transfer_concurrency") ?? 3}
                   onInput={(e) => patch("transfer_concurrency", Math.min(16, Math.max(1, parseInt(e.currentTarget.value) || 1)))} />
            <button type="button" class="num-field-btn" onClick={() => patch("transfer_concurrency", Math.max(1, (field("transfer_concurrency") ?? 3) - 1))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("transfer_concurrency", Math.min(16, (field("transfer_concurrency") ?? 3) + 1))}>+</button>
          </div>

          <label class="settings-label">Multipart parallelism</label>
          <div class="num-field">
            <input type="number" min={1} max={16}
                   value={field("multipart_parallelism") ?? 4}
                   onInput={(e) => patch("multipart_parallelism", Math.min(16, Math.max(1, parseInt(e.currentTarget.value) || 1)))} />
            <button type="button" class="num-field-btn" onClick={() => patch("multipart_parallelism", Math.max(1, (field("multipart_parallelism") ?? 4) - 1))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("multipart_parallelism", Math.min(16, (field("multipart_parallelism") ?? 4) + 1))}>+</button>
          </div>

          <label class="settings-label">Multipart threshold (MB)</label>
          <div class="num-field">
            <input type="number" min={5}
                   value={Math.round((field("multipart_threshold_bytes") ?? 8388608) / 1048576)}
                   onInput={(e) => patch("multipart_threshold_bytes", Math.max(5, parseInt(e.currentTarget.value) || 8) * 1048576)} />
            <button type="button" class="num-field-btn" onClick={() => patch("multipart_threshold_bytes", Math.max(5 * 1048576, (field("multipart_threshold_bytes") ?? 8388608) - 1048576))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("multipart_threshold_bytes", (field("multipart_threshold_bytes") ?? 8388608) + 1048576)}>+</button>
          </div>

          <label class="settings-label">Part size (MB)</label>
          <div class="num-field">
            <input type="number" min={5}
                   value={Math.round((field("part_size_bytes") ?? 8388608) / 1048576)}
                   onInput={(e) => patch("part_size_bytes", Math.max(5, parseInt(e.currentTarget.value) || 8) * 1048576)} />
            <button type="button" class="num-field-btn" onClick={() => patch("part_size_bytes", Math.max(5 * 1048576, (field("part_size_bytes") ?? 8388608) - 1048576))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("part_size_bytes", (field("part_size_bytes") ?? 8388608) + 1048576)}>+</button>
          </div>

          <label class="settings-label">Presign expires (seconds)</label>
          <div class="num-field">
            <input type="number" min={60} max={604800}
                   value={field("presign_default_expires_secs") ?? 3600}
                   onInput={(e) => patch("presign_default_expires_secs", Math.min(604800, Math.max(60, parseInt(e.currentTarget.value) || 60)))} />
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
                   value={field("request_log_ttl_days") ?? 14}
                   onInput={(e) => patch("request_log_ttl_days", Math.min(365, Math.max(1, parseInt(e.currentTarget.value) || 14)))} />
            <button type="button" class="num-field-btn" onClick={() => patch("request_log_ttl_days", Math.max(1, (field("request_log_ttl_days") ?? 14) - 1))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("request_log_ttl_days", Math.min(365, (field("request_log_ttl_days") ?? 14) + 1))}>+</button>
          </div>

          <label class="settings-label">Show hidden files</label>
          <div><input type="checkbox" checked={field("show_hidden") ?? false}
                       onChange={(e) => patch("show_hidden", e.currentTarget.checked)} /></div>

          <label class="settings-label">Confirm destructive ops</label>
          <div><input type="checkbox" checked={field("confirm_destructive") ?? true}
                       onChange={(e) => patch("confirm_destructive", e.currentTarget.checked)} /></div>
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
