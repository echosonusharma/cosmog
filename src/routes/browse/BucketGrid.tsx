import { createSignal, createResource, createMemo, For, Show } from "solid-js";
import { listBuckets, deleteBucket } from "../../api/buckets";
import { deleteObjects, listKeysUnderPrefix } from "../../api/objects";
import { listEncryptedBuckets } from "../../api/encryption";
import { notify } from "../../utils/notify";
import { setBrowseState, bumpBucketsRefresh } from "../../state/app";
import { toast } from "../../state/toast";
import { parseWireError } from "../../utils/errors";
import { confirmDialog } from "../../state/confirm";
import {
  IconHome, IconRefresh, IconTrash, IconPlus, IconBucket, IconSearch, IconX, IconLock,
} from "../../utils/icons";
import type { Bucket } from "../../types";
import { ErrorPopup } from "../../utils/ErrorPopup";
import { NewBucketModal } from "./modals";

// ── bucket grid ───────────────────────────────────────────────────────────────

export function BucketGrid(props: { accountId: string; accountName: string }) {
  const [refresh, setRefresh] = createSignal(0);
  const [errDismissed, setErrDismissed] = createSignal(false);
  const [buckets] = createResource<Bucket[], { a: string; r: number }>(
    () => ({ a: props.accountId, r: refresh() }),
    ({ a }) => { setErrDismissed(false); return listBuckets(a); },
  );
  // Encrypted bucket names for this account, in a Set for O(1) badge lookup.
  // Re-fetches whenever the grid is refreshed so a fresh enable/disable is
  // reflected without a page reload.
  const [encSet] = createResource<Set<string>, { a: string; r: number }>(
    () => ({ a: props.accountId, r: refresh() }),
    async ({ a }) => new Set(await listEncryptedBuckets(a).catch(() => [] as string[])),
  );
  const [showNew, setShowNew] = createSignal(false);
  const [filter, setFilter] = createSignal("");
  const filtered = createMemo(() => {
    const q = filter().trim().toLowerCase();
    const all = buckets.latest ?? [];
    if (!q) return all;
    return all.filter((b) => b.name.toLowerCase().includes(q));
  });

  async function handleDelete(name: string) {
    const ok = await confirmDialog({
      title: "Delete bucket?",
      body: `"${name}": all objects and the bucket itself will be removed. This action is irreversible.`,
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    try {
      await deleteBucket(props.accountId, name);
      setRefresh((n) => n + 1);
      bumpBucketsRefresh();
      notify("Bucket deleted", name);
    } catch (e) {
      // S3 refuses DeleteBucket on non-empty buckets — guide the user.
      if (parseWireError(e).code === "conflict") {
        const emptyAndDelete = await confirmDialog({
          title: "Bucket is not empty",
          body: `"${name}" still has objects. S3 refuses to delete a non-empty bucket.\n\nEmpty it (delete all objects) and then delete the bucket? This action is irreversible.`,
          confirmLabel: "Empty + delete",
          danger: true,
        });
        if (!emptyAndDelete) return;
        try {
          await emptyAndDeleteBucket(props.accountId, name);
          setRefresh((n) => n + 1);
          bumpBucketsRefresh();
          notify("Bucket deleted", `${name} emptied and deleted`);
        } catch (e2) {
          toast.err(e2);
        }
      } else {
        toast.err(e);
      }
    }
  }

  // Walk every page under bucket root via live S3 listing, batch-delete, then delete bucket.
  async function emptyAndDeleteBucket(accountId: string, name: string) {
    const keys = await listKeysUnderPrefix(accountId, name, "");
    for (let i = 0; i < keys.length; i += 1000) {
      await deleteObjects(accountId, name, keys.slice(i, i + 1000));
    }
    await deleteBucket(accountId, name);
  }

  return (
    <div class="bucket-grid-view">
      <div class="app-toolbar">
        <div class="toolbar-left">
          <div class="toolbar-nav">
            <button class="icon-btn" onClick={() => setRefresh((n) => n + 1)}><IconRefresh size={16} /><span class="btn-label-mobile">Refresh</span></button>
          </div>
          <div class="path-bar">
            <span class="path-icon"><IconHome size={14} /></span>
            <span class="breadcrumb-current">{props.accountName}</span>
          </div>
        </div>
        <div class="toolbar-search bucket-grid-search">
          <IconSearch size={13} class="toolbar-search-icon" />
          <input
            class="toolbar-search-input"
            placeholder="Filter buckets…"
            value={filter()}
            onInput={(e) => setFilter(e.currentTarget.value)}
          />
          <Show when={filter()}>
            <button class="toolbar-search-clear" onClick={() => setFilter("")}><IconX size={11} /></button>
          </Show>
        </div>
        <div class="toolbar-actions">
          <button class="btn-secondary toolbar-btn" onClick={() => setShowNew(true)}>
            <IconPlus size={14} /> <span class="btn-label-desktop">New bucket</span><span class="btn-label-mobile">Add Bucket</span>
          </button>
        </div>
      </div>

      <div class="bucket-grid-body">
        <Show when={buckets.loading}>
          <div class="loading-row"><span class="spinner" /> Loading buckets…</div>
        </Show>
        <Show when={buckets.error && !errDismissed()}>
          <ErrorPopup error={buckets.error} onClose={() => setErrDismissed(true)} />
        </Show>
        <Show when={!buckets.loading && !buckets.error}>
          <Show when={(buckets() ?? []).length > 0}
                fallback={
                  <div class="empty-state">
                    <span class="empty-icon"><IconBucket size={40} /></span>
                    No buckets yet
                  </div>
                }>
            <Show when={filtered().length === 0 && filter()}>
              <div class="empty-state">
                <span class="empty-icon"><IconBucket size={40} /></span>
                No buckets match "{filter()}"
              </div>
            </Show>
            <div class="bucket-grid">
              <For each={filtered()}>
                {(b) => (
                  <div
                    class="bucket-card"
                    classList={{ encrypted: encSet()?.has(b.name) }}
                  >
                    <button class="bucket-name"
                            onClick={() => setBrowseState({ bucket: b.name, prefix: "" })}>
                      <Show when={encSet()?.has(b.name)} fallback={<span class="bucket-icon"><IconBucket size={18} /></span>}>
                        <span class="bucket-icon encrypted">
                          <IconLock size={16} />
                        </span>
                      </Show>
                      <span class="truncate">{b.name}</span>
                    </button>
                    <button class="icon-btn danger bucket-del"
                            onClick={() => handleDelete(b.name)}><IconTrash size={15} /></button>
                  </div>
                )}
              </For>
            </div>
          </Show>
        </Show>
      </div>

      <Show when={showNew()}>
        <NewBucketModal accountId={props.accountId} onClose={() => setShowNew(false)}
                        onDone={() => { setRefresh((n) => n + 1); bumpBucketsRefresh(); }} />
      </Show>
    </div>
  );
}
