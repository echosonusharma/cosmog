import { createMemo, createSignal, For, Show } from "solid-js";
import { addAccount, deleteAccount, testAccount } from "../../api/accounts";
import { errMsg, toast } from "../../state/toast";
import { t, tp } from "../../i18n";
import { type ProviderDef } from "../../providers";
import { parseAwsCredentialsIni } from "../../utils/parseAwsCredentialsIni";
import { accountNameSchema } from "../../validation";
import { ProviderIconTile } from "./shared";
import { IniEditor } from "./IniEditor";

export function AwsProfileImportStep(props: {
  provider: ProviderDef;
  onBack: () => void;
  onDone: () => void;
}) {
  const [text, setText] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [resultMsg, setResultMsg] = createSignal<{ kind: "ok" | "err" | "warn"; msg: string } | null>(null);

  const parsed = createMemo(() => parseAwsCredentialsIni(text()));
  const canImport = () => !busy() && parsed().syntaxErrors.length === 0 && parsed().profiles.length > 0;

  async function doImport() {
    const { profiles } = parsed();
    if (!profiles.length) return;

    setBusy(true);
    setResultMsg(null);

    const failures: string[] = [];
    let imported = 0;

    for (const profile of profiles) {
      let id: string | null = null;
      try {
        const nameResult = accountNameSchema.safeParse(profile.name);
        if (!nameResult.success) {
          throw new Error(nameResult.error.issues[0]?.message ?? t("onboarding.import.invalidName"));
        }
        const acct = await addAccount({
          name: nameResult.data,
          protocol: "s3",
          region: profile.region,
          access_key_id: profile.access_key_id,
          secret_access_key: profile.secret_access_key,
          addressing_style: props.provider.addressing_style,
        });
        id = acct.id;
        await testAccount(acct.id);
        imported++;
      } catch (e) {
        if (id) await deleteAccount(id).catch(() => {});
        failures.push(`${profile.name}: ${errMsg(e)}`);
      }
    }

    setText("");
    setBusy(false);

    if (imported > 0) {
      if (failures.length) {
        toast.warn(
          t("onboarding.import.partial", { imported, failed: failures.length }),
          failures.join(" · "),
        );
      } else {
        toast.ok(
          tp("onboarding.import.done", imported),
          t("onboarding.import.doneDetail"),
        );
      }
      props.onDone();
      return;
    }

    setResultMsg({
      kind: "err",
      msg: failures.length === 1
        ? failures[0]
        : t("onboarding.import.allFailed", { errors: failures.join("\n") }),
    });
  }

  return (
    <div class="card card-wide">
      <button type="button" class="btn-back" onClick={props.onBack} disabled={busy()}>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="15" height="15">
          <path d="M19 12H5M12 5l-7 7 7 7"/>
        </svg>
        {t("common.back")}
      </button>

      <div class="provider-badge">
        <ProviderIconTile provider={props.provider} size={36} />
        <div>
          <div class="provider-badge-title">{t("onboarding.import.title")}</div>
          <div class="provider-sub">{t("onboarding.import.subtitle")}</div>
        </div>
      </div>

      <ol class="aws-import-steps">
        <li>
          {t("onboarding.import.step1")} <code class="aws-import-path">~/.aws/credentials</code>
          <span class="aws-import-path-hint"> ({t("onboarding.import.windows")} <code>%USERPROFILE%\.aws\credentials</code>)</span>
        </li>
        <li>{t("onboarding.import.step2")}</li>
        <li>{t("onboarding.import.step3")}</li>
      </ol>

      <div class="aws-import-note">
        {t("onboarding.import.note")}
      </div>

      <div class="field-label">{t("onboarding.import.fileLabel")}</div>
      <div class="ini-editor-wrap">
        <IniEditor value={text()} onChange={setText} disabled={busy()} />
      </div>

      <Show when={text().trim()}>
        <div class="aws-import-summary">
          <Show when={parsed().syntaxErrors.length > 0}>
            <div class="status-msg err">
              {tp("onboarding.import.fixErrors", parsed().syntaxErrors.length)}
            </div>
          </Show>

          <Show when={parsed().syntaxErrors.length === 0}>
            <Show when={parsed().profiles.length > 0}>
              <div class="status-msg ok">
                {tp("onboarding.import.ready", parsed().profiles.length, { names: parsed().profiles.map((p) => p.name).join(", ") })}
              </div>
            </Show>

            <Show when={parsed().skipped.length > 0}>
              <For each={parsed().skipped}>
                {(s) => (
                  <div class="status-msg warn">
                    {t("onboarding.import.skipped", { name: s.name, message: s.message })}
                  </div>
                )}
              </For>
            </Show>

            <Show when={parsed().profiles.length === 0 && parsed().skipped.length === 0}>
              <div class="status-msg err">
                {t("onboarding.import.none")}
              </div>
            </Show>
          </Show>
        </div>
      </Show>

      <Show when={resultMsg()}>
        <div class={`status-msg ${resultMsg()!.kind}`}>{resultMsg()!.msg}</div>
      </Show>

      <div class="btn-row">
        <button type="button" class="btn-secondary" onClick={props.onBack} disabled={busy()}>
          {t("onboarding.import.manual")}
        </button>
        <button
          type="button"
          class="btn-primary flex-1"
          disabled={!canImport()}
          onClick={doImport}
        >
          {busy()
            ? t("onboarding.import.importing")
            : parsed().profiles.length > 0
            ? tp("onboarding.import.action", parsed().profiles.length)
            : t("onboarding.import.actionIdle")}
        </button>
      </div>
    </div>
  );
}
