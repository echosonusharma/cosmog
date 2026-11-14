import { createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import {
  folderUploadAnalyze, folderUploadApprove, folderUploadCancel, folderUploadCheckRemote, folderUploadGetAnalysis,
} from "../../../api/folderUpload";
import { markJobStartedHere, refreshFolderJobs } from "../../../state/folderJobs";
import { folderDestPrefixSchema, parseSchema } from "../../../validation";
import type { FolderAnalysis, FolderRemoteCheck, OverwritePolicy } from "../../../types";

export type Phase = "scanning" | "summary" | "error";
export interface ScanProgress { files: number; dirs: number; bytes: number; current_dir: string }
export interface CheckState {
  status: "idle" | "checking" | "done" | "error";
  dest: string;
  listed: number;
  result: FolderRemoteCheck | null;
  error: unknown;
}

const DEST_DEBOUNCE_MS = 400;

// State machine of the folder upload modal: analyze, remote check per destination, approve.
export function createFolderFlow(opts: { accountId: string; bucket: string; localRoot: string; initialDest: string }) {
  const [phase, setPhase] = createSignal<Phase>("scanning");
  const [progress, setProgress] = createSignal<ScanProgress>({ files: 0, dirs: 0, bytes: 0, current_dir: "" });
  const [analysis, setAnalysis] = createSignal<FolderAnalysis | null>(null);
  const [error, setError] = createSignal<unknown>(null);
  const [destInput, setDestInput] = createSignal(opts.initialDest);
  const [check, setCheck] = createSignal<CheckState>({ status: "idle", dest: "", listed: 0, result: null, error: null });
  const [policy, setPolicy] = createSignal<OverwritePolicy | null>(null);
  const [approving, setApproving] = createSignal(false);
  const [approveError, setApproveError] = createSignal<unknown>(null);
  // Bumped after each completed check; reloads open item lists.
  const [checkGen, setCheckGen] = createSignal(0);
  const startedAt = Date.now();

  const destParsed = createMemo(() => parseSchema(folderDestPrefixSchema, destInput()));
  const dest = () => { const p = destParsed(); return p.success ? p.data : null; };

  let jobId: string | null = null;
  let disposed = false;
  let approved = false;
  const cancelJob = (id: string) => { folderUploadCancel(id).catch(() => {}); };

  folderUploadAnalyze(opts.accountId, opts.bucket, opts.localRoot, dest() ?? "", (e) => {
    if (e.kind === "started") {
      jobId = e.job_id;
      if (disposed) cancelJob(e.job_id);
    } else if (!disposed) {
      setProgress({ files: e.files, dirs: e.dirs, bytes: e.bytes, current_dir: e.current_dir });
    }
  }).then((a) => {
    jobId = a.job_id;
    if (disposed) { cancelJob(a.job_id); return; }
    setAnalysis(a);
    setPhase("summary");
  }, (e) => {
    if (disposed) return;
    setError(e);
    setPhase("error");
  });

  let checkSeq = 0;
  async function runCheck(d: string) {
    const id = jobId;
    if (!id || disposed) return;
    const seq = ++checkSeq;
    setPolicy(null);
    setCheck({ status: "checking", dest: d, listed: 0, result: null, error: null });
    try {
      const result = await folderUploadCheckRemote(id, d, (e) => {
        if (seq === checkSeq) setCheck((c) => ({ ...c, listed: e.listed }));
      });
      // Exact files/bytes/key_too_long for this destination come from the backend.
      const fresh = await folderUploadGetAnalysis(id);
      if (seq !== checkSeq || disposed) return;
      setAnalysis(fresh);
      setCheck({ status: "done", dest: d, listed: result.listed, result, error: null });
      setCheckGen((n) => n + 1);
    } catch (e) {
      // Silent only when superseded by a newer check or discarded; any other error (incl. canceled) is shown.
      if (seq !== checkSeq || disposed) return;
      setCheck({ status: "error", dest: d, listed: 0, result: null, error: e });
    }
  }

  // Re-run the check whenever the (valid) destination settles on a new value.
  createEffect(() => {
    if (phase() !== "summary") return;
    const d = dest();
    if (d === null || (d === check().dest && check().status !== "idle")) return;
    const first = check().status === "idle";
    const timer = setTimeout(() => runCheck(d), first ? 0 : DEST_DEBOUNCE_MS);
    onCleanup(() => clearTimeout(timer));
  });

  const checkCurrent = () => check().status === "done" && check().dest === dest();
  const existing = () => (checkCurrent() ? check().result?.existing ?? 0 : 0);
  const canApprove = () =>
    !approving() && phase() === "summary" && dest() !== null && checkCurrent() && (existing() === 0 || policy() !== null);

  async function approve(): Promise<boolean> {
    if (!jobId || disposed || !canApprove()) return false;
    setApproving(true); setApproveError(null);
    try {
      // Skip even when nothing exists, so keys appearing after the check are never silently overwritten.
      const job = await folderUploadApprove(jobId, existing() > 0 ? policy() : "skip", true);
      approved = true;
      markJobStartedHere(job.id);
      void refreshFolderJobs();
      return true;
    } catch (e) {
      setApproveError(e);
      return false;
    } finally { setApproving(false); }
  }

  /** Discards the job unless it was approved; safe before the job id is known. */
  function discard() {
    if (disposed) return;
    disposed = true;
    checkSeq++;
    if (!approved && jobId) cancelJob(jobId);
  }

  return {
    phase, progress, analysis, error, destInput, setDestInput, destParsed, dest,
    check, retryCheck: () => { const d = dest(); if (d !== null) void runCheck(d); },
    policy, setPolicy, approving, approveError, approve, canApprove, checkCurrent, existing, checkGen,
    discard, elapsedMs: () => Date.now() - startedAt, jobId: () => jobId,
  };
}

export type FolderFlow = ReturnType<typeof createFolderFlow>;
