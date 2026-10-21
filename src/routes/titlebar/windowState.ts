import { createSignal, onCleanup } from "solid-js";
import type { Window } from "@tauri-apps/api/window";

// Current Tauri window plus its maximized/focused state, for the window controls.
export function useWindowState() {
  const [win, setWin] = createSignal<Window | null>(null);
  const [maximized, setMaximized] = createSignal(false);
  const [focused, setFocused] = createSignal(document.hasFocus());
  const unlisten: Array<() => void> = [];
  let disposed = false;
  onCleanup(() => {
    disposed = true;
    unlisten.forEach((u) => u());
  });

  void import("@tauri-apps/api/window").then(async ({ getCurrentWindow }) => {
    const w = getCurrentWindow();
    setWin(w);
    setMaximized(await w.isMaximized());
    const subs = await Promise.all([
      w.onResized(async () => setMaximized(await w.isMaximized())),
      w.onFocusChanged(({ payload }) => setFocused(payload)),
    ]);
    // Unmounted while subscribing: drop the listeners right away.
    if (disposed) subs.forEach((u) => u());
    else unlisten.push(...subs);
  });

  return { win, maximized, focused };
}
