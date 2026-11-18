import { folderUploadReleasePick, folderUploadResume } from "../../api/folderUpload";
import { folderJobs, refreshFolderJobs } from "../../state/folderJobs";
import { toast } from "../../state/toast";
import { isAndroid } from "../../utils/platform";
import { isContentUri } from "../../utils/sourceName";
import { pickFolder } from "../browse/folderUpload/pickFolder";
import { t } from "../../i18n";
import type { FolderJob } from "../../types";

/** Android job paused because its folder grant is gone; picking the same tree restores it. */
export const needsRepick = (j: FolderJob) =>
  isAndroid && j.state === "paused" && j.pause_reason === "folder_source_unavailable" && isContentUri(j.source_root);

/** Jobs a fresh pick of `root` can resume (the backend returns the tree URI normalized, like source_root). */
export const repickable = (root: string) => folderJobs.filter((j) => needsRepick(j) && j.source_root === root);

/** Resumes every job waiting on `root`; returns them. */
export async function resumeRepicked(root: string): Promise<FolderJob[]> {
  const list = repickable(root);
  for (const j of list) {
    try { await folderUploadResume(j.id); } catch (e) { toast.err(e); }
  }
  if (list.length) {
    toast.info(t("folderUpload.android.accessRestored"));
    void refreshFolderJobs();
  }
  return list;
}

/** Resume button of a job that lost its folder: pick it again first. */
export async function repickAndResume(j: FolderJob): Promise<void> {
  const picked = await pickFolder();
  if (!picked) return;
  if (picked.root !== j.source_root) {
    void folderUploadReleasePick(picked.root).catch(() => {});
    toast.warn(t("folderUpload.android.wrongFolder"));
    return;
  }
  await resumeRepicked(picked.root);
}
