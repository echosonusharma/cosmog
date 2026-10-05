import { createSignal, createResource, Show } from "solid-js";
import { getBucketVersioning, putBucketVersioning } from "../../../api/buckets";
import { toast } from "../../../state/toast";
import { classifyBucketError, deniedMessage } from "./errors";
import { capWarning } from "./providerCaps";
import { DocLink } from "./DocLink";
import Spinner from "../../../utils/Spinner";
import { errMsg } from "../../../utils/errors";
import { t } from "../../../i18n";

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
        t(next ? "bucketconfig.versioning.enabledTitle" : "bucketconfig.versioning.suspendedTitle"),
        t(next ? "bucketconfig.versioning.enabledDetail" : "bucketconfig.versioning.suspendedDetail", { bucket: props.bucket }),
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
    if (kind === "unsupported") return t("bucketconfig.error.unsupported");
    return errMsg(e);
  }

  const snap = () => loaded.latest ?? loaded();

  return (
    <div class="bcfg-tab">
      <Show when={!(loaded.loading && loaded.latest == null)} fallback={<div class="bcfg-loading"><Spinner size={50} /><span>{t("bucketconfig.versioning.loading")}</span></div>}>
        <Show when={!(loaded.error && loaded.latest == null)} fallback={<div class="status-msg err">{errText(loaded.error, "get")}</div>}>
          <Show when={snap()!.unsupported}>
            <div class="status-msg warn">{t("bucketconfig.error.unsupported")}</div>
          </Show>
          <Show when={snap()!.denied}>
            <div class="status-msg err">{deniedMessage("versioning", "get")}</div>
          </Show>

          <Show when={!snap()!.unsupported && !snap()!.denied}>
            <Show when={capWarning(props.providerId, props.providerLabel, "versioning")}>
              {(w) => <div class="status-msg warn bcfg-provider-warn">{w()}</div>}
            </Show>
            <div class="modal-sub bcfg-hint">
              {t("bucketconfig.versioning.hint")} <DocLink providerId={props.providerId} tab="versioning" />
            </div>

            <div class="bcfg-versioning-row">
              <div class="bcfg-versioning-state">
                {t("bucketconfig.versioning.state")}{" "}
                <strong classList={{ "bcfg-on": snap()!.enabled, "bcfg-off": !snap()!.enabled }}>
                  {snap()!.enabled ? t("common.enabled") : t("bucketconfig.versioning.suspended")}
                </strong>
              </div>
              <button
                type="button"
                class="bcfg-toggle"
                classList={{ on: snap()!.enabled }}
                role="switch"
                aria-checked={snap()!.enabled}
                disabled={busy()}
                onClick={() => setEnabled(!snap()!.enabled)}
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
