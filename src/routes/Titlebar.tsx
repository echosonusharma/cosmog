import { createResource, createSignal, createEffect, Show } from "solid-js";
import { resolvedTheme } from "../state/theme";
import { isMobile } from "../utils/breakpoint";
import { SunIcon, MoonIcon, toggleTheme } from "../utils/icons";
import { t } from "../i18n";
import { checkLatestVersion, type UpdateInfo } from "../utils/updates";
import { UpdateModal } from "./mainapp/UpdateModal";
import { WindowControls } from "./titlebar/WindowControls";
import { desktopOs, isAndroid } from "../utils/platform";

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const getVersion = isTauri
  ? () => import("@tauri-apps/api/app").then((m) => m.getVersion())
  : () => Promise.resolve("");

export default function Titlebar() {
  const [version] = createResource(getVersion);
  const [updateInfo, setUpdateInfo] = createSignal<UpdateInfo | null>(null);
  const [modalOpen, setModalOpen] = createSignal(false);

  const showControls = () => isTauri && !isAndroid && !isMobile();

  createEffect(() => {
    const v = version();
    if (!v) return;
    checkLatestVersion(v).then(setUpdateInfo);
  });

  return (
    <>
      <div class={showControls() ? `titlebar titlebar-${desktopOs}` : "titlebar"} data-tauri-drag-region>
        <Show when={showControls() && desktopOs === "macos"}>
          <WindowControls os="macos" />
        </Show>
        <div class="titlebar-left" data-tauri-drag-region>
          <img src="/app-icon.svg" width="22" height="22" class="titlebar-logo" alt="" />
          <span class="titlebar-appname">Cosmog</span>
          <span class="titlebar-version">v{version()}</span>
          <Show when={updateInfo()}>
            <button class="titlebar-update-badge" onClick={() => setModalOpen(true)}>
              {t("shell.update.available", { version: updateInfo()!.version })}
            </button>
          </Show>
        </div>
        <div class="titlebar-controls">
          <button class="titlebar-btn" aria-label={t("shell.nav.toggleTheme")} title={t("shell.nav.toggleTheme")} onClick={toggleTheme}>
            {resolvedTheme() === "dark" ? <SunIcon /> : <MoonIcon />}
          </button>
          <Show when={showControls() && desktopOs !== "macos"}>
            <WindowControls os={desktopOs} />
          </Show>
        </div>
      </div>
      <Show when={modalOpen() && updateInfo()}>
        <UpdateModal info={updateInfo()!} onClose={() => setModalOpen(false)} />
      </Show>
    </>
  );
}
