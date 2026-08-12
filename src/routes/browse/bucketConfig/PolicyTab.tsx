import { createSignal, createResource, createEffect, Show } from "solid-js";
import { getBucketPolicy, putBucketPolicy, deleteBucketPolicy } from "../../../api/buckets";
import { CodeEditor } from "../../../utils/CodeEditor";
import { resolvedTheme } from "../../../state/theme";
import { toast } from "../../../state/toast";
import { classifyBucketError, deniedMessage } from "./errors";
import { capWarning } from "./providerCaps";
import { DocLink } from "./DocLink";

const EMPTY_POLICY = "";

export function PolicyTab(props: {
  accountId: string;
  bucket: string;
  providerId: string;
  providerLabel: string;
  onChanged: () => void;
}) {
  const [loaded, { refetch }] = createResource<
    { policy: string; denied: boolean; unsupported: boolean },
    { a: string; b: string }
  >(
    () => ({ a: props.accountId, b: props.bucket }),
    async ({ a, b }) => {
      try {
        const p = await getBucketPolicy(a, b);
        return { policy: p ?? EMPTY_POLICY, denied: false, unsupported: false };
      } catch (e) {
        const kind = classifyBucketError(e);
        if (kind === "unsupported") return { policy: EMPTY_POLICY, denied: false, unsupported: true };
        if (kind === "denied") return { policy: EMPTY_POLICY, denied: true, unsupported: false };
        throw e;
      }
    },
  );

  // Local editable copy; seeded whenever the fetched policy changes.
  const [content, setContent] = createSignal(EMPTY_POLICY);
  const [seeded, setSeeded] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");

  function prettify(s: string): string {
    try { return JSON.stringify(JSON.parse(s), null, 2); } catch { return s; }
  }

  // Seed the editor whenever a fresh policy loads successfully. `seeded` holds
  // the raw server value (the change key); the editor shows a pretty-printed
  // copy so incoming minified policies are auto-formatted on load.
  createEffect(() => {
    const l = loaded();
    if (!l || l.unsupported || l.denied) return;
    if (seeded() !== l.policy) {
      setSeeded(l.policy);
      setContent(prettify(l.policy));
    }
  });

  async function handleSave() {
    setErr("");
    const raw = content().trim();
    if (!raw) { setErr("Policy is empty. Use Delete to remove the policy, or paste valid JSON."); return; }
    try {
      JSON.parse(raw);
    } catch (e) {
      setErr(`Invalid JSON: ${(e as Error).message}`);
      return;
    }
    setBusy(true);
    try {
      await putBucketPolicy(props.accountId, props.bucket, raw);
      toast.ok("Policy saved", `Bucket policy for "${props.bucket}" updated`);
      setSeeded(null);
      await refetch();
      props.onChanged();
    } catch (e) {
      setErr(errText(e, "put"));
    } finally {
      setBusy(false);
    }
  }

  async function handleDelete() {
    setErr("");
    setBusy(true);
    try {
      await deleteBucketPolicy(props.accountId, props.bucket);
      toast.ok("Policy removed", `Bucket policy for "${props.bucket}" deleted`);
      setContent(EMPTY_POLICY);
      setSeeded(null);
      await refetch();
      props.onChanged();
    } catch (e) {
      setErr(errText(e, "put"));
    } finally {
      setBusy(false);
    }
  }

  function handleReset() {
    setErr("");
    setContent(prettify(seeded() ?? EMPTY_POLICY));
  }

  function errText(e: unknown, op: "get" | "put"): string {
    const kind = classifyBucketError(e);
    if (kind === "denied") return deniedMessage("policy", op);
    if (kind === "unsupported") return "Not supported by this provider";
    return (e as Error)?.message ?? String(e);
  }

  const snap = () => loaded.latest ?? loaded();

  return (
    <div class="bcfg-tab">
      <Show when={!(loaded.loading && loaded.latest == null)} fallback={<div class="bcfg-loading"><span class="spinner spinner-lg" /><span>Loading policy…</span></div>}>
        <Show when={!(loaded.error && loaded.latest == null)} fallback={<div class="status-msg err">{errText(loaded.error, "get")}</div>}>
          <Show when={snap()!.unsupported}>
            <div class="status-msg warn">Not supported by this provider</div>
          </Show>
          <Show when={snap()!.denied}>
            <div class="status-msg err">{deniedMessage("policy", "get")}</div>
          </Show>

          <Show when={!snap()!.unsupported && !snap()!.denied}>
            <Show when={capWarning(props.providerId, props.providerLabel, "policy")}>
              {(w) => <div class="status-msg warn bcfg-provider-warn">{w()}</div>}
            </Show>
            <div class="modal-sub bcfg-hint">
              Raw JSON bucket policy. An empty editor means no policy is set. Editing here overwrites the
              existing policy on Save. <DocLink providerId={props.providerId} tab="policy" />
            </div>

            <div class="bcfg-editor">
              <CodeEditor
                value={content()}
                ext="json"
                dark={resolvedTheme() === "dark"}
                gutters={true}
                onChange={setContent}
              />
            </div>

            <Show when={err()}><div class="status-msg err bcfg-status">{err()}</div></Show>

            <div class="btn-row bcfg-actions">
              <div class="bcfg-actions-grp">
                <button class="btn-ghost text-xs" disabled={busy()} onClick={() => setContent(prettify(content()))}>
                  Format
                </button>
                <button class="btn-secondary text-xs" disabled={busy()} onClick={handleReset}>
                  Reset
                </button>
              </div>
              <div class="bcfg-actions-grp">
                <button class="enc-disable-btn bcfg-danger-btn" disabled={busy()} onClick={handleDelete}>
                  {busy() ? "Working…" : "Delete"}
                </button>
                <button class="btn-primary text-xs" disabled={busy()} onClick={handleSave}>
                  {busy() ? "Saving…" : "Save"}
                </button>
              </div>
            </div>
          </Show>
        </Show>
      </Show>
    </div>
  );
}
