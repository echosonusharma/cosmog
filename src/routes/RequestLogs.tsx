import { createSignal, createEffect, createMemo, onMount, onCleanup, Index, Show } from "solid-js";
import { createVirtualizer } from "@tanstack/solid-virtual";
import { listen } from "@tauri-apps/api/event";
import { listRequestLogs, clearRequestLogs } from "../api/requestLogs";
import type { RequestLog } from "../types";
import { toast } from "../state/toast";
import { confirmDialog } from "../state/confirm";
import { IconSearch, IconTrash, IconX } from "../utils/icons";
import { Select } from "../utils/Select";
import { isMobile } from "../utils/breakpoint";
import { useBackHandler } from "../utils/androidBack";

import { OP_LABELS, opLabel, opColor } from "../utils/requestLogMeta";

// Fixed row height — must match CSS. Detail lives outside the list (right pane
// / bottom sheet), so the virtualizer never measures variable heights.
const ROW_H_DESKTOP = 40;
const ROW_H_MOBILE = 64;
const ROW_H = typeof window !== "undefined" && window.innerWidth <= 768
  ? ROW_H_MOBILE
  : ROW_H_DESKTOP;

function durationClass(ms: number): string {
  if (ms < 200) return "duration-fast";
  if (ms < 800) return "duration-medium";
  return "duration-slow";
}

