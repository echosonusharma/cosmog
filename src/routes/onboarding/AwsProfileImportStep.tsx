import { createMemo, createSignal, For, Show } from "solid-js";
import { addAccount, deleteAccount, testAccount } from "../../api/accounts";
import { errMsg, toast } from "../../state/toast";
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
          throw new Error(nameResult.error.issues[0]?.message ?? "Invalid profile name");
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
          `${imported} imported, ${failures.length} failed`,
          failures.join(" · "),
        );
      } else {
        toast.ok(
          imported === 1 ? "1 account imported" : `${imported} accounts imported`,
          "All profiles connected successfully",
        );
      }
      props.onDone();
      return;
    }

    setResultMsg({
      kind: "err",
      msg: failures.length === 1
        ? failures[0]
        : `Import failed for all profiles:\n${failures.join("\n")}`,
    });
  }

  return (
    <div class="card card-wide">
      <button type="button" class="btn-back" onClick={props.onBack} disabled={busy()}>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="15" height="15">
          <path d="M19 12H5M12 5l-7 7 7 7"/>
        </svg>
        Back
      </button>

      <div class="provider-badge">
        <ProviderIconTile provider={props.provider} size={36} />
        <div>
          <div class="provider-badge-title">Import AWS profiles</div>
          <div class="provider-sub">Paste your credentials file, profiles become separate accounts.</div>
        </div>
      </div>

      <ol class="aws-import-steps">
        <li>
          Open <code class="aws-import-path">~/.aws/credentials</code>
          <span class="aws-import-path-hint"> (Windows: <code>%USERPROFILE%\.aws\credentials</code>)</span>
        </li>
        <li>Copy the entire file and paste it below</li>
        <li>Click Import - keys are saved to your OS keychain, not kept in the app</li>
      </ol>

      <div class="aws-import-note">
        Paste the <strong>credentials</strong> file, not <code>config</code>. SSO-only profiles are skipped.
      </div>

      <div class="field-label">Credentials file</div>
      <div class="ini-editor-wrap">
        <IniEditor value={text()} onChange={setText} disabled={busy()} />
      </div>

      <Show when={text().trim()}>
        <div class="aws-import-summary">
          <Show when={parsed().syntaxErrors.length > 0}>
            <div class="status-msg err">
              Fix {parsed().syntaxErrors.length} syntax {parsed().syntaxErrors.length === 1 ? "error" : "errors"} before importing.
            </div>
          </Show>

          <Show when={parsed().syntaxErrors.length === 0}>
            <Show when={parsed().profiles.length > 0}>
              <div class="status-msg ok">
                {parsed().profiles.length} profile{parsed().profiles.length === 1 ? "" : "s"} ready:
                {" "}
                {parsed().profiles.map((p) => p.name).join(", ")}
              </div>
            </Show>

            <Show when={parsed().skipped.length > 0}>
              <For each={parsed().skipped}>
                {(s) => (
                  <div class="status-msg warn">
                    Skipped <strong>{s.name}</strong>: {s.message}
                  </div>
                )}
              </For>
            </Show>

            <Show when={parsed().profiles.length === 0 && parsed().skipped.length === 0}>
              <div class="status-msg err">
                No importable profiles found. Each section needs aws_access_key_id and aws_secret_access_key.
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
          Manual entry
        </button>
        <button
          type="button"
          class="btn-primary flex-1"
          disabled={!canImport()}
          onClick={doImport}
        >
          {busy()
            ? "Importing…"
            : parsed().profiles.length > 0
            ? `Import ${parsed().profiles.length} account${parsed().profiles.length === 1 ? "" : "s"}`
            : "Import"}
        </button>
      </div>
    </div>
  );
}
