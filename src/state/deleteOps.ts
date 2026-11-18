import { createStore, produce } from "solid-js/store";
import { cancelBulkOp } from "../api/bulk";

// Live delete ops shown in the bottom bar and used to dim rows being deleted.
export type DeleteOpState = "starting" | "running" | "canceling" | "done" | "failed" | "canceled";

export interface DeleteOp {
  id: string;
  kind: "folder" | "keys";
  accountId: string;
  bucket: string;
  /** Folder prefix for folder ops; empty for key ops. */
  prefix: string;
  deleted: number;
  failed: number;
  total: number | null;
  /** Folder ops: keys listed so far. */
  found: number | null;
  /** Key ops in a single batch have nothing to stop between. */
  cancelable: boolean;
  state: DeleteOpState;
}

const REMOVE_AFTER_MS = 2500;
const ACTIVE: DeleteOpState[] = ["starting", "running", "canceling"];

const [ops, setOps] = createStore<DeleteOp[]>([]);
export { ops as deleteOps };

// Non-reactive side data: key sets for key ops, backend op ids for folder ops.
const keySets = new Map<string, Set<string>>();
const backendIds = new Map<string, string>();
const cancelFlags = new Set<string>();

let seq = 0;
export function startDeleteOp(init: Pick<DeleteOp, "kind" | "accountId" | "bucket" | "prefix" | "cancelable"> & { keys?: string[] }): string {
  const id = `del-${++seq}`;
  if (init.keys) keySets.set(id, new Set(init.keys));
  setOps(ops.length, {
    id, kind: init.kind, accountId: init.accountId, bucket: init.bucket, prefix: init.prefix,
    cancelable: init.cancelable, deleted: 0, failed: 0, found: null, total: init.keys ? init.keys.length : null, state: "starting",
  });
  return id;
}

export function updateDeleteOp(id: string, patch: Partial<Pick<DeleteOp, "deleted" | "failed" | "found" | "state">>) {
  setOps(produce((list) => {
    const op = list.find((o) => o.id === id);
    // Keep "canceling" until the op actually ends.
    if (op && ACTIVE.includes(op.state)) {
      const state = op.state === "canceling" && patch.state === "running" ? "canceling" : patch.state;
      Object.assign(op, patch, state ? { state } : {});
    }
  }));
}

/** Marks the op finished and drops its row after a short moment. */
export function finishDeleteOp(id: string, state: "done" | "failed" | "canceled") {
  updateDeleteOp(id, { state });
  setTimeout(() => {
    setOps((list) => list.filter((o) => o.id !== id));
    keySets.delete(id);
    backendIds.delete(id);
    cancelFlags.delete(id);
  }, REMOVE_AFTER_MS);
}

/** Folder ops learn the backend id from Started; a cancel requested earlier fires then. */
export function setBackendId(id: string, backendId: string) {
  backendIds.set(id, backendId);
  if (cancelFlags.has(id)) void cancelBulkOp(backendId).catch(() => {});
}

export const isCancelRequested = (id: string) => cancelFlags.has(id);

export function cancelDeleteOp(id: string) {
  cancelFlags.add(id);
  updateDeleteOp(id, { state: "canceling" });
  const backendId = backendIds.get(id);
  if (backendId) void cancelBulkOp(backendId).catch(() => {});
}

export const isDeleteOpActive = (op: DeleteOp) => ACTIVE.includes(op.state);

/** True while a running op covers `key` (a file key or a folder prefix). */
export function isKeyDeleting(accountId: string, bucket: string, key: string): boolean {
  for (const op of ops) {
    if (!isDeleteOpActive(op) || op.accountId !== accountId || op.bucket !== bucket) continue;
    if (op.kind === "folder" ? key.startsWith(op.prefix) : keySets.get(op.id)?.has(key)) return true;
  }
  return false;
}
