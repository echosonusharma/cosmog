import { invoke } from "@tauri-apps/api/core";
import type { NightWatch, WatchStatus } from "../types";

export interface AddWatchInput {
  account_id: string;
  bucket: string;
  local_dir: string;
  tree_uri?: string | null;
  key_prefix: string;
  ignore_file: string | null;
  full_scan_secs: number;
  delete_policy: string;
}

export type UpdateWatchInput = Partial<{
  key_prefix: string;
  ignore_file: string | null;
  full_scan_secs: number;
  delete_policy: string;
}>;

export const nwListWatches = (): Promise<NightWatch[]> =>
  invoke("nw_list_watches");

export const nwAddWatch = (input: AddWatchInput): Promise<NightWatch> =>
  invoke("nw_add_watch", {
    accountId: input.account_id,
    bucket: input.bucket,
    localDir: input.local_dir,
    treeUri: input.tree_uri ?? null,
    keyPrefix: input.key_prefix,
    ignoreFile: input.ignore_file,
    fullScanSecs: input.full_scan_secs,
    deletePolicy: input.delete_policy,
  });

export const nwUpdateWatch = (id: string, patch: UpdateWatchInput): Promise<NightWatch> =>
  invoke("nw_update_watch", {
    id,
    keyPrefix: patch.key_prefix,
    ignoreFile: patch.ignore_file,
    fullScanSecs: patch.full_scan_secs,
    deletePolicy: patch.delete_policy,
  });

export const nwDeleteWatch = (id: string): Promise<void> =>
  invoke("nw_delete_watch", { id });

export const nwSetWatchEnabled = (id: string, enabled: boolean): Promise<void> =>
  invoke("nw_set_watch_enabled", { id, enabled });

export const nwGetStatus = (): Promise<WatchStatus[]> =>
  invoke("nw_get_status");

export const nwPickTree = (): Promise<{ uri: string; display_name: string }> =>
  invoke("nw_pick_tree");

export const nwQuitBackground = (): Promise<void> =>
  invoke("nw_quit_background");
