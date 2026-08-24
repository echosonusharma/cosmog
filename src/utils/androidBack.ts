import { createEffect, onCleanup } from "solid-js";

// App state is signal-driven (WebView canGoBack() is always false), so MainActivity forwards every
// back press to window.__androidBack(); handlers run LIFO (later-registered child/overlay wins).

type BackHandler = () => boolean;

const handlers: BackHandler[] = [];

export function pushBackHandler(fn: BackHandler): () => void {
  handlers.push(fn);
  return () => {
    const i = handlers.lastIndexOf(fn);
    if (i >= 0) handlers.splice(i, 1);
  };
}

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
