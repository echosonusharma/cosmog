import { createSignal, createResource, createEffect, Show } from "solid-js";
import { getBucketPolicy, putBucketPolicy, deleteBucketPolicy } from "../../../api/buckets";
import { CodeEditor } from "../../../utils/CodeEditor";
import { resolvedTheme } from "../../../state/theme";
import { toast } from "../../../state/toast";
import { classifyBucketError, deniedMessage } from "./errors";
import { capWarning } from "./providerCaps";
import { DocLink } from "./DocLink";
import Spinner from "../../../utils/Spinner";
import { errMsg } from "../../../utils/errors";
import { t } from "../../../i18n";

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

  // `seeded` holds the raw server value as the change key; the editor shows a
  // pretty-printed copy so minified policies auto-format on load.
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
    if (!raw) { setErr(t("bucketconfig.policy.empty")); return; }
    try {
      JSON.parse(raw);
    } catch (e) {
      setErr(t("bucketconfig.policy.invalidJson", { error: (e as Error).message }));
      return;
    }
    setBusy(true);
    try {
      await putBucketPolicy(props.accountId, props.bucket, raw);
      toast.ok(t("bucketconfig.policy.savedTitle"), t("bucketconfig.policy.savedDetail", { bucket: props.bucket }));
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
      toast.ok(t("bucketconfig.policy.removedTitle"), t("bucketconfig.policy.removedDetail", { bucket: props.bucket }));
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
    if (kind === "unsupported") return t("bucketconfig.error.unsupported");
    return errMsg(e);
  }

  const snap = () => loaded.latest ?? loaded();

  return (
    <div class="bcfg-tab">
      <Show when={!(loaded.loading && loaded.latest == null)} fallback={<div class="bcfg-loading"><Spinner size={50} /><span>{t("bucketconfig.policy.loading")}</span></div>}>
        <Show when={!(loaded.error && loaded.latest == null)} fallback={<div class="status-msg err">{errText(loaded.error, "get")}</div>}>
          <Show when={snap()!.unsupported}>
            <div class="status-msg warn">{t("bucketconfig.error.unsupported")}</div>
          </Show>
          <Show when={snap()!.denied}>
            <div class="status-msg err">{deniedMessage("policy", "get")}</div>
          </Show>

          <Show when={!snap()!.unsupported && !snap()!.denied}>
            <Show when={capWarning(props.providerId, props.providerLabel, "policy")}>
              {(w) => <div class="status-msg warn bcfg-provider-warn">{w()}</div>}
            </Show>
            <div class="modal-sub bcfg-hint">
              {t("bucketconfig.policy.hint")} <DocLink providerId={props.providerId} tab="policy" />
            </div>

            <div class="bcfg-editor">
              <CodeEditor
                value={content()}
                ext="json"
                dark={resolvedTheme() === "dark"}
                gutters={true}
                original={prettify(seeded() ?? EMPTY_POLICY)}
                onChange={setContent}
              />
            </div>

            <Show when={err()}><div class="status-msg err bcfg-status">{err()}</div></Show>

            <div class="btn-row bcfg-actions">
              <div class="bcfg-actions-grp">
                <button class="btn-ghost text-xs" disabled={busy()} onClick={() => setContent(prettify(content()))}>
                  {t("bucketconfig.policy.format")}
                </button>
                <button class="btn-secondary text-xs" disabled={busy()} onClick={handleReset}>
                  {t("common.reset")}
                </button>
              </div>
              <div class="bcfg-actions-grp">
                <button class="enc-disable-btn bcfg-danger-btn" disabled={busy()} onClick={handleDelete}>
                  {busy() ? t("bucketconfig.working") : t("common.delete")}
                </button>
                <button class="btn-primary text-xs" disabled={busy()} onClick={handleSave}>
                  {busy() ? t("common.saving") : t("common.save")}
                </button>
              </div>
            </div>
          </Show>
        </Show>
      </Show>
    </div>
  );
}
