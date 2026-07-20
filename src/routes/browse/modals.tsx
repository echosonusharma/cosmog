import { createSignal, For, Show } from "solid-js";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { createBucket } from "../../api/buckets";
import { moveObject } from "../../api/objects";
import { enqueueUpload, enqueueDownload } from "../../api/transfers";
import { toast, errMsg } from "../../state/toast";
import { basename } from "../../utils/fmt";
import { IconLock } from "../../utils/icons";
import type { CachedObjectMeta } from "../../types";
import { pathFromDialog, resolveUploadPath, resolveDownloadPath, displayNameFromUri, registerSafFinalize, withTimestamp } from "./helpers";
import { isMobile } from "../../utils/breakpoint";
import { invoke } from "@tauri-apps/api/core";

function displayName(p: string): string {
  if (p.startsWith("content://") || p.startsWith("file://")) return displayNameFromUri(p, "file");
  return basename(p);
}

// ── modals ────────────────────────────────────────────────────────────────────

export function DownloadModal(props: {
  obj: CachedObjectMeta;
  defaultDir: string;
  onClose: () => void;
}) {
  const mobile = isMobile();
  const defaultPath = `${props.defaultDir}/${props.obj.basename}`.replace(/\/+/g, "/");
  // Desktop: writable text path input. Mobile: file must come from SAF picker
  // (a text path lands in app cache and is invisible to the user).
  const [dest, setDest] = createSignal(mobile ? "" : defaultPath);
  const [safUri, setSafUri] = createSignal<string | null>(null);
  // Human label shown to the user on mobile after they pick a location.
  const [pickedLabel, setPickedLabel] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");

  // The save dialog pre-creates a 0-byte placeholder the moment a location is
  // picked. Until the download is actually queued, an abandoned pick (modal
  // canceled, or a re-pick replacing it) must delete that placeholder or the
  // user finds an empty file at the old location.
  let queued = false;
  function dropPlaceholder(uri: string | null) {
    if (uri) invoke("delete_saf_document", { uri }).catch(() => {});
  }
  function close() {
    if (!queued) dropPlaceholder(safUri());
    props.onClose();
  }

  async function browse() {
    // Pre-fill the picker with a timestamped name so the extension stays
    // intact and repeated downloads never collide on the same filename.
    const suggested = mobile ? withTimestamp(props.obj.basename) : dest() || defaultPath;
    const sel = await saveDialog({ defaultPath: suggested });
    if (!sel) return;
    const raw = pathFromDialog(sel);
    const { path, safUri: uri } = await resolveDownloadPath(raw, props.obj.basename);
    const prev = safUri();
    if (prev && prev !== uri) dropPlaceholder(prev);
    setDest(path);
    setSafUri(uri);
    setPickedLabel(uri ? displayNameFromUri(uri, props.obj.basename) : path);
  }

  async function submit() {
    if (!dest().trim()) return;
    setBusy(true); setErr("");
    try {
      const { path: target, safUri: resolvedUri } = await resolveDownloadPath(dest().trim(), props.obj.basename);
      const finalUri = safUri() ?? resolvedUri;
      const res = await enqueueDownload(props.obj.account_id, props.obj.bucket, props.obj.key, target);
      if (finalUri && res?.transfer_id) {
        registerSafFinalize(res.transfer_id, target, finalUri);
      }
      queued = true;
      props.onClose();
    } catch (e) {
      setErr(errMsg(e));
    } finally { setBusy(false); }
  }

  const canSubmit = () => !busy() && (mobile ? !!safUri() : !!dest().trim());

  return (
    <div class="modal-backdrop" onClick={close}>
      <div class="modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-title">Download</div>
        <div class="modal-sub">{props.obj.key}</div>
        <Show
          when={mobile}
          fallback={
            <>
              <label class="modal-label">Save to</label>
              <div class="file-picker-row">
                <input class="field" value={dest()} onInput={(e) => setDest(e.currentTarget.value.trim())} disabled={busy()} />
                <button type="button" class="btn-secondary" disabled={busy()} onClick={browse}>Browse</button>
              </div>
            </>
          }
        >
          <Show
            when={safUri()}
            fallback={
              <button type="button" class="btn-secondary btn-block mt-2" disabled={busy()} onClick={browse}>
                Choose location
              </button>
            }
          >
            <label class="modal-label">Saving to</label>
            <div class="file-picker-row">
              <span class="field truncate field-static">{pickedLabel()}</span>
              <button type="button" class="btn-secondary" disabled={busy()} onClick={browse}>Change</button>
            </div>
          </Show>
        </Show>
        <Show when={err()}><div class="status-msg err">{err()}</div></Show>
        <div class="btn-row mt-3">
          <button class="btn-secondary btn-half" onClick={close}>Cancel</button>
          <button class="btn-primary btn-half" disabled={!canSubmit()} onClick={submit}>
            {busy() ? "Queuing…" : "Download"}
          </button>
        </div>
      </div>
    </div>
  );
}

