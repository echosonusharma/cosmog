import { invoke } from "@tauri-apps/api/core";
import type { RequestLog } from "../types";

export interface ListRequestLogsOpts {
  limit?: number;
  offset?: number;
  search?: string;
  /** "ok" | "error" */
  status?: string;
  operation?: string;
}

export function listRequestLogs(opts: ListRequestLogsOpts = {}): Promise<RequestLog[]> {
  return invoke("list_request_logs", {
    limit: opts.limit ?? null,
    offset: opts.offset ?? null,
    search: opts.search ?? null,
    status: opts.status ?? null,
    operation: opts.operation ?? null,
  });
}

export function countRequestLogs(opts: Omit<ListRequestLogsOpts, "limit" | "offset"> = {}): Promise<number> {
  return invoke("count_request_logs", {
    search: opts.search ?? null,
    status: opts.status ?? null,
    operation: opts.operation ?? null,
  });
}

export function clearRequestLogs(): Promise<void> {
  return invoke("clear_request_logs");
}

export function purgeOldRequestLogs(): Promise<number> {
  return invoke("purge_old_request_logs");
}
