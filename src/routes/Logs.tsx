import {
  createSignal, createMemo, onCleanup, For, Show,
} from "solid-js";
import { listen } from "@tauri-apps/api/event";
import { getLogTail } from "../api/logs";
import { listRequestLogs, clearRequestLogs } from "../api/requestLogs";
import type { RequestLog } from "../types";
import { toast } from "../state/toast";
import { confirmDialog } from "../state/confirm";
import { IconSearch, IconTrash } from "../utils/icons";
import { Select } from "../utils/Select";

// ── tab state ─────────────────────────────────────────────────────────────────

type Tab = "requests" | "system";

// ── request logs ──────────────────────────────────────────────────────────────

const OP_LABELS: Record<string, string> = {
  list_buckets: "List Buckets",
  create_bucket: "Create Bucket",
  delete_bucket: "Delete Bucket",
  head_bucket: "Head Bucket",
  put_bucket_acl: "Set Bucket ACL",
  get_bucket_versioning: "Get Versioning",
  put_bucket_versioning: "Set Versioning",
  head_object: "Head Object",
  create_folder: "Create Folder",
  delete_object: "Delete Object",
  delete_objects: "Batch Delete",
  delete_object_version: "Delete Version",
  list_objects: "List Objects",
  list_object_versions: "List Versions",
  copy_object: "Copy Object",
  put_object_acl: "Set Object ACL",
  presign_get: "Presign URL",
  read_object_range: "Preview Object",
  get_object_tagging: "Get Tags",
  put_object_tagging: "Set Tags",
  delete_object_tagging: "Delete Tags",
  put_object: "Upload",
  put_object_bytes: "Save Object",
  get_object: "Download",
  abort_multipart_upload: "Abort Multipart",
};

const OP_COLORS: Record<string, string> = {
  put_object:            "#22c55e",
  put_object_bytes:      "#22c55e",
  get_object:            "#3b82f6",
  delete_object:         "#ef4444",
  delete_objects:        "#ef4444",
  delete_object_version: "#ef4444",
  delete_object_tagging: "#ef4444",
  delete_bucket:         "#ef4444",
  create_bucket:         "#a855f7",
  create_folder:         "#a855f7",
  copy_object:           "#f59e0b",
  presign_get:           "#06b6d4",
  abort_multipart_upload:"#f97316",
  head_bucket:           "#6366f1",
  head_object:           "#6366f1",
  list_buckets:          "#14b8a6",
  list_objects:          "#8b5cf6",
  list_object_versions:  "#6366f1",
  put_bucket_acl:        "#ec4899",
  put_object_acl:        "#ec4899",
  put_bucket_versioning: "#ec4899",
  get_bucket_versioning: "#94a3b8",
  read_object_range:     "#06b6d4",
  get_object_tagging:    "#94a3b8",
  put_object_tagging:    "#f59e0b",
};

function durationClass(ms: number): string {
  if (ms < 200) return "duration-fast";
  if (ms < 800) return "duration-medium";
  return "duration-slow";
}

function opLabel(op: string): string {
  return OP_LABELS[op] ?? op.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
}

