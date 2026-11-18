import { createSignal, createResource, For, Show, createEffect, onMount, onCleanup, ErrorBoundary, untrack, on } from "solid-js";
import { uploadsLanded } from "../../state/landed";
import { createPagedBrowse } from "../../utils/usePagedBrowse";
import {
  searchObjects, bucketIndexStatus,
  enableBucketIndex, disableBucketIndex, reindexBucket, syncPrefix,
} from "../../api/search";
import { getBucketEncryptionStatus, hasEncryptionIdentity } from "../../api/encryption";
import { deleteObject, presignGet, createFolder, headObject } from "../../api/objects";
import { runFolderDelete, runKeysDelete } from "./deleteJobs";
import { isKeyDeleting } from "../../state/deleteOps";
import { notify } from "../../utils/notify";
import {
  navigateToPrefix, currentView,
  pendingPreview, setPendingPreview,
} from "../../state/app";
import { useBackHandler } from "../../utils/androidBack";
import { toast, errMsg } from "../../state/toast";
import { confirmDialog } from "../../state/confirm";
import { confirmDestructive, isShownKey, showHiddenFiles } from "../../state/settings";
import { getPref, setPref } from "../../state/prefs";
import type { CachedObjectMeta } from "../../types";
import { DownloadModal, UploadModal, NewFolderModal, RenameModal } from "./modals";
import { NewFileModal } from "./NewFileModal";
import { extOf } from "../../utils/fmt";
import { EncryptionModal } from "./EncryptionModal";
import { VersionHistoryModal } from "./versionHistory/VersionHistoryModal";
import { PreviewPane } from "./PreviewPane";
import { ColumnPane } from "./ColumnPane";
import { Toolbar } from "./Toolbar";
import { IndexBar } from "./IndexBar";
import { BulkBar } from "./BulkBar";
import { SearchResultsPane } from "./SearchResultsPane";
import { ContextMenu, type CtxMenu } from "./ContextMenu";
import { ListView } from "./ListView";
import Spinner from "../../utils/Spinner";
import { t, tp } from "../../i18n";
import { folderUploadPlatform, isDesktopShell } from "../../utils/platform";
import { FolderUploadModal } from "./folderUpload/FolderUploadModal";
import { pickFolder } from "./folderUpload/pickFolder";
import { resumeRepicked } from "../transfers/repickSource";
import { sourceDisplayName } from "../../utils/sourceName";
import { useDesktopDrop } from "./folderUpload/useDesktopDrop";

