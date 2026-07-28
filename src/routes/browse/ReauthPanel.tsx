import { createSignal } from "solid-js";
import { updateAccount, testAccount } from "../../api/accounts";
import { bumpAccountsRefresh, bumpBucketsRefresh } from "../../state/app";
import { toast } from "../../state/toast";

// Re-enter a missing secret without a full re-setup.
export function ReauthPanel(props: { accountId: string; accountName: string; onDone: () => void }) {
  const [secret, setSecret] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  async function reconnect() {
    const s = secret().trim();
    if (!s || busy()) return;
    setBusy(true);
    try {
      await updateAccount(props.accountId, { secret_access_key: s });
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
        type="password"
        placeholder="Secret Access Key"
        autocomplete="off"
        value={secret()}
        disabled={busy()}
        onInput={(e) => setSecret(e.currentTarget.value)}
        onKeyDown={(e) => { if (e.key === "Enter") reconnect(); }}
      />
      <div class="err-popup-actions">
        <button class="btn-primary btn-xs" disabled={!secret().trim() || busy()} onClick={reconnect}>
          {busy() ? "Reconnecting…" : "Reconnect"}
        </button>
      </div>
    </div>
  );
}
