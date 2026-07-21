import { createEffect, onCleanup } from "solid-js";

// Android back button / back gesture handling.
//
// The app is a single SolidJS view driven by signals, not the browser history,
// so the WebView's canGoBack() is always false and the OS back action would
// exit the app. Instead MainActivity forwards every back press to
// window.__androidBack(); we run a LIFO stack of handlers and return true when
// one consumes the press. false lets the OS proceed (background / exit).
//
// Handlers registered later sit on top. Since a child component mounts after
// its parent, a nested overlay's handler is checked before the shell's — the
// back press unwinds the UI from the most-nested layer outward.

type BackHandler = () => boolean;

const handlers: BackHandler[] = [];

export function pushBackHandler(fn: BackHandler): () => void {
  handlers.push(fn);
  return () => {
    const i = handlers.lastIndexOf(fn);
    if (i >= 0) handlers.splice(i, 1);
  };
}

// Returns true if some handler consumed the back press.
function runBack(): boolean {
  for (let i = handlers.length - 1; i >= 0; i--) {
    if (handlers[i]()) return true;
  }
  return false;
}

// Register `fn` only while `active()` is true (e.g. an overlay is open).
export function useBackHandler(active: () => boolean, fn: BackHandler): void {
  createEffect(() => {
    if (!active()) return;
    const off = pushBackHandler(fn);
    onCleanup(off);
  });
}

if (typeof window !== "undefined") {
  (window as unknown as { __androidBack: () => boolean }).__androidBack = runBack;
}
