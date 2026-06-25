import { createSignal, createEffect, createRoot } from "solid-js";

export type Theme = "light" | "dark" | "system";

const [pref, setPref] = createSignal<Theme>("system");
const [resolved, setResolved] = createSignal<"dark" | "light">("dark");

export { pref as themePref };
export { resolved as resolvedTheme };

function systemDark(): boolean {
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? true;
}

function apply(t: Theme) {
  const r = t === "system" ? (systemDark() ? "dark" : "light") : t;
  setResolved(r);
  document.documentElement.setAttribute("data-theme", r);
}

export function setTheme(t: Theme) {
  setPref(t);
  apply(t);
}

if (window.matchMedia) {
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    if (pref() === "system") apply("system");
  });
}

createRoot(() => createEffect(() => apply(pref())));
