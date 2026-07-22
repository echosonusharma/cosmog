import { createResource, Show } from "solid-js";
import { resolvedTheme } from "../state/theme";
import { isMobile } from "../utils/breakpoint";
import { SunIcon, MoonIcon, toggleTheme } from "../utils/icons";

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

let win: { minimize(): void; toggleMaximize(): void; close(): void } | null = null;
if (isTauri) {
  import("@tauri-apps/api/window").then((m) => { win = m.getCurrentWindow(); });
}

const getVersion = isTauri
  ? () => import("@tauri-apps/api/app").then((m) => m.getVersion())
  : () => Promise.resolve("");

export default function Titlebar() {
  const [version] = createResource(getVersion);
  return (
    <div class="titlebar" data-tauri-drag-region>
      <div class="titlebar-left" data-tauri-drag-region>
        <img src="/app-icon.svg" width="22" height="22" class="titlebar-logo" alt="" />
        <span class="titlebar-appname">Cosmog</span>
        <span class="titlebar-version">v{version()}</span>
      </div>
      <div class="titlebar-controls">
        <button class="titlebar-btn" onClick={toggleTheme}>
          {resolvedTheme() === "dark" ? <SunIcon /> : <MoonIcon />}
        </button>
        <Show when={!isMobile()}>
          <div class="titlebar-sep" />
        </Show>
        <Show when={isTauri && !isMobile()}>
          <button class="titlebar-btn" onClick={() => win?.minimize()}>
            <svg width="13" height="13" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round">
              <path d="M5 12h14"/>
            </svg>
          </button>
          <button class="titlebar-btn" onClick={() => win?.toggleMaximize()}>
            <svg width="11" height="11" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.9" fill="none">
              <rect x="4" y="4" width="16" height="16" rx="2.5"/>
            </svg>
          </button>
          <button class="titlebar-btn close" onClick={() => win?.close()}>
            <svg width="13" height="13" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round">
              <path d="M6 6l12 12M18 6L6 18"/>
            </svg>
          </button>
        </Show>
      </div>
    </div>
  );
}
