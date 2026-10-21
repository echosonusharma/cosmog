import { Match, Switch } from "solid-js";
import type { DesktopOs } from "../../utils/platform";
import { t } from "../../i18n";
import { useWindowState } from "./windowState";

// Window buttons drawn to look like each OS's native ones (decorations are off).
export function WindowControls(props: { os: DesktopOs }) {
  const state = useWindowState();
  return (
    <Switch>
      <Match when={props.os === "macos"}><MacControls {...state} /></Match>
      <Match when={props.os === "windows"}><WinControls {...state} /></Match>
      <Match when={props.os === "linux"}><LinuxControls {...state} /></Match>
    </Switch>
  );
}

type State = ReturnType<typeof useWindowState>;

// Traffic lights: close, minimize, zoom. Green enters full screen; Option-click zooms, like macOS.
function MacControls(s: State) {
  async function zoom(e: MouseEvent) {
    const w = s.win();
    if (!w) return;
    if (e.altKey) await w.toggleMaximize();
    else await w.setFullscreen(!(await w.isFullscreen()));
  }
  return (
    <div class="wc wc-mac" classList={{ "is-blurred": !s.focused() }}>
      <button class="wc-btn wc-close" aria-label={t("shell.window.close")} onClick={() => s.win()?.close()}>
        <svg viewBox="0 0 12 12"><path d="M3.8 3.8l4.4 4.4M8.2 3.8L3.8 8.2" stroke="currentColor" stroke-width="1.2" /></svg>
      </button>
      <button class="wc-btn wc-min" aria-label={t("shell.window.minimize")} onClick={() => s.win()?.minimize()}>
        <svg viewBox="0 0 12 12"><path d="M3 6h6" stroke="currentColor" stroke-width="1.2" /></svg>
      </button>
      <button class="wc-btn wc-max" aria-label={t("shell.window.maximize")} onClick={zoom}>
        <svg viewBox="0 0 12 12"><path d="M3.6 8.4V4.8l3.6 3.6zM8.4 3.6v3.6L4.8 3.6z" fill="currentColor" /></svg>
      </button>
    </div>
  );
}

// Windows 11 caption buttons: wide, square, flush right; close turns red on hover.
function WinControls(s: State) {
  const maxLabel = () => t(s.maximized() ? "shell.window.restore" : "shell.window.maximize");
  return (
    <div class="wc wc-win" classList={{ "is-blurred": !s.focused() }}>
      <button class="wc-btn" title={t("shell.window.minimize")} aria-label={t("shell.window.minimize")} onClick={() => s.win()?.minimize()}>
        <svg viewBox="0 0 10 10"><path d="M0 5.5h10" stroke="currentColor" /></svg>
      </button>
      <button class="wc-btn" title={maxLabel()} aria-label={maxLabel()} onClick={() => s.win()?.toggleMaximize()}>
        {s.maximized() ? (
          <svg viewBox="0 0 10 10" fill="none" stroke="currentColor">
            <rect x="0.5" y="2.5" width="7" height="7" rx="1" />
            <path d="M2.5 2.5v-.5a1.5 1.5 0 0 1 1.5-1.5h4a1.5 1.5 0 0 1 1.5 1.5v4a1.5 1.5 0 0 1-1.5 1.5h-.5" />
          </svg>
        ) : (
          <svg viewBox="0 0 10 10" fill="none" stroke="currentColor"><rect x="0.5" y="0.5" width="9" height="9" rx="1" /></svg>
        )}
      </button>
      <button class="wc-btn wc-close" title={t("shell.window.close")} aria-label={t("shell.window.close")} onClick={() => s.win()?.close()}>
        <svg viewBox="0 0 10 10"><path d="M0.5 0.5l9 9M9.5 0.5l-9 9" stroke="currentColor" /></svg>
      </button>
    </div>
  );
}

// GNOME / Adwaita: round tinted buttons, minimize, maximize, close on the right.
function LinuxControls(s: State) {
  const maxLabel = () => t(s.maximized() ? "shell.window.restore" : "shell.window.maximize");
  return (
    <div class="wc wc-linux" classList={{ "is-blurred": !s.focused() }}>
      <button class="wc-btn" aria-label={t("shell.window.minimize")} onClick={() => s.win()?.minimize()}>
        <svg viewBox="0 0 16 16"><path d="M4.5 11.5h7" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" /></svg>
      </button>
      <button class="wc-btn" aria-label={maxLabel()} onClick={() => s.win()?.toggleMaximize()}>
        {s.maximized() ? (
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
            <rect x="4.75" y="6.75" width="6.5" height="6.5" rx="1" />
            <path d="M6.75 4.75h3.5a2 2 0 0 1 2 2v3.5" stroke-linecap="round" />
          </svg>
        ) : (
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="4.25" y="4.25" width="7.5" height="7.5" rx="1" /></svg>
        )}
      </button>
      <button class="wc-btn" aria-label={t("shell.window.close")} onClick={() => s.win()?.close()}>
        <svg viewBox="0 0 16 16"><path d="M5 5l6 6M11 5l-6 6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" /></svg>
      </button>
    </div>
  );
}
