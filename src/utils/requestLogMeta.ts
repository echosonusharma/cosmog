/** Human labels and chart colors for S3 API operation keys in request logs. */
export const OP_LABELS: Record<string, string> = {
  list_buckets: "List Buckets",
  create_bucket: "Create Bucket",
  delete_bucket: "Delete Bucket",
  head_bucket: "Head Bucket",
  put_bucket_acl: "Set Bucket ACL",
  get_bucket_versioning: "Get Versioning",
  put_bucket_versioning: "Set Versioning",
  get_bucket_policy: "Get Policy",
  put_bucket_policy: "Set Policy",
  delete_bucket_policy: "Delete Policy",
  get_bucket_cors: "Get CORS",
  put_bucket_cors: "Set CORS",
  delete_bucket_cors: "Delete CORS",
  head_object: "Head Object",
  create_folder: "Create Folder",
  delete_object: "Delete Object",
  delete_objects: "Batch Delete",
  delete_object_version: "Delete Version",
  restore_object_version: "Restore Version",
  list_objects: "List Objects",
  list_object_versions: "List Versions",
  copy_object: "Copy Object",
  put_object_acl: "Set Object ACL",
  presign_get: "Presign URL",
  read_object_range: "Preview Object",
  read_object_full: "Read Object",
  get_object_tagging: "Get Tags",
  put_object_tagging: "Set Tags",
  delete_object_tagging: "Delete Tags",
  put_object: "Upload",
  put_object_bytes: "Save Object",
  get_object: "Download",
  abort_multipart_upload: "Abort Multipart",
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
  return OP_LABELS[op] ?? op.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
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
  return accountName ?? accountId ?? "Unknown";
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

/** Compact axis / tooltip count (e.g. 32.6k). */
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
