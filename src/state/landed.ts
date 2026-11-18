import { createSignal } from "solid-js";

// Uploads that finished, so open browsers can show the new objects.
export interface Landed { accountId: string; bucket: string; prefix: string; seq: number; kind: LandedKind }
/** file: one upload done. job: a running job made progress. end: a job ended, worth a live re-list. */
export type LandedKind = "file" | "job" | "end";

const [landed, setLanded] = createSignal<Landed | null>(null);
export { landed as uploadsLanded };

let seq = 0;
/** `prefix`: folder the objects landed under. */
export function notifyLanded(accountId: string, bucket: string, prefix: string, kind: LandedKind) {
  setLanded({ accountId, bucket, prefix, seq: ++seq, kind });
}
