import { createSignal, For, Show } from "solid-js";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { createBucket } from "../../api/buckets";
import { moveObject } from "../../api/objects";
import { enqueueUpload, enqueueDownload } from "../../api/transfers";
import { notify } from "../../utils/notify";
import { toast, errMsg } from "../../state/toast";
import { basename } from "../../utils/fmt";
import type { CachedObjectMeta } from "../../types";
import { pathFromDialog } from "./helpers";

// ── modals ────────────────────────────────────────────────────────────────────

export function DownloadModal(props: {
  obj: CachedObjectMeta;
  defaultDir: string;
  onClose: () => void;
}) {
  const defaultPath = `${props.defaultDir}/${props.obj.basename}`.replace(/\/+/g, "/");
  const [dest, setDest] = createSignal(defaultPath);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");

  async function browse() {
    const sel = await saveDialog({ defaultPath: dest() });
    if (sel) setDest(pathFromDialog(sel));
  }

  async function submit() {
    if (!dest().trim()) return;
    setBusy(true); setErr("");
    try {
      await enqueueDownload(props.obj.account_id, props.obj.bucket, props.obj.key, dest().trim());
      props.onClose();
      notify("Download started", props.obj.basename);
    } catch (e) {
      setErr(errMsg(e));
    } finally { setBusy(false); }
  }

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-title">Download</div>
        <div class="modal-sub">{props.obj.key}</div>
        <label class="modal-label">Save to</label>
        <div class="file-picker-row">
          <input class="field" value={dest()} onInput={(e) => setDest(e.currentTarget.value.trim())} disabled={busy()} />
          <button type="button" class="btn-secondary" disabled={busy()} onClick={browse}>Browse</button>
        </div>
        <Show when={err()}><div class="status-msg err">{err()}</div></Show>
        <div class="btn-row mt-3">
          <button class="btn-secondary" style="flex:1" onClick={props.onClose}>Cancel</button>
          <button class="btn-primary" style="flex:1" disabled={!dest().trim() || busy()} onClick={submit}>
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
  onClose: () => void;
  onQueued?: () => void;
}) {
  const [files, setFiles] = createSignal<string[]>(props.initialFiles ?? []);
  const [keyPrefix, setKeyPrefix] = createSignal(props.prefix);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");

  async function browse() {
    const sel = await openDialog({ multiple: true, directory: false });
    if (!sel) return;
    const arr = Array.isArray(sel) ? sel : [sel];
    setFiles(arr.map(pathFromDialog).filter(Boolean));
  }

  async function submit() {
    const list = files();
    if (!list.length) return;
    setBusy(true); setErr("");
    try {
      for (const path of list) {
        const key = keyPrefix().trim()
          ? keyPrefix().trim().replace(/\/?$/, "/") + basename(path)
          : basename(path);
        await enqueueUpload(props.accountId, props.bucket, key, path);
      }
      props.onClose();
      props.onQueued?.();
      notify("Upload queued", `${list.length} file${list.length > 1 ? "s" : ""} queued for upload`);
    } catch (e) {
      setErr(errMsg(e));
    } finally { setBusy(false); }
  }

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-title">Upload files</div>
        <div class="file-picker-row">
          <span class="field truncate" style="display:flex;align-items:center;color:var(--text-muted)">
            {files().length === 0 ? "No files selected"
              : files().length === 1 ? basename(files()[0])
              : `${files().length} files selected`}
          </span>
          <button type="button" class="btn-secondary" disabled={busy()} onClick={browse}>Browse</button>
        </div>
        <Show when={files().length > 1}>
          <div class="upload-file-list">
            <For each={files()}>{(f) => <div class="upload-file-item">{basename(f)}</div>}</For>
          </div>
        </Show>
        <label class="modal-label">Key prefix (optional)</label>
        <input class="field" placeholder={props.prefix || "folder/"} value={keyPrefix()}
               onInput={(e) => setKeyPrefix(e.currentTarget.value)} disabled={busy()} />
        <Show when={err()}><div class="status-msg err">{err()}</div></Show>
        <div class="btn-row mt-3">
          <button class="btn-secondary" style="flex:1" onClick={props.onClose}>Cancel</button>
          <button class="btn-primary" style="flex:1" disabled={!files().length || busy()} onClick={submit}>
            {busy() ? "Queuing…" : `Upload${files().length > 1 ? ` (${files().length})` : ""}`}
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
      toast.ok(`Bucket "${name().trim()}" created`);
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
          <button class="btn-secondary" style="flex:1" onClick={props.onClose}>Cancel</button>
          <button class="btn-primary" style="flex:1" disabled={!name().trim() || busy()} onClick={submit}>
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
        <div class="modal-sub" style="font-size:11px;color:var(--muted);margin-bottom:4px">Path</div>
        <input class="field" placeholder="path/to/folder-name"
               value={path()}
               onInput={(e) => setPath(e.currentTarget.value.trim())}
               onKeyDown={(e) => e.key === "Enter" && submit()}
               ref={(el) => setTimeout(() => { el.focus(); el.setSelectionRange(el.value.length, el.value.length); }, 0)} />
        <div class="btn-row mt-3">
          <button class="btn-secondary" style="flex:1" onClick={props.onClose}>Cancel</button>
          <button class="btn-primary" style="flex:1" disabled={!path().trim().replace(/\//g, "")} onClick={submit}>
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
      toast.ok("Renamed");
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
          <button class="btn-secondary" style="flex:1" onClick={props.onClose}>Cancel</button>
          <button class="btn-primary" style="flex:1" disabled={busy()} onClick={submit}>
            {busy() ? "Working…" : "Rename"}
          </button>
        </div>
      </div>
    </div>
  );
}
