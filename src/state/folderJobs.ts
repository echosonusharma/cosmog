import { createMemo, createRoot, createSignal } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { listen } from "@tauri-apps/api/event";
import { folderUploadListJobs } from "../api/folderUpload";
import { isDesktopShell } from "../utils/platform";
import { notifyLanded } from "./landed";
import type { FolderJob, FolderJobChangedEvent, FolderJobState } from "../types";

// One shared poller + event listener for folder upload jobs (desktop only).
const RUNNING_POLL_MS = 1000;
const IDLE_POLL_MS = 5000;

export const ACTIVE_JOB_STATES: FolderJobState[] = ["queued", "running", "paused"];
export const FINISHED_JOB_STATES: FolderJobState[] = ["completed", "completed_with_errors", "canceled", "failed"];
export const isJobFinished = (j: FolderJob) => FINISHED_JOB_STATES.includes(j.state);
export const isJobMoving = (j: FolderJob) => j.state === "running" || j.state === "queued";

const [jobs, setJobs] = createStore<FolderJob[]>([]);
const [loaded, setLoaded] = createSignal(false);
const [supported, setSupported] = createSignal(isDesktopShell);

export { jobs as folderJobs, loaded as folderJobsLoaded, supported as folderUploadSupported };

/** Approved jobs only; analyzing/analyzed belong to an open modal. */
const visible = createRoot(() => createMemo(() =>
  jobs.filter((j) => j.state !== "analyzing" && j.state !== "analyzed")));
export const visibleFolderJobs = visible;
export const movingFolderJobs = createRoot(() => createMemo(() => jobs.filter(isJobMoving)));

// Jobs approved in this session; anything else running at first load was resumed at startup.
const sessionStarted = new Set<string>();
export const markJobStartedHere = (id: string) => { sessionStarted.add(id); };
export const startedHere = (id: string) => sessionStarted.has(id);

type Listener = (list: FolderJob[], first: boolean) => void;
const listeners: Listener[] = [];
export function onFolderJobsUpdate(fn: Listener) { listeners.push(fn); }

// Only a backend without the command (Tauri: "Command x not found") turns the poller off; other errors are transient.
const isMissingCommand = (e: unknown) =>
  /command (\S+ )?not found|unknown command/i.test(e instanceof Error ? e.message : String(e));

// Last seen done count and state per job, to tell browsers when new objects landed.
const lastSeen = new Map<string, { done: number; state: FolderJobState }>();
function announceLanded(list: FolderJob[]) {
  for (const j of list) {
    const prev = lastSeen.get(j.id);
    const done = j.counts.done ?? 0;
    lastSeen.set(j.id, { done, state: j.state });
    if (!prev) continue;
    const ended = !FINISHED_JOB_STATES.includes(prev.state) && FINISHED_JOB_STATES.includes(j.state);
    if (ended || done > prev.done) notifyLanded(j.account_id, j.bucket, j.dest_prefix, ended ? "end" : "job");
  }
}

let reqSeq = 0;
let appliedSeq = 0;
export async function refreshFolderJobs(): Promise<void> {
  if (!supported()) return;
  const seq = ++reqSeq;
  try {
    const list = await folderUploadListJobs();
    if (seq <= appliedSeq) return;
    appliedSeq = seq;
    list.sort((a, b) => b.created_at - a.created_at);
    announceLanded(list);
    setJobs(reconcile(list, { key: "id" }));
    const first = !loaded();
    setLoaded(true);
    for (const fn of listeners) fn(list, first);
  } catch (e) {
    if (isMissingCommand(e)) setSupported(false);
  }
}

let started = false;
/** Idempotent; called once by MainApp. */
export function startFolderJobsPoller() {
  if (started || !isDesktopShell) return;
  started = true;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const loop = async () => {
    clearTimeout(timer);
    await refreshFolderJobs();
    // An event-driven run may overlap the timer's: keep a single pending timer.
    clearTimeout(timer);
    if (!supported()) return;
    const fast = jobs.some((j) => j.state === "running" || j.state === "queued");
    timer = setTimeout(loop, fast ? RUNNING_POLL_MS : IDLE_POLL_MS);
  };
  void loop();
  listen<FolderJobChangedEvent>("folder-upload-changed", () => { void loop(); }).catch(() => {});
}
