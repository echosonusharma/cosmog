// Derive which subsystem emitted a log line; tracing target prefix wins over message keywords.
import { t } from "../i18n";

export interface LogSource { key: string; label: string; color: string; }

// label is a getter so it follows the active language.
const SRC_NIGHT_WATCHER: LogSource = { key: "night-watcher", get label() { return t("logs.source.nightWatcher"); }, color: "#a45cf0" };
const SRC_SCHEDULER:     LogSource = { key: "scheduler",     get label() { return t("logs.source.scheduler"); },     color: "#06b6d4" };
const SRC_TRANSFERS:     LogSource = { key: "transfers",     get label() { return t("logs.source.transfers"); },     color: "#f59e0b" };
const SRC_BUCKETS:       LogSource = { key: "buckets",       get label() { return t("logs.source.buckets"); },    color: "#14b8a6" };
const SRC_ENCRYPTION:    LogSource = { key: "encryption",    get label() { return t("logs.source.encryption"); },    color: "#ec4899" };
const SRC_SEARCH:        LogSource = { key: "search",        get label() { return t("logs.source.search"); },        color: "#8b5cf6" };
const SRC_MCP:           LogSource = { key: "mcp",           get label() { return t("logs.source.mcp"); },           color: "#6366f1" };
const SRC_APP:           LogSource = { key: "app",           get label() { return t("logs.source.app"); },           color: "#94a3b8" };

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
  ["mcp", SRC_MCP],
];

const ALL_SOURCES: LogSource[] = [
  SRC_NIGHT_WATCHER, SRC_SCHEDULER, SRC_TRANSFERS,
  SRC_BUCKETS, SRC_ENCRYPTION, SRC_SEARCH, SRC_MCP, SRC_APP,
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
  if (m.includes("mcp server") || m.includes("mcp client")) return SRC_MCP;
  return SRC_APP;
}
