import { createSignal, createEffect, onCleanup, For, Show } from "solid-js";
import { getLogTail } from "../api/logs";
import { IconTrash } from "../utils/icons";

interface ParsedLine {
  ts: string;
  level: string;
  span: string | null;
  fields: Record<string, string>;
  msg: string;
  json: unknown | null;
}

const ANSI_RE = /\[[0-9;]*m/g;
function stripAnsi(s: string) { return s.replace(ANSI_RE, ""); }

// Match optional `span_name{key=value key=value}` segment.
const SPAN_RE = /^([a-zA-Z_][a-zA-Z0-9_]*)\{([^}]*)\}:\s*/;

function parseFields(s: string): Record<string, string> {
  // tokens like `key=value` (value may contain spaces if quoted — keep simple).
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

  // After span there may be `target::module: message` — drop leading module path
  // up to the first `: ` so the visible message is the actual log text.
  const colonIdx = rest.indexOf(": ");
  let msg = rest;
  if (colonIdx > 0 && /^[a-zA-Z_][\w:]*$/.test(rest.slice(0, colonIdx))) {
    msg = rest.slice(colonIdx + 2);
  }

  return { ts, level, span, fields, msg, json: tryJson(msg) };
}

function levelClass(l: string): string {
  switch (l) {
    case "INFO":  return "info";
    case "WARN":  return "warn";
    case "ERROR": return "error";
    default:      return "debug";
  }
}

export default function Logs() {
  const [lines, setLines] = createSignal<ParsedLine[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [clearedAt, setClearedAt] = createSignal<string | null>(null);
  let bottomRef: HTMLDivElement | undefined;

  async function load() {
    try {
      const tail = await getLogTail(512 * 1024);
      const parsed = tail.content
        .split("\n")
        .map(parseLine)
        .filter(Boolean) as ParsedLine[];
      const anchor = clearedAt();
      const visible = anchor
        ? parsed.filter((l) => l.ts > anchor)
        : parsed;
      setLines(visible);
    } catch {
      setLines([]);
    } finally {
      setLoading(false);
    }
  }

  load();
  const timer = setInterval(load, 3000);
  onCleanup(() => clearInterval(timer));

  createEffect(() => {
    lines();
    bottomRef?.scrollIntoView({ behavior: "smooth" });
  });

  function clearLog() {
    const last = lines().at(-1)?.ts ?? new Date().toISOString();
    setClearedAt(last);
    setLines([]);
  }

  return (
    <div class="view-container">
      <div class="logs-header">
        <h2 style="margin:0;font-size:15px;font-weight:600;letter-spacing:-.01em">Activity log</h2>
        <span style="font-size:11.5px;color:var(--muted);font-family:var(--font-mono);display:inline-flex;align-items:center;gap:6px">
          <span class="logs-tailing-dot" />
          tailing
        </span>
        <div style="flex:1" />
        <Show when={lines().length > 0}>
          <button class="btn-ghost" style="font-size:12px;border:1px solid var(--border);border-radius:8px;padding:5px 11px" onClick={clearLog}>
            <IconTrash size={13} /> Clear
          </button>
        </Show>
      </div>

      <Show when={loading()}>
        <div class="loading-row"><span class="spinner" /> Loading logs…</div>
      </Show>

      <Show when={!loading()}>
        <Show when={lines().length === 0}
              fallback={
                <div class="logs-body">
                  <For each={lines()}>
                    {(line) => <LogRow line={line} />}
                  </For>
                  <div ref={bottomRef} />
                </div>
              }>
          <div class="empty-state">
            <span style="font-size:13px;color:var(--text-faint)">No log entries yet</span>
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
          <Show when={props.line.span}>
            <span class="log-span">{props.line.span}</span>
          </Show>
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
