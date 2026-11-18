import { invoke, Channel } from "@tauri-apps/api/core";
import type {
  FolderAnalysis, FolderAnalyzeEvent, FolderItemFilter, FolderItemPage, FolderJob,
  FolderRemoteCheck, FolderRemoteEvent, LocalPathKind, OverwritePolicy,
} from "../types";

function channel<T>(cb: (e: T) => void): Channel<T> {
  const ch = new Channel<T>();
  ch.onmessage = cb;
  return ch;
}

export const localPathKinds = (paths: string[]): Promise<LocalPathKind[]> =>
  invoke("local_path_kinds", { paths });

/** Android only: SAF folder picker; rejects with code "canceled" when dismissed. */
export const folderUploadPickTree = (): Promise<{ uri: string; display_name: string }> =>
  invoke("fu_pick_tree");

/** Android: a picked tree that will not be used; the backend drops its grant if nothing else needs it. */
export const folderUploadReleasePick = (uri: string): Promise<void> =>
  invoke("fu_release_pick", { uri });

export const folderUploadAnalyze = (
  accountId: string,
  bucket: string,
  localRoot: string,
  destPrefix: string,
  cb: (e: FolderAnalyzeEvent) => void,
): Promise<FolderAnalysis> =>
  invoke("folder_upload_analyze", { accountId, bucket, localRoot, destPrefix, onEvent: channel(cb) });

export const folderUploadCheckRemote = (
  jobId: string,
  destPrefix: string,
  cb: (e: FolderRemoteEvent) => void,
): Promise<FolderRemoteCheck> =>
  invoke("folder_upload_check_remote", { jobId, destPrefix, onEvent: channel(cb) });

export const folderUploadGetAnalysis = (jobId: string): Promise<FolderAnalysis> =>
  invoke("folder_upload_get_analysis", { jobId });

export const folderUploadListItems = (
  jobId: string,
  filter: FolderItemFilter,
  offset: number,
  limit: number,
): Promise<FolderItemPage> =>
  invoke("folder_upload_list_items", { jobId, filter, offset, limit });

export const folderUploadApprove = (
  jobId: string,
  overwritePolicy: OverwritePolicy | null,
  createDirMarkers: boolean,
): Promise<FolderJob> =>
  invoke("folder_upload_approve", { jobId, overwritePolicy: overwritePolicy ?? null, createDirMarkers });

export const folderUploadListJobs = (): Promise<FolderJob[]> =>
  invoke("folder_upload_list_jobs");

export const folderUploadGetJob = (jobId: string): Promise<FolderJob> =>
  invoke("folder_upload_get_job", { jobId });

export const folderUploadPause = (jobId: string): Promise<void> =>
  invoke("folder_upload_pause", { jobId });

export const folderUploadResume = (jobId: string): Promise<void> =>
  invoke("folder_upload_resume", { jobId });

export const folderUploadCancel = (jobId: string): Promise<void> =>
  invoke("folder_upload_cancel", { jobId });

export const folderUploadRetryFailed = (jobId: string): Promise<number> =>
  invoke("folder_upload_retry_failed", { jobId });

export const folderUploadRemove = (jobId: string): Promise<void> =>
  invoke("folder_upload_remove", { jobId });

export const folderUploadClearFinished = (): Promise<number> =>
  invoke("folder_upload_clear_finished");

export const appCloseQuits = (): Promise<boolean> =>
  invoke("app_close_quits");
