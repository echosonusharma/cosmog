import { createSignal, createResource, createMemo, createEffect, onCleanup, For, Show } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { currentView } from "../state/app";
import { Select } from "../utils/Select";
import { IconTrash, IconFolder, IconEye, IconAlertCircle } from "../utils/icons";
import { toast } from "../state/toast";
import { confirmDialog } from "../state/confirm";
import { formatRelative } from "../utils/fmt";
import { IS_MOBILE_OS } from "../utils/notify";
import { listAccounts } from "../api/accounts";
import { listBuckets } from "../api/buckets";
import {
  nwListWatches, nwAddWatch, nwDeleteWatch, nwSetWatchEnabled, nwGetStatus,
  nwPickTree, nwQuitBackground,
} from "../api/nightWatcher";
import type { NightWatch, WatchStatus } from "../types";

const DEFAULT_FULL_SCAN_SECS = 300;

// delete_policy is fixed to 'keep' for the MVP: remote files are never deleted.
const DELETE_POLICY = "keep";

interface AddForm {
  account_id: string;
  bucket: string;
  local_dir: string;
  tree_uri: string;
  key_prefix: string;
  ignore_file: string;
  full_scan_secs: number;
}

const EMPTY_FORM: AddForm = {
  account_id: "",
  bucket: "",
  local_dir: "",
  tree_uri: "",
  key_prefix: "",
  ignore_file: "",
  full_scan_secs: DEFAULT_FULL_SCAN_SECS,
};

