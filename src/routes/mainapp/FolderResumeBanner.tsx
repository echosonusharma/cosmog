import { createMemo, Show } from "solid-js";
import { folderUploadPause } from "../../api/folderUpload";
import { folderJobs, isJobMoving, refreshFolderJobs } from "../../state/folderJobs";
import { toast } from "../../state/toast";
import { IconPause, IconX } from "../../utils/icons";
import { t, tp } from "../../i18n";
import { jobName } from "../transfers/folderJobHelpers";
import { filesLeft, resumedFolderJobs, setResumedFolderJobs } from "./folderJobNotify";

// In-app "Resuming folder upload" notice with Pause; native notifications have no actions on desktop.
export function FolderResumeBanner() {
  const jobs = createMemo(() => folderJobs.filter((j) => resumedFolderJobs().includes(j.id) && isJobMoving(j)));
  const dismiss = () => setResumedFolderJobs([]);
  async function pause() {
    for (const j of jobs()) await folderUploadPause(j.id).catch((e) => toast.err(e));
    dismiss();
    void refreshFolderJobs();
  }
  return (
    <Show when={jobs()[0]}>
      {(j) => (
        <div class="fu-resume-banner" role="status">
          <span class="fu-resume-text truncate">{tp("folderUpload.resume.banner", filesLeft(j()), { name: jobName(j()) })}</span>
          <button class="btn-secondary fu-small-btn" onClick={pause}>
            <IconPause size={12} /> {t("folderUpload.job.actions.pause")}
          </button>
          <button class="icon-btn" title={t("common.close")} aria-label={t("common.close")} onClick={dismiss}><IconX size={13} /></button>
        </div>
      )}
    </Show>
  );
}
