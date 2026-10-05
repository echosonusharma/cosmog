import { Show, createSignal, createMemo, createResource, createEffect, onCleanup } from "solid-js";
import type { JSX } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { getVersion, getTauriVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { IconBug } from "../../utils/icons";
import { t } from "../../i18n";
import Spinner from "../../utils/Spinner";

// Swaps {name} placeholders in a message for JSX nodes.
function rich(msg: string, nodes: Record<string, JSX.Element>): JSX.Element[] {
  return msg.split(/(\{\w+\})/).map((p) => (/^\{\w+\}$/.test(p) ? nodes[p.slice(1, -1)] ?? p : p));
}

const GITHUB_ISSUES_URL = "https://github.com/echosonusharma/cosmog/issues";

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

type DeviceInfo = { os: string; os_version: string; arch: string; model: string | null };

async function loadBugInfo() {
  const appVersion = isTauri ? await getVersion() : "unknown";
  const tauriVersion = isTauri ? await getTauriVersion() : "unknown";
  // navigator only knows the WebView runtime (e.g. "Linux aarch64" on Android),
  // so pull the real OS/arch/device from the native backend when available.
  const device: DeviceInfo | null = isTauri
    ? await invoke<DeviceInfo>("get_device_info").catch(() => null)
    : null;
  const ua = navigator.userAgent;
  const os = device ? `${device.os} ${device.os_version}` : navigator.platform;
  const arch = device?.arch ?? "unknown";
  const model = device?.model ?? null;
  const screen = `${window.screen.width}x${window.screen.height} @${window.devicePixelRatio}x`;
  const locale = navigator.language;
  return { appVersion, tauriVersion, ua, os, arch, model, screen, locale };
}

export function BugReportModal(props: { onClose: () => void }) {
  const [info] = createResource(loadBugInfo);
  const [copied, setCopied] = createSignal(false);
  const [showLoader, setShowLoader] = createSignal(true);
  const loadStartedAt = Date.now();

  createEffect(() => {
    if (info.error) {
      setShowLoader(false);
      return;
    }
    const data = info.latest;
    if (!data) return;
    const remaining = Math.max(0, 450 - (Date.now() - loadStartedAt));
    const t = setTimeout(() => setShowLoader(false), remaining);
    onCleanup(() => clearTimeout(t));
  });

  const infoReady = createMemo(() => !showLoader() && !!info.latest);

  const infoText = () => {
    const d = info.latest;
    if (!d) return "";
    const rows = [
      `App Version:   ${d.appVersion}`,
      `Tauri Version: ${d.tauriVersion}`,
      `OS:            ${d.os}`,
      `Arch:          ${d.arch}`,
    ];
    if (d.model) rows.push(`Device:        ${d.model}`);
    rows.push(
      `Screen:        ${d.screen}`,
      `Locale:        ${d.locale}`,
      `User Agent:    ${d.ua}`,
    );
    return rows.join("\n");
  };

  function copyInfo() {
    navigator.clipboard.writeText(infoText()).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  }

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="bug-modal" onClick={(e) => e.stopPropagation()}>
        <div class="bug-modal-header">
          <span class="bug-modal-title">
            <span class="bug-modal-icon-wrap"><IconBug size={15} /></span>
            {t("shell.bug.title")}
          </span>
          <button class="cd-modal-close" onClick={props.onClose} aria-label={t("common.close")}>✕</button>
        </div>

        <div class="bug-modal-body">
          <p class="bug-modal-desc">
            {t("shell.bug.desc")}
          </p>

          <div class="bug-info-section">
            <div class="bug-info-header">
              <span class="bug-info-label">{t("shell.bug.systemInfo")}</span>
              <button class="bug-copy-btn" disabled={!infoReady()} onClick={copyInfo}>
                {copied() ? t("common.copied") : t("common.copy")}
              </button>
            </div>
            <Show when={infoReady()}>
              <dl class="bug-info-grid">
                <dt>{t("shell.bug.appVersion")}</dt><dd>{info.latest!.appVersion}</dd>
                <dt>{t("shell.bug.tauriVersion")}</dt><dd>{info.latest!.tauriVersion}</dd>
                <dt>{t("shell.bug.os")}</dt><dd>{info.latest!.os}</dd>
                <dt>{t("shell.bug.arch")}</dt><dd>{info.latest!.arch}</dd>
                <Show when={info.latest!.model}>
                  <dt>{t("shell.bug.device")}</dt><dd>{info.latest!.model}</dd>
                </Show>
                <dt>{t("shell.bug.screen")}</dt><dd>{info.latest!.screen}</dd>
                <dt>{t("shell.bug.locale")}</dt><dd>{info.latest!.locale}</dd>
                <dt>{t("shell.bug.userAgent")}</dt><dd>{info.latest!.ua}</dd>
              </dl>
            </Show>
            <Show when={showLoader() && !info.error}>
              <div class="bug-info-loading" aria-busy="true">
                <Spinner size={50} />
                <span>{t("shell.bug.loading")}</span>
              </div>
            </Show>
            <Show when={info.error}>
              <div class="bug-info-error">{t("shell.bug.loadError")}</div>
            </Show>
          </div>

          <div class="bug-steps">
            <div class="bug-steps-title">{t("shell.bug.howTo")}</div>
            <ol class="bug-steps-list">
              <li>{rich(t("shell.bug.step1"), { copy: <strong>{t("common.copy")}</strong> })}</li>
              <li>{rich(t("shell.bug.step2"), { open: <strong>{t("shell.bug.openIssue")}</strong> })}</li>
              <li>{rich(t("shell.bug.step3"), { context: <em>Additional context</em> })}</li>
            </ol>
          </div>

          <button
            class="bug-open-btn"
            onClick={() => openUrl(GITHUB_ISSUES_URL).catch(() => {})}
          >
            {t("shell.bug.openIssue")}
          </button>
        </div>
      </div>
    </div>
  );
}
