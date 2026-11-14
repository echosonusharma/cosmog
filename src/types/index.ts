export interface Account {
  id: string;
  name: string;
  protocol: string;
  endpoint: string | null;
  region: string;
  access_key_id: string;
  addressing_style: string;
  created_at: number;
  updated_at: number;
  /** Set by list_accounts when the secret is missing from this device's keychain. */
  needs_reauth?: boolean;
}

export interface Bucket {
  name: string;
  created_at: number | null;
}

export interface NightWatch {
  id: string;
  account_id: string;
  bucket: string;
  local_dir: string;
  tree_uri: string | null;
  key_prefix: string;
  ignore_file: string | null;
  delete_policy: string;
  full_scan_secs: number;
  enabled: boolean;
  created_at: number;
}

export interface WatchStatus {
  id: string;
  enabled: boolean;
  last_scan_at: number | null;
  files_tracked: number;
  last_error: string | null;
}

export interface CachedObjectMeta {
  account_id: string;
  bucket: string;
  key: string;
  size: number;
  etag: string | null;
  last_modified: number | null;
  storage_class: string | null;
  content_type: string | null;
  extension: string | null;
  basename: string;
  version_id: string | null;
  synced_at: number;
}

export interface BrowseResult {
  objects: CachedObjectMeta[];
  subprefixes: string[];
  mode: "indexed" | "live";
  continuation: string | null;
  truncated: boolean;
  last_synced_at: number | null;
}

export type TransferStatus = "pending" | "active" | "paused" | "done" | "failed" | "canceled";
export type Direction = "upload" | "download";

export interface Transfer {
  id: string;
  account_id: string;
  bucket: string;
  key: string;
  direction: Direction;
  local_path: string;
  bytes_total: number | null;
  bytes_done: number;
  status: TransferStatus;
  upload_id: string | null;
  error: string | null;
  created_at: number;
  updated_at: number;
  origin: TransferOrigin;
}

export type TransferOrigin = "user" | "nightwatch";

export type TransferEvent =
  | { kind: "started"; transfer_id: string; bytes_total: number | null }
  | { kind: "progress"; transfer_id: string; bytes_done: number; bytes_total: number | null }
  | { kind: "multipart_initiated"; transfer_id: string; upload_id: string; part_size: number }
  | { kind: "part_completed"; transfer_id: string; upload_id: string; part_number: number; etag: string }
  | { kind: "done"; transfer_id: string; etag: string | null }
  | { kind: "failed"; transfer_id: string; error: string }
  | { kind: "canceled"; transfer_id: string };

export interface AppSettings {
  default_download_dir: string | null;
  transfer_concurrency: number;
  multipart_parallelism: number;
  multipart_threshold_bytes: number;
  part_size_bytes: number;
  prefix_sync_ttl_secs: number;
  presign_default_expires_secs: number;
  theme: "light" | "dark" | "system";
  show_hidden: boolean;
  confirm_destructive: boolean;
  auto_preview: boolean;
  http_proxy: string | null;
  custom_ca_path: string | null;
  request_log_ttl_days: number;
}

export interface RequestLog {
  id: string;
  account_id: string | null;
  account_name: string | null;
  operation: string;
  http_method: string | null;
  request_url: string | null;
  request_params: string | null;
  response_meta: string | null;
  bucket: string | null;
  key: string | null;
  status: "ok" | "error";
  response_status: number | null;
  error_code: string | null;
  error_msg: string | null;
  duration_ms: number;
  created_at: number;
}

export interface RequestLogAccountStat {
  account_id: string | null;
  account_name: string | null;
  count: number;
  error_count: number;
  avg_duration_ms: number;
}

export interface RequestLogOperationStat {
  operation: string;
  count: number;
  error_count: number;
  avg_duration_ms: number;
}

export interface RequestLogDayStat {
  day: number;
  count: number;
  error_count: number;
}

export interface RequestLogDayAccountStat {
  day: number;
  account_id: string | null;
  account_name: string | null;
  count: number;
}

export interface RequestLogBucketStat {
  bucket: string;
  count: number;
}

export interface RequestLogStats {
  period_days: number;
  since_ts: number;
  total: number;
  ok_count: number;
  error_count: number;
  avg_duration_ms: number;
  by_account: RequestLogAccountStat[];
  by_operation: RequestLogOperationStat[];
  by_day: RequestLogDayStat[];
  by_day_by_account: RequestLogDayAccountStat[];
  top_buckets: RequestLogBucketStat[];
}

export type SearchScope =
  | { kind: "bucket" }
  | { kind: "prefix"; prefix: string; recursive: boolean };

export interface SearchFilters {
  extensions?: string[];
  size_min?: number;
  size_max?: number;
  modified_after?: number;
  modified_before?: number;
}

export interface SearchQuery {
  account_id: string;
  bucket: string;
  scope: SearchScope;
  query?: string;
  filters?: SearchFilters;
  sort?: "name" | "size" | "modified" | "extension";
  sort_dir?: "asc" | "desc";
  page_size?: number;
  cursor?: number;
}

export interface FacetBucket {
  value: string;
  count: number;
}

export interface SearchResult {
  objects: CachedObjectMeta[];
  total: number;
  facets: {
    extensions: FacetBucket[];
    size_buckets: FacetBucket[];
  };
  next_cursor: number | null;
}

