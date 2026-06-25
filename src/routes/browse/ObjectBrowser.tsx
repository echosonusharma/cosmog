import { createSignal, createResource, For, Show, createEffect, onMount, onCleanup, ErrorBoundary } from "solid-js";
import { browsePrefix } from "../../api/browse";
import {
  searchObjects, bucketIndexStatus,
  enableBucketIndex, disableBucketIndex, reindexBucket,
} from "../../api/search";
import {
  deleteObject, deleteObjects, presignGet,
  listKeysUnderPrefix,
} from "../../api/objects";
import { notify } from "../../utils/notify";
import {
  navigateToPrefix,
  pendingPreview, setPendingPreview,
} from "../../state/app";
import { toast } from "../../state/toast";
import { confirmDialog } from "../../state/confirm";
import type { CachedObjectMeta } from "../../types";
import { DownloadModal, UploadModal, NewFolderModal, RenameModal } from "./modals";
import { PreviewPane } from "./PreviewPane";
import { ColumnPane } from "./ColumnPane";
import { Toolbar } from "./Toolbar";
import { IndexBar } from "./IndexBar";
import { BulkBar } from "./BulkBar";
import { SearchResultsPane } from "./SearchResultsPane";
import { ContextMenu, type CtxMenu } from "./ContextMenu";
import { ListView } from "./ListView";

// ── object browser ────────────────────────────────────────────────────────────

