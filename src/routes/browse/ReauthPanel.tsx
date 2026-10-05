import { createSignal, Show } from "solid-js";
import { updateAccount, testAccount } from "../../api/accounts";
import { bumpAccountsRefresh, bumpBucketsRefresh } from "../../state/app";
import { toast } from "../../state/toast";
import { t } from "../../i18n";
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
      toast.ok(t("browse.reauth.doneTitle"), t("browse.reauth.doneDetail", { name: props.accountName }));
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
      <div class="err-popup-header"><span class="err-popup-title">{t("browse.reauth.title")}</span></div>
      <p class="err-popup-msg">
        {t("browse.reauth.body", { name: props.accountName })}
      </p>
      <input
        class="field"
        classList={{ "field-error": !!error() }}
        type="password"
        placeholder={t("browse.reauth.placeholder")}
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
          {busy() ? t("browse.reauth.reconnecting") : t("browse.reauth.reconnect")}
        </button>
      </div>
    </div>
  );
}
