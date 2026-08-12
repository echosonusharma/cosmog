import { createSignal, createResource, For, Show } from "solid-js";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  listObjectVersions,
  deleteObjectVersion,
  restoreObjectVersion,
} from "../../../api/objects";
import { enqueueDownload } from "../../../api/transfers";
import { toast, errMsg } from "../../../state/toast";
import { confirmDialog } from "../../../state/confirm";
import { basename } from "../../../utils/fmt";
import { IconX } from "../../../utils/icons";
import type { ObjectVersion } from "../../../types";
import { pathFromDialog, resolveDownloadPath, registerSafFinalize, withTimestamp } from "../helpers";
import { VersionRow } from "./VersionRow";

const MAX_PAGES = 20;

// ListObjectVersions with a prefix returns every key that shares the prefix.
// Follow continuation, accumulating only versions whose key matches exactly,
// capped at MAX_PAGES so a huge bucket can't spin forever.
async function fetchAllVersions(
  accountId: string,
  bucket: string,
  key: string,
): Promise<{ versions: ObjectVersion[]; truncated: boolean }> {
  const out: ObjectVersion[] = [];
  let continuation: string | undefined = undefined;
  let pages = 0;
  let truncated = false;
  do {
    const res = await listObjectVersions(accountId, bucket, key, continuation);
    for (const v of res.versions) {
      if (v.key === key) out.push(v);
    }
    continuation = res.continuation ?? undefined;
    pages += 1;
    if (continuation && pages >= MAX_PAGES) {
      truncated = true;
      break;
    }
  } while (continuation);

  // Newest first: latest on top, then by last_modified desc.
  out.sort((a, b) => {
    if (a.is_latest !== b.is_latest) return a.is_latest ? -1 : 1;
    return (b.last_modified ?? 0) - (a.last_modified ?? 0);
  });
  return { versions: out, truncated };
}

export function VersionHistoryModal(props: {
  accountId: string;
  bucket: string;
  objectKey: string;
  onClose: () => void;
  onChanged?: () => void;
}) {
  const [busy, setBusy] = createSignal(false);

  const [data, { refetch }] = createResource(
    () => ({ a: props.accountId, b: props.bucket, k: props.objectKey }),
    ({ a, b, k }) => fetchAllVersions(a, b, k),
  );

  const name = () => basename(props.objectKey);

  async function handleDownload(v: ObjectVersion) {
    try {
      const sel = await saveDialog({ defaultPath: withTimestamp(name()) });
      if (!sel) return;
      const raw = pathFromDialog(sel);
      const { path, safUri } = await resolveDownloadPath(raw, name());
      const res = await enqueueDownload(
        props.accountId,
        props.bucket,
        props.objectKey,
        path,
        v.version_id ?? undefined,
      );
      if (safUri && res?.transfer_id) registerSafFinalize(res.transfer_id, path, safUri);
      toast.ok("Download queued", `Version ${(v.version_id ?? "current").slice(0, 8)} of "${name()}"`);
    } catch (e) {
      toast.err(e);
    }
  }

  async function handleRestore(v: ObjectVersion) {
    if (!v.version_id) return;
    const ok = await confirmDialog({
      title: "Restore this version?",
      body: v.is_delete_marker
        ? `Removing the delete marker will bring "${name()}" back as its previous version.`
        : `"${name()}" will be restored to this version. It becomes the current version; existing versions are kept.`,
      confirmLabel: "Restore",
    });
    if (!ok) return;
    setBusy(true);
    try {
      if (v.is_delete_marker) {
        // Un-delete: remove the delete marker so the prior version becomes the
        // current one. Copying from a delete marker is rejected by S3.
        await deleteObjectVersion(props.accountId, props.bucket, props.objectKey, v.version_id);
        toast.ok("Object restored", `Delete marker removed; "${name()}" is back`);
      } else {
        await restoreObjectVersion(props.accountId, props.bucket, props.objectKey, v.version_id);
        toast.ok("Version restored", `"${name()}" now points to version ${v.version_id.slice(0, 8)}`);
      }
      await refetch();
      props.onChanged?.();
    } catch (e) {
      toast.err(e);
    } finally {
      setBusy(false);
    }
  }

  async function handleDelete(v: ObjectVersion) {
    if (!v.version_id) return;
    const ok = await confirmDialog({
      title: "Delete this version permanently?",
      body: `Version ${v.version_id.slice(0, 8)} of "${name()}" will be erased for good. Unlike a normal delete (which keeps prior versions), this cannot be undone.`,
      confirmLabel: "Delete permanently",
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    try {
      await deleteObjectVersion(props.accountId, props.bucket, props.objectKey, v.version_id);
      toast.ok("Version deleted", `Version ${v.version_id.slice(0, 8)} of "${name()}" was permanently removed`);
      await refetch();
      props.onChanged?.();
    } catch (e) {
      toast.err(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="modal vh-modal" onClick={(e) => e.stopPropagation()}>
        <div class="vh-header">
          <div>
            <div class="modal-title">Version history</div>
            <div class="vh-key">{props.objectKey}</div>
          </div>
          <button class="icon-btn vh-close-x" title="Close" onClick={props.onClose}>
            <IconX size={16} />
          </button>
        </div>

        <div class="vh-body">
          <Show when={data.loading && data.latest == null}>
            <div class="vh-loading">
              <span class="spinner" /> Loading versions…
            </div>
          </Show>

          <Show when={data.error && data.latest == null}>
            <div class="status-msg err">{errMsg(data.error)}</div>
          </Show>

          <Show when={data.error && data.latest != null}>
            <div class="status-msg err">Couldn't refresh versions: {errMsg(data.error)}</div>
          </Show>

          <Show when={data.latest ?? data()}>
            {(d) => (
              <Show
                when={d().versions.length > 0}
                fallback={<div class="vh-empty">No versions found for this object.</div>}
              >
                <div class="vh-list" classList={{ loading: data.loading }}>
                  <For each={d().versions}>
                    {(v) => (
                      <VersionRow
                        version={v}
                        busy={busy()}
                        onDownload={handleDownload}
                        onRestore={handleRestore}
                        onDelete={handleDelete}
                      />
                    )}
                  </For>
                </div>
                <Show when={d().truncated}>
                  <div class="vh-truncated">
                    Showing the first {MAX_PAGES} pages of versions. Older versions may exist and are not listed.
                  </div>
                </Show>
              </Show>
            )}
          </Show>
        </div>
      </div>
    </div>
  );
}