export function UploadModal(props: {
  accountId: string;
  bucket: string;
  prefix: string;
  initialFiles?: string[];
  encrypted?: boolean;
  onClose: () => void;
  onQueued?: () => void;
}) {
  const [files, setFiles] = createSignal<string[]>(props.initialFiles ?? []);
  const [keyPrefix, setKeyPrefix] = createSignal(props.prefix);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");
  // Which file we're currently encrypting (encrypted buckets stream through
  // crypto::encrypt_file synchronously before enqueue returns, so a 3 GB file
  // can spend minutes here with the button stuck on "Queuing…"). Surface the
  // file name so the user knows something is happening.
  const [currentIdx, setCurrentIdx] = createSignal(0);

  async function browse() {
    const sel = await openDialog({ multiple: true, directory: false });
    if (!sel) return;
    const arr = Array.isArray(sel) ? sel : [sel];
    setFiles(arr.map(pathFromDialog).filter(Boolean));
  }

  async function submit() {
    const list = files();
    if (!list.length) return;
    setBusy(true); setErr(""); setCurrentIdx(0);
    try {
      for (let i = 0; i < list.length; i++) {
        setCurrentIdx(i);
        const rawPath = list[i];
        const { path, name } = await resolveUploadPath(rawPath);
        const key = keyPrefix().trim()
          ? keyPrefix().trim().replace(/\/?$/, "/") + name
          : name;
        await enqueueUpload(props.accountId, props.bucket, key, path);
      }
      props.onClose();
      props.onQueued?.();
    } catch (e) {
      setErr(errMsg(e));
    } finally { setBusy(false); }
  }

  function submitLabel() {
    if (!busy()) return `Upload${files().length > 1 ? ` (${files().length})` : ""}`;
    if (props.encrypted) {
      const idx = currentIdx();
      const name = files()[idx];
      const suffix = files().length > 1 ? ` (${idx + 1}/${files().length})` : "";
      return name ? `Encrypting${suffix}…` : "Encrypting…";
    }
    return "Queuing…";
  }

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-title">Upload files</div>
        <div class="file-picker-row">
          <span class="field truncate field-static">
            {files().length === 0 ? "No files selected"
              : files().length === 1 ? displayName(files()[0])
              : `${files().length} files selected`}
          </span>
          <button type="button" class="btn-secondary" disabled={busy()} onClick={browse}>Browse</button>
        </div>
        <Show when={files().length > 1}>
          <div class="upload-file-list">
            <For each={files()}>{(f) => <div class="upload-file-item">{displayName(f)}</div>}</For>
          </div>
        </Show>
        <label class="modal-label">Key prefix (optional)</label>
        <input class="field" placeholder={props.prefix || "folder/"} value={keyPrefix()}
               onInput={(e) => setKeyPrefix(e.currentTarget.value)} disabled={busy()} />
        <Show when={props.encrypted}>
          <div class="upload-encrypted-note">
            <span class="upload-encrypted-note-icon"><IconLock size={18} /></span>
            <span class="upload-encrypted-note-text">Encrypted bucket. Files lock on this device before upload; large files may take a moment.</span>
          </div>
        </Show>
        <Show when={busy() && props.encrypted && files().length > 0}>
          <div class="upload-encrypting-progress">
            <span class="spinner" />
            <span class="upload-encrypting-progress-label">
              Encrypting <code>{displayName(files()[currentIdx()] ?? "")}</code>
            </span>
          </div>
        </Show>
        <Show when={err()}><div class="status-msg err">{err()}</div></Show>
        <div class="btn-row mt-3">
          <button class="btn-secondary btn-half" onClick={props.onClose} disabled={busy()}>Cancel</button>
          <button class="btn-primary btn-half" disabled={!files().length || busy()} onClick={submit}>
            {submitLabel()}
          </button>
        </div>
      </div>
    </div>
  );
}