export default function NightWatcher() {
  // Watches + status poll every 2.5s. Kept in reconciled stores (keyed by id)
  // so unchanged rows keep their identity: the <For> below never recreates DOM
  // on a poll, which is what caused the periodic loader/flicker.
  const [watches, setWatches] = createStore<{ list: NightWatch[]; loaded: boolean }>({ list: [], loaded: false });
  const [status, setStatus] = createStore<{ list: WatchStatus[] }>({ list: [] });
  const [accounts] = createResource(listAccounts);
  const [busy, setBusy] = createSignal(false);

  async function refetchWatches() {
    try {
      const w = await nwListWatches();
      setWatches("list", reconcile(w, { key: "id" }));
      setWatches("loaded", true);
    } catch { /* keep previous list on a transient poll error */ }
  }
  async function refetchStatus() {
    try {
      const s = await nwGetStatus();
      setStatus("list", reconcile(s, { key: "id" }));
    } catch { /* keep previous status */ }
  }

  const [form, setForm] = createSignal<AddForm>({ ...EMPTY_FORM });

  function field<K extends keyof AddForm>(key: K): AddForm[K] {
    return form()[key];
  }
  function patch<K extends keyof AddForm>(key: K, val: AddForm[K]) {
    setForm((p) => ({ ...p, [key]: val }));
  }

  // Buckets for the account picked in the add form. Refetches when the
  // selected account changes; errors swallowed so a hiccup never breaks the form.
  const [buckets] = createResource(
    () => field("account_id") || null,
    async (id: string) => (id ? await listBuckets(id).catch(() => []) : []),
  );

  // The view stays mounted (hidden via CSS), so poll status/watches while it
  // is the active view. Scans + uploads happen in the background; without this
  // the initial "0 files / never" never refreshes.
  createEffect(() => {
    if (currentView() !== "night-watcher") return;
    refetchWatches();
    refetchStatus();
    const t = setInterval(() => {
      refetchWatches();
      refetchStatus();
    }, 2500);
    onCleanup(() => clearInterval(t));
  });

  const statusFor = (id: string): WatchStatus | undefined =>
    status.list.find((s) => s.id === id);

  const accountName = (id: string) =>
    accounts()?.find((a) => a.id === id)?.name ?? id;

  const canAdd = createMemo(() => {
    if (!field("account_id") || !field("bucket")) return false;
    return IS_MOBILE_OS ? !!field("tree_uri") : !!field("local_dir");
  });

  async function pickDir() {
    try {
      const sel = await openDialog({ directory: true, multiple: false });
      if (typeof sel === "string") patch("local_dir", sel);
    } catch (e) { toast.err(e); }
  }

  // Android SAF folder pick: store the tree URI, use its display name as label.
  async function pickTree() {
    try {
      const { uri, display_name } = await nwPickTree();
      patch("tree_uri", uri);
      patch("local_dir", display_name);
    } catch (e) {
      // User dismissed the system picker: not an error, stay quiet.
      if (String(e).toLowerCase().includes("canceled")) return;
      toast.err(e);
    }
  }

  async function quitBackground() {
    try {
      await nwQuitBackground();
    } catch (e) { toast.err(e); }
  }

  async function addWatch() {
    if (!canAdd()) return;
    setBusy(true);
    try {
      const f = form();
      await nwAddWatch({
        account_id: f.account_id,
        bucket: f.bucket,
        local_dir: f.local_dir,
        tree_uri: f.tree_uri || null,
        key_prefix: f.key_prefix.trim(),
        ignore_file: f.ignore_file.trim() || null,
        full_scan_secs: f.full_scan_secs,
        delete_policy: DELETE_POLICY,
      });
      setForm({ ...EMPTY_FORM });
      await refetchWatches();
      await refetchStatus();
      toast.ok("Watch added", "Night Watcher is now syncing this directory");
    } catch (e) { toast.err(e); }
    finally { setBusy(false); }
  }

  async function toggleEnabled(w: NightWatch) {
    try {
      await nwSetWatchEnabled(w.id, !w.enabled);
      await refetchWatches();
      await refetchStatus();
    } catch (e) { toast.err(e); }
  }

  async function removeWatch(w: NightWatch) {
    const ok = await confirmDialog({
      title: "Delete this watch?",
      body: `Stops syncing "${w.local_dir}". Remote files are left untouched.`,
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    try {
      await nwDeleteWatch(w.id);
      await refetchWatches();
      await refetchStatus();
      toast.ok("Watch deleted", `Stopped syncing "${w.local_dir}"; remote files were left untouched`);
    } catch (e) { toast.err(e); }
  }

  return (
    <div class="view-container">
      <div class="view-header">
        <span class="section-title">Night Watcher</span>
        <Show when={!IS_MOBILE_OS && watches.list.length > 0}>
          <button class="nw-quit-btn" onClick={quitBackground}>
            Quit background sync
          </button>
        </Show>
      </div>

      <div class="nw-body">
        <div class="nw-intro">
          <div class="nw-intro-badge"><IconEye size={18} /></div>
          <div class="nw-intro-text">
            <span class="nw-intro-lead">The watch that never sleeps</span>
            <span class="nw-intro-body">
              Night Watcher guards a directory by keeping a safe copy in your bucket,
              syncing every change one-way as it happens.
            </span>
          </div>
        </div>

        <div class="settings-section">
          <div class="settings-section-title">Watched directories</div>

          <Show when={!watches.loaded}>
            <div class="loading-row"><span class="spinner" /> Loading watches…</div>
          </Show>

          <Show when={watches.loaded && watches.list.length === 0}>
            <div class="nw-empty">No directories are being watched yet. Add one below.</div>
          </Show>

          <Show when={watches.list.length > 0}>
            <div class="nw-list">
              <For each={watches.list}>
                {(w) => {
                  const st = () => statusFor(w.id);
                  return (
                    <div class="nw-item" classList={{ disabled: !w.enabled, "has-error": !!st()?.last_error }}>
                      <div class="nw-item-main">
                        <div class="nw-item-dir" title={w.local_dir}>{w.local_dir}</div>
                        <div class="nw-item-target">
                          {accountName(w.account_id)} · {w.bucket}
                          <Show when={w.key_prefix}>
                            <span class="nw-item-prefix">/{w.key_prefix}</span>
                          </Show>
                        </div>
                        <div class="nw-item-meta">
                          <span class="nw-meta-chip">{st()?.files_tracked ?? 0} files</span>
                          <span class="nw-meta-chip">
                            Last scan: {st()?.last_scan_at ? formatRelative(st()!.last_scan_at!) : "never"}
                          </span>
                          <span class="nw-meta-chip">Scan every {w.full_scan_secs}s</span>
                        </div>
                        <Show when={st()?.last_error}>
                          <div class="nw-item-error" role="alert">
                            <IconAlertCircle size={13} />
                            <span>{st()!.last_error}</span>
                          </div>
                        </Show>
                      </div>

                      <div class="nw-item-actions">
                        <label class="nw-toggle">
                          <input
                            type="checkbox"
                            checked={w.enabled}
                            onChange={() => toggleEnabled(w)}
                          />
                          <span class="nw-toggle-label">{w.enabled ? "On" : "Off"}</span>
                        </label>
                        <button
                          class="nw-delete-btn"
                          onClick={() => removeWatch(w)}
                          aria-label="Delete watch"
                        >
                          <IconTrash size={14} />
                        </button>
                      </div>
                    </div>
                  );
                }}
              </For>
            </div>
          </Show>

          <Show when={!IS_MOBILE_OS && watches.list.length > 0}>
            <div class="nw-quit-caption">
              Closing the window keeps Cosmog syncing in the background. Use
              "Quit background sync" to fully quit.
            </div>
          </Show>
        </div>

        <div class="settings-section">
          <div class="settings-section-title">Add watch</div>

          <Show when={IS_MOBILE_OS}>
            <div class="nw-note">
              On Android, syncing runs on the periodic scan interval below.
              File changes are picked up on the next scan, not the instant they
              happen.
            </div>
          </Show>

          <div class="settings-grid">
            <label class="settings-label">Account</label>
            <Select
              value={field("account_id")}
              placeholder="Select an account…"
              options={(accounts() ?? []).map((a) => ({ value: a.id, label: a.name }))}
              onChange={(v) => { patch("account_id", v); patch("bucket", ""); }}
            />

            <label class="settings-label">Bucket</label>
            <Select
              value={field("bucket")}
              placeholder={field("account_id") ? "Select a bucket…" : "Pick an account first"}
              disabled={!field("account_id")}
              options={(buckets() ?? []).map((b) => ({ value: b.name, label: b.name }))}
              onChange={(v) => patch("bucket", v)}
            />

            <Show
              when={IS_MOBILE_OS}
              fallback={
                <>
                  <label class="settings-label">Local directory</label>
                  <div class="nw-dir-field">
                    <input
                      class="field"
                      placeholder="/path/to/folder"
                      value={field("local_dir")}
                      onInput={(e) => patch("local_dir", e.currentTarget.value)}
                    />
                    <button type="button" class="nw-dir-btn" onClick={pickDir}>
                      <IconFolder size={14} /> Browse
                    </button>
                  </div>
                </>
              }
            >
              <label class="settings-label">Folder</label>
              <div class="nw-pick-field">
                <button type="button" class="nw-dir-btn" onClick={pickTree}>
                  <IconFolder size={14} /> Pick folder
                </button>
                <Show when={field("tree_uri")}>
                  <span class="nw-chosen-folder" title={field("local_dir")}>
                    {field("local_dir")}
                  </span>
                </Show>
              </div>
            </Show>

            <label class="settings-label">Key prefix</label>
            <input
              class="field"
              placeholder="uploads/ (optional)"
              value={field("key_prefix")}
              onInput={(e) => patch("key_prefix", e.currentTarget.value)}
            />

            <label class="settings-label">Ignore file</label>
            <input
              class="field"
              placeholder="/path/to/.cosmogignore (optional)"
              value={field("ignore_file")}
              onInput={(e) => patch("ignore_file", e.currentTarget.value)}
            />

            <label class="settings-label">Full scan interval (seconds)</label>
            <div class="num-field">
              <input
                type="number"
                min={30}
                value={field("full_scan_secs")}
                onInput={(e) => patch("full_scan_secs", Math.max(30, parseInt(e.currentTarget.value) || DEFAULT_FULL_SCAN_SECS))}
              />
              <button type="button" class="num-field-btn" onClick={() => patch("full_scan_secs", Math.max(30, field("full_scan_secs") - 30))}>−</button>
              <button type="button" class="num-field-btn" onClick={() => patch("full_scan_secs", field("full_scan_secs") + 30)}>+</button>
            </div>

            <label class="settings-label">Delete policy</label>
            <div class="nw-policy-field">
              <input class="field" value="Keep (remote files are never deleted)" disabled />
            </div>
          </div>

          <div class="btn-row mt-4">
            <button class="btn-primary" onClick={addWatch} disabled={busy() || !canAdd()}>
              {busy() ? "Adding…" : "Add watch"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
