import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-sans/600.css";
import "@fontsource/ibm-plex-sans/700.css";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import "@fontsource/ibm-plex-mono/600.css";
import { createSignal } from "solid-js";
import { render } from "solid-js/web";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";
import BootScreen from "./routes/BootScreen";
import { boot } from "./boot";
import "./styles/index.css";

if (import.meta.env.DEV) {
  document.addEventListener("keydown", (e) => {
    if (e.key === "F12" || (e.ctrlKey && e.shiftKey && e.key === "I")) {
      e.preventDefault();
      invoke("open_devtools").catch(() => {});
    }
  });
}

// Block native context menu everywhere (components show their own)
document.addEventListener("contextmenu", (e) => e.preventDefault());

// Sync render swaps the static splash; boot() advances stage labels.
const root = document.getElementById("root")!;
// Inner panes can flash scrollbars while content settles; hide until booted.
document.body.classList.add("booting");
const [stage, setStage] = createSignal("Loading preferences…");
const disposeBoot = render(() => <BootScreen stage={stage} />, root);
document.getElementById("boot-splash")?.remove();

void boot(setStage).then(({ error }) => {
  disposeBoot();
  render(() => <App bootError={error} />, root);
  requestAnimationFrame(() => document.body.classList.remove("booting"));
});
