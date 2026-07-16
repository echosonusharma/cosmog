import { createMemo, createEffect, Show, Index, onMount, onCleanup } from "solid-js";
import { createVirtualizer } from "@tanstack/solid-virtual";
import { errMsg } from "../../state/toast";
import { goUpPrefix, navigateToPrefix } from "../../state/app";
import { formatBytes, formatDate, basename } from "../../utils/fmt";
import {
  FileIcon, fileTypeLabel,
  IconBack, IconDownload, IconLink, IconTrash, IconBucket,
} from "../../utils/icons";
import type { CachedObjectMeta } from "../../types";
import type { PagedBrowseState } from "../../utils/usePagedBrowse";

export type ListItem =
  | { kind: "folder"; sub: string }
  | { kind: "file"; obj: CachedObjectMeta };

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

  let listScrollEl!: HTMLDivElement;
  const listVirtualizer = createVirtualizer({
    get count() { return listItems().length; },
    getScrollElement: () => listScrollEl,
    estimateSize: () => LIST_ROW_H,
    overscan: 15,
  });

  // Re-measure virtualizer when switching to list view (display:none = 0 clientHeight)
  createEffect(() => {
    if (props.visible) {
      listItems().length;
      requestAnimationFrame(() => listVirtualizer.measure());
    }
  });

  // Re-measure when the scroll container resizes (e.g. preview pane opens/closes)
  onMount(() => {
    const ro = new ResizeObserver(() => requestAnimationFrame(() => listVirtualizer.measure()));
    ro.observe(listScrollEl);
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
          ref={listScrollEl}
          class={`object-list object-list-scroll ${props.hasSel ? "has-selection" : ""}`}
          classList={{ loading: props.browseData.loading }}
        >
          <Show when={props.browseData.error}>
            <div class="status-msg err list-status-msg">{errMsg(props.browseData.error)}</div>
          </Show>

          <Show when={props.browseData.loading && !props.browseData.initialLoaded}>
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
                      <div style={{
                        position: "absolute",
                        top: 0,
                        left: 0,
                        width: "100%",
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
                                onContextMenu={(e) => props.onCtxFile(e, obj())}
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
                                  <span class="obj-name" title={obj().key}>{obj().basename}</span>
                                </div>
                                <div class="obj-type">{obj().key.endsWith("/") ? "Folder" : fileTypeLabel(obj().basename)}</div>
                                <div class="obj-size">{formatBytes(obj().size)}</div>
                                <div class="obj-date">{obj().last_modified ? formatDate(obj().last_modified) : "-"}</div>
                                <div class="obj-actions" onClick={(e) => e.stopPropagation()}>
                                  <button class="icon-btn" title="Download" onClick={() => props.onDownload(obj())}><IconDownload size={15} /></button>
                                  <button class="icon-btn" title="Copy link" onClick={() => props.onCopyLink(obj())}><IconLink size={15} /></button>
                                  <button class="icon-btn danger" title="Delete" onClick={() => props.onDelete(obj())}><IconTrash size={15} /></button>
                                </div>
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
