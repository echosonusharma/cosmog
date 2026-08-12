import { createMemo, createSignal, createEffect, Show, Index, onMount, onCleanup } from "solid-js";
import { createVirtualizer } from "@tanstack/solid-virtual";
import { errMsg } from "../../state/toast";
import { goUpPrefix, navigateToPrefix } from "../../state/app";
import { formatBytes, formatDate, basename } from "../../utils/fmt";
import {
  FileIcon, fileTypeLabel,
  IconBack, IconDownload, IconLink, IconTrash, IconBucket, IconMore,
} from "../../utils/icons";
import type { CachedObjectMeta } from "../../types";
import type { PagedBrowseState } from "../../utils/usePagedBrowse";

export type ListItem =
  | { kind: "folder"; sub: string }
  | { kind: "file"; obj: CachedObjectMeta };

// Row height must match the rendered row exactly, or the virtualizer's slot
// spacing (estimateSize + translateY) drifts from the real rows and they
// overlap. Mobile uses taller touch rows than desktop. Portrait-locked, so a
// one-shot read at mount is enough — no need to react to viewport changes.
// Compact 30px rows on every platform, matching the miller column pane so item
// spacing is identical across all views.
const LIST_ROW_H = 30;

export function ListView(props: {
  prefix: string;
  browseData: PagedBrowseState;
  onLoadMore: () => void;
  hasSel: boolean;
  selected: Set<string>;
  visible: boolean;
  onToggleSel: (key: string) => void;
  onPreview: (obj: CachedObjectMeta) => void;
  onDownload: (obj: CachedObjectMeta) => void;
  onCopyLink: (obj: CachedObjectMeta) => void;
  onDelete: (obj: CachedObjectMeta) => void;
  onCtxFile: (e: MouseEvent, obj: CachedObjectMeta) => void;
  onCtxFolder: (e: MouseEvent, sub: string) => void;
  pendingFolders?: string[];
}) {
  const listItems = createMemo<ListItem[]>(() => {
    const d = props.browseData;
    if (!d.initialLoaded) return [];
    const realSubs = new Set(d.subprefixes);
    const optimistic = (props.pendingFolders ?? []).filter((f) => !realSubs.has(f));
    return [
      ...[...d.subprefixes, ...optimistic].map((sub: string) => ({ kind: "folder" as const, sub })),
      ...d.objects.map((obj: CachedObjectMeta) => ({ kind: "file" as const, obj })),
    ];
  });

  // The virtualizer reads the scroll element's viewport height only when the
  // element reference *changes* (via observeElementRect). measure() alone never
  // re-reads it. On mount/first-show the element is laid out after the initial
  // read, so the cached rect is 0 and no rows render. Toggle the element signal
  // (null -> element) to force a fresh viewport read after layout settles.
  let scrollDiv: HTMLDivElement | undefined;
  const [virtScrollEl, setVirtScrollEl] = createSignal<HTMLDivElement | null>(null);
  const listVirtualizer = createVirtualizer({
    get count() { return listItems().length; },
    getScrollElement: () => virtScrollEl(),
    estimateSize: () => LIST_ROW_H,
    overscan: 15,
  });

  const refreshViewport = () => {
    if (!scrollDiv) return;
    // Rows already render (or nothing to show): cheap re-measure, no blink.
    if (listVirtualizer.getVirtualItems().length > 0 || listItems().length === 0) {
      listVirtualizer.measure();
      return;
    }
    // Items exist but none render: viewport rect is stale (0). Toggle the
    // element ref to force virtual-core to re-observe and re-read the height.
    setVirtScrollEl(null);
    requestAnimationFrame(() => { if (scrollDiv) setVirtScrollEl(scrollDiv); });
  };

  // Re-read viewport when shown (display:none = 0 height) or when data arrives.
  createEffect(() => {
    listItems().length;
    if (props.visible) requestAnimationFrame(refreshViewport);
  });

  // Re-read when the scroll container resizes (e.g. preview pane opens/closes).
  onMount(() => {
    if (!scrollDiv) return;
    const ro = new ResizeObserver(() => requestAnimationFrame(refreshViewport));
    ro.observe(scrollDiv);
    onCleanup(() => ro.disconnect());
  });

  return (
    <>
      <div class="col-header">
        <button>Name</button>
        <div>Type</div>
        <div class="col-num">Size</div>
        <div>Modified</div>
        <div />
      </div>

      <div class="list-view-scroll-wrap">
        <div
          ref={(el) => { scrollDiv = el; setVirtScrollEl(el); }}
          class={`object-list object-list-scroll ${props.hasSel ? "has-selection" : ""}`}
          classList={{ loading: props.browseData.loading && listItems().length > 0 }}
        >
          <Show when={props.browseData.error}>
            <div class="status-msg err list-status-msg">{errMsg(props.browseData.error)}</div>
          </Show>

          <Show when={props.browseData.loading && listItems().length === 0}>
            <div class="loading-row"><span class="spinner" /> Loading…</div>
          </Show>

          {/* ".." back row — outside virtual list so it's always at top */}
          <Show when={props.prefix !== "" && props.browseData.initialLoaded}>
            <button class="obj-row folder-row" onClick={goUpPrefix} style={`height:${LIST_ROW_H}px`}>
              <div class="obj-name-cell">
                <span class="obj-checkbox-spacer" />
                <IconBack size={16} class="muted" />
                <span class="obj-name">..</span>
              </div>
              <div class="obj-type">Folder</div>
              <div class="obj-size" />
              <div class="obj-date" />
              <div />
            </button>
          </Show>

          <Show when={props.browseData.initialLoaded && !props.browseData.loading && !props.browseData.error && listItems().length === 0}>
            <div class="empty-state">
              <span class="empty-icon"><IconBucket size={36} /></span>
              Empty prefix
            </div>
          </Show>

          <Show when={props.browseData.continuation}>
            <button
              class="loadmore-row"
              disabled={props.browseData.loading}
              onClick={props.onLoadMore}
            >
              {props.browseData.loading ? "Loading more…" : `Load more (${props.browseData.objects.length} loaded)`}
            </button>
          </Show>

          <Show when={listItems().length > 0}>
            <div style={{ height: `${listVirtualizer.getTotalSize()}px`, position: "relative" }}>
              <Index each={listVirtualizer.getVirtualItems()}>
                {(vrow) => {
                  const item = () => listItems()[vrow().index];
                  return (
                    <Show when={item()}>
                      <div class="virtual-row" style={{
                        height: `${LIST_ROW_H}px`,
                        transform: `translateY(${vrow().start}px)`,
                      }}>
                        <Show when={item().kind === "folder"}>
                          <button
                            class="obj-row folder-row"
                            style={`height:${LIST_ROW_H}px;width:100%`}
                            onClick={() => navigateToPrefix((item() as { kind: "folder"; sub: string }).sub)}
                            onContextMenu={(e) => props.onCtxFolder(e, (item() as { kind: "folder"; sub: string }).sub)}
                          >
                            <div class="obj-name-cell">
                              <span class="obj-checkbox-spacer" />
                              <FileIcon name={(item() as { kind: "folder"; sub: string }).sub} folder />
                              <span class="obj-name">{basename((item() as { kind: "folder"; sub: string }).sub.replace(/\/$/, ""))}</span>
                            </div>
                            <div class="obj-type">Folder</div>
                            <div class="obj-size" />
                            <div class="obj-date" />
                            <div class="obj-actions" />
                          </button>
                        </Show>
                        <Show when={item().kind === "file"}>
                          {(() => {
                            const obj = () => (item() as { kind: "file"; obj: CachedObjectMeta }).obj;
                            return (
                              <div
                                class={`obj-row ${props.selected.has(obj().key) ? "selected" : ""}`}
                                style={`height:${LIST_ROW_H}px`}
                                onClick={(e) => {
                                  if (e.metaKey || e.ctrlKey) props.onToggleSel(obj().key);
                                  else props.onPreview(obj());
                                }}
                              >
                                <div class="obj-name-cell">
                                  <input type="checkbox" class="obj-checkbox"
                                         checked={props.selected.has(obj().key)}
                                         onClick={(e) => e.stopPropagation()}
                                         onChange={() => props.onToggleSel(obj().key)} />
                                  <FileIcon name={obj().basename} />
                                  <span class="obj-name">{obj().basename}</span>
                                </div>
                                <div class="obj-type">{obj().key.endsWith("/") ? "Folder" : fileTypeLabel(obj().basename)}</div>
                                <div class="obj-size">{formatBytes(obj().size)}</div>
                                <div class="obj-date">{obj().last_modified ? formatDate(obj().last_modified) : "-"}</div>
                                <div class="obj-actions" onClick={(e) => e.stopPropagation()}>
                                  <button class="icon-btn" onClick={() => props.onDownload(obj())}><IconDownload size={15} /></button>
                                  <button class="icon-btn" onClick={() => props.onCopyLink(obj())}><IconLink size={15} /></button>
                                  <button class="icon-btn danger" onClick={() => props.onDelete(obj())}><IconTrash size={15} /></button>
                                </div>
                                <button
                                  class="obj-kebab icon-btn"
                                  title="Actions"
                                  onClick={(e) => { e.stopPropagation(); props.onCtxFile(e, obj()); }}
                                >
                                  <IconMore size={16} />
                                </button>
                              </div>
                            );
                          })()}
                        </Show>
                      </div>
                    </Show>
                  );
                }}
              </Index>
            </div>
          </Show>
        </div>
      </div>
    </>
  );
}
