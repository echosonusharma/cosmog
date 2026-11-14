import { createSignal } from "solid-js";
import { accounts } from "../../state/app";
import { onFolderJobsUpdate, isJobFinished, isJobMoving, startedHere } from "../../state/folderJobs";
import { notify, notifId, CHANNEL_ALERTS, CHANNEL_EVENTS, CHANNEL_PROGRESS } from "../../utils/notify";
import { errMsg } from "../../utils/errors";
import { t, tp } from "../../i18n";
import type { FolderJob } from "../../types";
import { jobName } from "../transfers/folderJobHelpers";

// Jobs resumed at startup that this session did not start; drives the in-app banner.
const [resumed, setResumed] = createSignal<string[]>([]);
export { resumed as resumedFolderJobs, setResumed as setResumedFolderJobs };

export const filesLeft = (j: FolderJob) => (j.counts.pending ?? 0) + (j.counts.uploading ?? 0);

const notifiedStart = new Set<string>();
const notifiedEnd = new Set<string>();
let watching = false;

function send(j: FolderJob, title: string, body: string, channelId: string) {
  const acct = accounts().find((a) => a.id === j.account_id);
  notify(title, body, { id: notifId(`folder:${j.id}`), summary: acct?.name, channelId, autoCancel: true });
}

// Exactly two native notifications per job: one at start (or resume), one at its terminal state.
export function watchFolderJobNotifications() {
  if (watching) return;
  watching = true;
  onFolderJobsUpdate((list, first) => {
    const account = (j: FolderJob) => accounts().find((a) => a.id === j.account_id)?.name ?? t("shell.xfer.accountFallback");
    const p = (j: FolderJob) => ({ name: jobName(j), bucket: j.bucket, account: account(j) });
    if (first) {
      const res: string[] = [];
      for (const j of list) {
        if (isJobFinished(j)) { notifiedEnd.add(j.id); continue; }
        notifiedStart.add(j.id);
        if (isJobMoving(j) && !startedHere(j.id)) {
          res.push(j.id);
          send(j, t("folderUpload.notify.resumed", p(j)), tp("folderUpload.notify.resumedBody", filesLeft(j)), CHANNEL_PROGRESS);
        }
      }
      setResumed(res);
      return;
    }
    for (const j of list) {
      if (isJobMoving(j) && !notifiedStart.has(j.id)) {
        notifiedStart.add(j.id);
        send(j, t("folderUpload.notify.start", p(j)), tp("folderUpload.notify.startBody", j.total_files, p(j)), CHANNEL_PROGRESS);
      }
      if (!isJobFinished(j) || notifiedEnd.has(j.id)) continue;
      notifiedEnd.add(j.id);
      notifiedStart.add(j.id);
      switch (j.state) {
        case "completed":
          send(j, t("folderUpload.notify.done", p(j)), t("folderUpload.notify.doneBody", p(j)), CHANNEL_EVENTS); break;
        case "completed_with_errors":
          // Errors can be missing or late-excluded files with no failures; avoid "0 files failed".
          send(j, t("folderUpload.notify.doneErrors", p(j)), (j.counts.failed ?? 0) > 0
            ? tp("folderUpload.notify.doneErrorsBody", j.counts.failed ?? 0)
            : t("folderUpload.notify.doneErrorsBodyOther"), CHANNEL_ALERTS); break;
        case "canceled":
          send(j, t("folderUpload.notify.canceled", p(j)), t("folderUpload.notify.canceledBody", p(j)), CHANNEL_EVENTS); break;
        default:
          send(j, t("folderUpload.notify.failed", p(j)), t("folderUpload.notify.failedBody", { ...p(j), error: errMsg(j.error) }), CHANNEL_ALERTS);
      }
    }
  });
}
