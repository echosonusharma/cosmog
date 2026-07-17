import { IconSidebar } from "../../utils/icons";
import { resolvedTheme, setTheme } from "../../state/theme";
import { currentView } from "../../state/app";

function viewTitle() {
  switch (currentView()) {
    case "browse":    return "Browser";
    case "transfers": return "Transfers";
    case "logs":      return "Logs";
    case "settings":  return "Settings";
    default:          return "";
  }
}

function toggleTheme() {
  setTheme(resolvedTheme() === "dark" ? "light" : "dark");
}

function SunIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round">
      <circle cx="12" cy="12" r="4"/>
      <path d="M12 2v2M12 20v2M4.22 4.22l1.42 1.42M18.36 18.36l1.42 1.42M2 12h2M20 12h2M4.22 19.78l1.42-1.42M18.36 5.64l1.42-1.42"/>
    </svg>
  );
}

function MoonIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round">
      <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/>
    </svg>
  );
}

export function MobileHeader(props: { onOpenSidebar: () => void }) {
  return (
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
      <div class="mobile-header-section">{viewTitle()}</div>
      <button
        class="mobile-header-btn"
        onClick={toggleTheme}
        aria-label="Toggle theme"
      >
        {resolvedTheme() === "dark" ? <SunIcon /> : <MoonIcon />}
      </button>
    </div>
  );
}
