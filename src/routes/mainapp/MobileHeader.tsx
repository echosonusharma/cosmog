import { createSignal, onMount, Show } from "solid-js";
import { IconSidebar, SunIcon, MoonIcon, toggleTheme } from "../../utils/icons";
import { resolvedTheme } from "../../state/theme";
import { currentView } from "../../state/app";
import { checkLatestVersion, type UpdateInfo } from "../../utils/updates";
import { UpdateModal } from "./UpdateModal";

function viewTitle() {
  switch (currentView()) {
    case "browse":    return "Browser";
    case "transfers": return "Transfers";
    case "logs":      return "Logs";
    case "settings":  return "Settings";
    default:          return "";
  }
}

export function MobileHeader(props: { onOpenSidebar: () => void }) {
  const [updateInfo, setUpdateInfo] = createSignal<UpdateInfo | null>(null);
  const [modalOpen, setModalOpen] = createSignal(false);

  onMount(() => {
    import("@tauri-apps/api/app")
      .then((m) => m.getVersion())
      .then((v) => checkLatestVersion(v))
      .then(setUpdateInfo)
      .catch(() => {});
  });

  return (
    <>
      <div class="mobile-header">
        <button
          class="mobile-header-btn"
          onClick={props.onOpenSidebar}
          aria-label="Open sidebar"
        >
          <IconSidebar size={18} />
        </button>
        <div class="mobile-header-brand">
          <img src="/app-icon.svg" width="22" height="22" class="mobile-header-logo" alt="" />
          <span class="mobile-header-appname">Cosmog</span>
        </div>
        <Show when={updateInfo()}>
          <button class="mobile-header-update-badge" onClick={() => setModalOpen(true)}>
            v{updateInfo()!.version} available
          </button>
        </Show>
        <div class="mobile-header-section">{viewTitle()}</div>
        <button
          class="mobile-header-btn"
          onClick={toggleTheme}
          aria-label="Toggle theme"
        >
          {resolvedTheme() === "dark" ? <SunIcon size={15} /> : <MoonIcon size={15} />}
        </button>
      </div>
      <Show when={modalOpen() && updateInfo()}>
        <UpdateModal info={updateInfo()!} onClose={() => setModalOpen(false)} />
      </Show>
    </>
  );
}