export function ObjectBrowser(props: {
  accountId: string;
  accountName: string;
  bucket: string;
  prefix: string;
  defaultDownloadDir: string;
}) {
  const [refresh, setRefresh] = createSignal(0);
  const [refreshing, setRefreshing] = createSignal(false);

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
  createEffect(() => { props.bucket; setSearchQuery(""); setDebouncedQuery(""); });

  const [indexStatus, { refetch: refetchIndex }] = createResource(
    () => ({ a: props.accountId, b: props.bucket }),
    ({ a, b }) => bucketIndexStatus(a, b),
  );

  const [encStatus, { refetch: refetchEncStatus }] = createResource(
    () => ({ a: props.accountId, b: props.bucket }),
    ({ a, b }) => getBucketEncryptionStatus(a, b),
  );
  // Preflight the keychain so we can prompt re-import when the OS layer lost
  // the age identity; refetches with encStatus so the banner clears on import.
  const [identityPresent, { refetch: refetchIdentityPresent }] = createResource(
    () => {
      const s = encStatus.latest ?? encStatus();
      if (!s || !s.enabled) return null;
      return { a: props.accountId, b: props.bucket };
    },
    ({ a, b }) => hasEncryptionIdentity(a, b),
  );
  const [showEncryption, setShowEncryption] = createSignal(false);

  // Accumulating search: page 1 on query change, further pages appended
  // via next_cursor (a single fixed fetch would silently cap results).
  const SEARCH_PAGE = 200;
  const [searchItems, setSearchItems] = createSignal<CachedObjectMeta[]>([]);
  const [searchTotal, setSearchTotal] = createSignal(0);
  const [searchCursor, setSearchCursor] = createSignal<number | null>(null);
  const [searchLoading, setSearchLoading] = createSignal(false);
  const [searchMoreLoading, setSearchMoreLoading] = createSignal(false);
  let searchSeq = 0;

  async function runSearch(cursor: number | null) {
    const a = props.accountId, b = props.bucket, q = debouncedQuery();
    if (!q) return;
    const seq = ++searchSeq;
    cursor == null ? setSearchLoading(true) : setSearchMoreLoading(true);
    try {
      const res = await searchObjects({
        account_id: a, bucket: b, scope: { kind: "bucket" },
        query: q, filters: {}, sort: "name", sort_dir: "asc",
        page_size: SEARCH_PAGE, cursor: cursor ?? undefined,
      });
      if (seq !== searchSeq) return; // superseded by a newer query
      setSearchItems(prev => cursor == null ? res.objects : [...prev, ...res.objects]);
      setSearchTotal(res.total);
      setSearchCursor(res.next_cursor);
    } catch (e) {
      if (seq === searchSeq) toast.err(e);
    } finally {
      if (seq === searchSeq) { setSearchLoading(false); setSearchMoreLoading(false); }
    }
  }

  createEffect(() => {
    const q = debouncedQuery();
    props.accountId; props.bucket; refresh();
    if (!q) {
      // Bump seq to drop any in-flight fetch and reset all search state incl.
      // loading flags — a superseded fetch skips its own finally.
      searchSeq++;
      setSearchItems([]); setSearchTotal(0); setSearchCursor(null);
      setSearchLoading(false); setSearchMoreLoading(false);
      return;
    }
    runSearch(null);
  });

  const loadMoreSearch = () => {
    if (searchCursor() != null && !searchMoreLoading()) runSearch(searchCursor());
  };

  async function toggleIndex() {
    setIndexBusy(true);
    try {
      if (indexStatus()?.enabled) {
        const ok = await confirmDialog({
          title: t("browse.index.disableTitle"),
          body: t("browse.index.disableBody"),
          confirmLabel: t("browse.index.disable"), danger: true,
        });
        if (!ok) { setIndexBusy(false); return; }
        await disableBucketIndex(props.accountId, props.bucket);
        toast.ok(t("browse.index.disabledTitle"), t("browse.index.disabledDetail", { bucket: props.bucket }));
      } else {
        await enableBucketIndex(props.accountId, props.bucket);
        toast.ok(t("browse.index.startedTitle"), t("browse.index.startedDetail", { bucket: props.bucket }));
      }
      refetchIndex();
    } catch (e) { toast.err(e); }
    finally { setIndexBusy(false); }
  }

  async function handleReindex() {
    setIndexBusy(true);
    try {
      await reindexBucket(props.accountId, props.bucket);
      toast.ok(t("browse.index.reindexedTitle"), t("browse.index.reindexedDetail", { bucket: props.bucket }));
      refetchIndex();
    } catch (e) { toast.err(e); }
    finally { setIndexBusy(false); }
  }

  // Indexed: live-LIST the current folder into cache (upsert + sweep), re-read
  // from index; unindexed browse already hits S3 so a counter bump suffices.
  async function handleRefresh() {
    if (refreshing()) return;
    setRefreshing(true);
    const started = Date.now();
    try {
      const st = indexStatus.latest ?? indexStatus();
      const indexed = !!st?.enabled && st.last_full_sync_at != null;
      if (indexed) {
        await syncPrefix(props.accountId, props.bucket, props.prefix, false);
      }
      setRefresh((n) => n + 1);
    } catch (e) { toast.err(e); }
    finally {
      const wait = 500 - (Date.now() - started);
      if (wait > 0) await new Promise((r) => setTimeout(r, wait));
      setRefreshing(false);
    }
  }
  // Uploads into this bucket show up without a manual refresh: a cache re-read when they touch
  // this folder (slowed while a job runs, skipped during search), and one refresh when a job
  // ends (a live re-list of this folder for indexed buckets).
  const LANDED_MS = 2500;
  const LANDED_JOB_MS = 10_000;
  let landedTimer: ReturnType<typeof setTimeout> | undefined;
  let landedDue = 0;
  let landedEnd = false;
  const clearLanded = () => { clearTimeout(landedTimer); landedTimer = undefined; landedEnd = false; };
  onCleanup(clearLanded);
  createEffect(on(() => [props.accountId, props.bucket], clearLanded, { defer: true }));
  createEffect(on(uploadsLanded, (l) => {
    if (!l || l.accountId !== props.accountId || l.bucket !== props.bucket) return;
    const end = l.kind === "end";
    if (!end) {
      const p = props.prefix;
      if (searchQuery() || !(l.prefix.startsWith(p) || p.startsWith(l.prefix))) return;
    }
    landedEnd ||= end;
    // A pending timer covers this event unless this one is due sooner.
    const due = Date.now() + (l.kind === "job" ? LANDED_JOB_MS : LANDED_MS);
    if (landedTimer && landedDue <= due) return;
    clearTimeout(landedTimer);
    landedDue = due;
    const { accountId, bucket } = props;
    landedTimer = setTimeout(async () => {
      landedTimer = undefined;
      const final = landedEnd;
      landedEnd = false;
      if (accountId !== props.accountId || bucket !== props.bucket) return;
      try {
        const st = indexStatus.latest ?? indexStatus();
        if (final && st?.enabled && st.last_full_sync_at != null) {
          await syncPrefix(accountId, bucket, props.prefix, false);
        }
      } catch { /* best-effort; the cache re-read below still runs */ }
      setRefresh((n) => n + 1);
    }, due - Date.now());
  }, { defer: true }));

  const { state: browseData, loadMore: browseLoadMore } = createPagedBrowse(() => ({
    accountId: props.accountId,
    bucket: props.bucket,
    prefix: props.prefix,
    refresh: refresh(),
  }));

  const storedView = getPref<string | null>("viewMode", null);
  const isViewMode = (v: string | null): v is "list" | "columns" => v === "list" || v === "columns";
  const [viewMode, setViewMode] = createSignal<"list" | "columns">(
    isViewMode(storedView) ? storedView : "columns"
  );
  const saveViewMode = (m: "list" | "columns") => {
    setPref("viewMode", m);
    setViewMode(m);
  };

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
  const [showNewFile, setShowNewFile] = createSignal<string | null>(null);
  const [editNewKey, setEditNewKey] = createSignal<string | null>(null);
  const [downloadTarget, setDownloadTarget] = createSignal<CachedObjectMeta | null>(null);
  const [renameTarget, setRenameTarget] = createSignal<CachedObjectMeta | null>(null);
  const [versionTarget, setVersionTarget] = createSignal<CachedObjectMeta | null>(null);
  const [previewTarget, setPreviewTarget] = createSignal<CachedObjectMeta | null>(null);
  // Forces an open preview to refetch when its object changes underneath it
  // (e.g. version restore) without the key changing.
  const [previewReload, setPreviewReload] = createSignal(0);
  const [ctxMenu, setCtxMenu] = createSignal<CtxMenu | null>(null);
  const [dragOver, setDragOver] = createSignal(false);
  const [pendingDrop, setPendingDrop] = createSignal<string[]>([]);
  const [pendingFolders, setPendingFolders] = createSignal<string[]>([]);
  // Folder upload: local root (path or SAF tree URI), its display name, target prefix.
  const [folderUpload, setFolderUpload] = createSignal<{ root: string; name: string; prefix: string } | null>(null);
  async function startFolderUpload(prefix: string) {
    try {
      const picked = await pickFolder();
      if (!picked) return;
      // Android: picking a folder a paused job lost access to restores it; resume instead of a new upload.
      if ((await resumeRepicked(picked.root)).length) return;
      setFolderUpload({ ...picked, prefix });
    } catch (e) { toast.err(e); }
  }
  const uploadFolder = folderUploadPlatform ? (prefix: string) => void startFolderUpload(prefix) : undefined;

  // Android back: unwind this browser's own overlays (most-modal first) before
  // the shell handles view/prefix navigation; selection clears last.
  useBackHandler(() => true, () => {
    if (ctxMenu()) { setCtxMenu(null); return true; }
    if (versionTarget()) { setVersionTarget(null); return true; }
    if (previewTarget()) { setPreviewTarget(null); return true; }
    if (renameTarget()) { setRenameTarget(null); return true; }
    if (downloadTarget()) { setDownloadTarget(null); return true; }
    if (showNewFolder() !== null) { setShowNewFolder(null); return true; }
    if (showNewFile() !== null) { setShowNewFile(null); return true; }
    if (showUpload() !== false) { setShowUpload(false); setPendingDrop([]); return true; }
    if (showEncryption()) { setShowEncryption(false); return true; }
    if (selected().size > 0) { setSelected(new Set<string>()); return true; }
    return false;
  });

  createEffect(() => {
    const realSubs = new Set(browseData.subprefixes);
    if (realSubs.size > 0)
      setPendingFolders((prev) => prev.filter((f) => !realSubs.has(f)));
  });

  createEffect(() => { props.prefix; props.bucket; props.accountId; setSelected(new Set<string>()); });
  createEffect(() => { props.bucket; props.accountId; setPendingFolders([]); });
  createEffect(() => { viewMode(); setSelected(new Set<string>()); setPreviewTarget(null); });
  // Hiding dotfiles must also drop them from the selection, or bulk actions hit invisible keys.
  createEffect(() => {
    if (showHiddenFiles()) return;
    const cur = untrack(selected);
    const kept = [...cur].filter(isShownKey);
    if (kept.length !== cur.size) setSelected(new Set(kept));
  });

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
  // Only the manual refresh drives the toolbar spinner — browse loading already
  // has pane/overlay feedback, and a badge flash causes jitter.
  const showSyncing = () => refreshing();

  const deleting = (key: string) => isKeyDeleting(props.accountId, props.bucket, key);

  function toggleSel(key: string) {
    const s = new Set<string>(selected());
    s.has(key) ? s.delete(key) : s.add(key);
    setSelected(s);
  }

  // Props are live: a bucket switch mid-operation must not retarget later calls.
  const sameTarget = (accountId: string, bucket: string) =>
    props.accountId === accountId && props.bucket === bucket;

  async function handleDeleteFolder(sub: string) {
    const accountId = props.accountId;
    const bucket = props.bucket;
    const ok = await confirmDestructive({
      title: t("browse.delete.folderTitle"),
      body: t("browse.delete.folderBody", { sub }),
      confirmLabel: t("common.delete"),
      danger: true,
    });
    if (!ok) return;
    try {
      const res = await runFolderDelete(accountId, bucket, sub);
      if (sameTarget(accountId, bucket)) {
        dropPreviewUnder(accountId, bucket, sub, res.canceled || !!res.error || res.failed > 0);
        // Selection is per folder; leftovers under `sub` just get deselected.
        setSelected((prev) => new Set([...prev].filter((k) => !k.startsWith(sub))));
        setPendingFolders((prev) => prev.filter((f) => !f.startsWith(sub)));
        setRefresh((n) => n + 1);
      }
      if (res.canceled) {
        toast.info(t("browse.delete.canceledTitle"), t("browse.delete.canceledBody", { deleted: res.deleted, name: sub }));
      } else if (res.error) {
        toast.warn(
          t("browse.delete.failedAfter", { deleted: res.deleted, name: sub, message: errMsg(res.error) }),
          t("browse.delete.failedTitle"),
        );
      } else if (res.firstError) {
        toast.warn(
          t("browse.delete.partialFolder", {
            deleted: res.deleted, failed: res.failed, sub,
            key: res.firstError.key, message: res.firstError.message,
          }),
          t("browse.delete.partialTitle"),
        );
      } else {
        notify(t("browse.delete.folderDone"), `${sub} · ${bucket}`, {
          largeBody: t("browse.delete.folderDoneLarge", { sub, bucket }),
        });
      }
    } catch (e) { toast.err(e); }
  }

  // After a partial folder delete, keep the preview only if its object still exists.
  function dropPreviewUnder(accountId: string, bucket: string, sub: string, partial: boolean) {
    const key = previewTarget()?.key;
    if (!key?.startsWith(sub)) return;
    if (!partial) { setPreviewTarget(null); return; }
    headObject(accountId, bucket, key).catch(() => {
      if (sameTarget(accountId, bucket) && previewTarget()?.key === key) setPreviewTarget(null);
    });
  }

  async function handleDelete(obj: CachedObjectMeta) {
    const ok = await confirmDestructive({
      title: t("browse.delete.objectTitle"),
      body: t("browse.delete.objectBody", { key: obj.key }),
      confirmLabel: t("common.delete"),
      danger: true,
    });
    if (!ok) return;
    try {
      await deleteObject(obj.account_id, obj.bucket, obj.key);
      if (previewTarget()?.key === obj.key) setPreviewTarget(null);
      setRefresh((n) => n + 1);
      notify(t("browse.delete.objectDone", { name: obj.key.split("/").pop() || obj.key }), obj.bucket, {
        largeBody: t("browse.delete.objectDoneLarge", { key: obj.key, bucket: obj.bucket }),
      });
    } catch (e) { toast.err(e); }
  }

  async function handleBulkDelete() {
    const keys = Array.from(selected());
    if (!keys.length) return;
    const accountId = props.accountId;
    const bucket = props.bucket;
    const ok = await confirmDestructive({
      title: tp("browse.delete.bulkTitle", keys.length),
      body: keys.slice(0, 5).join("\n") + (keys.length > 5 ? "\n" + t("browse.delete.andMore", { count: keys.length - 5 }) : "") + "\n\n" + t("browse.delete.irreversible"),
      confirmLabel: t("common.delete"),
      danger: true,
    });
    if (!ok) return;
    // Rows show their own deleting state; leftovers are reselected at the end.
    if (sameTarget(accountId, bucket)) setSelected(new Set<string>());
    const res = await runKeysDelete(accountId, bucket, keys);
    if (sameTarget(accountId, bucket)) {
      const gone = new Set(res.deleted);
      if (previewTarget() && gone.has(previewTarget()!.key)) setPreviewTarget(null);
      // Failed and skipped keys get reselected so the user can retry.
      setSelected((prev) => new Set([...prev, ...keys.filter((k) => !gone.has(k))]));
      setRefresh((n) => n + 1);
    }
    if (res.error) { toast.err(res.error); return; }
    if (res.canceled) {
      toast.info(t("browse.delete.canceledTitle"), t("browse.delete.canceledBody", { deleted: res.deleted.length, name: bucket }));
    } else if (res.errors.length) {
      const first = res.errors[0];
      toast.warn(
        t("browse.delete.partialBucket", {
          deleted: res.deleted.length, failed: res.errors.length, bucket,
          key: first.key, message: first.message,
        }),
        t("browse.delete.partialTitle"),
      );
    } else {
      const n = res.deleted.length;
      notify(tp("browse.delete.bulkDone", n), bucket, {
        largeBody: tp("browse.delete.bulkDoneLarge", n, { bucket }),
      });
    }
  }

  async function handleCopyLink(obj: CachedObjectMeta) {
    try {
      // Backend refuses presign for encrypted objects unless the caller opts in
      // to sharing ciphertext — prompt first.
      let allowCiphertext = false;
      if ((encStatus.latest ?? encStatus())?.enabled) {
        const ok = await confirmDialog({
          title: t("browse.link.shareTitle"),
          body: t("browse.link.shareBody", { key: obj.key }),
          confirmLabel: t("browse.link.copyAnyway"),
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
          t("browse.link.encCopiedBody", { name: linkName }),
          t("browse.link.encCopiedTitle"),
        );
      } else {
        toast.ok(t("browse.link.copiedTitle"), t("browse.link.copiedDetail", { name: linkName }));
      }
    } catch (e) { toast.err(e); }
  }

  // Desktop: native drops arrive via useDesktopDrop with real paths. Elsewhere the DOM
  // drop has no paths, so it routes to the Upload dialog.
  let browserEl: HTMLDivElement | undefined;
  useDesktopDrop({
    area: () => browserEl,
    enabled: () => currentView() === "browse" && showUpload() === false && !folderUpload(),
    onOver: setDragOver,
    onFolder: (root) => setFolderUpload({ root, name: sourceDisplayName(root), prefix: props.prefix }),
    onFiles: (paths) => { setPendingDrop(paths); setShowUpload(props.prefix); },
  });
  function onDragOver(e: DragEvent) {
    e.preventDefault();
    if (!isDesktopShell) setDragOver(true);
  }
  function onDragLeave() { if (!isDesktopShell) setDragOver(false); }
  function onDrop(e: DragEvent) {
    e.preventDefault();
    if (isDesktopShell) return;
    setDragOver(false);
    toast.info(t("browse.drop.unsupportedTitle"), t("browse.drop.unsupportedDetail"));
    setShowUpload(props.prefix);
  }

  // Keep the menu on-screen: a row-end kebab (or long-press near an edge) can
  // put the anchor point flush against the viewport border.
  function clampMenu(x: number, y: number): { x: number; y: number } {
    const MENU_W = 210, MENU_H = 320, PAD = 8;
    return {
      x: Math.max(PAD, Math.min(x, window.innerWidth - MENU_W - PAD)),
      y: Math.max(PAD, Math.min(y, window.innerHeight - MENU_H - PAD)),
    };
  }

  function openCtx(e: MouseEvent, obj: CachedObjectMeta) {
    e.preventDefault(); e.stopPropagation();
    const { x, y } = clampMenu(e.clientX, e.clientY);
    setCtxMenu({ kind: "file", x, y, obj });
  }

  function openCtxFolder(e: MouseEvent, sub: string) {
    e.preventDefault(); e.stopPropagation();
    const { x, y } = clampMenu(e.clientX, e.clientY);
    setCtxMenu({ kind: "folder", x, y, sub });
  }

  onMount(() => {
    // Close on any click EXCEPT the kebab that opens it (that same click would
    // otherwise close the menu instantly) or clicks inside the menu.
    const close = (e: MouseEvent) => {
      const t = e.target as HTMLElement | null;
      if (t?.closest?.(".col-kebab, .obj-kebab, .context-menu")) return;
      setCtxMenu(null);
    };
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

  // Scroll only on folder/column navigation — not on every preview file click
  // (that smooth-scroll caused a whole-UI hitch when switching files).
  createEffect(() => { colPrefixes(); scrollColumnsRight(); });

  return (
    <div class="object-browser"
         ref={browserEl}
         onDragOver={onDragOver}
         onDragLeave={onDragLeave}
         onDrop={onDrop}>
      <Toolbar
        accountId={props.accountId}
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
        onRefresh={handleRefresh}
        onNewFolder={() => setShowNewFolder(props.prefix)}
        onNewFile={() => setShowNewFile(props.prefix)}
        onUpload={() => setShowUpload(props.prefix)}
        onUploadFolder={uploadFolder && (() => uploadFolder(props.prefix))}
      />

      <Show when={!browseData.initialLoaded && !browseData.error}>
        <div class="browse-loading-overlay">
          <Spinner size={50} />
        </div>
      </Show>

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
            <strong>{t("browse.enc.missing")}</strong>{" "}
            {t("browse.enc.missingBody")}
          </div>
          <button class="btn-primary" onClick={() => setShowEncryption(true)}>
            {t("browse.enc.loadKey")}
          </button>
        </div>
      </Show>

      <Show when={searchQuery()}>
        <SearchResultsPane
          searchQuery={searchQuery()}
          objects={searchItems()}
          total={searchTotal()}
          loading={searchLoading()}
          loadingMore={searchMoreLoading()}
          hasMore={searchCursor() != null}
          onLoadMore={loadMoreSearch}
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

      <div class="browse-area" classList={{ hidden: !!searchQuery(), "has-preview": !!previewTarget() }}>

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
                    isDeleting={deleting}
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

      <div class="browse-view list" classList={{ hidden: viewMode() !== "list" || !!searchQuery() }}>
        <ListView
          prefix={props.prefix}
          browseData={browseData}
          pendingFolders={pendingFolders().filter((f) => {
            const rel = f.slice(props.prefix.length);
            return f.startsWith(props.prefix) && rel.replace(/\/$/, "").indexOf("/") === -1;
          })}
          onLoadMore={browseLoadMore}
          isDeleting={deleting}
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
          onNewFolder={() => setShowNewFolder(props.prefix)}
          onUpload={() => setShowUpload(props.prefix)}
          onUploadFolder={uploadFolder && (() => uploadFolder(props.prefix))}
        />
      </div>

      <Show when={previewTarget()}>
        <ErrorBoundary fallback={(err, reset) => (
          <div class="preview-pane preview-err-card">
            <span class="preview-err-card-msg">{t("browse.previewError", { message: errMsg(err) })}</span>
            <button class="btn-ghost preview-err-card-close" onClick={() => { setPreviewTarget(null); reset(); }}>{t("common.close")}</button>
          </div>
        )}>
          <PreviewPane
            obj={previewTarget()!}
            onClose={() => setPreviewTarget(null)}
            onDownload={() => { const o = previewTarget(); if (o) setDownloadTarget(o); }}
            onCopyLink={() => { const o = previewTarget(); if (o) handleCopyLink(o); }}
            encrypted={(encStatus.latest ?? encStatus())?.enabled}
            reloadToken={previewReload()}
            onListChanged={() => setRefresh((n) => n + 1)}
            startEditing={editNewKey() === previewTarget()?.key}
            onEditStarted={() => setEditNewKey(null)}
          />
        </ErrorBoundary>
      </Show>

      </div>

      <Show when={dragOver()}>
        <div class="drop-overlay">{isDesktopShell ? t("folderUpload.drop.overlay") : t("browse.drop.overlay")}</div>
      </Show>

      <Show when={ctxMenu()}>
        {(m) => (
          <ContextMenu
            menu={m()}
            onClose={() => setCtxMenu(null)}
            onNewFolder={(prefix) => setShowNewFolder(prefix)}
            onNewFile={(prefix) => setShowNewFile(prefix)}
            onUploadHere={(sub) => setShowUpload(sub)}
            onUploadFolderHere={uploadFolder}
            onDeleteFolder={handleDeleteFolder}
            onPreview={(obj) => setPreviewTarget(obj)}
            onDownload={(obj) => setDownloadTarget(obj)}
            onCopyLink={handleCopyLink}
            onRename={(obj) => setRenameTarget(obj)}
            onVersionHistory={(obj) => setVersionTarget(obj)}
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
                     onClose={() => { setShowUpload(false); setPendingDrop([]); }} />
      </Show>

      <Show when={folderUpload()} keyed>
        {(fu) => <FolderUploadModal accountId={props.accountId} bucket={props.bucket}
                                    localRoot={fu.root} name={fu.name} prefix={fu.prefix}
                                    encrypted={(encStatus.latest ?? encStatus())?.enabled ?? false}
                                    onClose={() => { setFolderUpload(null); setRefresh((n) => n + 1); }} />}
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

      <Show when={showNewFile() !== null}>
        <NewFileModal
          accountId={props.accountId}
          bucket={props.bucket}
          prefix={showNewFile()!}
          onClose={() => setShowNewFile(null)}
          onCreated={(key, mime) => {
            const base = key.split("/").pop() || key;
            setRefresh((n) => n + 1);
            setEditNewKey(key);
            setPreviewTarget({
              account_id: props.accountId, bucket: props.bucket, key,
              size: 0, etag: null, last_modified: null, storage_class: null,
              content_type: mime, extension: extOf(base) || null, basename: base,
              version_id: null, synced_at: 0,
            });
          }}
        />
      </Show>

      <Show when={renameTarget()}>
        {(obj) => <RenameModal obj={obj()} onClose={() => setRenameTarget(null)}
                                onDone={(newKey) => {
                                  const cur = previewTarget();
                                  if (cur?.key === obj().key) {
                                    const basename = newKey.split("/").pop() || newKey;
                                    const dot = basename.lastIndexOf(".");
                                    setPreviewTarget({
                                      ...cur, key: newKey, basename, version_id: null,
                                      extension: dot > 0 ? basename.slice(dot + 1).toLowerCase() : null,
                                    });
                                  }
                                  setRefresh((n) => n + 1);
                                }} />}
      </Show>

      {/* keyed: plain value, so the modal can still read the key after closing mid-request */}
      <Show when={versionTarget()} keyed>
        {(obj) => <VersionHistoryModal
                    accountId={props.accountId}
                    bucket={props.bucket}
                    objectKey={obj.key}
                    onClose={() => setVersionTarget(null)}
                    onChanged={() => {
                      setRefresh((n) => n + 1);
                      // If the object whose versions changed is the one on screen,
                      // force the preview to re-download the new latest content.
                      if (previewTarget()?.key === obj.key) setPreviewReload((n) => n + 1);
                    }} />}
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