function opColor(op: string): string {
  return OP_COLORS[op] ?? "#6b7280";
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

function RequestLogs() {
  const [logs, setLogs] = createSignal<RequestLog[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [fetchError, setFetchError] = createSignal<string | null>(null);
  const [search, setSearch] = createSignal("");
  const [statusFilter, setStatusFilter] = createSignal("");   // "" | "ok" | "error"
  const [opFilter, setOpFilter] = createSignal("");           // "" | operation
  const [expanded, setExpanded] = createSignal<string | null>(null);
  let searchTimeout: ReturnType<typeof setTimeout> | undefined;
  let eventTimeout: ReturnType<typeof setTimeout> | undefined;

  // Generation counter: a slow in-flight response must not overwrite the
  // result of a newer search/filter change.
  let loadGen = 0;
  async function load() {
    const gen = ++loadGen;
    try {
      const rows = await listRequestLogs({
        limit: 500,
        search: search() || undefined,
        status: statusFilter() || undefined,
        operation: opFilter() || undefined,
      });
      if (gen !== loadGen) return;
      setLogs(rows);
      setFetchError(null);
    } catch (e) {
      if (gen !== loadGen) return;
      const msg = e instanceof Error ? e.message : String(e);
      console.error("list_request_logs failed:", e);
      setFetchError(msg);
      setLogs([]);
    } finally {
      if (gen === loadGen) setLoading(false);
    }
  }

  load();
  // Push-based: backend emits "request-log-added" after each DB insert.
  // Coalesce bursts (bulk delete/upload = hundreds of events) into one reload.
  // onCleanup must be registered synchronously — inside listen()'s .then there
  // is no reactive owner and the cleanup would silently never run.
  let disposed = false;
  let unlistenFn: (() => void) | null = null;
  onCleanup(() => {
    disposed = true;
    clearTimeout(searchTimeout);
    clearTimeout(eventTimeout);
    unlistenFn?.();
  });
  listen<void>("request-log-added", () => {
    clearTimeout(eventTimeout);
    eventTimeout = setTimeout(load, 250);
  }).then((unlisten) => {
    if (disposed) unlisten();
    else unlistenFn = unlisten;
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
      toast.ok("Request logs cleared", "All recorded S3 request history was deleted");
    } catch (e) {
      toast.err(e);
    }
  }

  const today = () => new Date().toLocaleDateString("en-US", { month: "short", day: "numeric" });

  return (
    <div class="view-container min-h-0">
      {/* toolbar */}
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

        {/* operation filter */}
        <Select
          value={opFilter()}
          placeholder="All operations"
          options={Object.keys(OP_LABELS).map((op) => ({ value: op, label: OP_LABELS[op] }))}
          class="logs-select logs-select-op"
          onChange={(v) => { setOpFilter(v); load(); }}
        />

        {/* status filter */}
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
          <button
            class="btn-ghost logs-clear-btn"
            onClick={doClear}
          >
            <IconTrash size={13} /> Clear all
          </button>
        </Show>
      </div>

      {/* list */}
      <Show when={loading()}>
        <div class="loading-row"><span class="spinner" /> Loading…</div>
      </Show>
      <Show when={!loading()}>
        <Show when={fetchError()}>
          <div class="empty-state">
            <span class="logs-fetch-err">
              Error: {fetchError()}
            </span>
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
          <div class="logs-body min-h-0" id="req-log-scroll">
            <For each={logs()}>
              {(log) => {
                const isExpanded = () => expanded() === log.id;
                const dateLabel = fmtDate(log.created_at);
                const color = opColor(log.operation);
                const isErr = log.status === "error";
                return (
                  <div
                    class={`req-log-row${isErr ? " req-log-error" : ""}${isExpanded() ? " req-log-open" : ""}`}
                    style={{ "--row-color": isErr ? "#ef4444" : color }}
                    onClick={() => setExpanded(isExpanded() ? null : log.id)}
                  >
                    {/* ── collapsed row ── */}
                    <div class="req-log-main">
                      {/* colored status dot */}
                      <span class="req-log-dot" style={{ background: isErr ? "#ef4444" : "#22c55e" }} />

                      {/* timestamp */}
                      <span class="req-log-ts">
                        <Show when={dateLabel !== today()}>
                          <span class="req-log-ts-date">{dateLabel}</span>
                        </Show>
                        {fmtTime(log.created_at)}
                      </span>

                      {/* operation badge */}
                      <span
                        class="req-log-op"
                        style={{ "--op-color": color }}
                      >
                        {opLabel(log.operation)}
                      </span>

                      {/* account pill */}
                      <Show when={log.account_name}>
                        <span class="req-log-account">{log.account_name}</span>
                      </Show>

                      {/* bucket / key path */}
                      <span class="req-log-target">
                        <Show when={log.bucket}>
                          <span class="req-log-bucket">{log.bucket}</span>
                        </Show>
                        <Show when={log.key}>
                          <span class="req-log-sep">/</span>
                          <span class="req-log-key">{truncateKey(log.key)}</span>
                        </Show>
                      </span>

                      <div class="flex-1" />

                      {/* duration, color-coded */}
                      <span class={`req-log-duration ${durationClass(log.duration_ms)}`}>
                        {fmtDuration(log.duration_ms)}
                      </span>

                      {/* chevron */}
                      <span class="req-log-chevron">{isExpanded() ? "▾" : "▸"}</span>
                    </div>

                    {/* ── expanded detail card ── */}
                    <Show when={isExpanded()}>
                      {/* stop propagation: text selection inside the card must
                          not bubble to the row's collapse toggle */}
                      <div class="req-log-detail" onClick={(e) => e.stopPropagation()}>
                        {/* header strip */}
                        <div class="req-log-detail-header" style={{ "border-left-color": color }}>
                          <Show when={log.http_method}>
                            <span class="req-log-http-method">{log.http_method}</span>
                          </Show>
                          <span class="req-log-detail-op" style={{ color }}>
                            {opLabel(log.operation)}
                          </span>
                          <span class="req-log-detail-raw-op">{log.operation}</span>
                          <div class="flex-1" />
                          <Show when={log.response_status}>
                            <span class={`req-log-http-status ${isErr ? "req-log-http-status-err" : "req-log-http-status-ok"}`}>
                              HTTP {log.response_status}
                            </span>
                          </Show>
                          <span
                            class={`req-log-detail-status-badge ${isErr ? "req-log-detail-err-badge" : "req-log-detail-ok-badge"}`}
                          >
                            {isErr ? "✕ error" : "✓ ok"}
                          </span>
                        </div>

                        {/* URL bar */}
                        <Show when={log.request_url}>
                          <div class="req-log-url-bar">
                            <span class="req-log-chip-label req-log-chip-label-url">URL</span>
                            <span class="req-log-url-text">{log.request_url}</span>
                            <button
                              class="req-log-copy-btn"

                              onClick={(e) => { e.stopPropagation(); navigator.clipboard.writeText(log.request_url!); }}
                            >⎘</button>
                          </div>
                        </Show>

                        {/* meta chips */}
                        <div class="req-log-detail-chips">
                          <Show when={log.account_name}>
                            <span class="req-log-chip req-log-chip-account">
                              <span class="req-log-chip-label">account</span>
                              {log.account_name}
                            </span>
                          </Show>
                          <Show when={log.bucket}>
                            <span class="req-log-chip req-log-chip-bucket">
                              <span class="req-log-chip-label">bucket</span>
                              {log.bucket}
                            </span>
                          </Show>
                          <span class={`req-log-chip req-log-chip-duration ${durationClass(log.duration_ms)}`}>
                            <span class="req-log-chip-label">duration</span>
                            {log.duration_ms}ms
                          </span>
                          <span class="req-log-chip req-log-chip-time">
                            <span class="req-log-chip-label">time</span>
                            {new Date(log.created_at * 1000).toISOString().replace("T", " ").replace("Z", " UTC")}
                          </span>
                        </div>

                        {/* object key */}
                        <Show when={log.key}>
                          <div class="req-log-detail-key-row">
                            <span class="req-log-chip-label req-log-chip-label-nostretch">key</span>
                            <code class="req-log-detail-key">{log.key}</code>
                          </div>
                        </Show>

                        {/* request params JSON */}
                        <Show when={log.request_params && log.request_params !== "null"}>
                          <div class="req-log-params-block">
                            <div class="req-log-params-header">
                              <span class="req-log-chip-label">request params</span>
                            </div>
                            <pre class="req-log-params-json">{(() => {
                              try { return JSON.stringify(JSON.parse(log.request_params!), null, 2); }
                              catch { return log.request_params; }
                            })()}</pre>
                          </div>
                        </Show>

                        {/* response metadata JSON */}
                        <Show when={log.response_meta && log.response_meta !== "null"}>
                          <div class="req-log-params-block">
                            <div class="req-log-params-header">
                              <span class="req-log-chip-label">response</span>
                            </div>
                            <pre class="req-log-params-json">{(() => {
                              try { return JSON.stringify(JSON.parse(log.response_meta!), null, 2); }
                              catch { return log.response_meta; }
                            })()}</pre>
                          </div>
                        </Show>

                        {/* error box */}
                        <Show when={isErr}>
                          <div class="req-log-detail-error-box">
                            <div class="req-log-detail-error-header">
                              <Show when={log.error_code}>
                                <span class="req-log-detail-error-code">{log.error_code}</span>
                              </Show>
                              <span class="req-log-detail-error-label">ERROR</span>
                            </div>
                            <Show when={log.error_msg}>
                              <p class="req-log-detail-error-msg">{log.error_msg}</p>
                            </Show>
                          </div>
                        </Show>
                      </div>
                    </Show>
                  </div>
                );
              }}
            </For>
          </div>
        </Show>
      </Show>
    </div>
  );
}

// ── system log (existing tail viewer) ─────────────────────────────────────────

interface ParsedLine {
  ts: string;
  level: string;
  span: string | null;
  fields: Record<string, string>;
  msg: string;
  json: unknown | null;
}

const ANSI_RE = /\[[0-9;]*m/g;
function stripAnsi(s: string) { return s.replace(ANSI_RE, ""); }
const SPAN_RE = /^([a-zA-Z_][a-zA-Z0-9_]*)\{([^}]*)\}:\s*/;

function parseFields(s: string): Record<string, string> {
  const out: Record<string, string> = {};
  const re = /(\w+)=("([^"]*)"|(\S+))/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(s)) !== null) {
    out[m[1]] = m[3] ?? m[4] ?? "";
  }
  return out;
}

