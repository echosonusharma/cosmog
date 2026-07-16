import { createSignal, Show } from "solid-js";
import { addAccount, testAccount, deleteAccount } from "../../api/accounts";
import { errMsg } from "../../state/toast";
import { type ProviderDef } from "../../providers";
import { regionFromEndpoint } from "../../utils/regionFromEndpoint";
import { ProviderIconTile, LabeledField } from "./shared";

// ── step 3: account setup ─────────────────────────────────────────────────────

export function AccountSetupStep(props: {
  provider: ProviderDef;
  onBack: () => void;
  onDone: () => void;
}) {
  const [name, setName] = createSignal(props.provider.label);
  const [endpoint, setEndpoint] = createSignal(props.provider.endpoint);
  const [accessKey, setAccessKey] = createSignal("");
  const [secretKey, setSecretKey] = createSignal("");

  const [status, setStatus] = createSignal<
    | { kind: "idle" }
    | { kind: "loading"; action: "test" | "save" }
    | { kind: "ok"; buckets: number }
    | { kind: "err"; msg: string }
  >({ kind: "idle" });

  function valid() {
    return (
      name().trim() &&
      accessKey().trim() &&
      secretKey().trim() &&
      (!props.provider.custom_endpoint || endpoint().trim())
    );
  }

  async function doTest() {
    if (!valid()) return;
    setStatus({ kind: "loading", action: "test" });
    let id: string | null = null;
    try {
      const acct = await addAccount({
        name: name().trim(),
        protocol: "s3",
        endpoint: endpoint().trim() || undefined,
        region: regionFromEndpoint(props.provider, endpoint().trim()),
        access_key_id: accessKey().trim(),
        secret_access_key: secretKey().trim(),
        addressing_style: props.provider.addressing_style,
      });
      id = acct.id;
      const buckets = await testAccount(id);
      setStatus({ kind: "ok", buckets });
    } catch (e) {
      setStatus({ kind: "err", msg: errMsg(e) });
    } finally {
      if (id) await deleteAccount(id).catch(() => {});
    }
  }

  async function doSave() {
    if (!valid()) return;
    setStatus({ kind: "loading", action: "save" });
    let id: string | null = null;
    try {
      const acct = await addAccount({
        name: name().trim(),
        protocol: "s3",
        endpoint: endpoint().trim() || undefined,
        region: regionFromEndpoint(props.provider, endpoint().trim()),
        access_key_id: accessKey().trim(),
        secret_access_key: secretKey().trim(),
        addressing_style: props.provider.addressing_style,
      });
      id = acct.id;
      await testAccount(id);
      props.onDone();
    } catch (e) {
      if (id) await deleteAccount(id).catch(() => {});
      setStatus({ kind: "err", msg: errMsg(e) });
    }
  }

  const busy = () => status().kind === "loading";

  function handleSubmit(e: Event) {
    e.preventDefault();
    if (valid() && !busy()) doSave();
  }

  return (
    <form class="card" onSubmit={handleSubmit}>
      <button type="button" class="btn-back" onClick={props.onBack}>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="15" height="15">
          <path d="M19 12H5M12 5l-7 7 7 7"/>
        </svg>
        Back
      </button>
      <div class="provider-badge">
        <ProviderIconTile provider={props.provider} size={36} />
        <div>
          <div class="provider-badge-title">Connect {props.provider.label}</div>
          <div class="provider-sub">Credentials are encrypted in your OS keychain.</div>
        </div>
      </div>
      <div class="fields">
        <LabeledField label="Account label" placeholder={props.provider.label} value={name()} onInput={setName} disabled={busy()} />
        <Show when={props.provider.custom_endpoint}>
          <LabeledField
            label="Endpoint"
            placeholder={props.provider.endpoint_placeholder ?? "https://…"}
            value={endpoint()}
            onInput={setEndpoint}
            disabled={busy()}
          />
        </Show>
        <LabeledField label="Access Key ID" placeholder="Your access key ID" value={accessKey()} onInput={setAccessKey} disabled={busy()} />
        <LabeledField label="Secret Access Key" placeholder="••••••••••••••••••••" value={secretKey()} onInput={setSecretKey} type="password" disabled={busy()} />
      </div>

      <Show when={status().kind !== "idle"}>
        <div
          class={`status-msg ${
            status().kind === "ok"
              ? "ok"
              : status().kind === "err"
              ? "err"
              : "loading"
          }`}
        >
          {status().kind === "loading" &&
            `${(status() as { action: string }).action === "test" ? "Testing connection" : "Saving"}…`}
          {status().kind === "ok" &&
            `Connected to ${props.provider.label} · found ${(status() as { buckets: number }).buckets} bucket(s)`}
          {status().kind === "err" && (status() as { msg: string }).msg}
        </div>
      </Show>

      <div class="btn-row">
        <button type="button" class="btn-secondary" disabled={!valid() || busy()} onClick={doTest}>
          Test connection
        </button>
        <button type="submit" class="btn-primary flex-1" disabled={!valid() || busy()}>
          Save &amp; open
        </button>
      </div>
    </form>
  );
}
