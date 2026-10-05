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
import { confirmDestructive } from "../../../state/settings";
import { basename } from "../../../utils/fmt";
import { IconX } from "../../../utils/icons";
import type { ObjectVersion } from "../../../types";
import { pathFromDialog, resolveDownloadPath, registerSafFinalize, withTimestamp } from "../helpers";
import { VersionRow } from "./VersionRow";
import Spinner from "../../../utils/Spinner";
import { t } from "../../../i18n";

const MAX_PAGES = 20;

// ListObjectVersions with a prefix returns every key sharing the prefix:
// accumulate only exact key matches, capped at MAX_PAGES for huge buckets.
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
      toast.ok(t("bucketconfig.versions.downloadQueued"), t("bucketconfig.versions.versionOf", { id: (v.version_id ?? t("bucketconfig.versions.current")).slice(0, 8), name: name() }));
    } catch (e) {
      toast.err(e);
    }
  }

  async function handleRestore(v: ObjectVersion) {
    if (!v.version_id) return;
    const ok = await confirmDialog({
      title: t("bucketconfig.versions.restoreConfirmTitle"),
      body: t(v.is_delete_marker ? "bucketconfig.versions.restoreMarkerBody" : "bucketconfig.versions.restoreBody", { name: name() }),
      confirmLabel: t("bucketconfig.versions.restore"),
    });
    if (!ok) return;
    setBusy(true);
    try {
      if (v.is_delete_marker) {
        // Un-delete by removing the marker (S3 rejects copying from one); the
        // prior version becomes current again.
        await deleteObjectVersion(props.accountId, props.bucket, props.objectKey, v.version_id);
        toast.ok(t("bucketconfig.versions.objectRestored"), t("bucketconfig.versions.markerRemoved", { name: name() }));
      } else {
        await restoreObjectVersion(props.accountId, props.bucket, props.objectKey, v.version_id);
        toast.ok(t("bucketconfig.versions.versionRestored"), t("bucketconfig.versions.versionRestoredDetail", { name: name(), id: v.version_id.slice(0, 8) }));
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
    const ok = await confirmDestructive({
      title: t("bucketconfig.versions.deleteConfirmTitle"),
      body: t("bucketconfig.versions.deleteConfirmBody", { id: v.version_id.slice(0, 8), name: name() }),
      confirmLabel: t("bucketconfig.versions.deletePermanently"),
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    try {
      await deleteObjectVersion(props.accountId, props.bucket, props.objectKey, v.version_id);
      toast.ok(t("bucketconfig.versions.versionDeleted"), t("bucketconfig.versions.versionDeletedDetail", { id: v.version_id.slice(0, 8), name: name() }));
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
            <div class="modal-title">{t("bucketconfig.versions.title")}</div>
            <div class="vh-key">{props.objectKey}</div>
          </div>
          <button class="icon-btn vh-close-x" title={t("common.close")} onClick={props.onClose}>
            <IconX size={16} />
          </button>
        </div>

        <div class="vh-body">
          <Show when={data.loading && data.latest == null}>
            <div class="vh-loading">
              <Spinner /> {t("bucketconfig.versions.loading")}
            </div>
          </Show>

          <Show when={data.error && data.latest == null}>
            <div class="status-msg err">{errMsg(data.error)}</div>
          </Show>

          <Show when={data.error && data.latest != null}>
            <div class="status-msg err">{t("bucketconfig.versions.refreshFailed", { error: errMsg(data.error) })}</div>
          </Show>

          <Show when={data.latest ?? data()}>
            {(d) => (
              <Show
                when={d().versions.length > 0}
                fallback={<div class="vh-empty">{t("bucketconfig.versions.empty")}</div>}
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
                    {t("bucketconfig.versions.truncated", { pages: MAX_PAGES })}
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
