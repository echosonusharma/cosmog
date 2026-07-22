import { IconSidebar, SunIcon, MoonIcon, toggleTheme } from "../../utils/icons";
import { resolvedTheme } from "../../state/theme";
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
        {resolvedTheme() === "dark" ? <SunIcon size={15} /> : <MoonIcon size={15} />}
      </button>
    </div>
  );
}
