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
  | { kind: "multipart_initiated"; transfer_id: string; upload_id: string }
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
