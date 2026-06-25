import { invoke, Channel } from "@tauri-apps/api/core";
import type { Transfer, TransferStatus, TransferEvent } from "../types";

export interface EnqueueResult {
  transfer_id: string;
}

export type TransferEventCb = (e: TransferEvent) => void;

function makeChannel(cb?: TransferEventCb): Channel<TransferEvent> {
  const ch = new Channel<TransferEvent>();
  if (cb) ch.onmessage = cb;
  else ch.onmessage = () => {};
  return ch;
}

export const listTransfers = (status?: TransferStatus): Promise<Transfer[]> =>
  invoke("list_transfers", { status: status ?? null });

export const cancelTransfer = (id: string): Promise<void> =>
  invoke("cancel_transfer", { id });

export const clearCompletedTransfers = (): Promise<number> =>
  invoke("clear_completed_transfers");

export const clearTransfer = (id: string): Promise<void> =>
  invoke("clear_transfer", { id });

export const retryTransfer = (id: string, cb?: TransferEventCb): Promise<EnqueueResult> =>
  invoke("retry_transfer", { id, onEvent: makeChannel(cb) });

export const enqueueUpload = (
  accountId: string,
  bucket: string,
  key: string,
  localPath: string,
  cb?: TransferEventCb,
): Promise<EnqueueResult> =>
  invoke("enqueue_upload", {
    accountId,
    bucket,
    key,
    localPath,
    options: null,
    onEvent: makeChannel(cb),
  });

export const enqueueDownload = (
  accountId: string,
  bucket: string,
  key: string,
  localPath: string,
  versionId?: string,
  cb?: TransferEventCb,
): Promise<EnqueueResult> =>
  invoke("enqueue_download", {
    accountId,
    bucket,
    key,
    localPath,
    versionId: versionId ?? null,
    onEvent: makeChannel(cb),
  });
