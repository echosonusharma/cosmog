import { createSignal, Show } from "solid-js";
import { addAccount, testAccount, deleteAccount } from "../../api/accounts";
import { errMsg } from "../../state/toast";
import { t, tp } from "../../i18n";
import { type ProviderDef } from "../../providers";
import { regionFromEndpoint } from "../../utils/regionFromEndpoint";
import {
  ACCOUNT_NAME_MAX_LENGTH,
  clampAccountName,
  createOnboardingAccountSchema,
  parseSchema,
} from "../../validation";
import { ProviderIconTile, LabeledField } from "./shared";
import { AwsProfileImportStep } from "./AwsProfileImportStep";

export function AccountSetupStep(props: {
  provider: ProviderDef;
  onBack: () => void;
  onDone: () => void;
}) {
  const [mode, setMode] = createSignal<"manual" | "import">("manual");
  const isAws = () => props.provider.id === "aws";
  const [name, setName] = createSignal(props.provider.label);
  const [endpoint, setEndpoint] = createSignal(props.provider.endpoint);
  const [accessKey, setAccessKey] = createSignal("");
  const [secretKey, setSecretKey] = createSignal("");
  const [errors, setErrors] = createSignal<Record<string, string>>({});

  const [status, setStatus] = createSignal<
    | { kind: "idle" }
    | { kind: "loading"; action: "test" | "save" }
    | { kind: "ok"; buckets: number }
    | { kind: "err"; msg: string }
  >({ kind: "idle" });

  function formValues() {
    return {
      name: name(),
      accessKey: accessKey(),
      secretKey: secretKey(),
      endpoint: endpoint(),
    };
  }

  function schema() {
    return createOnboardingAccountSchema(props.provider);
  }

  function validateForm() {
    const result = parseSchema(schema(), formValues());
    if (!result.success) {
      setErrors(result.fieldErrors);
      return false;
    }
    setErrors({});
    return true;
  }

  function valid() {
    return schema().safeParse(formValues()).success;
  }

  function clearError(key: string) {
    setErrors((current) => {
      if (!(key in current)) return current;
      const next = { ...current };
      delete next[key];
      return next;
    });
  }

  async function doTest() {
    if (!validateForm()) return;
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
      const buckets = await testAccount(id!);
      setStatus({ kind: "ok", buckets });
    } catch (e) {
      setStatus({ kind: "err", msg: errMsg(e) });
    } finally {
      if (id) await deleteAccount(id).catch(() => {});
    }
  }

  async function doSave() {
    if (!validateForm()) return;
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
      await testAccount(id!);
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
    <Show
      when={mode() === "manual"}
      fallback={
        <AwsProfileImportStep
          provider={props.provider}
          onBack={() => setMode("manual")}
          onDone={props.onDone}
        />
      }
    >
    <form class="card" onSubmit={handleSubmit}>
      <button type="button" class="btn-back" onClick={props.onBack}>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="15" height="15">
          <path d="M19 12H5M12 5l-7 7 7 7"/>
        </svg>
        {t("common.back")}
      </button>
      <div class="provider-badge">
        <ProviderIconTile provider={props.provider} size={36} />
        <div>
          <div class="provider-badge-title">{t("onboarding.account.connect", { provider: props.provider.label })}</div>
          <div class="provider-sub">{t("onboarding.account.keychainNote")}</div>
        </div>
      </div>
      <div class="fields">
        <LabeledField
          label={t("onboarding.account.label")}
          placeholder={props.provider.label}
          value={name()}
          maxLength={ACCOUNT_NAME_MAX_LENGTH}
          error={errors().name}
          onInput={(v) => { setName(clampAccountName(v)); clearError("name"); }}
          disabled={busy()}
        />
        <Show when={props.provider.custom_endpoint}>
          <LabeledField
            label={t("onboarding.field.endpoint")}
            placeholder={props.provider.endpoint_placeholder ?? "https://…"}
            value={endpoint()}
            error={errors().endpoint}
            onInput={(v) => { setEndpoint(v); clearError("endpoint"); }}
            disabled={busy()}
          />
        </Show>
        <LabeledField
          label={t("onboarding.field.accessKeyId")}
          placeholder={t("onboarding.account.accessKeyPlaceholder")}
          value={accessKey()}
          error={errors().accessKey}
          onInput={(v) => { setAccessKey(v); clearError("accessKey"); }}
          disabled={busy()}
        />
        <LabeledField
          label={t("onboarding.field.secretAccessKey")}
          placeholder="••••••••••••••••••••"
          value={secretKey()}
          error={errors().secretKey}
          onInput={(v) => { setSecretKey(v); clearError("secretKey"); }}
          type="password"
          disabled={busy()}
        />
      </div>

      <Show when={isAws()}>
        <button
          type="button"
          class="btn-ghost aws-import-link"
          disabled={busy()}
          onClick={() => setMode("import")}
        >
          {t("onboarding.account.importLink")}
        </button>
      </Show>

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
            ((status() as { action: string }).action === "test" ? t("onboarding.account.testing") : t("common.saving"))}
          {status().kind === "ok" &&
            tp("onboarding.account.connected", (status() as { buckets: number }).buckets, { provider: props.provider.label })}
          {status().kind === "err" && (status() as { msg: string }).msg}
        </div>
      </Show>

      <div class="btn-row">
        <button type="button" class="btn-secondary" disabled={!valid() || busy()} onClick={doTest}>
          {t("onboarding.account.test")}
        </button>
        <button type="submit" class="btn-primary flex-1" disabled={!valid() || busy()}>
          {t("onboarding.account.saveOpen")}
        </button>
      </div>
    </form>
    </Show>
  );
}