function tryJson(s: string): unknown | null {
  const t = s.trim();
  if (!(t.startsWith("{") && t.endsWith("}")) && !(t.startsWith("[") && t.endsWith("]"))) return null;
  try { return JSON.parse(t); } catch { return null; }
}

function parseLine(raw: string): ParsedLine | null {
  const clean = stripAnsi(raw).trim();
  if (!clean) return null;
  const m = clean.match(/^(\d{4}-\d{2}-\d{2}T[\d:.]+Z?)\s+(INFO|DEBUG|WARN|ERROR|TRACE)\s+(.+)$/);
  let ts = "", level = "DEBUG", rest = clean;
  if (m) { ts = m[1].replace("T", " ").replace(/\.\d+Z?$/, ""); level = m[2]; rest = m[3]; }
  let span: string | null = null;
  let fields: Record<string, string> = {};
  const spanMatch = rest.match(SPAN_RE);
  if (spanMatch) {
    span = spanMatch[1];
    fields = parseFields(spanMatch[2]);
    rest = rest.slice(spanMatch[0].length);
  }
  const colonIdx = rest.indexOf(": ");
  let msg = rest;
  if (colonIdx > 0 && /^[a-zA-Z_][\w:]*$/.test(rest.slice(0, colonIdx))) {
    msg = rest.slice(colonIdx + 2);
  }
  return { ts, level, span, fields, msg, json: tryJson(msg) };
}

