import { createSignal, onMount, Show } from "solid-js";
import { Select } from "../../utils/Select";
import { t } from "../../i18n";
import { setTheme, themePref, type Theme } from "../../state/theme";
import { appSettings, loadSettings, saveSettings, resetAllSettings } from "../../state/settings";
import { editorHighlightTheme, setEditorHighlightTheme, EDITOR_HIGHLIGHT_THEMES, type EditorHighlightThemeId } from "../../state/editorTheme";
import { toast } from "../../state/toast";
import { confirmDialog } from "../../state/confirm";
import { parseSchema, settingsPatchSchema } from "../../validation";
import type { AppSettings } from "../../types";
import Spinner from "../../utils/Spinner";
import { LanguageSelect } from "./LanguageSelect";

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
  async function chooseTheme(theme: Theme) {
    setTheme(theme);
    try { await saveSettings({ theme }); } catch (e) { toast.err(e); }
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
      toast.ok(t("settings.form.saved"), t("settings.form.savedDetail"));
    } catch (e) { toast.err(e); }
    finally { setBusy(false); }
  }

  async function doReset() {
    const ok = await confirmDialog({
      title: t("settings.form.resetTitle"),
      body: t("settings.form.resetBody"),
      confirmLabel: t("common.reset"),
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    try {
      const s = await resetAllSettings();
      setTheme(s.theme ?? "system");
      setForm({});
      toast.ok(t("settings.form.restored"), t("settings.form.restoredDetail"));
    } catch (e) { toast.err(e); }
    finally { setBusy(false); }
  }

  const dirty = () => Object.keys(form()).length > 0;

  return (
    <div class="settings-section">
      <div class="settings-section-title">{t("settings.form.general")}</div>
      <Show when={loading() && !appSettings()}>
        <div class="loading-row"><Spinner /> {t("settings.form.loading")}</div>
      </Show>
      <Show when={appSettings()}>
        <div class="settings-grid">
          <label class="settings-label">{t("settings.form.theme")}</label>
          <Select
            value={themePref()}
            options={[
              { value: "system", label: t("settings.theme.system") },
              { value: "dark", label: t("settings.theme.dark") },
              { value: "light", label: t("settings.theme.light") },
            ]}
            onChange={(v) => chooseTheme(v as Theme)}
          />

          <label class="settings-label">{t("settings.language")}</label>
          <LanguageSelect />

          <label class="settings-label">{t("settings.form.editorTheme")}</label>
          <Select
            value={editorHighlightTheme()}
            options={EDITOR_HIGHLIGHT_THEMES.map((th) => ({ value: th.id, label: th.label }))}
            onChange={(v) => setEditorHighlightTheme(v as EditorHighlightThemeId)}
          />

          <label class="settings-label">{t("settings.form.downloadDir")}</label>
          <input class="field" placeholder="~/Downloads"
                 value={field("default_download_dir") ?? ""}
                 onInput={(e) => patch("default_download_dir", (e.currentTarget.value.trim() || null) as string | null)} />

          <label class="settings-label">{t("settings.form.concurrency")}</label>
          <div class="num-field">
            <input type="number" min={1} max={16}
                   value={field("transfer_concurrency") ?? 3}
                   onChange={(e) => commitNum("transfer_concurrency", e, 1, 16, 1)} />
            <button type="button" class="num-field-btn" onClick={() => patch("transfer_concurrency", Math.max(1, (field("transfer_concurrency") ?? 3) - 1))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("transfer_concurrency", Math.min(16, (field("transfer_concurrency") ?? 3) + 1))}>+</button>
          </div>

          <label class="settings-label">{t("settings.form.parallelism")}</label>
          <div class="num-field">
            <input type="number" min={1} max={16}
                   value={field("multipart_parallelism") ?? 4}
                   onChange={(e) => commitNum("multipart_parallelism", e, 1, 16, 1)} />
            <button type="button" class="num-field-btn" onClick={() => patch("multipart_parallelism", Math.max(1, (field("multipart_parallelism") ?? 4) - 1))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("multipart_parallelism", Math.min(16, (field("multipart_parallelism") ?? 4) + 1))}>+</button>
          </div>

          <label class="settings-label">{t("settings.form.multipartThreshold")}</label>
          <div class="num-field">
            <input type="number" min={5}
                   value={Math.round((field("multipart_threshold_bytes") ?? 8388608) / 1048576)}
                   onChange={(e) => commitNum("multipart_threshold_bytes", e, 5, Infinity, 8, 1048576)} />
            <button type="button" class="num-field-btn" onClick={() => patch("multipart_threshold_bytes", Math.max(5 * 1048576, (field("multipart_threshold_bytes") ?? 8388608) - 1048576))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("multipart_threshold_bytes", (field("multipart_threshold_bytes") ?? 8388608) + 1048576)}>+</button>
          </div>

          <label class="settings-label">{t("settings.form.partSize")}</label>
          <div class="num-field">
            <input type="number" min={5}
                   value={Math.round((field("part_size_bytes") ?? 8388608) / 1048576)}
                   onChange={(e) => commitNum("part_size_bytes", e, 5, Infinity, 8, 1048576)} />
            <button type="button" class="num-field-btn" onClick={() => patch("part_size_bytes", Math.max(5 * 1048576, (field("part_size_bytes") ?? 8388608) - 1048576))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("part_size_bytes", (field("part_size_bytes") ?? 8388608) + 1048576)}>+</button>
          </div>

          <label class="settings-label">{t("settings.form.presignExpires")}</label>
          <div class="num-field">
            <input type="number" min={60} max={604800}
                   value={field("presign_default_expires_secs") ?? 3600}
                   onChange={(e) => commitNum("presign_default_expires_secs", e, 60, 604800, 60)} />
            <button type="button" class="num-field-btn" onClick={() => patch("presign_default_expires_secs", Math.max(60, (field("presign_default_expires_secs") ?? 3600) - 60))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("presign_default_expires_secs", Math.min(604800, (field("presign_default_expires_secs") ?? 3600) + 60))}>+</button>
          </div>

          <label class="settings-label">{t("settings.form.proxy")}</label>
          <input class="field" placeholder={t("settings.form.proxyPlaceholder")}
                 value={field("http_proxy") ?? ""}
                 onInput={(e) => patch("http_proxy", (e.currentTarget.value.trim() || null) as string | null)} />

          <label class="settings-label">{t("settings.form.caPath")}</label>
          <input class="field" placeholder={t("settings.form.caPlaceholder")}
                 value={field("custom_ca_path") ?? ""}
                 onInput={(e) => patch("custom_ca_path", (e.currentTarget.value.trim() || null) as string | null)} />

          <label class="settings-label">{t("settings.form.logRetention")}</label>
          <div class="num-field">
            <input type="number" min={1} max={365}
                   value={field("request_log_ttl_days") ?? 30}
                   onChange={(e) => commitNum("request_log_ttl_days", e, 1, 365, 30)} />
            <button type="button" class="num-field-btn" onClick={() => patch("request_log_ttl_days", Math.max(1, (field("request_log_ttl_days") ?? 30) - 1))}>−</button>
            <button type="button" class="num-field-btn" onClick={() => patch("request_log_ttl_days", Math.min(365, (field("request_log_ttl_days") ?? 30) + 1))}>+</button>
          </div>

          <label class="settings-label">{t("settings.form.showHidden")}</label>
          <div><input type="checkbox" checked={field("show_hidden") ?? false}
                       onChange={(e) => patch("show_hidden", e.currentTarget.checked)} /></div>

          <label class="settings-label">{t("settings.form.confirmDestructive")}</label>
          <div><input type="checkbox" checked={field("confirm_destructive") ?? true}
                       onChange={(e) => patch("confirm_destructive", e.currentTarget.checked)} /></div>

          <label class="settings-label">{t("settings.form.autoPreview")}</label>
          <div><input type="checkbox" checked={field("auto_preview") ?? false}
                       onChange={(e) => patch("auto_preview", e.currentTarget.checked)} /></div>
        </div>

        <div class="btn-row mt-4">
          <button class="btn-secondary" onClick={doReset} disabled={busy()}>{t("settings.form.resetDefaults")}</button>
          <button class="btn-primary" onClick={save} disabled={busy() || !dirty()}>
            {busy() ? t("common.saving") : t("settings.form.saveChanges")}
          </button>
        </div>
      </Show>
    </div>
  );
}
