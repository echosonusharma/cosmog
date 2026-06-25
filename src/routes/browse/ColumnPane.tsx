import { createSignal, createResource, createMemo, Show, createEffect, onCleanup, Index } from "solid-js";
import { createVirtualizer } from "@tanstack/solid-virtual";
import { browsePrefix } from "../../api/browse";
import { errMsg } from "../../state/toast";
import { basename } from "../../utils/fmt";
import { FileIcon, IconChevronR } from "../../utils/icons";
import type { CachedObjectMeta } from "../../types";

// ── column pane (miller columns view) ────────────────────────────────────────

export const COL_ITEM_H = 30; // px — must match .col-pane-item height in CSS

function ColumnPaneVirtual(props: {
  items: Array<{ kind: "folder"; key: string } | { kind: "file"; obj: CachedObjectMeta }>;
  loading: boolean;
  selectedKey: string | null;
  onSelectFolder: (sub: string) => void;
  onSelectFile: (obj: CachedObjectMeta) => void;
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
      class="col-pane"
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
  onSelectFolder: (sub: string) => void;
  onSelectFile: (obj: CachedObjectMeta) => void;
  onCtxFolder?: (e: MouseEvent, sub: string) => void;
  onCtxFile?: (e: MouseEvent, obj: CachedObjectMeta) => void;
  onCtxPane?: (e: MouseEvent, prefix: string) => void;
  refresh: number;
}) {
  const [pollCount, setPollCount] = createSignal(0);
  const [data] = createResource(
    () => [props.accountId, props.bucket, props.prefix, props.refresh, pollCount()] as const,
    ([a, b, p]) => browsePrefix(a, b, p),
  );

  createEffect(() => {
    if (!data()?.refreshing) { setPollCount(0); return; }
    if (pollCount() >= 10) return;
    const t = setTimeout(() => setPollCount((n) => n + 1), 1500);
    onCleanup(() => clearTimeout(t));
  });

  const items = createMemo(() => {
    const d = data.latest;
    if (!d) return [];
    const folders = d.subprefixes.map((key) => ({ kind: "folder" as const, key }));
    const files = d.objects.map((obj) => ({ kind: "file" as const, obj }));
    return [...folders, ...files];
  });

  const hasData = () => !!data.latest;
  const isRefreshing = () => !!data.latest?.refreshing;

  return (
    <Show when={hasData()} fallback={
      <div class="col-pane" style="overflow-y:auto">
        <Show when={data.loading}>
          <div style="padding:12px;display:flex;justify-content:center"><span class="spinner" /></div>
        </Show>
        <Show when={data.error}>
          <div style="padding:8px;font-size:11px;color:var(--red)">{errMsg(data.error)}</div>
        </Show>
      </div>
    }>
      <Show when={items().length > 0} fallback={
        <div class="col-pane" style="overflow-y:auto;display:flex;align-items:flex-start;justify-content:center;padding-top:12px">
          <Show when={isRefreshing()} fallback={
            <span style="color:var(--faint);font-size:12px">Empty folder</span>
          }>
            <span class="spinner" />
          </Show>
        </div>
      }>
        <ColumnPaneVirtual
          items={items()}
          loading={data.loading}
          selectedKey={props.selectedKey}
          onSelectFolder={props.onSelectFolder}
          onSelectFile={props.onSelectFile}
          onCtxFolder={props.onCtxFolder}
          onCtxFile={props.onCtxFile}
          onCtxPane={(e) => props.onCtxPane?.(e, props.prefix)}
        />
      </Show>
    </Show>
  );
}
