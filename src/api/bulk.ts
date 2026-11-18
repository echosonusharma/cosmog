import { invoke, Channel } from "@tauri-apps/api/core";
import type { TransferEvent } from "../types";

export interface BulkDeleteResult {
  deleted: number;
  failed: number;
  found: number;
  /** First 100 per-key failures; `failed` counts all. */
  errors: Array<{ key: string; message: string }>;
  canceled: boolean;
  /** Wire error of a hard failure; counts cover the work before it. */
  error: string | null;
}

// Progress: bytes_done = deleted, bytes_total = keys found so far.
export function deleteFolder(
  accountId: string,
  bucket: string,
  prefix: string,
  onEvent: (e: TransferEvent) => void,
): Promise<BulkDeleteResult> {
  const ch = new Channel<TransferEvent>();
  ch.onmessage = onEvent;
  return invoke("delete_folder_cmd", { accountId, bucket, prefix, onEvent: ch });
}

export const cancelBulkOp = (opId: string): Promise<void> =>
  invoke("cancel_bulk_op", { opId });
