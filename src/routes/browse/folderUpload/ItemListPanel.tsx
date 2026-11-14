import { createEffect, createSignal, Index, on, Show } from "solid-js";
import { createVirtualizer } from "@tanstack/solid-virtual";
import { folderUploadListItems } from "../../../api/folderUpload";
import { formatBytes, formatDate } from "../../../utils/fmt";
import { errMsg } from "../../../utils/errors";
import Spinner from "../../../utils/Spinner";
import { t, type TKey } from "../../../i18n";
import type { FolderItem, FolderItemFilter } from "../../../types";

const PAGE = 200;
// Must match .fu-item height in folder-upload.css.
const ROW_H = 44;

function detail(it: FolderItem): string {
  if (it.exclude_reason) {
    if (it.link_target) return t("folderUpload.excluded.linkTarget", { target: it.link_target });
    return t(`folderUpload.excluded.${it.exclude_reason}` as TKey);
  }
  if (it.error) return errMsg(it.error);
  if (it.drift) return t(`folderUpload.item.drift.${it.drift}` as TKey);
  if (it.remote_exists) {
    return t("folderUpload.list.remote", {
      size: it.remote_size == null ? "-" : formatBytes(it.remote_size),
      date: it.remote_mtime == null ? "-" : formatDate(it.remote_mtime / 1000),
    });
  }
  return t(`folderUpload.item.state.${it.state}` as TKey);
}

// An empty rel_path is the picked folder itself (e.g. an unreadable root).
const itemName = (it: FolderItem) => it.rel_path || t("folderUpload.item.root");

// Paged, virtualized list of job items; pages load as they scroll into view.
export function ItemListPanel(props: { jobId: string; filter: FolderItemFilter; reload?: number }) {
  const [pages, setPages] = createSignal(new Map<number, FolderItem[]>());
  const [total, setTotal] = createSignal<number | null>(null);
  const [failed, setFailed] = createSignal(false);
  let inflight = new Set<number>();
  let gen = 0;

  async function loadPage(p: number) {
    if (inflight.has(p) || pages().has(p)) return;
    const set = inflight;
    set.add(p);
    const g = gen;
    try {
      const res = await folderUploadListItems(props.jobId, props.filter, p * PAGE, PAGE);
      if (g !== gen) return;
      setTotal(res.total);
      setPages((m) => new Map(m).set(p, res.items));
    } catch {
      if (g === gen) setFailed(true);
    } finally { set.delete(p); }
  }

  createEffect(on(() => [props.jobId, props.filter, props.reload], () => {
    gen++;
    inflight = new Set();
    setPages(new Map());
    setFailed(false);
    void loadPage(0);
  }));

  const [scrollEl, setScrollEl] = createSignal<HTMLDivElement | null>(null);
  const virt = createVirtualizer({
    get count() { return total() ?? 0; },
    getScrollElement: () => scrollEl(),
    estimateSize: () => ROW_H,
    overscan: 10,
  });

  createEffect(() => {
    for (const v of virt.getVirtualItems()) void loadPage(Math.floor(v.index / PAGE));
  });

  const itemAt = (i: number) => pages().get(Math.floor(i / PAGE))?.[i % PAGE];

  return (
    <div class="fu-list">
      <Show when={failed()}><div class="status-msg err">{t("folderUpload.list.loadFailed")}</div></Show>
      <Show when={total() === null && !failed()}><div class="loading-row"><Spinner /> {t("common.loading")}</div></Show>
      <Show when={total() === 0}><div class="fu-list-empty">{t("folderUpload.list.empty")}</div></Show>
      <div class="fu-list-scroll" ref={setScrollEl}>
        <div class="fu-list-inner" style={{ height: `${virt.getTotalSize()}px` }}>
          <Index each={virt.getVirtualItems()}>
            {(v) => (
              <div class="fu-item" style={{ transform: `translateY(${v().start}px)` }}>
                <Show when={itemAt(v().index)} fallback={<span class="fu-item-name faint">…</span>}>
                  {(it) => (<>
                    <div class="fu-item-main">
                      <span class="fu-item-name truncate" title={itemName(it())}>{itemName(it())}</span>
                      <span class="fu-item-size">{it().kind === "file" ? formatBytes(it().size) : ""}</span>
                    </div>
                    <div class="fu-item-detail truncate" title={detail(it())}>{detail(it())}</div>
                  </>)}
                </Show>
              </div>
            )}
          </Index>
        </div>
      </div>
    </div>
  );
}
