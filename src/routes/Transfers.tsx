import { createSignal, For, Show, onCleanup } from "solid-js";
import {
  listTransfers, cancelTransfer, clearCompletedTransfers, clearTransfer, retryTransfer,
} from "../api/transfers";
import { toast, errMsg } from "../state/toast";
import { confirmDialog } from "../state/confirm";
import { IconX, IconTransfer } from "../utils/icons";
import type { Transfer } from "../types";
import { STATUS_ORDER } from "./transfers/helpers";
import { moveSafFinalize, discardSafDownload } from "./browse/helpers";
import { TransferRow } from "./transfers/TransferRow";
import { EncryptionModal } from "./browse/EncryptionModal";
import { getBucketEncryptionStatus, hasEncryptionIdentity } from "../api/encryption";

// ── page ─────────────────────────────────────────────────────────────────────

type Filter = "all" | "active" | "done" | "failed" | "canceled";

export default function Transfers() {
  const [transfers, setTransfers] = createSignal<Transfer[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [err, setErr] = createSignal("");
  const [filter, setFilter] = createSignal<Filter>("all");
  // Load-key flow: opened from a failed transfer row whose error indicates
  // the encryption key is not present locally. We fetch enable status +
  // identity presence for the target bucket before rendering the modal so
  // it lands directly in the import branch.
  const [keyModal, setKeyModal] = createSignal<
    { accountId: string; bucket: string; enabled: boolean; identityPresent: boolean } | null
  >(null);
  async function openKeyModal(accountId: string, bucket: string) {
    try {
      const [status, present] = await Promise.all([
        getBucketEncryptionStatus(accountId, bucket),
        hasEncryptionIdentity(accountId, bucket),
      ]);
      setKeyModal({ accountId, bucket, enabled: status.enabled, identityPresent: present });
    } catch (e) { toast.err(e); }
  }

  async function load() {
    setErr("");
    try {
      const list = await listTransfers();
      list.sort(
        (a, b) =>
          STATUS_ORDER.indexOf(a.status) - STATUS_ORDER.indexOf(b.status) ||
          b.updated_at - a.updated_at,
      );
      setTransfers(list);
    } catch (e) {
      setErr(errMsg(e));
    } finally {
      setLoading(false);
    }
  }

  load();

  let timer: ReturnType<typeof setInterval>;
  function setupTimer(activeMs: number) {
    if (timer) clearInterval(timer);
    timer = setInterval(load, activeMs);
  }
  setupTimer(700);
  onCleanup(() => clearInterval(timer));

  const hasActive = () =>
    transfers().some((t) => t.status === "active" || t.status === "pending");

  let prevActive = false;
  const rateTimer = setInterval(() => {
    const ha = hasActive();
    if (ha !== prevActive) {
      prevActive = ha;
      setupTimer(ha ? 700 : 4000);
    }
  }, 1000);
  onCleanup(() => clearInterval(rateTimer));

  // No toast on success: the MainApp transfer poll posts a proper native
  // notification (filename + account + bucket) when the status flips.
  async function cancel(id: string) {
    try { await cancelTransfer(id); await load(); }
    catch (e) { toast.err(e); }
  }
  async function clearOne(id: string) {
    // Clearing a failed download abandons its retry path; drop the pending
    // SAF entry and the 0-byte placeholder at the picked location with it.
    discardSafDownload(id);
    try { await clearTransfer(id); }
    catch (e) { toast.err(e); return; }
    setTransfers((prev) => prev.filter((t) => t.id !== id));
  }
  async function retry(id: string) {
    try {
      const res = await retryTransfer(id);
      // Retry re-enqueues under a fresh id; carry the pending SAF finalize
      // over so the retried download still lands at the picked location.
      if (res?.transfer_id) moveSafFinalize(id, res.transfer_id);
      await load();
    }
    catch (e) { toast.err(e); }
  }
  async function clearAll() {
    const ok = await confirmDialog({
      title: "Clear completed transfers?",
      body: "All finished, failed, and canceled entries will be removed.",
      confirmLabel: "Clear",
    });
    if (!ok) return;
    for (const t of transfers()) {
      if (t.status === "failed" && t.direction === "download") discardSafDownload(t.id);
    }
    try { await clearCompletedTransfers(); await load(); }
    catch (e) { toast.err(e); }
  }

  const filtered = () => {
    let list = transfers();
    switch (filter()) {
      case "active": list = list.filter((t) => t.status === "active" || t.status === "pending"); break;
      case "done":     list = list.filter((t) => t.status === "done"); break;
      case "failed":   list = list.filter((t) => t.status === "failed"); break;
      case "canceled": list = list.filter((t) => t.status === "canceled"); break;
    }
    return list;
  };

  const counts = () => ({
    all:      transfers().length,
    active:   transfers().filter((t) => t.status === "active" || t.status === "pending").length,
    done:     transfers().filter((t) => t.status === "done").length,
    failed:   transfers().filter((t) => t.status === "failed").length,
    canceled: transfers().filter((t) => t.status === "canceled").length,
  });

  const hasDone = () =>
    transfers().some((t) => ["done", "failed", "canceled"].includes(t.status));

  return (
    <div class="view-container">
      <div class="transfer-filterbar">
        <h2 class="transfer-filterbar-title">Transfers</h2>
        <div class="filter-chips">
          <button class={`chip ${filter() === "all"    ? "active" : ""}`} onClick={() => setFilter("all")}>
            All <span class="chip-count">{counts().all}</span>
          </button>
          <button class={`chip ${filter() === "active" ? "active" : ""}`} onClick={() => setFilter("active")}>
            Active <span class="chip-count">{counts().active}</span>
          </button>
          <button class={`chip ${filter() === "done"   ? "active" : ""}`} onClick={() => setFilter("done")}>
            Done <span class="chip-count">{counts().done}</span>
          </button>
          <button class={`chip ${filter() === "failed" ? "active" : ""}`} onClick={() => setFilter("failed")}>
            Failed <span class="chip-count">{counts().failed}</span>
          </button>
          <button class={`chip ${filter() === "canceled" ? "active" : ""}`} onClick={() => setFilter("canceled")}>
            Canceled <span class="chip-count">{counts().canceled}</span>
          </button>
        </div>
        <div class="flex-1" />
        <Show when={hasDone()}>
          <button class="btn-ghost transfer-clear-btn" onClick={clearAll}>
            <IconX size={13} /> Clear completed
          </button>
        </Show>
      </div>

      <Show when={err()}><div class="status-msg err transfer-err-msg">{err()}</div></Show>
      <Show when={loading()}>
        <div class="loading-row"><span class="spinner" /> Loading…</div>
      </Show>

      <Show when={!loading()}>
        <Show when={filtered().length > 0}
              fallback={
                <div class="empty-state">
                  <span class="empty-icon"><IconTransfer size={36} /></span>
                  {filter() === "all" ? "No transfers yet" : `No ${filter()} transfers`}
                </div>
              }>
          <div class="transfer-list">
            <For each={filtered()}>
              {(t) => (
                <TransferRow
                  t={t}
                  onCancel={() => cancel(t.id)}
                  onClear={() => clearOne(t.id)}
                  onRetry={() => retry(t.id)}
                  onLoadKey={openKeyModal}
                />
              )}
            </For>
          </div>
        </Show>
      </Show>

      <div class="transfer-footer-bar">
        <div class="flex-1" />
        <span class="faint">
          {counts().active > 0 && `${counts().active} active`}
          {counts().active > 0 && (counts().done > 0 || counts().failed > 0) && " · "}
          {counts().done > 0 && `${counts().done} done`}
          {counts().done > 0 && counts().failed > 0 && " · "}
          {counts().failed > 0 && `${counts().failed} failed`}
          {(counts().done > 0 || counts().failed > 0) && counts().canceled > 0 && " · "}
          {counts().canceled > 0 && `${counts().canceled} canceled`}
          {counts().all === 0 && "No transfers"}
        </span>
      </div>

      <Show when={keyModal()}>
        {(m) => (
          <EncryptionModal
            accountId={m().accountId}
            bucket={m().bucket}
            enabled={m().enabled}
            identityPresent={m().identityPresent}
            onClose={() => setKeyModal(null)}
            onChanged={() => { setKeyModal(null); load(); }}
          />
        )}
      </Show>
    </div>
  );
}
