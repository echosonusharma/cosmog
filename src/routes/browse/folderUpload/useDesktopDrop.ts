import { onCleanup, onMount } from "solid-js";
import { localPathKinds } from "../../../api/folderUpload";
import { toast } from "../../../state/toast";
import { isDesktopShell } from "../../../utils/platform";
import { t } from "../../../i18n";

interface Pos { x: number; y: number }

// True when the topmost element at the (physical, webview-relative) point is inside `area`
// and not inside a fixed overlay (modal, menu, preview) stacked over it.
function overArea(area: HTMLElement | undefined, pos: Pos | undefined): boolean {
  if (!area) return false;
  if (!pos) return true;
  const dpr = window.devicePixelRatio || 1;
  let el = document.elementFromPoint(pos.x / dpr, pos.y / dpr);
  if (!el || !area.contains(el)) return false;
  for (; el && el !== area; el = el.parentElement) {
    if (getComputedStyle(el).position === "fixed") return false;
  }
  return true;
}

// Native (Tauri) file drops: one folder opens the folder flow, files only open the upload modal.
export function useDesktopDrop(opts: {
  area: () => HTMLElement | undefined;
  enabled: () => boolean;
  onOver: (over: boolean) => void;
  onFolder: (path: string) => void;
  onFiles: (paths: string[]) => void;
}) {
  if (!isDesktopShell) return;
  onMount(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    void import("@tauri-apps/api/webview").then(({ getCurrentWebview }) =>
      getCurrentWebview().onDragDropEvent(async (ev) => {
        const p = ev.payload;
        if (p.type === "leave") { opts.onOver(false); return; }
        const inside = opts.enabled() && overArea(opts.area(), p.position);
        if (p.type !== "drop") { opts.onOver(inside); return; }
        opts.onOver(false);
        if (!inside || !p.paths.length) return;
        const kinds = await localPathKinds(p.paths).catch(() => null);
        if (!kinds || kinds.length !== p.paths.length) {
          toast.info(t("folderUpload.drop.unreadableTitle"), t("folderUpload.drop.unreadableDetail"));
          return;
        }
        // The browser may have changed (modal opened, view switched) while classifying.
        if (!opts.enabled()) return;
        const dirs = p.paths.filter((_, i) => kinds[i] === "dir");
        const files = p.paths.filter((_, i) => kinds[i] === "file");
        if (dirs.length === 1 && dirs.length === p.paths.length) opts.onFolder(dirs[0]);
        else if (files.length === p.paths.length) opts.onFiles(files);
        else if (dirs.length > 0) toast.info(t("folderUpload.drop.oneFolderTitle"), t("folderUpload.drop.oneFolderDetail"));
        else toast.info(t("folderUpload.drop.unreadableTitle"), t("folderUpload.drop.unreadableDetail"));
      }),
    ).then((fn) => { if (disposed) void Promise.resolve().then(fn).catch(() => {}); else unlisten = fn; }).catch(() => {});
    onCleanup(() => { disposed = true; if (unlisten) void Promise.resolve().then(unlisten).catch(() => {}); });
  });
}
