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
      class={`col-pane ${props.active ? "col-pane-active" : ""}`}
      style={{ "overflow-y": "auto", opacity: props.loading ? "0.45" : "1", transition: "opacity 0.12s" }}
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
                <div style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  height: `${COL_ITEM_H}px`,
                  transform: `translateY(${vrow().start}px)`,
                }}>
                  <Show when={item().kind === "folder"}>
                    <button
                      class={`col-pane-item ${props.selectedKey === (item() as { kind: "folder"; key: string }).key ? "selected" : ""}`}
                      style="height:100%;width:100%"
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
                      class={`col-pane-item ${props.selectedKey === (item() as { kind: "file"; obj: CachedObjectMeta }).obj.key ? "selected" : ""}`}
                      style="height:100%;width:100%"
                      onClick={() => props.onSelectFile((item() as { kind: "file"; obj: CachedObjectMeta }).obj)}
                      onContextMenu={(e) => props.onCtxFile?.(e, (item() as { kind: "file"; obj: CachedObjectMeta }).obj)}
                    >
                      <FileIcon name={(item() as { kind: "file"; obj: CachedObjectMeta }).obj.basename} />
                      <span class="col-pane-name">{(item() as { kind: "file"; obj: CachedObjectMeta }).obj.basename}</span>
                    </button>
                  </Show>
                  <Show when={item().kind === "loadmore"}>
                    <button
                      class="col-pane-item col-pane-loadmore"
                      style="height:100%;width:100%;justify-content:center;color:var(--muted)"
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
}) {
  const { state, loadMore } = createPagedBrowse(() => ({
    accountId: props.accountId,
    bucket: props.bucket,
    prefix: props.prefix,
    refresh: props.refresh,
  }));

  const items = createMemo<Row[]>(() => {
    const folders: Row[] = state.subprefixes.map((key) => ({ kind: "folder", key }));
    const files: Row[] = state.objects.map((obj) => ({ kind: "file", obj }));
    const rows: Row[] = [...folders, ...files];
    if (state.continuation) rows.push({ kind: "loadmore" });
    return rows;
  });

  const hasData = () => state.initialLoaded;

  return (
    <Show when={hasData()} fallback={
      <div class={`col-pane ${props.active ? "col-pane-active" : ""}`} style="overflow-y:auto">
        <Show when={state.loading}>
          <div style="padding:12px;display:flex;justify-content:center"><span class="spinner" /></div>
        </Show>
        <Show when={state.error}>
          <div style="padding:8px;font-size:11px;color:var(--red)">{errMsg(state.error)}</div>
        </Show>
      </div>
    }>
      <Show when={items().length > 0} fallback={
        <div class={`col-pane ${props.active ? "col-pane-active" : ""}`} style="overflow-y:auto;display:flex;align-items:flex-start;justify-content:center;padding-top:12px">
          <span style="color:var(--faint);font-size:12px">Empty folder</span>
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
