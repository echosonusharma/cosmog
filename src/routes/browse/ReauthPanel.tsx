import { createSignal, Show } from "solid-js";
import { updateAccount, testAccount } from "../../api/accounts";
import { bumpAccountsRefresh, bumpBucketsRefresh } from "../../state/app";
import { toast } from "../../state/toast";
import { parseSchema, reauthSecretSchema } from "../../validation";

// Re-enter a missing secret without a full re-setup.
export function ReauthPanel(props: { accountId: string; accountName: string; onDone: () => void }) {
  const [secret, setSecret] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");

  async function reconnect() {
    const result = parseSchema(reauthSecretSchema, secret());
    if (!result.success) {
      setError(result.message);
      return;
    }
    if (busy()) return;
    setBusy(true);
    setError("");
    try {
      await updateAccount(props.accountId, { secret_access_key: result.data });
      await testAccount(props.accountId);
      toast.ok("Reconnected", `Credentials for "${props.accountName}" were restored`);
      bumpAccountsRefresh();
      bumpBucketsRefresh();
      props.onDone();
    } catch (e) {
      toast.err(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="reauth-panel">
      <div class="err-popup-header"><span class="err-popup-title">Reconnect account</span></div>
      <p class="err-popup-msg">
        The secret key for "{props.accountName}" is no longer stored on this device.
        This can happen after reinstalling the app. Re-enter it to reconnect. Your
        buckets and settings are intact.
      </p>
      <input
        class="field"
        classList={{ "field-error": !!error() }}
        type="password"
        placeholder="Secret Access Key"
        autocomplete="off"
        value={secret()}
        disabled={busy()}
        onInput={(e) => { setSecret(e.currentTarget.value); setError(""); }}
        onKeyDown={(e) => { if (e.key === "Enter") reconnect(); }}
      />
      <Show when={error()}>
        <div class="field-hint">{error()}</div>
      </Show>
      <div class="err-popup-actions">
        <button class="btn-primary btn-xs" disabled={!secret().trim() || busy()} onClick={reconnect}>
          {busy() ? "Reconnecting…" : "Reconnect"}
        </button>
      </div>
    </div>
  );
}
