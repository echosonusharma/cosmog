import { createSignal, createResource, For, Show, createEffect, onMount, onCleanup, ErrorBoundary } from "solid-js";
import { createPagedBrowse } from "../../utils/usePagedBrowse";
import {
  searchObjects, bucketIndexStatus,
  enableBucketIndex, disableBucketIndex, reindexBucket,
} from "../../api/search";
import { getBucketEncryptionStatus, hasEncryptionIdentity } from "../../api/encryption";
import {
  deleteObject, deleteObjects, presignGet,
  listKeysUnderPrefix, createFolder,
} from "../../api/objects";
import { notify } from "../../utils/notify";
import {
  navigateToPrefix,
  pendingPreview, setPendingPreview,
} from "../../state/app";
import { toast, errMsg } from "../../state/toast";
import { confirmDialog } from "../../state/confirm";
import type { CachedObjectMeta } from "../../types";
import { DownloadModal, UploadModal, NewFolderModal, RenameModal } from "./modals";
import { EncryptionModal } from "./EncryptionModal";
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
  createEffect(() => { props.bucket; setSearchQuery(""); setDebouncedQuery(""); mutateSearchResults(undefined); });

  const [indexStatus, { refetch: refetchIndex }] = createResource(
    () => ({ a: props.accountId, b: props.bucket }),
    ({ a, b }) => bucketIndexStatus(a, b),
  );

  const [encStatus, { refetch: refetchEncStatus }] = createResource(
    () => ({ a: props.accountId, b: props.bucket }),
    ({ a, b }) => getBucketEncryptionStatus(a, b),
  );
  // Preflight the keychain so we can prompt the user to re-import the age
  // identity when the OS layer lost it (new machine, keychain wipe, different
  // OS user). Refetches when encStatus changes so the banner clears the moment
  // an import succeeds.
  const [identityPresent, { refetch: refetchIdentityPresent }] = createResource(
    () => {
      const s = encStatus.latest ?? encStatus();
      if (!s || !s.enabled) return null;
      return { a: props.accountId, b: props.bucket };
    },
    ({ a, b }) => hasEncryptionIdentity(a, b),
  );
  const [showEncryption, setShowEncryption] = createSignal(false);

  const [searchResults, { mutate: mutateSearchResults }] = createResource(
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
        toast.ok("Index disabled", `Cached metadata for "${props.bucket}" was cleared`);
      } else {
        await enableBucketIndex(props.accountId, props.bucket);
        toast.ok("Indexing started", `Building the metadata index for "${props.bucket}"`);
      }
      refetchIndex();
    } catch (e) { toast.err(e); }
    finally { setIndexBusy(false); }
  }

  async function handleReindex() {
    setIndexBusy(true);
    try {
      await reindexBucket(props.accountId, props.bucket);
      toast.ok("Re-indexed", `Metadata for "${props.bucket}" was refreshed`);
      refetchIndex();
    } catch (e) { toast.err(e); }
    finally { setIndexBusy(false); }
  }
  const { state: browseData, loadMore: browseLoadMore } = createPagedBrowse(() => ({
    accountId: props.accountId,
    bucket: props.bucket,
    prefix: props.prefix,
    refresh: refresh(),
  }));

  const storedView = localStorage.getItem("cosmog:viewMode");
  const isViewMode = (v: string | null): v is "list" | "columns" => v === "list" || v === "columns";
  const [viewMode, setViewMode] = createSignal<"list" | "columns">(
    isViewMode(storedView) ? storedView : "columns"
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
  const [pendingFolders, setPendingFolders] = createSignal<string[]>([]);

  // Auto-prune pending folders once real browse data for the current prefix covers them.
  createEffect(() => {
    const realSubs = new Set(browseData.subprefixes);
    if (realSubs.size > 0)
      setPendingFolders((prev) => prev.filter((f) => !realSubs.has(f)));
  });

  createEffect(() => { props.prefix; props.bucket; props.accountId; setSelected(new Set<string>()); });
  createEffect(() => { viewMode(); setSelected(new Set<string>()); setPreviewTarget(null); });

  // Consume pendingPreview set by navigateToObject (from Search).
  createEffect(() => {
    const pending = pendingPreview();
    if (!pending) return;
    if (pending.account_id !== props.accountId || pending.bucket !== props.bucket) return;
    if (browseData.loading) return;
    setPreviewTarget(pending);
    setPendingPreview(null);
  });

  createEffect(() => { props.bucket; setPreviewTarget(null); });
  const showSyncing = () => browseData.loading;

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
      notify("Folder deleted", `${sub} · ${props.bucket}`, {
        largeBody: `Deleted folder "${sub}" and its contents from "${props.bucket}"`,
      });
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
      notify(`Deleted ${obj.key.split("/").pop() || obj.key}`, obj.bucket, {
        largeBody: `Deleted "${obj.key}" from "${obj.bucket}"`,
      });
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
        toast.warn(
          `${res.deleted.length} deleted, ${res.errors.length} failed in "${props.bucket}"`,
          "Partial delete",
        );
      } else {
        const n = res.deleted.length;
        notify(`Deleted ${n} object${n > 1 ? "s" : ""}`, props.bucket, {
          largeBody: `Deleted ${n} object${n > 1 ? "s" : ""} from "${props.bucket}"`,
        });
      }
    } catch (e) { toast.err(e); }
  }

  async function handleCopyLink(obj: CachedObjectMeta) {
    try {
      // Encrypted buckets: backend refuses presign for encrypted objects unless
      // the caller explicitly opts in to sharing ciphertext. Prompt first.
      let allowCiphertext = false;
      if ((encStatus.latest ?? encStatus())?.enabled) {
        const ok = await confirmDialog({
          title: "Share encrypted object?",
          body: `"${obj.key}" is encrypted. A shared link downloads the locked file as-is, not the readable version. The person you share it with will need your key file to open it. Continue?`,
          confirmLabel: "Copy link anyway",
          danger: true,
        });
        if (!ok) return;
        allowCiphertext = true;
      }
      const url = await presignGet(obj.account_id, obj.bucket, obj.key, undefined, allowCiphertext);
      await navigator.clipboard.writeText(url);
      const linkName = obj.key.split("/").pop() || obj.key;
      if (allowCiphertext) {
        toast.warn(
          `Link for "${linkName}" is on the clipboard. The recipient gets the locked file, share your key file separately so they can open it.`,
          "Encrypted link copied",
        );
      } else {
        toast.ok("Link copied", `Shareable link for "${linkName}" is on the clipboard`);
      }
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

  let columnsScrollEl: HTMLDivElement | undefined;
  let scrollPending = false;

  function scrollColumnsRight() {
    if (!columnsScrollEl || scrollPending) return;
    scrollPending = true;
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        columnsScrollEl?.scrollTo({ left: columnsScrollEl.scrollWidth, behavior: "smooth" });
        scrollPending = false;
      });
    });
  }

  createEffect(() => { colPrefixes(); scrollColumnsRight(); });
  createEffect(() => { previewTarget(); scrollColumnsRight(); });

  return (
    <div class="object-browser"
         onDragOver={onDragOver}
         onDragLeave={onDragLeave}
         onDrop={onDrop}>
      <Toolbar
        accountName={props.accountName}
        bucket={props.bucket}
        prefix={props.prefix}
        indexStatus={indexStatus}
        indexBusy={indexBusy()}
        onToggleIndex={toggleIndex}
        encryptionEnabled={(encStatus.latest ?? encStatus())?.enabled ?? false}
        onOpenEncryption={() => setShowEncryption(true)}
        searchQuery={searchQuery()}
        onSearchInput={setSearchQuery}
        onClearSearch={() => setSearchQuery("")}
        showSyncing={showSyncing()}
        mode={browseData.mode}
        viewMode={viewMode()}
        onViewMode={saveViewMode}
        onRefresh={() => { setRefresh((n) => n + 1); }}
        onNewFolder={() => setShowNewFolder(props.prefix)}
        onUpload={() => setShowUpload(props.prefix)}
      />

      {/* ── initial load overlay — covers content area while first page fetches ── */}
      <Show when={!browseData.initialLoaded && !browseData.error}>
        <div class="browse-loading-overlay">
          <span class="spinner spinner-lg" />
        </div>
      </Show>

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

      {/* Identity-missing banner: bucket has encryption configured but the
          OS keychain does not hold the secret (fresh install, keychain wipe,
          different OS user). Downloads/previews of encrypted objects will
          fail until the user imports a backup identity file. */}
      <Show when={(encStatus.latest ?? encStatus())?.enabled && identityPresent() === false}>
        <div class="enc-identity-banner">
          <div class="enc-identity-banner-text">
            <strong>Encryption key missing on this device.</strong>{" "}
            Files in this bucket cannot be opened until you load the key file you saved earlier.
          </div>
          <button class="btn-primary" onClick={() => setShowEncryption(true)}>
            Load key
          </button>
        </div>
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
      <div class="browse-area" classList={{ hidden: !!searchQuery() }}>

      {/* ── columns view ── */}
        <div class="browse-view" classList={{ hidden: viewMode() !== "columns" || !!searchQuery() }}>
          <div class="columns-scroll" ref={columnsScrollEl}>
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
                    active={i() === colPrefixes().length - 1}
                    onSelectFolder={(sub) => navigateToPrefix(sub)}
                    onSelectFile={(obj) => { setPreviewTarget(obj); }}
                    onCtxFolder={openCtxFolder}
                    onCtxFile={openCtx}
                    onCtxPane={(e, prefix) => setCtxMenu({ kind: "pane", x: e.clientX, y: e.clientY, prefix })}
                    refresh={refresh()}
                    pendingFolders={pendingFolders().filter((f) => {
                      const rel = f.slice(pfx.length);
                      return f.startsWith(pfx) && rel.replace(/\/$/, "").indexOf("/") === -1;
                    })}
                  />
                );
              }}
            </For>
          </div>
        </div>

      {/* ── list view ── */}
      <div class="browse-view list" classList={{ hidden: viewMode() !== "list" || !!searchQuery() }}>
        <ListView
          prefix={props.prefix}
          browseData={browseData}
          pendingFolders={pendingFolders().filter((f) => {
            const rel = f.slice(props.prefix.length);
            return f.startsWith(props.prefix) && rel.replace(/\/$/, "").indexOf("/") === -1;
          })}
          onLoadMore={browseLoadMore}
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
          <div class="preview-pane preview-err-card">
            <span class="preview-err-card-msg">Preview error: {errMsg(err)}</span>
            <button class="btn-ghost preview-err-card-close" onClick={() => { setPreviewTarget(null); reset(); }}>Close</button>
          </div>
        )}>
          <PreviewPane
            obj={previewTarget()!}
            onClose={() => setPreviewTarget(null)}
            onDownload={() => { const o = previewTarget(); if (o) setDownloadTarget(o); }}
            onCopyLink={() => { const o = previewTarget(); if (o) handleCopyLink(o); }}
            encrypted={(encStatus.latest ?? encStatus())?.enabled}
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
                     encrypted={(encStatus.latest ?? encStatus())?.enabled ?? false}
                     onClose={() => { setShowUpload(false); setPendingDrop([]); }}
                     onQueued={() => { const t = setTimeout(() => setRefresh((n) => n + 1), 1500); onCleanup(() => clearTimeout(t)); }} />
      </Show>

      <Show when={showNewFolder() !== null}>
        <NewFolderModal
          prefix={showNewFolder()!}
          onClose={() => setShowNewFolder(null)}
          onDone={(folderKey) => {
            setPendingFolders((prev) => prev.includes(folderKey) ? prev : [...prev, folderKey]);
            createFolder(props.accountId, props.bucket, folderKey)
              .then(() => setRefresh((n) => n + 1))
              .catch((e) => {
                toast.err(e);
                setPendingFolders((prev) => prev.filter((f) => f !== folderKey));
              });
          }}
        />
      </Show>

      <Show when={renameTarget()}>
        {(obj) => <RenameModal obj={obj()} onClose={() => setRenameTarget(null)}
                                onDone={() => setRefresh((n) => n + 1)} />}
      </Show>

      <Show when={showEncryption()}>
        <EncryptionModal
          accountId={props.accountId}
          bucket={props.bucket}
          enabled={(encStatus.latest ?? encStatus())?.enabled ?? false}
          identityPresent={identityPresent() ?? true}
          onClose={() => setShowEncryption(false)}
          onChanged={() => { refetchEncStatus(); refetchIdentityPresent(); }}
        />
      </Show>
    </div>
  );
}
