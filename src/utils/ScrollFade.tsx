import { onCleanup, onMount, type JSX } from "solid-js";

// Scroll box that marks itself when content is clipped below, so CSS can fade
// the cut-off row. Flag clears once scrolled to the end.
export function ScrollFade(props: { class?: string; children: JSX.Element }) {
  let el!: HTMLDivElement;

  function sync() {
    if (!el) return;
    const more = el.scrollHeight - el.clientHeight - el.scrollTop > 2;
    el.dataset.overflow = more ? "true" : "false";
  }

  onMount(() => {
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    // Rows come and go with filters, so watch the list contents too.
    const mo = new MutationObserver(sync);
    mo.observe(el, { childList: true, subtree: true, characterData: true });
    el.addEventListener("scroll", sync, { passive: true });
    sync();
    onCleanup(() => {
      ro.disconnect();
      mo.disconnect();
      el.removeEventListener("scroll", sync);
    });
  });

  return (
    <div ref={el} class={props.class} data-overflow="false">
      {props.children}
    </div>
  );
}
