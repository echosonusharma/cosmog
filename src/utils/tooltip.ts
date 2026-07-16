const SHOW_DELAY_MS = 2000;

export function installTooltip() {
  const el = document.createElement("div");
  el.className = "app-tooltip";
  document.body.appendChild(el);
  let hoverEl: HTMLElement | null = null;
  let showTimer: ReturnType<typeof setTimeout> | undefined;

  function clearTimer() {
    if (showTimer) { clearTimeout(showTimer); showTimer = undefined; }
  }

  function hide() {
    clearTimer();
    hoverEl = null;
    el.classList.remove("visible");
  }

  function show(target: HTMLElement, text: string) {
    el.textContent = text;
    const r = target.getBoundingClientRect();
    el.style.left = `${r.right + 10}px`;
    el.style.top = `${r.top + r.height / 2}px`;
    el.classList.add("visible");
  }

  function update(e: MouseEvent) {
    const target = (e.target as HTMLElement | null)?.closest?.("[data-tt]") as HTMLElement | null ?? null;
    if (target === hoverEl) return;
    clearTimer();
    el.classList.remove("visible");
    hoverEl = target;
    if (!target) return;
    const text = target.getAttribute("data-tt");
    if (!text) { hoverEl = null; return; }
    showTimer = setTimeout(() => {
      if (hoverEl === target) show(target, text);
    }, SHOW_DELAY_MS);
  }

  document.addEventListener("mouseover", update);
  document.addEventListener("mouseleave", hide, true);
  document.addEventListener("scroll", hide, true);
  window.addEventListener("blur", hide);
}