export function ObjectBrowser(props: {
  accountId: string;
  accountName: string;
  bucket: string;
  prefix: string;
  defaultDownloadDir: string;
}) {
  const [refresh, setRefresh] = createSignal(0);
  const [forceRefresh, setForceRefresh] = createSignal(false);

  // ── search ────────────────────────────────────────────────────────────────
  const [searchQuery, setSearchQuery] = createSignal("");
  const [debouncedQuery, setDebouncedQuery] = createSignal("");
  const [indexBusy, setIndexBusy] = createSignal(false);
  let debounceTimer: ReturnType<typeof setTimeout>;
  onCleanup(() => clearTimeout(debounceTimer));
  createEffect(() => {
    const q = searchQuery();
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => setDebouncedQuery(q), 300);
  });
  // clear search when bucket changes
  createEffect(() => { props.bucket; setSearchQuery(""); setDebouncedQuery(""); });

  const [indexStatus, { refetch: refetchIndex }] = createResource(
    () => ({ a: props.accountId, b: props.bucket }),
    ({ a, b }) => bucketIndexStatus(a, b),
  );

  const [searchResults] = createResource(
    () => debouncedQuery()
      ? { a: props.accountId, b: props.bucket, q: debouncedQuery(), r: refresh() }
      : null,
    ({ a, b, q }) => searchObjects({
      account_id: a, bucket: b,
      scope: { kind: "bucket" },
      query: q, filters: {}, sort: "name", sort_dir: "asc", page_size: 200,
    }),
  );

  async function toggleIndex() {
    setIndexBusy(true);
    try {
      if (indexStatus()?.enabled) {
        const ok = await confirmDialog({
          title: "Disable index?",
          body: "Cached metadata for this bucket will be cleared.",
          confirmLabel: "Disable", danger: true,
        });
        if (!ok) { setIndexBusy(false); return; }
        await disableBucketIndex(props.accountId, props.bucket);
        toast.ok("Index disabled");
      } else {
        await enableBucketIndex(props.accountId, props.bucket);
        toast.ok("Indexing started");
      }
      refetchIndex();
    } catch (e) { toast.err(e); }
    finally { setIndexBusy(false); }
  }

  async function handleReindex() {
    setIndexBusy(true);
    try {
      await reindexBucket(props.accountId, props.bucket);
      toast.ok("Re-indexed");
      refetchIndex();
    } catch (e) { toast.err(e); }
    finally { setIndexBusy(false); }
  }
  const [browseData] = createResource(
    () => ({ a: props.accountId, b: props.bucket, p: props.prefix, r: refresh(), f: forceRefresh() }),
    ({ a, b, p, f }) => browsePrefix(a, b, p, f),
  );
  // Reset forceRefresh after the resource has queued a fetch, not inside the fetcher.
  createEffect(() => { if (forceRefresh() && browseData.loading) setForceRefresh(false); });

  const VALID_VIEW_MODES = ["list", "columns"] as const;
  const storedView = localStorage.getItem("cosmog:viewMode");
  const [viewMode, setViewMode] = createSignal<"list" | "columns">(
    VALID_VIEW_MODES.includes(storedView as any) ? (storedView as "list" | "columns") : "columns"
  );
  const saveViewMode = (m: "list" | "columns") => {
    localStorage.setItem("cosmog:viewMode", m);
    setViewMode(m);
  };

  // derive column prefixes from current prefix path
  const colPrefixes = () => {
    const segs = props.prefix.split("/").filter(Boolean);
    const result: string[] = [""];
    let acc = "";
    for (const seg of segs) { acc += seg + "/"; result.push(acc); }
    return result;
  };

  const [selected, setSelected] = createSignal<Set<string>>(new Set());
  const [showUpload, setShowUpload] = createSignal<string | false>(false);
  const [showNewFolder, setShowNewFolder] = createSignal<string | null>(null);
  const [downloadTarget, setDownloadTarget] = createSignal<CachedObjectMeta | null>(null);
  const [renameTarget, setRenameTarget] = createSignal<CachedObjectMeta | null>(null);
  const [previewTarget, setPreviewTarget] = createSignal<CachedObjectMeta | null>(null);
  const [ctxMenu, setCtxMenu] = createSignal<CtxMenu | null>(null);
  const [dragOver, setDragOver] = createSignal(false);
  const [pendingDrop, setPendingDrop] = createSignal<string[]>([]);

  createEffect(() => { props.prefix; setSelected(new Set<string>()); });
  createEffect(() => { viewMode(); setSelected(new Set<string>()); });

  // Consume pendingPreview set by navigateToObject (from Search).
  createEffect(() => {
    const pending = pendingPreview();
    if (!pending) return;
    if (pending.account_id !== props.accountId || pending.bucket !== props.bucket) return;
    if (browseData.loading) return;
    setPreviewTarget(pending);
    setPendingPreview(null);
  });

  // Poll while BE is doing a background sync — `refreshing` is a snapshot
  // in the response, not a live flag, so re-fetch periodically until BE
  // reports refreshing=false. Capped at 10 retries (~15s) so a stuck BE
  // flag never spins the UI forever.
  const [pollCount, setPollCount] = createSignal(0);
  createEffect(() => { props.prefix; props.bucket; setPollCount(0); });
  createEffect(() => { props.bucket; setPreviewTarget(null); });
  createEffect(() => {
    if (!browseData()?.refreshing) return;
    if (pollCount() >= 10) return;
    const t = setTimeout(() => {
      setPollCount((n) => n + 1);
      setRefresh((n) => n + 1);
    }, 1500);
    onCleanup(() => clearTimeout(t));
  });
  const showSyncing = () => !!browseData()?.refreshing && pollCount() < 10;

  function toggleSel(key: string) {
    const s = new Set<string>(selected());
    s.has(key) ? s.delete(key) : s.add(key);
    setSelected(s);
  }

  async function handleDeleteFolder(sub: string) {
    const ok = await confirmDialog({
      title: "Delete folder?",
      body: `"${sub}" and all its contents will be permanently deleted. This action is irreversible.`,
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    try {
      const keys = await listKeysUnderPrefix(props.accountId, props.bucket, sub);
      if (keys.length) {
        for (let i = 0; i < keys.length; i += 1000)
          await deleteObjects(props.accountId, props.bucket, keys.slice(i, i + 1000));
      }
      setRefresh((n) => n + 1);
      notify("Folder deleted", sub);
    } catch (e) { toast.err(e); }
  }

  async function handleDelete(obj: CachedObjectMeta) {
    const ok = await confirmDialog({
      title: "Delete object?",
      body: `${obj.key}\n\nThis action is irreversible.`,
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    try {
      await deleteObject(obj.account_id, obj.bucket, obj.key);
      if (previewTarget()?.key === obj.key) setPreviewTarget(null);
      setRefresh((n) => n + 1);
      notify("Deleted", obj.key);
    } catch (e) { toast.err(e); }
  }

  async function handleBulkDelete() {
    const keys = Array.from(selected());
    if (!keys.length) return;
    const ok = await confirmDialog({
      title: `Delete ${keys.length} object${keys.length > 1 ? "s" : ""}?`,
      body: keys.slice(0, 5).join("\n") + (keys.length > 5 ? `\n…and ${keys.length - 5} more` : "") + "\n\nThis action is irreversible.",
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    try {
      const res = await deleteObjects(props.accountId, props.bucket, keys);
      if (previewTarget() && keys.includes(previewTarget()!.key)) setPreviewTarget(null);
      setSelected(new Set<string>());
      setRefresh((n) => n + 1);
      if (res.errors.length) {
        toast.warn(`Deleted ${res.deleted.length}, ${res.errors.length} failed`);
        notify("Partial delete", `${res.deleted.length} deleted, ${res.errors.length} failed`);
      } else {
        notify("Deleted", `${res.deleted.length} object${res.deleted.length > 1 ? "s" : ""} deleted`);
      }
    } catch (e) { toast.err(e); }
  }

  async function handleCopyLink(obj: CachedObjectMeta) {
    try {
      const url = await presignGet(obj.account_id, obj.bucket, obj.key);
      await navigator.clipboard.writeText(url);
      toast.ok("Link copied");
    } catch (e) { toast.err(e); }
  }

  // Drag-drop wiring (Tauri webview file drops would need plugin-fs; for now show overlay on dragover)
  function onDragOver(e: DragEvent) {
    e.preventDefault();
    setDragOver(true);
  }
  function onDragLeave() { setDragOver(false); }
  function onDrop(e: DragEvent) {
    e.preventDefault();
    setDragOver(false);
    // Browser-level dropped files (won't have absolute paths in Tauri; fallback to clicking Upload)
    const paths: string[] = [];
    const dt = e.dataTransfer;
    if (dt) {
      for (let i = 0; i < dt.files.length; i++) {
        const f: any = dt.files[i];
        if (f.path) paths.push(f.path);
      }
    }
    if (paths.length) {
      setPendingDrop(paths);
      setShowUpload(props.prefix);
    } else {
      toast.info("Drag-drop unsupported here, use Upload button");
      setShowUpload(props.prefix);
    }
  }

  function openCtx(e: MouseEvent, obj: CachedObjectMeta) {
    e.preventDefault(); e.stopPropagation();
    setCtxMenu({ kind: "file", x: e.clientX, y: e.clientY, obj });
  }

  function openCtxFolder(e: MouseEvent, sub: string) {
    e.preventDefault(); e.stopPropagation();
    setCtxMenu({ kind: "folder", x: e.clientX, y: e.clientY, sub });
  }

  onMount(() => {
    const close = () => setCtxMenu(null);
    document.addEventListener("click", close);
    onCleanup(() => document.removeEventListener("click", close));

    const resetDrag = () => setDragOver(false);
    window.addEventListener("dragend", resetDrag);
    window.addEventListener("blur", resetDrag);
    onCleanup(() => {
      window.removeEventListener("dragend", resetDrag);
      window.removeEventListener("blur", resetDrag);
    });
  });

  const hasSel = () => selected().size > 0;

  return (
    <div class="object-browser"
         onDragOver={onDragOver}
         onDragLeave={onDragLeave}
         onDrop={onDrop}
         style="position:relative">
      <Toolbar
        accountName={props.accountName}
        bucket={props.bucket}
        prefix={props.prefix}
        indexStatus={indexStatus}
        indexBusy={indexBusy()}
        onToggleIndex={toggleIndex}
        searchQuery={searchQuery()}
        onSearchInput={setSearchQuery}
        onClearSearch={() => setSearchQuery("")}
        showSyncing={showSyncing()}
        stale={!!browseData()?.stale}
        viewMode={viewMode()}
        onViewMode={saveViewMode}
        onRefresh={() => { setForceRefresh(true); setRefresh((n) => n + 1); }}
        onNewFolder={() => setShowNewFolder(props.prefix)}
        onUpload={() => setShowUpload(props.prefix)}
      />

      {/* ── index status bar (shown when search active) ── */}
      <Show when={searchQuery()}>
        <IndexBar
          accountId={props.accountId}
          bucket={props.bucket}
          indexStatus={indexStatus}
          indexBusy={indexBusy()}
          refetchIndex={refetchIndex}
          onReindex={handleReindex}
        />
      </Show>

      <Show when={hasSel() && !searchQuery()}>
        <BulkBar
          count={selected().size}
          onClear={() => setSelected(new Set<string>())}
          onDelete={handleBulkDelete}
        />
      </Show>

      {/* ── search results ── */}
      <Show when={searchQuery()}>
        <SearchResultsPane
          searchQuery={searchQuery()}
          searchResults={searchResults}
          indexStatus={indexStatus}
          indexBusy={indexBusy()}
          onEnableIndex={toggleIndex}
          onSelectResult={(obj) => setPreviewTarget(obj)}
          onCtxResult={openCtx}
          onDownload={(obj) => setDownloadTarget(obj)}
          onCopyLink={handleCopyLink}
          onClearSearch={() => setSearchQuery("")}
        />
      </Show>

      {/* ── view area: all view modes + shared preview as flex-row siblings ── */}
      <div style={{ display: searchQuery() ? "none" : "flex", flex: "1", overflow: "hidden", "min-height": "0" }}>

      {/* ── columns view ── */}
        <div style={{ display: viewMode() === "columns" && !searchQuery() ? "flex" : "none", flex: "1", overflow: "hidden" }}>
          <div class="columns-scroll">
            <For each={colPrefixes()}>
              {(pfx, i) => {
                const nextPfx = () => colPrefixes()[i() + 1] ?? null;
                const selKey = () => nextPfx() ?? previewTarget()?.key ?? null;
                return (
                  <ColumnPane
                    accountId={props.accountId}
                    bucket={props.bucket}
                    prefix={pfx}
                    selectedKey={selKey()}
                    onSelectFolder={(sub) => navigateToPrefix(sub)}
                    onSelectFile={(obj) => { setPreviewTarget(obj); }}
                    onCtxFolder={openCtxFolder}
                    onCtxFile={openCtx}
                    onCtxPane={(e, prefix) => setCtxMenu({ kind: "pane", x: e.clientX, y: e.clientY, prefix })}
                    refresh={refresh()}
                  />
                );
              }}
            </For>
          </div>
        </div>

      {/* ── list view ── */}
      <div style={{ display: viewMode() === "list" && !searchQuery() ? "flex" : "none", flex: "1", overflow: "hidden", "flex-direction": "column" }}>
        <ListView
          prefix={props.prefix}
          browseData={browseData}
          hasSel={hasSel()}
          selected={selected()}
          visible={viewMode() === "list"}
          onToggleSel={toggleSel}
          onPreview={(obj) => setPreviewTarget(obj)}
          onDownload={(obj) => setDownloadTarget(obj)}
          onCopyLink={handleCopyLink}
          onDelete={handleDelete}
          onCtxFile={openCtx}
          onCtxFolder={openCtxFolder}
        />
      </div>{/* end list view */}

      {/* ── shared preview pane (single instance for all view modes) ── */}
      <Show when={previewTarget()}>
        <ErrorBoundary fallback={(err, reset) => (
          <div class="preview-pane" style="padding:16px;display:flex;flex-direction:column;gap:8px">
            <span style="color:var(--err);font-size:12px">Preview error: {String(err)}</span>
            <button class="btn-ghost" style="font-size:12px;align-self:flex-start" onClick={() => { setPreviewTarget(null); reset(); }}>Close</button>
          </div>
        )}>
          <PreviewPane
            obj={previewTarget()!}
            onClose={() => setPreviewTarget(null)}
            onDownload={() => { const o = previewTarget(); if (o) setDownloadTarget(o); }}
            onCopyLink={() => { const o = previewTarget(); if (o) handleCopyLink(o); }}
          />
        </ErrorBoundary>
      </Show>

      </div>{/* end view area */}

      <Show when={dragOver()}>
        <div class="drop-overlay">Drop files to upload</div>
      </Show>

      <Show when={ctxMenu()}>
        {(m) => (
          <ContextMenu
            menu={m()}
            onClose={() => setCtxMenu(null)}
            onNewFolder={(prefix) => setShowNewFolder(prefix)}
            onUploadHere={(sub) => setShowUpload(sub)}
            onDeleteFolder={handleDeleteFolder}
            onPreview={(obj) => setPreviewTarget(obj)}
            onDownload={(obj) => setDownloadTarget(obj)}
            onCopyLink={handleCopyLink}
            onRename={(obj) => setRenameTarget(obj)}
            onDelete={handleDelete}
          />
        )}
      </Show>

      <Show when={downloadTarget()}>
        {(obj) => <DownloadModal obj={obj()} defaultDir={props.defaultDownloadDir}
                                  onClose={() => setDownloadTarget(null)} />}
      </Show>

      <Show when={showUpload() !== false}>
        <UploadModal accountId={props.accountId} bucket={props.bucket} prefix={showUpload() as string}
                     initialFiles={pendingDrop()}
                     onClose={() => { setShowUpload(false); setPendingDrop([]); }}
                     onQueued={() => { const t = setTimeout(() => setRefresh((n) => n + 1), 1500); onCleanup(() => clearTimeout(t)); }} />
      </Show>

      <Show when={showNewFolder() !== null}>
        <NewFolderModal
          prefix={showNewFolder()!}
          onClose={() => setShowNewFolder(null)}
          onDone={(folderKey) => { navigateToPrefix(folderKey); setRefresh((n) => n + 1); }}
        />
      </Show>

      <Show when={renameTarget()}>
        {(obj) => <RenameModal obj={obj()} onClose={() => setRenameTarget(null)}
                                onDone={() => setRefresh((n) => n + 1)} />}
      </Show>
    </div>
  );
}
