import { createSignal, createMemo, onCleanup, For, Show } from "solid-js";
import { getLogTail } from "../api/logs";
import { IconSearch, IconTrash } from "../utils/icons";
import { Select } from "../utils/Select";
import { parseLine, type ParsedLine } from "../utils/parseLine";
import { sourceByKey, sourceLabel } from "../utils/logSource";
import { LogRow } from "./LogRow";

export function SystemLog() {
  const [lines, setLines] = createSignal<ParsedLine[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [clearedAt, setClearedAt] = createSignal<string | null>(null);
  const [search, setSearch] = createSignal("");
  const [levelFilter, setLevelFilter] = createSignal("");
  const [sourceFilter, setSourceFilter] = createSignal<string>("");   // "" | source key

  // Click a source chip to filter to it; click the active one (or All) to clear.
  function toggleSource(key: string) {
    setSourceFilter((cur) => (cur === key ? "" : key));
  }

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
    const src = sourceFilter();
    const out: ParsedLine[] = [];
    for (const l of lines()) {
      if (lvl && l.level !== lvl) continue;
      if (src && l.source.key !== src) continue;
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

        <Show when={sourceFilter()}>
          <button
            class="logs-active-source"
            style={{ "--src-color": sourceByKey(sourceFilter()).color }}
            title="Clear source filter"
            onClick={() => setSourceFilter("")}
          >
            <span class="logs-source-dot" />
            {sourceLabel(sourceFilter())}
            <span class="logs-active-source-x">✕</span>
          </button>
        </Show>

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
        <div class="loading-row logs-loading"><span class="spinner" /> Loading logs…</div>
      </Show>
      <Show when={!loading()}>
        <Show
          when={filtered().length > 0}
          fallback={
            <div class="empty-state">
              <span class="logs-empty-text">
                {search() || levelFilter() || sourceFilter() ? "No results" : "No log entries yet"}
              </span>
            </div>
          }
        >
          <div class="logs-body min-h-0">
            <For each={filtered()}>
              {(line) => (
                <LogRow
                  line={line}
                  activeSource={sourceFilter()}
                  onSourceClick={toggleSource}
                />
              )}
            </For>
          </div>
        </Show>
      </Show>
    </div>
  );
}
