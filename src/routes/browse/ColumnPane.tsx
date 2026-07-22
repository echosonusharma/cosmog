import { createMemo, Show, Index } from "solid-js";
import { createVirtualizer } from "@tanstack/solid-virtual";
import { createPagedBrowse } from "../../utils/usePagedBrowse";
import { errMsg } from "../../state/toast";
import { basename } from "../../utils/fmt";
import { FileIcon, IconChevronR } from "../../utils/icons";
import type { CachedObjectMeta } from "../../types";

// ── column pane (miller columns view) ────────────────────────────────────────

export const COL_ITEM_H = 30; // px — must match .col-pane-item height in CSS

type Row =
  | { kind: "folder"; key: string }
  | { kind: "file"; obj: CachedObjectMeta }
  | { kind: "loadmore" };

function ColumnPaneVirtual(props: {
  items: Row[];
  loading: boolean;
  selectedKey: string | null;
  active?: boolean;
  onSelectFolder: (sub: string) => void;
  onSelectFile: (obj: CachedObjectMeta) => void;
  onLoadMore: () => void;
  onCtxFolder?: (e: MouseEvent, sub: string) => void;
  onCtxFile?: (e: MouseEvent, obj: CachedObjectMeta) => void;
  onCtxPane?: (e: MouseEvent) => void;
}) {
  let scrollEl!: HTMLDivElement;

  const virtualizer = createVirtualizer({
    get count() { return props.items.length; },
    getScrollElement: () => scrollEl,
    estimateSize: () => COL_ITEM_H,
    overscan: 10,
  });

  return (
    <div
      ref={scrollEl}
      class={`col-pane col-pane-scroll ${props.active ? "col-pane-active" : ""}`}
      classList={{ loading: props.loading }}
      onContextMenu={(e) => {
        if (e.target === e.currentTarget) { e.preventDefault(); e.stopPropagation(); props.onCtxPane?.(e); }
      }}
    >
      <div style={{ height: `${virtualizer.getTotalSize()}px`, position: "relative" }}>
        <Index each={virtualizer.getVirtualItems()}>
          {(vrow) => {
            const item = () => props.items[vrow().index];
            return (
              <Show when={item()}>
                <div class="virtual-row" style={{
                  height: `${COL_ITEM_H}px`,
                  transform: `translateY(${vrow().start}px)`,
                }}>
                  <Show when={item().kind === "folder"}>
                    <button
                      class={`col-pane-item fill-cell ${props.selectedKey === (item() as { kind: "folder"; key: string }).key ? "selected" : ""}`}
                      onClick={() => props.onSelectFolder((item() as { kind: "folder"; key: string }).key)}
                      onContextMenu={(e) => props.onCtxFolder?.(e, (item() as { kind: "folder"; key: string }).key)}
                    >
                      <FileIcon name={(item() as { kind: "folder"; key: string }).key} folder />
                      <span class="col-pane-name">{basename((item() as { kind: "folder"; key: string }).key.replace(/\/$/, ""))}</span>
                      <IconChevronR size={12} class="col-pane-chev" />
                    </button>
                  </Show>
                  <Show when={item().kind === "file"}>
                    <button
                      class={`col-pane-item fill-cell ${props.selectedKey === (item() as { kind: "file"; obj: CachedObjectMeta }).obj.key ? "selected" : ""}`}
                      onClick={() => props.onSelectFile((item() as { kind: "file"; obj: CachedObjectMeta }).obj)}
                      onContextMenu={(e) => props.onCtxFile?.(e, (item() as { kind: "file"; obj: CachedObjectMeta }).obj)}
                    >
                      <FileIcon name={(item() as { kind: "file"; obj: CachedObjectMeta }).obj.basename} />
                      <span class="col-pane-name">{(item() as { kind: "file"; obj: CachedObjectMeta }).obj.basename}</span>
                    </button>
                  </Show>
                  <Show when={item().kind === "loadmore"}>
                    <button
                      class="col-pane-item col-pane-loadmore fill-cell"
                      onClick={props.onLoadMore}
                      disabled={props.loading}
                    >
                      {props.loading ? "Loading…" : "Load more"}
                    </button>
                  </Show>
                </div>
              </Show>
            );
          }}
        </Index>
      </div>
    </div>
  );
}

export function ColumnPane(props: {
  accountId: string;
  bucket: string;
  prefix: string;
  selectedKey: string | null;
  active?: boolean;
  onSelectFolder: (sub: string) => void;
  onSelectFile: (obj: CachedObjectMeta) => void;
  onCtxFolder?: (e: MouseEvent, sub: string) => void;
  onCtxFile?: (e: MouseEvent, obj: CachedObjectMeta) => void;
  onCtxPane?: (e: MouseEvent, prefix: string) => void;
  refresh: number;
  pendingFolders?: string[];
}) {
  const { state, loadMore } = createPagedBrowse(() => ({
    accountId: props.accountId,
    bucket: props.bucket,
    prefix: props.prefix,
    refresh: props.refresh,
  }));

  const items = createMemo<Row[]>(() => {
    const realSubs = new Set(state.subprefixes);
    const optimistic = (props.pendingFolders ?? []).filter((f) => !realSubs.has(f));
    const folders: Row[] = [...state.subprefixes, ...optimistic].map((key) => ({ kind: "folder", key }));
    const files: Row[] = state.objects.map((obj) => ({ kind: "file", obj }));
    const rows: Row[] = [...folders, ...files];
    if (state.continuation) rows.push({ kind: "loadmore" });
    return rows;
  });

  const hasData = () => (state.initialLoaded || (props.pendingFolders ?? []).length > 0) && !state.error;

  return (
    <Show when={hasData()} fallback={
      <div class={`col-pane col-pane-scroll ${props.active ? "col-pane-active" : ""}`}>
        <Show when={state.loading && !state.error}>
          <div class="col-pane-inline-spinner"><span class="spinner" /></div>
        </Show>
        <Show when={state.error}>
          <div class="col-pane-inline-err">{errMsg(state.error)}</div>
        </Show>
      </div>
    }>
      <Show when={items().length > 0} fallback={
        <div class={`col-pane col-pane-empty ${props.active ? "col-pane-active" : ""}`}>
          <Show when={!state.loading} fallback={
            <div class="col-pane-inline-spinner"><span class="spinner" /></div>
          }>
            <span class="col-pane-empty-text">Empty folder</span>
          </Show>
        </div>
      }>
        <ColumnPaneVirtual
          items={items()}
          loading={state.loading}
          selectedKey={props.selectedKey}
          active={props.active}
          onSelectFolder={props.onSelectFolder}
          onSelectFile={props.onSelectFile}
          onLoadMore={loadMore}
          onCtxFolder={props.onCtxFolder}
          onCtxFile={props.onCtxFile}
          onCtxPane={(e) => props.onCtxPane?.(e, props.prefix)}
        />
      </Show>
    </Show>
  );
}
