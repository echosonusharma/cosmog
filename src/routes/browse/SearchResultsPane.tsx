import { Show, For, createSignal, createEffect, onMount, onCleanup } from "solid-js";
import type { Resource } from "solid-js";
import {
  FileIcon, fileTypeLabel,
  IconDownload, IconLink, IconSearch,
} from "../../utils/icons";
import { formatBytes, formatDate } from "../../utils/fmt";
import { navigateToPrefix } from "../../state/app";
import { highlightText } from "../../utils/highlight";
import type { CachedObjectMeta, BucketIndexStatus } from "../../types";

export function SearchResultsPane(props: {
  searchQuery: string;
  objects: CachedObjectMeta[];
  total: number;
  loading: boolean;
  loadingMore: boolean;
  hasMore: boolean;
  onLoadMore: () => void;
  indexStatus: Resource<BucketIndexStatus | undefined>;
  indexBusy: boolean;
  onEnableIndex: () => void;
  onSelectResult: (obj: CachedObjectMeta) => void;
  onCtxResult: (e: MouseEvent, obj: CachedObjectMeta) => void;
  onDownload: (obj: CachedObjectMeta) => void;
  onCopyLink: (obj: CachedObjectMeta) => void;
  onClearSearch: () => void;
}) {
  const [focusIdx, setFocusIdx] = createSignal(0);
  const [hoverLocked, setHoverLocked] = createSignal(false);
  let hoverLockTimer: ReturnType<typeof setTimeout> | undefined;
  let listEl: HTMLDivElement | undefined;
  const setListEl = (el: HTMLDivElement) => { listEl = el; };
  onCleanup(() => clearTimeout(hoverLockTimer));
  function lockHover(ms = 450) {
    setHoverLocked(true);
    clearTimeout(hoverLockTimer);
    hoverLockTimer = setTimeout(() => setHoverLocked(false), ms);
  }

  // Query change → jump to first row. Objects identity change (not just length)
  // also clamps, so a new 4-item result doesn't keep stale hover =1.
  createEffect(() => {
    props.searchQuery;
    lockHover(600);
    setFocusIdx(0);
  });
  createEffect(() => {
    const objs = props.objects;
    if (hoverLocked()) lockHover(250);
    const n = objs.length;
    setFocusIdx((i) => (n === 0 ? -1 : Math.max(0, Math.min(i, n - 1))));
  });

  createEffect(() => {
    const i = focusIdx();
    if (i < 0 || !listEl) return;
    const row = listEl.querySelectorAll<HTMLElement>(".obj-row")[i];
    row?.scrollIntoView({ block: "nearest" });
  });

  function activate(obj: CachedObjectMeta) {
    navigateToPrefix(obj.key.includes("/") ? obj.key.slice(0, obj.key.lastIndexOf("/") + 1) : "");
    props.onClearSearch();
    props.onSelectResult(obj);
  }

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      const objs = props.objects;
      if (!objs.length) return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.isComposing || e.keyCode === 229) return;

      const t = e.target as HTMLElement | null;
      if (!t) return;
      // Only when focus is in the search box or results list — don't steal
      // arrows/Enter from the rest of the browse chrome.
      const inSearchInput =
        t.tagName === "INPUT" && t.classList.contains("toolbar-search-input");
      const inResults = !!t.closest(".search-results-pane");
      if (!inSearchInput && !inResults) return;
      if (t.closest(".modal, .context-menu, [role='dialog']")) return;

      const n = objs.length;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setFocusIdx((i) => Math.min(n - 1, Math.max(0, i) + 1));
        lockHover(300);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setFocusIdx((i) => Math.max(0, (i < 0 ? 0 : i) - 1));
        lockHover(300);
      } else if (e.key === "Enter") {
        const i = focusIdx();
        if (i < 0 || i >= n) return;
        // Don't activate when Enter targets a button inside the row.
        if (t.closest(".obj-actions, button, a")) return;
        e.preventDefault();
        activate(objs[i]);
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  return (
    <div class="search-results-pane">
      {/* Latch: while a new query fetches, keep the previous results under a
          corner spinner instead of flashing "Searching…" on every keystroke. */}
      <Show when={props.loading && props.objects.length > 0}>
        <span class="spinner corner-spinner" />
      </Show>
      <Show when={props.loading && props.objects.length === 0}>
        <div class="loading-row"><span class="spinner" /> Searching…</div>
      </Show>
      <Show when={!props.loading || props.objects.length > 0}>
        <Show when={props.objects.length > 0}
              fallback={
                <Show when={!props.loading}>
                  <Show when={!props.indexStatus()?.enabled}
                        fallback={
                          <div class="empty-state">
                            <span class="empty-icon"><IconSearch size={32} /></span>
                            No results for "{props.searchQuery}"
                          </div>
                        }>
                    <div class="empty-state">
                      <span class="empty-icon"><IconSearch size={32} /></span>
                      <span>Bucket not indexed</span>
                      <button class="btn-primary search-enable-index-btn" disabled={props.indexBusy} onClick={props.onEnableIndex}>
                        Enable index
                      </button>
                    </div>
                  </Show>
                </Show>
              }>
          <div class="results-header">{props.total.toLocaleString()} matches</div>
          <div class="object-list search-results-list" ref={setListEl}>
            <For each={props.objects}>
              {(obj, i) => (
                <div
                  class="obj-row"
                  classList={{ "kb-active": focusIdx() === i() }}
                  onMouseMove={() => {
                    if (hoverLocked()) return;
                    setFocusIdx(i());
                  }}
                  onClick={() => activate(obj)}
                  onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); props.onCtxResult(e, obj); }}
                >
                  <div class="obj-name-cell">
                    <span class="obj-checkbox-spacer" />
                    <FileIcon name={obj.basename} />
                    <span class="obj-name">{highlightText(obj.key, props.searchQuery)}</span>
                  </div>
                  <div class="obj-type">{fileTypeLabel(obj.basename)}</div>
                  <div class="obj-size">{formatBytes(obj.size)}</div>
                  <div class="obj-date">{obj.last_modified ? formatDate(obj.last_modified) : "-"}</div>
                  <div class="obj-actions" onClick={(e) => e.stopPropagation()}>
                    <button class="icon-btn" onClick={() => props.onDownload(obj)}><IconDownload size={15} /></button>
                    <button class="icon-btn" onClick={() => props.onCopyLink(obj)}><IconLink size={15} /></button>
                  </div>
                </div>
              )}
            </For>
          </div>
          <Show when={props.hasMore}>
            <div class="search-load-more">
              <button class="btn-secondary" disabled={props.loadingMore} onClick={props.onLoadMore}>
                {props.loadingMore ? "Loading…" : "Load more"}
              </button>
            </div>
          </Show>
        </Show>
      </Show>
    </div>
  );
}