function levelClass(l: string): string {
  switch (l) {
    case "INFO": return "info";
    case "WARN": return "warn";
    case "ERROR": return "error";
    default: return "debug";
  }
}

function SystemLog() {
  const [lines, setLines] = createSignal<ParsedLine[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [clearedAt, setClearedAt] = createSignal<string | null>(null);
  const [search, setSearch] = createSignal("");
  const [levelFilter, setLevelFilter] = createSignal("");

  async function load() {
    try {
      const tail = await getLogTail(512 * 1024);
      const parsed = tail.content.split("\n").map(parseLine).filter(Boolean) as ParsedLine[];
      const anchor = clearedAt();
      setLines(anchor ? parsed.filter((l) => l.ts > anchor) : parsed);
    } catch { setLines([]); } finally { setLoading(false); }
  }

  load();
  const timer = setInterval(load, 3000);
  onCleanup(() => clearInterval(timer));

  // Newest first + level/text filters, all client-side.
  const filtered = createMemo(() => {
    const q = search().trim().toLowerCase();
    const lvl = levelFilter();
    const out: ParsedLine[] = [];
    for (const l of lines()) {
      if (lvl && l.level !== lvl) continue;
      if (q) {
        const hay = `${l.ts} ${l.level} ${l.span ?? ""} ${l.msg} ${Object.entries(l.fields).map(([k, v]) => `${k}=${v}`).join(" ")}`.toLowerCase();
        if (!hay.includes(q)) continue;
      }
      out.push(l);
    }
    return out.reverse();
  });

  return (
    <div class="view-container min-h-0">
      <div class="logs-header">
        <div class="logs-search-wrap">
          <IconSearch size={13} class="logs-search-icon" />
          <input
            class="field logs-search-input"
            placeholder="Search message, span, field…"
            value={search()}
            onInput={(e) => setSearch(e.currentTarget.value)}
          />
        </div>

        <Select
          value={levelFilter()}
          placeholder="All levels"
          options={[
            { value: "ERROR", label: "Error" },
            { value: "WARN", label: "Warn" },
            { value: "INFO", label: "Info" },
            { value: "DEBUG", label: "Debug" },
          ]}
          class="logs-select logs-select-level"
          onChange={setLevelFilter}
        />

        <span class="logs-tailing-label">
          <span class="logs-tailing-dot" /> tailing
        </span>
        <Show when={lines().length > 0}>
          <button
            class="btn-ghost logs-clear-btn"
            onClick={() => { setClearedAt(lines().at(-1)?.ts ?? new Date().toISOString()); setLines([]); }}
          >
            <IconTrash size={13} /> Clear
          </button>
        </Show>
      </div>
      <Show when={loading()}>
        <div class="loading-row"><span class="spinner" /> Loading logs…</div>
      </Show>
      <Show when={!loading()}>
        <Show
          when={filtered().length > 0}
          fallback={
            <div class="empty-state">
              <span class="logs-empty-text">
                {search() || levelFilter() ? "No results" : "No log entries yet"}
              </span>
            </div>
          }
        >
          <div class="logs-body min-h-0">
            <For each={filtered()}>{(line) => <LogRow line={line} />}</For>
          </div>
        </Show>
      </Show>
    </div>
  );
}

function LogRow(props: { line: ParsedLine }) {
  const [open, setOpen] = createSignal(false);
  const fieldEntries = () => Object.entries(props.line.fields);
  const hasFields = () => fieldEntries().length > 0;
  const hasJson = () => props.line.json !== null;
  return (
    <div class="log-line">
      <span class="log-ts">{props.line.ts}</span>
      <span class={`log-level ${levelClass(props.line.level)}`}>{props.line.level}</span>
      <div class="log-main">
        <div class="log-headline">
          <Show when={props.line.span}><span class="log-span">{props.line.span}</span></Show>
          <Show when={hasFields()}>
            <span class="log-fields">
              <For each={fieldEntries()}>
                {([k, v]) => <span class="log-kv"><span class="log-k">{k}</span><span class="log-v">{v}</span></span>}
              </For>
            </span>
          </Show>
          <span class="log-msg">{props.line.msg}</span>
          <Show when={hasJson()}>
            <button class="log-json-toggle" onClick={() => setOpen(!open())}>{open() ? "−" : "+"} json</button>
          </Show>
        </div>
        <Show when={hasJson() && open()}>
          <pre class="log-json">{JSON.stringify(props.line.json, null, 2)}</pre>
        </Show>
      </div>
    </div>
  );
}

// ── main export ───────────────────────────────────────────────────────────────

export default function Logs() {
  const [tab, setTab] = createSignal<Tab>("requests");
  return (
    <div class="view-container">
      {/* tab bar */}
      <div class="logs-header logs-tabbar">
        <h2 class="logs-tabbar-title">Logs</h2>
        <button
          class={`logs-tab${tab() === "requests" ? " active" : ""}`}
          onClick={() => setTab("requests")}
        >
          API Requests
        </button>
        <button
          class={`logs-tab${tab() === "system" ? " active" : ""}`}
          onClick={() => setTab("system")}
        >
          System
        </button>
      </div>

      <Show when={tab() === "requests"}><RequestLogs /></Show>
      <Show when={tab() === "system"}><SystemLog /></Show>
    </div>
  );
}
