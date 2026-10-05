import { t, type TKey } from "../i18n";

// Label keys per operation; translated lazily in opLabel().
export const OP_KEYS: Record<string, TKey> = {
  list_buckets: "logs.op.list_buckets",
  create_bucket: "logs.op.create_bucket",
  delete_bucket: "logs.op.delete_bucket",
  head_bucket: "logs.op.head_bucket",
  put_bucket_acl: "logs.op.put_bucket_acl",
  get_bucket_versioning: "logs.op.get_bucket_versioning",
  put_bucket_versioning: "logs.op.put_bucket_versioning",
  get_bucket_policy: "logs.op.get_bucket_policy",
  put_bucket_policy: "logs.op.put_bucket_policy",
  delete_bucket_policy: "logs.op.delete_bucket_policy",
  get_bucket_cors: "logs.op.get_bucket_cors",
  put_bucket_cors: "logs.op.put_bucket_cors",
  delete_bucket_cors: "logs.op.delete_bucket_cors",
  head_object: "logs.op.head_object",
  create_folder: "logs.op.create_folder",
  delete_object: "logs.op.delete_object",
  delete_objects: "logs.op.delete_objects",
  delete_object_version: "logs.op.delete_object_version",
  restore_object_version: "logs.op.restore_object_version",
  list_objects: "logs.op.list_objects",
  list_object_versions: "logs.op.list_object_versions",
  copy_object: "logs.op.copy_object",
  put_object_acl: "logs.op.put_object_acl",
  presign_get: "logs.op.presign_get",
  read_object_range: "logs.op.read_object_range",
  read_object_full: "logs.op.read_object_full",
  get_object_tagging: "logs.op.get_object_tagging",
  put_object_tagging: "logs.op.put_object_tagging",
  delete_object_tagging: "logs.op.delete_object_tagging",
  put_object: "logs.op.put_object",
  put_object_bytes: "logs.op.put_object_bytes",
  get_object: "logs.op.get_object",
  abort_multipart_upload: "logs.op.abort_multipart_upload",
};

export const OP_COLORS: Record<string, string> = {
  put_object: "#22c55e",
  put_object_bytes: "#22c55e",
  get_object: "#3b82f6",
  delete_object: "#ef4444",
  delete_objects: "#ef4444",
  delete_object_version: "#ef4444",
  delete_object_tagging: "#ef4444",
  delete_bucket: "#ef4444",
  delete_bucket_policy: "#ef4444",
  delete_bucket_cors: "#ef4444",
  create_bucket: "#a855f7",
  create_folder: "#a855f7",
  restore_object_version: "#a855f7",
  copy_object: "#f59e0b",
  presign_get: "#06b6d4",
  abort_multipart_upload: "#f97316",
  head_bucket: "#6366f1",
  head_object: "#6366f1",
  list_buckets: "#14b8a6",
  list_objects: "#8b5cf6",
  list_object_versions: "#6366f1",
  put_bucket_acl: "#ec4899",
  put_object_acl: "#ec4899",
  put_bucket_versioning: "#ec4899",
  put_bucket_policy: "#ec4899",
  put_bucket_cors: "#ec4899",
  get_bucket_versioning: "#94a3b8",
  get_bucket_policy: "#94a3b8",
  get_bucket_cors: "#94a3b8",
  read_object_range: "#06b6d4",
  read_object_full: "#3b82f6",
  get_object_tagging: "#94a3b8",
  put_object_tagging: "#f59e0b",
};

export const CHART_PALETTE = [
  "#a45cf0",
  "#4f9dff",
  "#37c9a8",
  "#f0b429",
  "#ff7a5c",
  "#e05cc0",
  "#8a94a6",
];

export function opLabel(op: string): string {
  return OP_KEYS[op] ? t(OP_KEYS[op]) : op.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
}

export function opColor(op: string, fallbackIndex = 0): string {
  return OP_COLORS[op] ?? CHART_PALETTE[fallbackIndex % CHART_PALETTE.length];
}

export function accountKey(
  accountId: string | null | undefined,
  accountName: string | null | undefined,
): string {
  if (accountId && accountName) return `${accountId}\x1f${accountName}`;
  return accountId ?? accountName ?? "unknown";
}

export function accountLabel(
  accountId: string | null | undefined,
  accountName: string | null | undefined,
): string {
  return accountName ?? accountId ?? t("logs.account.unknown");
}

type AccountRef = {
  account_id: string | null;
  account_name: string | null;
};

function shortIdSuffix(id: string): string {
  return id.length > 8 ? id.slice(-6) : id;
}

/** Unique label when exact name or truncated form would collide in the UI. */
function disambiguatedAccountLabel(base: string, key: string): string {
  const suffix = shortIdSuffix(key);
  const maxBase = 18;
  const shortBase =
    base.length > maxBase ? `${base.slice(0, maxBase - 1)}…` : base;
  return `${shortBase} (${suffix})`;
}

/** Build display labels; suffix account id when names collide or truncate the same. */
export function accountLabelMap(
  accounts: AccountRef[],
  truncateAt = 20,
): Map<string, string> {
  const entries = accounts.map((a) => {
    const key = accountKey(a.account_id, a.account_name);
    const base = accountLabel(a.account_id, a.account_name);
    const id = a.account_id ?? key;
    return { key, base, id };
  });

  const truncated = (base: string) => chartLegendLabel(base, truncateAt);

  const exactCounts = new Map<string, number>();
  const truncCounts = new Map<string, number>();
  for (const e of entries) {
    exactCounts.set(e.base, (exactCounts.get(e.base) ?? 0) + 1);
    const t = truncated(e.base);
    truncCounts.set(t, (truncCounts.get(t) ?? 0) + 1);
  }

  const labels = new Map<string, string>();
  for (const e of entries) {
    const needs =
      (exactCounts.get(e.base) ?? 0) > 1
      || (truncCounts.get(truncated(e.base)) ?? 0) > 1;
    labels.set(e.key, needs ? disambiguatedAccountLabel(e.base, e.id) : e.base);
  }
  return labels;
}

export function accountLabelFromMap(
  labels: Map<string, string>,
  accountId: string | null | undefined,
  accountName: string | null | undefined,
): string {
  return labels.get(accountKey(accountId, accountName)) ?? accountLabel(accountId, accountName);
}

export function formatChartCount(v: number): string {
  const n = Math.abs(v);
  if (n >= 1_000_000) return `${(v / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${(v / 1000).toFixed(1)}k`;
  return String(Math.round(v));
}

/** Short label for crowded chart legends; keeps a trailing (id) disambiguator. */
export function chartLegendLabel(label: string, max = 22): string {
  if (label.length <= max) return label;
  const paren = label.match(/^(.+)\s(\([^)]+\))$/);
  if (paren) {
    const [, base, suffix] = paren;
    const room = max - suffix.length - 1;
    if (room >= 4) {
      const short =
        base.length > room ? `${base.slice(0, room - 1)}…` : base;
      return `${short} ${suffix}`;
    }
  }
  return `${label.slice(0, max - 1)}…`;
}
