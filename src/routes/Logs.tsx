import { createSignal, createEffect, onCleanup, For, Show } from "solid-js";
import { getLogTail } from "../api/logs";
import { IconTrash } from "../utils/icons";

interface ParsedLine {
  ts: string;
  level: string;
  msg: string;
}

function parseLine(raw: string): ParsedLine | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  // tracing format: "2026-06-20T18:42:01.123Z  INFO cosmog: ..."
  // or: "2026-06-20T18:42:01  INFO some message"
  const m = trimmed.match(/^(\d{4}-\d{2}-\d{2}T[\d:.]+Z?)\s+(INFO|DEBUG|WARN|ERROR|TRACE)\s+(.+)$/);
  if (m) return { ts: m[1].replace("T", " ").replace(/\.\d+Z?$/, ""), level: m[2], msg: m[3] };
  // fallback — show raw
  return { ts: "", level: "DEBUG", msg: trimmed };
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

  // Auto-scroll to bottom when new lines arrive
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
                    {(line) => (
                      <div class="log-line">
                        <span class="log-ts">{line.ts}</span>
                        <span class={`log-level ${levelClass(line.level)}`}>{line.level}</span>
                        <span class="log-msg">{line.msg}</span>
                      </div>
                    )}
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