export interface BucketIndexStatus {
  enabled: boolean;
  last_full_sync_at: number | null;
  object_count: number;
  scan_continuation: string | null;
  scan_started_at: number | null;
  auto_reindex_secs: number | null;
}

export interface BucketStats {
  object_count: number;
  total_bytes: number;
  by_storage_class: Array<{
    storage_class: string;
    object_count: number;
    total_bytes: number;
  }>;
  by_extension: Array<{
    extension: string;
    object_count: number;
    total_bytes: number;
  }>;
  extension_count: number;
  by_month: Array<{
    month: string;
    object_count: number;
    total_bytes: number;
  }>;
  largest: Array<{
    key: string;
    basename: string;
    size: number;
  }>;
}

export interface ObjectVersion {
  key: string;
  version_id: string | null;
  is_latest: boolean;
  is_delete_marker: boolean;
  size: number | null;
  etag: string | null;
  last_modified: number | null;
}

export interface ObjectPreview {
  bytes: number[];
  content_type: string | null;
  total_size: number | null;
  truncated: boolean;
}

export interface CorsRule {
  id: string | null;
  allowed_origins: string[];
  allowed_methods: string[];
  allowed_headers: string[];
  expose_headers: string[];
  max_age_seconds: number | null;
}

export interface CorsConfig {
  rules: CorsRule[];
}

// Folder upload (desktop). Mirrors the backend contract; timestamps are unix seconds.
export type FolderJobState =
  | "analyzing" | "analyzed" | "queued" | "running" | "paused"
  | "completed" | "completed_with_errors" | "canceled" | "failed";
export type FolderItemState =
  | "pending" | "uploading" | "done" | "skipped_exists" | "skipped_missing" | "failed" | "canceled" | "excluded";
export type ExcludeReason =
  | "symlink" | "special" | "unreadable" | "unreadable_dir" | "non_utf8" | "control_chars" | "key_too_long" | "file_too_large";
export type OverwritePolicy = "skip" | "overwrite";
export type FolderItemFilter = "all" | "existing" | "excluded" | "failed" | "skipped" | "drift" | "hidden";
export type LocalPathKind = "file" | "dir" | "other" | "missing";
export type FolderWarning = "many_files" | "large_total" | "hidden_present" | "excluded_present" | "encrypted_temp_space";

/** ext "" = no extension, "*" = rollup of the rest. */
export interface ExtStat { ext: string; files: number; bytes: number }
export interface DirStat { rel_path: string; files: number; bytes: number }

export interface FolderAnalysis {
  job_id: string;
  source_root: string;
  dest_prefix: string;
  encrypted: boolean;
  files: number;
  dirs: number;
  empty_dirs: number;
  total_bytes: number;
  zero_byte_files: number;
  by_ext: ExtStat[];
  hidden: { files: number; bytes: number; top_dirs: DirStat[] };
  top_dirs: DirStat[];
  largest_files: { rel_path: string; size: number }[];
  excluded: Record<ExcludeReason, number>;
  windows_unsafe_names: number;
  deepest: { rel_path: string; depth: number } | null;
  longest_rel_bytes: number;
  largest_file_bytes: number;
  warnings: FolderWarning[];
}

export type FolderAnalyzeEvent =
  | { kind: "started"; job_id: string }
  | { kind: "progress"; job_id: string; files: number; dirs: number; bytes: number; current_dir: string };

export type FolderRemoteEvent = { kind: "progress"; job_id: string; listed: number };

export interface FolderRemoteCheck {
  existing: number;
  /** Sum of remote (destination) sizes; ciphertext sizes on encrypted buckets. */
  existing_bytes: number;
  listed: number;
  versioning: "enabled" | "disabled" | "unknown";
  key_too_long: number;
}

export interface FolderItem {
  seq: number;
  rel_path: string;
  key: string;
  kind: "file" | "dir" | "symlink" | "special";
  size: number;
  mtime_ms: number;
  state: FolderItemState;
  exclude_reason: ExcludeReason | null;
  link_target: string | null;
  remote_exists: boolean;
  remote_size: number | null;
  /** Milliseconds. */
  remote_mtime: number | null;
  drift: "changed" | "changed_during_upload" | null;
  error: string | null;
  attempts: number;
}

export interface FolderItemPage { items: FolderItem[]; total: number }

export interface FolderJob {
  id: string;
  account_id: string;
  bucket: string;
  source_root: string;
  dest_prefix: string;
  state: FolderJobState;
  waiting: "network" | "rate_limited" | null;
  /** "user" or a plain error code; the wire error JSON is in `error`. */
  pause_reason: string | null;
  overwrite_policy: OverwritePolicy | null;
  total_files: number;
  total_bytes: number;
  bytes_done: number;
  /** Item counts by state; `done` counts uploaded files only (no folder markers). */
  counts: Record<FolderItemState, number>;
  drift: number;
  appeared_after_check: number;
  inflight: { rel_path: string; bytes_done: number; bytes_total: number | null }[];
  error: string | null;
  created_at: number;
  started_at: number | null;
  finished_at: number | null;
  updated_at: number;
}

export interface FolderJobChangedEvent { job_id: string; state: FolderJobState }