function fmtTime(ts: number): string {
  const d = new Date(ts * 1000);
  return d.toLocaleTimeString("en-US", { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function fmtDate(ts: number): string {
  const d = new Date(ts * 1000);
  return d.toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

function fmtDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

function truncateKey(key: string | null, max = 48): string {
  if (!key) return "";
  if (key.length <= max) return key;
  const half = Math.floor((max - 3) / 2);
  return `${key.slice(0, half)}…${key.slice(-half)}`;
}

function prettyJson(raw: string): string {
  try { return JSON.stringify(JSON.parse(raw), null, 2); }
  catch { return raw; }
}

const PAGE = 100;

function RequestLogDetail(props: { log: RequestLog }) {
  const color = () => opColor(props.log.operation);
  const isErr = () => props.log.status === "error";
  return (
    <div class="req-log-detail">
      <div class="req-log-detail-header" style={{ "border-left-color": color() }}>
        <Show when={props.log.http_method}>
          <span class="req-log-http-method">{props.log.http_method}</span>
        </Show>
        <span class="req-log-detail-op" style={{ color: color() }}>
          {opLabel(props.log.operation)}
        </span>
        <span class="req-log-detail-raw-op">{props.log.operation}</span>
        <div class="flex-1" />
        <Show when={props.log.response_status}>
          <span class={`req-log-http-status ${isErr() ? "req-log-http-status-err" : "req-log-http-status-ok"}`}>
            HTTP {props.log.response_status}
          </span>
        </Show>
        <span
          class={`req-log-detail-status-badge ${isErr() ? "req-log-detail-err-badge" : "req-log-detail-ok-badge"}`}
        >
          {isErr() ? "✕ error" : "✓ ok"}
        </span>
      </div>

      <Show when={props.log.request_url}>
        <div class="req-log-url-bar">
          <span class="req-log-chip-label req-log-chip-label-url">URL</span>
          <span class="req-log-url-text">{props.log.request_url}</span>
          <button
            class="req-log-copy-btn"
            onClick={() => navigator.clipboard.writeText(props.log.request_url!)}
          >⎘</button>
        </div>
      </Show>

      <div class="req-log-detail-chips">
        <Show when={props.log.account_name}>
          <span class="req-log-chip req-log-chip-account">
            <span class="req-log-chip-label">account</span>
            {props.log.account_name}
          </span>
        </Show>
        <Show when={props.log.bucket}>
          <span class="req-log-chip req-log-chip-bucket">
            <span class="req-log-chip-label">bucket</span>
            {props.log.bucket}
          </span>
        </Show>
        <span class={`req-log-chip req-log-chip-duration ${durationClass(props.log.duration_ms)}`}>
          <span class="req-log-chip-label">duration</span>
          {props.log.duration_ms}ms
        </span>
        <span class="req-log-chip req-log-chip-time">
          <span class="req-log-chip-label">time</span>
          {new Date(props.log.created_at * 1000).toISOString().replace("T", " ").replace("Z", " UTC")}
        </span>
      </div>

      <Show when={props.log.key}>
        <div class="req-log-detail-key-row">
          <span class="req-log-chip-label req-log-chip-label-nostretch">key</span>
          <code class="req-log-detail-key">{props.log.key}</code>
        </div>
      </Show>

      <Show when={props.log.request_params && props.log.request_params !== "null"}>
        <div class="req-log-params-block">
          <div class="req-log-params-header">
            <span class="req-log-chip-label">request params</span>
          </div>
          <pre class="req-log-params-json">{prettyJson(props.log.request_params!)}</pre>
        </div>
      </Show>

      <Show when={props.log.response_meta && props.log.response_meta !== "null"}>
        <div class="req-log-params-block">
          <div class="req-log-params-header">
            <span class="req-log-chip-label">response</span>
          </div>
          <pre class="req-log-params-json">{prettyJson(props.log.response_meta!)}</pre>
        </div>
      </Show>

      <Show when={isErr()}>
        <div class="req-log-detail-error-box">
          <div class="req-log-detail-error-header">
            <Show when={props.log.error_code}>
              <span class="req-log-detail-error-code">{props.log.error_code}</span>
            </Show>
            <span class="req-log-detail-error-label">ERROR</span>
          </div>
          <Show when={props.log.error_msg}>
            <p class="req-log-detail-error-msg">{props.log.error_msg}</p>
          </Show>
        </div>
      </Show>
    </div>
  );
}

export function RequestLogs(props: { active?: boolean }) {
  const isActive = () => props.active !== false;
  const [logs, setLogs] = createSignal<RequestLog[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [loadingMore, setLoadingMore] = createSignal(false);
  const [hasMore, setHasMore] = createSignal(false);
  const [fetchError, setFetchError] = createSignal<string | null>(null);
  const [search, setSearch] = createSignal("");
  const [statusFilter, setStatusFilter] = createSignal("");
  const [opFilter, setOpFilter] = createSignal("");
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  let searchTimeout: ReturnType<typeof setTimeout> | undefined;
  let eventTimeout: ReturnType<typeof setTimeout> | undefined;

  const selected = createMemo(() => {
    const id = selectedId();
    if (!id) return null;
    return logs().find((r) => r.id === id) ?? null;
  });

  createEffect(() => {
    const id = selectedId();
    if (id && !logs().some((r) => r.id === id)) setSelectedId(null);
  });

  useBackHandler(() => selectedId() !== null, () => {
    setSelectedId(null);
    return true;
  });

  let scrollDiv: HTMLDivElement | undefined;
  const [virtScrollEl, setVirtScrollEl] = createSignal<HTMLDivElement | null>(null);
  const rowVirtualizer = createVirtualizer({
    get count() { return logs().length; },
    getScrollElement: () => virtScrollEl(),
    getItemKey: (i) => logs()[i]?.id ?? i,
    estimateSize: () => ROW_H,
    overscan: 12,
  });

  const refreshViewport = () => {
    if (!scrollDiv) return;
    if (rowVirtualizer.getVirtualItems().length > 0 || logs().length === 0) {
      rowVirtualizer.measure();
      return;
    }
    setVirtScrollEl(null);
    requestAnimationFrame(() => { if (scrollDiv) setVirtScrollEl(scrollDiv); });
  };
  createEffect(() => { logs().length; requestAnimationFrame(refreshViewport); });
  // Side pane open/close changes list width → remasure viewport.
  createEffect(() => { selectedId(); isMobile(); requestAnimationFrame(refreshViewport); });
  onMount(() => {
    if (!scrollDiv) return;
    const ro = new ResizeObserver(() => requestAnimationFrame(refreshViewport));
    ro.observe(scrollDiv);
    onCleanup(() => ro.disconnect());
  });

  let loadGen = 0;
  let lastPageOffset = -1;
  async function load() {
    const gen = ++loadGen;
    lastPageOffset = -1;
    try {
      const rows = await listRequestLogs({
        limit: PAGE,
        offset: 0,
        search: search() || undefined,
        status: statusFilter() || undefined,
        operation: opFilter() || undefined,
      });
      if (gen !== loadGen) return;
      setLogs(rows);
      setHasMore(rows.length === PAGE);
      setFetchError(null);
    } catch (e) {
      if (gen !== loadGen) return;
      const msg = e instanceof Error ? e.message : String(e);
      console.error("list_request_logs failed:", e);
      setFetchError(msg);
      setLogs([]);
      setHasMore(false);
    } finally {
      if (gen === loadGen) setLoading(false);
    }
  }

  async function loadMore() {
    if (loadingMore() || !hasMore()) return;
    const offset = logs().length;
    if (offset === lastPageOffset) return;
    lastPageOffset = offset;
    const gen = loadGen;
    setLoadingMore(true);
    try {
      const rows = await listRequestLogs({
        limit: PAGE,
        offset,
        search: search() || undefined,
        status: statusFilter() || undefined,
        operation: opFilter() || undefined,
      });
      if (gen !== loadGen) return;
      setLogs((prev) => {
        const seen = new Set(prev.map((r) => r.id));
        const fresh = rows.filter((r) => !seen.has(r.id));
        return fresh.length ? [...prev, ...fresh] : prev;
      });
      setHasMore(rows.length === PAGE);
    } catch (e) {
      console.error("list_request_logs (page) failed:", e);
    } finally {
      setLoadingMore(false);
    }
  }

  createEffect(() => {
    const items = rowVirtualizer.getVirtualItems();
    const last = items[items.length - 1];
    if (last && last.index >= logs().length - 10) loadMore();
  });

  createEffect(() => {
    if (!isActive()) return;
    load();
  });

  let disposed = false;
  let unlistenFn: (() => void) | null = null;
  onCleanup(() => {
    disposed = true;
    clearTimeout(searchTimeout);
    clearTimeout(eventTimeout);
    unlistenFn?.();
  });
  onMount(() => {
    listen<void>("request-log-added", () => {
      if (!isActive()) return;
      clearTimeout(eventTimeout);
      eventTimeout = setTimeout(() => {
        if (!scrollDiv || scrollDiv.scrollTop < 120) load();
      }, 250);
    }).then((unlisten) => {
      if (disposed) unlisten();
      else unlistenFn = unlisten;
    });
  });

  function onSearch(q: string) {
    setSearch(q);
    clearTimeout(searchTimeout);
    searchTimeout = setTimeout(load, 300);
  }

  async function doClear() {
    const ok = await confirmDialog({
      title: "Clear all request logs?",
      body: "All recorded S3 API request history will be deleted permanently.",
      confirmLabel: "Clear",
      danger: true,
    });
    if (!ok) return;
    try {
      await clearRequestLogs();
      setLogs([]);
      setHasMore(false);
      lastPageOffset = -1;
      setSelectedId(null);
      toast.ok("Request logs cleared", "All recorded S3 request history was deleted");
    } catch (e) {
      toast.err(e);
    }
  }

  const today = () => new Date().toLocaleDateString("en-US", { month: "short", day: "numeric" });

  function selectRow(id: string) {
    setSelectedId((cur) => (cur === id ? null : id));
  }

  const detailToolbar = () => (
    <div class="req-log-detail-toolbar">
      <span class="req-log-detail-toolbar-title">Request detail</span>
      <button class="icon-btn" aria-label="Close detail" onClick={() => setSelectedId(null)}>
        <IconX size={16} />
      </button>
    </div>
  );

  return (
    <div class="view-container min-h-0">
      <div class="logs-header">
        <div class="logs-search-wrap">
          <IconSearch size={13} class="logs-search-icon" />
          <input
            class="field logs-search-input"
            placeholder="Search operation, bucket, key…"
            value={search()}
            onInput={(e) => onSearch(e.currentTarget.value)}
          />
        </div>

        <Select
          value={opFilter()}
          placeholder="All operations"
          options={Object.keys(OP_LABELS).map((op) => ({ value: op, label: OP_LABELS[op] }))}
          class="logs-select logs-select-op"
          onChange={(v) => { setOpFilter(v); load(); }}
        />

        <Select
          value={statusFilter()}
          placeholder="All statuses"
          options={[
            { value: "ok", label: "Success" },
            { value: "error", label: "Error" },
          ]}
          class="logs-select logs-select-status"
          onChange={(v) => { setStatusFilter(v); load(); }}
        />
        <Show when={logs().length > 0}>
          <button class="btn-ghost logs-clear-btn" onClick={doClear}>
            <IconTrash size={13} /> Clear all
          </button>
        </Show>
      </div>

      <Show when={loading()}>
        <div class="loading-row logs-loading"><span class="spinner" /> Loading…</div>
      </Show>
      <Show when={!loading()}>
        <Show when={fetchError()}>
          <div class="empty-state">
            <span class="logs-fetch-err">Error: {fetchError()}</span>
          </div>
        </Show>
        <Show
          when={!fetchError() && logs().length > 0}
          fallback={
            <Show when={!fetchError()}>
              <div class="empty-state">
                <span class="logs-empty-text">
                  {search() || opFilter() || statusFilter() ? "No results" : "No API requests logged yet"}
                </span>
              </div>
            </Show>
          }
        >
          <div class="req-log-layout">
            <div
              ref={(el) => { scrollDiv = el; setVirtScrollEl(el); }}
              class="logs-body min-h-0"
              id="req-log-scroll"
            >
              <div style={{ height: `${rowVirtualizer.getTotalSize()}px`, position: "relative" }}>
                <Index each={rowVirtualizer.getVirtualItems()}>
                  {(vrow) => {
                    const log = () => logs()[vrow().index];
                    return (
                      <Show when={log()}>
                        {(() => {
                          const dateLabel = () => fmtDate(log()!.created_at);
                          const color = () => opColor(log()!.operation);
                          const isErr = () => log()!.status === "error";
                          const isSelected = () => selectedId() === log()!.id;
                          return (
                            <div
                              class={`req-log-row${isErr() ? " req-log-error" : ""}${isSelected() ? " req-log-open" : ""}`}
                              style={{
                                position: "absolute", top: 0, left: 0, width: "100%",
                                height: `${ROW_H}px`,
                                transform: `translateY(${vrow().start}px)`,
                                "--row-color": isErr() ? "#ef4444" : color(),
                              }}
                              onClick={() => selectRow(log()!.id)}
                            >
                              <div class="req-log-main">
                                <span class="req-log-dot" style={{ background: isErr() ? "#ef4444" : "#22c55e" }} />

                                <span class="req-log-ts">
                                  <Show when={dateLabel() !== today()}>
                                    <span class="req-log-ts-date">{dateLabel()}</span>
                                  </Show>
                                  {fmtTime(log()!.created_at)}
                                </span>

                                <span class="req-log-op" style={{ "--op-color": color() }}>
                                  {opLabel(log()!.operation)}
                                </span>

                                <Show when={log()!.account_name}>
                                  <span class="req-log-account">{log()!.account_name}</span>
                                </Show>

                                <Show when={log()!.bucket || log()!.key}>
                                  <span class="req-log-target">
                                    <Show when={log()!.bucket}>
                                      <span class="req-log-bucket">{log()!.bucket}</span>
                                    </Show>
                                    <Show when={log()!.key}>
                                      <span class="req-log-sep">/</span>
                                      <span class="req-log-key">{truncateKey(log()!.key)}</span>
                                    </Show>
                                  </span>
                                </Show>

                                <div class="flex-1" />

                                <span class={`req-log-duration ${durationClass(log()!.duration_ms)}`}>
                                  {fmtDuration(log()!.duration_ms)}
                                </span>
                              </div>
                            </div>
                          );
                        })()}
                      </Show>
                    );
                  }}
                </Index>
              </div>
              <Show when={loadingMore()}>
                <div class="req-log-more"><span class="spinner" /> Loading more…</div>
              </Show>
            </div>

            {/* Desktop: right inspector pane */}
            <Show when={!isMobile() && selectedId()}>
              <aside class="req-log-detail-pane">
                {detailToolbar()}
                <Show when={selected()}>
                  {(log) => <RequestLogDetail log={log()} />}
                </Show>
              </aside>
            </Show>
          </div>
        </Show>
      </Show>

      {/* Mobile: bottom sheet */}
      <Show when={isMobile() && selectedId()}>
        <div class="req-log-sheet-backdrop" onClick={() => setSelectedId(null)}>
          <div class="req-log-sheet" onClick={(e) => e.stopPropagation()}>
            {detailToolbar()}
            <Show when={selected()}>
              {(log) => <RequestLogDetail log={log()} />}
            </Show>
          </div>
        </div>
      </Show>
    </div>
  );
}
