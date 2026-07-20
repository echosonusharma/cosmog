import { createSignal } from "solid-js";

const MOBILE_MAX = 768;

const mql = typeof window !== "undefined" ? window.matchMedia(`(max-width: ${MOBILE_MAX}px)`) : null;

const [isMobile, setIsMobile] = createSignal(mql?.matches ?? false);

if (mql) {
  mql.addEventListener("change", (e) => setIsMobile(e.matches));
}

export { isMobile };
