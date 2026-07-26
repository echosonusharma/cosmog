import { createSignal, createResource, Show } from "solid-js";
import { getBucketVersioning, putBucketVersioning } from "../../../api/buckets";
import { toast } from "../../../state/toast";
import { classifyBucketError, deniedMessage } from "./errors";
import { capWarning } from "./providerCaps";
import { DocLink } from "./DocLink";

export function VersioningTab(props: {
  accountId: string;
  bucket: string;
  providerId: string;
  providerLabel: string;
  onChanged: () => void;
}) {
  const [loaded, { refetch }] = createResource<
    { enabled: boolean; denied: boolean; unsupported: boolean },
    { a: string; b: string }
  >(
    () => ({ a: props.accountId, b: props.bucket }),
    async ({ a, b }) => {
      try {
        const on = await getBucketVersioning(a, b);
        return { enabled: on, denied: false, unsupported: false };
      } catch (e) {
        const kind = classifyBucketError(e);
        if (kind === "unsupported") return { enabled: false, denied: false, unsupported: true };
        if (kind === "denied") return { enabled: false, denied: true, unsupported: false };
        throw e;
      }
    },
  );

  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");

  async function setEnabled(next: boolean) {
    if (busy()) return;
    setErr("");
    setBusy(true);
    try {
      await putBucketVersioning(props.accountId, props.bucket, next);
      toast.ok(
        next ? "Versioning enabled" : "Versioning suspended",
        `Versioning for "${props.bucket}" ${next ? "enabled" : "suspended"}`,
      );
      await refetch();
      props.onChanged();
    } catch (e) {
      setErr(errText(e, "put"));
    } finally {
      setBusy(false);
    }
  }

  function errText(e: unknown, op: "get" | "put"): string {
    const kind = classifyBucketError(e);
    if (kind === "denied") return deniedMessage("versioning", op);
    if (kind === "unsupported") return "Not supported by this provider";
    return (e as Error)?.message ?? String(e);
  }

  return (
    <div class="bcfg-tab">
      <Show when={!loaded.loading} fallback={<div class="bcfg-loading"><span class="spinner spinner-lg" /><span>Loading versioning…</span></div>}>
        <Show when={!loaded.error} fallback={<div class="status-msg err">{errText(loaded.error, "get")}</div>}>
          <Show when={loaded()!.unsupported}>
            <div class="status-msg warn">Not supported by this provider</div>
          </Show>
          <Show when={loaded()!.denied}>
            <div class="status-msg err">{deniedMessage("versioning", "get")}</div>
          </Show>

          <Show when={!loaded()!.unsupported && !loaded()!.denied}>
            <Show when={capWarning(props.providerId, props.providerLabel, "versioning")}>
              {(w) => <div class="status-msg warn bcfg-provider-warn">{w()}</div>}
            </Show>
            <div class="modal-sub bcfg-hint">
              Versioning keeps previous copies of an object when it is overwritten or deleted. Once enabled,
              S3 versioning can be <strong>suspended</strong> but never fully removed, and existing versions
              are retained. <DocLink providerId={props.providerId} tab="versioning" />
            </div>

            <div class="bcfg-versioning-row">
              <div class="bcfg-versioning-state">
                Current state:{" "}
                <strong classList={{ "bcfg-on": loaded()!.enabled, "bcfg-off": !loaded()!.enabled }}>
                  {loaded()!.enabled ? "Enabled" : "Suspended"}
                </strong>
              </div>
              <button
                type="button"
                class="bcfg-toggle"
                classList={{ on: loaded()!.enabled }}
                role="switch"
                aria-checked={loaded()!.enabled}
                disabled={busy()}
                onClick={() => setEnabled(!loaded()!.enabled)}
              >
                <span class="bcfg-toggle-knob" />
              </button>
            </div>

            <Show when={err()}><div class="status-msg err bcfg-status">{err()}</div></Show>
          </Show>
        </Show>
      </Show>
    </div>
  );
}
