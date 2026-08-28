import { createSignal, createMemo, createEffect, onMount, onCleanup, For, Show } from "solid-js";
import { currentView } from "../state/app";
import { getLogTail } from "../api/logs";
import { IconSearch, IconTrash, IconX } from "../utils/icons";
import { Select } from "../utils/Select";
import { parseLine, type ParsedLine } from "../utils/parseLine";
import { sourceByKey, sourceLabel } from "../utils/logSource";
import { LogRow } from "./LogRow";

export function SystemLog(props: { active?: boolean }) {
  const isActive = () => props.active !== false;
  const [lines, setLines] = createSignal<ParsedLine[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [clearedAt, setClearedAt] = createSignal<string | null>(null);
  const [search, setSearch] = createSignal("");
  const [levelFilter, setLevelFilter] = createSignal("");
  const [sourceFilter, setSourceFilter] = createSignal<string>("");
  const [focusIdx, setFocusIdx] = createSignal(0);
  const [hoverLocked, setHoverLocked] = createSignal(false);
  let hoverLockTimer: ReturnType<typeof setTimeout> | undefined;
  let bodyEl: HTMLDivElement | undefined;
  const setBodyEl = (el: HTMLDivElement) => { bodyEl = el; };
  function lockHover(ms = 450) {
    setHoverLocked(true);
    clearTimeout(hoverLockTimer);
    hoverLockTimer = setTimeout(() => setHoverLocked(false), ms);
  }
  onCleanup(() => clearTimeout(hoverLockTimer));

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

  // Both log tabs stay mounted (hidden via CSS) to preserve scroll/selection;
  // only poll the 512 KB tail while this tab is selected and Logs is visible.
  createEffect(() => {
    if (currentView() !== "logs" || !isActive()) return;
    load();
    const timer = setInterval(load, 3000);
    onCleanup(() => clearInterval(timer));
  });

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

  createEffect(() => {
    search();
    levelFilter();
    sourceFilter();
    lockHover(600);
    setFocusIdx(0);
  });
  createEffect(() => {
    const n = filtered().length;
    if (hoverLocked()) lockHover(250);
    setFocusIdx((i) => (n === 0 ? -1 : Math.max(0, Math.min(i, n - 1))));
  });
  createEffect(() => {
    const i = focusIdx();
    if (i < 0 || !bodyEl) return;
    const row = bodyEl.querySelectorAll<HTMLElement>(".log-line")[i];
    row?.scrollIntoView({ block: "nearest" });
  });
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      const n = filtered().length;
      if (!n) return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.isComposing || (e as any).keyCode === 229) return;
      const t = e.target as HTMLElement | null;
      if (!t) return;
      const inSearch = t.tagName === "INPUT" && t.classList.contains("logs-search-input");
      const inBody = !!t.closest(".logs-body, .logs-header");
      if (!inSearch && !inBody) return;
      if (t.closest(".modal, .context-menu, [role='dialog']")) return;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setFocusIdx((i) => Math.min(n - 1, Math.max(0, i) + 1));
        lockHover(300);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setFocusIdx((i) => Math.max(0, (i < 0 ? 0 : i) - 1));
        lockHover(300);
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
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
          <Show when={search()}>
            <button class="logs-search-clear" aria-label="Clear search" onClick={() => setSearch("")}>
              <IconX size={12} />
            </button>
          </Show>
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
          <div class="logs-body min-h-0" ref={setBodyEl}>
            <For each={filtered()}>
              {(line, i) => (
                <LogRow
                  line={line}
                  activeSource={sourceFilter()}
                  onSourceClick={toggleSource}
                  searchQuery={search()}
                  focused={focusIdx() === i()}
                  onHover={() => {
                    if (hoverLocked()) return;
                    setFocusIdx(i());
                  }}
                />
              )}
            </For>
          </div>
        </Show>
      </Show>
    </div>
  );
}
