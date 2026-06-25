import { createSignal, For, Show, onCleanup } from "solid-js";
import {
  listTransfers, cancelTransfer, clearCompletedTransfers, clearTransfer, retryTransfer,
} from "../api/transfers";
import { formatBytes } from "../utils/fmt";
import { toast, errMsg } from "../state/toast";
import { confirmDialog } from "../state/confirm";
import { IconArrowUpLine, IconX, IconTransfer } from "../utils/icons";
import type { Transfer } from "../types";
import { STATUS_ORDER, recordAndComputeSpeed } from "./transfers/helpers";
import { TransferRow } from "./transfers/TransferRow";

// ── page ─────────────────────────────────────────────────────────────────────

type Filter = "all" | "active" | "done" | "failed";

export default function Transfers() {
  const [transfers, setTransfers] = createSignal<Transfer[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [err, setErr] = createSignal("");
  const [filter, setFilter] = createSignal<Filter>("all");

  async function load() {
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

  async function cancel(id: string) {
    try { await cancelTransfer(id); await load(); toast.ok("Cancelled"); }
    catch (e) { toast.err(e); }
  }
  async function clearOne(id: string) {
    try { await clearTransfer(id); }
    catch (e) { toast.err(e); return; }
    setTransfers((prev) => prev.filter((t) => t.id !== id));
  }
  async function retry(id: string) {
    try { await retryTransfer(id); await load(); toast.ok("Retrying"); }
    catch (e) { toast.err(e); }
  }
  async function clearAll() {
    const ok = await confirmDialog({
      title: "Clear completed transfers?",
      body: "All finished, failed, and canceled entries will be removed.",
      confirmLabel: "Clear",
    });
    if (!ok) return;
    try { await clearCompletedTransfers(); await load(); }
    catch (e) { toast.err(e); }
  }

  const filtered = () => {
    let list = transfers();
    switch (filter()) {
      case "active": list = list.filter((t) => t.status === "active" || t.status === "pending"); break;
      case "done":   list = list.filter((t) => t.status === "done"); break;
      case "failed": list = list.filter((t) => t.status === "failed" || t.status === "canceled"); break;
    }
    return list;
  };

  const counts = () => ({
    all:    transfers().length,
    active: transfers().filter((t) => t.status === "active" || t.status === "pending").length,
    done:   transfers().filter((t) => t.status === "done").length,
    failed: transfers().filter((t) => t.status === "failed" || t.status === "canceled").length,
  });

  const hasDone = () =>
    transfers().some((t) => ["done", "failed", "canceled"].includes(t.status));

  const activeSpeedTotal = () =>
    transfers()
      .filter((t) => t.status === "active")
      .reduce((sum, t) => sum + recordAndComputeSpeed(t), 0);

  return (
    <div class="view-container">
      <div class="transfer-filterbar">
        <h2 style="margin:0;font-size:15px;font-weight:600;letter-spacing:-.01em">Transfers</h2>
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
        </div>
        <div style="flex:1" />
        <Show when={hasDone()}>
          <button class="btn-ghost" style="font-size:12px;padding:6px 11px;border:1px solid var(--border);border-radius:8px" onClick={clearAll}>
            <IconX size={13} /> Clear completed
          </button>
        </Show>
      </div>

      <Show when={err()}><div class="status-msg err" style="margin:12px 20px">{err()}</div></Show>
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
                />
              )}
            </For>
          </div>
        </Show>
      </Show>

      <div class="transfer-footer-bar">
        <Show when={counts().active > 0}>
          <span style="display:flex;align-items:center;gap:4px">
            <IconArrowUpLine size={12} /> {formatBytes(activeSpeedTotal())}/s
          </span>
        </Show>
        <div style="flex:1" />
        <span style="color:var(--faint)">
          {counts().active > 0 && `${counts().active} active`}
          {counts().active > 0 && (counts().done > 0 || counts().failed > 0) && " · "}
          {counts().done > 0 && `${counts().done} done`}
          {counts().done > 0 && counts().failed > 0 && " · "}
          {counts().failed > 0 && `${counts().failed} failed`}
          {counts().all === 0 && "No transfers"}
        </span>
      </div>
    </div>
  );
}
