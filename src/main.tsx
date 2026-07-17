import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-sans/600.css";
import "@fontsource/ibm-plex-sans/700.css";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import "@fontsource/ibm-plex-mono/600.css";
import { render } from "solid-js/web";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";
import "./styles/index.css";

if (import.meta.env.DEV) {
  // F12 / Ctrl+Shift+I open devtools — dev builds only
  document.addEventListener("keydown", (e) => {
    if (e.key === "F12" || (e.ctrlKey && e.shiftKey && e.key === "I")) {
      e.preventDefault();
      invoke("open_devtools").catch(() => {});
    }
  });
}

// Block native context menu everywhere (components show their own)
document.addEventListener("contextmenu", (e) => e.preventDefault());

render(() => <App />, document.getElementById("root")!);