export function NewBucketModal(props: { accountId: string; onClose: () => void; onDone: () => void }) {
  const [name, setName] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");

  async function submit() {
    if (!name().trim()) return;
    setBusy(true);
    try {
      await createBucket(props.accountId, name().trim());
      props.onDone(); props.onClose();
      toast.ok("Bucket created", `"${name().trim()}" is ready to use`);
    } catch (e) { setErr(errMsg(e)); } finally { setBusy(false); }
  }

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-title">New bucket</div>
        <input class="field" placeholder="bucket-name" value={name()}
               onInput={(e) => setName(e.currentTarget.value.trim())} disabled={busy()}
               onKeyDown={(e) => e.key === "Enter" && submit()} />
        <Show when={err()}><div class="status-msg err">{err()}</div></Show>
        <div class="btn-row mt-3">
          <button class="btn-secondary btn-half" onClick={props.onClose}>Cancel</button>
          <button class="btn-primary btn-half" disabled={!name().trim() || busy()} onClick={submit}>
            {busy() ? "Creating…" : "Create"}
          </button>
        </div>
      </div>
    </div>
  );
}

export function NewFolderModal(props: {
  prefix: string;
  onClose: () => void;
  onDone: (folderKey: string) => void;
}) {
  const initial = props.prefix ? props.prefix.replace(/\/$/, "") + "/" : "";
  const [path, setPath] = createSignal(initial);

  function submit() {
    const cleaned = path().trim().replace(/\/+/g, "/").replace(/^\//, "").replace(/\/$/, "");
    if (!cleaned) return;
    props.onDone(cleaned + "/");
    props.onClose();
  }

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-title">New folder</div>
        <div class="modal-sub modal-sub-path-label">Path</div>
        <input class="field" placeholder="path/to/folder-name"
               value={path()}
               onInput={(e) => setPath(e.currentTarget.value.trim())}
               onKeyDown={(e) => e.key === "Enter" && submit()}
               ref={(el) => setTimeout(() => { el.focus(); el.setSelectionRange(el.value.length, el.value.length); }, 0)} />
        <div class="btn-row mt-3">
          <button class="btn-secondary btn-half" onClick={props.onClose}>Cancel</button>
          <button class="btn-primary btn-half" disabled={!path().trim().replace(/\//g, "")} onClick={submit}>
            Create
          </button>
        </div>
      </div>
    </div>
  );
}

export function RenameModal(props: {
  obj: CachedObjectMeta;
  onClose: () => void;
  onDone: () => void;
}) {
  const [newKey, setNewKey] = createSignal(props.obj.key);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");

  async function submit() {
    const target = newKey().trim();
    if (!target || target === props.obj.key) { props.onClose(); return; }
    setBusy(true);
    try {
      await moveObject(props.obj.account_id, props.obj.bucket, props.obj.key, props.obj.bucket, target);
      props.onDone(); props.onClose();
      toast.ok(`Renamed ${props.obj.key.split("/").pop() || props.obj.key}`, `Now at "${target}" in "${props.obj.bucket}"`);
    } catch (e) { setErr(errMsg(e)); } finally { setBusy(false); }
  }

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-title">Rename / Move</div>
        <div class="modal-sub">{props.obj.key}</div>
        <input class="field" value={newKey()}
               onInput={(e) => setNewKey(e.currentTarget.value.trim())} disabled={busy()} autofocus
               onKeyDown={(e) => e.key === "Enter" && submit()} />
        <Show when={err()}><div class="status-msg err">{err()}</div></Show>
        <div class="btn-row mt-3">
          <button class="btn-secondary btn-half" onClick={props.onClose}>Cancel</button>
          <button class="btn-primary btn-half" disabled={busy()} onClick={submit}>
            {busy() ? "Working…" : "Rename"}
          </button>
        </div>
      </div>
    </div>
  );
}
