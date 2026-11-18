import { deleteFolder } from "../../api/bulk";
import { deleteObjects } from "../../api/objects";
import {
  finishDeleteOp, isCancelRequested, setBackendId, startDeleteOp, updateDeleteOp,
} from "../../state/deleteOps";

// S3 DeleteObjects limit per call.
const DELETE_BATCH = 1000;

export interface DeleteError { key: string; message: string }

export interface FolderDeleteOutcome {
  deleted: number;
  failed: number;
  firstError: DeleteError | null;
  canceled: boolean;
  /** Wire error of a hard failure mid-way; `deleted` counts what went before it. */
  error: string | null;
}

/** Backend streaming folder delete, mirrored into the delete ops store. Throws only if it never ran. */
export async function runFolderDelete(accountId: string, bucket: string, prefix: string): Promise<FolderDeleteOutcome> {
  const id = startDeleteOp({ kind: "folder", accountId, bucket, prefix, cancelable: true });
  try {
    const res = await deleteFolder(accountId, bucket, prefix, (e) => {
      if (e.kind === "started") {
        setBackendId(id, e.transfer_id);
        updateDeleteOp(id, { state: "running" });
      } else if (e.kind === "progress") {
        updateDeleteOp(id, { deleted: e.bytes_done, found: e.bytes_total, state: "running" });
      }
    });
    updateDeleteOp(id, { deleted: res.deleted, failed: res.failed, found: res.found });
    finishDeleteOp(id, res.canceled ? "canceled" : res.error || res.failed ? "failed" : "done");
    return {
      deleted: res.deleted, failed: res.failed, canceled: res.canceled, error: res.error,
      firstError: res.errors[0] ?? null,
    };
  } catch (e) {
    finishDeleteOp(id, "failed");
    throw e;
  }
}

export interface KeysDeleteOutcome {
  deleted: string[];
  errors: DeleteError[];
  canceled: boolean;
  /** Set when a batch call threw; earlier batches may have gone through. */
  error?: unknown;
}

/** Frontend-batched delete of explicit keys; cancel stops between batches. */
export async function runKeysDelete(accountId: string, bucket: string, keys: string[]): Promise<KeysDeleteOutcome> {
  const id = startDeleteOp({
    kind: "keys", accountId, bucket, prefix: "", keys, cancelable: keys.length > DELETE_BATCH,
  });
  const out: KeysDeleteOutcome = { deleted: [], errors: [], canceled: false };
  updateDeleteOp(id, { state: "running" });
  try {
    for (let i = 0; i < keys.length; i += DELETE_BATCH) {
      if (isCancelRequested(id)) { out.canceled = true; break; }
      const part = await deleteObjects(accountId, bucket, keys.slice(i, i + DELETE_BATCH));
      out.deleted.push(...part.deleted);
      out.errors.push(...part.errors);
      updateDeleteOp(id, { deleted: out.deleted.length, failed: out.errors.length });
    }
  } catch (e) {
    out.error = e;
  }
  finishDeleteOp(id, out.canceled ? "canceled" : out.error || out.errors.length ? "failed" : "done");
  return out;
}
