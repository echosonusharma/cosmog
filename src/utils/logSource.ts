// Derive which subsystem emitted a log line so it can be tagged + filtered.
// Precedence: tracing target prefix first, then message keyword match.

export interface LogSource { key: string; label: string; color: string; }

const SRC_NIGHT_WATCHER: LogSource = { key: "night-watcher", label: "Night Watcher", color: "#a45cf0" };
const SRC_SCHEDULER:     LogSource = { key: "scheduler",     label: "Scheduler",     color: "#06b6d4" };
const SRC_TRANSFERS:     LogSource = { key: "transfers",     label: "Transfers",     color: "#f59e0b" };
const SRC_BUCKETS:       LogSource = { key: "buckets",       label: "Buckets/S3",    color: "#14b8a6" };
const SRC_ENCRYPTION:    LogSource = { key: "encryption",    label: "Encryption",    color: "#ec4899" };
const SRC_SEARCH:        LogSource = { key: "search",        label: "Search",        color: "#8b5cf6" };
const SRC_APP:           LogSource = { key: "app",           label: "App",           color: "#94a3b8" };

// target substring -> source. Checked before keyword matching.
const TARGET_MAP: Array<[string, LogSource]> = [
  ["night_watcher", SRC_NIGHT_WATCHER],
  ["scheduler", SRC_SCHEDULER],
  ["transfer", SRC_TRANSFERS],
  ["commands::buckets", SRC_BUCKETS],
  ["store::s3", SRC_BUCKETS],
  ["commands::objects", SRC_BUCKETS],
  ["encryption", SRC_ENCRYPTION],
  ["crypto", SRC_ENCRYPTION],
  ["commands::search", SRC_SEARCH],
  ["sync", SRC_SEARCH],
];

const ALL_SOURCES: LogSource[] = [
  SRC_NIGHT_WATCHER, SRC_SCHEDULER, SRC_TRANSFERS,
  SRC_BUCKETS, SRC_ENCRYPTION, SRC_SEARCH, SRC_APP,
];

export function sourceByKey(key: string): LogSource {
  return ALL_SOURCES.find((s) => s.key === key) ?? SRC_APP;
}

export function sourceLabel(key: string): string {
  return sourceByKey(key).label;
}

export function logSource(target: string | null, message: string): LogSource {
  const t = (target ?? "").toLowerCase();
  const m = message.toLowerCase();
  // Night watcher is the priority: match on message keyword regardless of target.
  if (m.includes("night watcher")) return SRC_NIGHT_WATCHER;
  if (t) {
    for (const [needle, src] of TARGET_MAP) if (t.includes(needle)) return src;
  }
  if (m.includes("scheduler")) return SRC_SCHEDULER;
  if (m.includes("transfer") || m.includes("upload queue") || m.includes("download queue")) return SRC_TRANSFERS;
  if (m.includes("encryption") || m.includes("crypto")) return SRC_ENCRYPTION;
  return SRC_APP;
}
