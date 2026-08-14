import { createSignal, For, Show } from "solid-js";
import {
  addAccount, testAccount, updateAccount,
  type AddAccountInput, type Account,
} from "../../api/accounts";
import { toast } from "../../state/toast";
import { bumpBucketsRefresh, bumpAccountsRefresh } from "../../state/app";
import { PROVIDERS, PICKABLE_PROVIDERS, type ProviderDef, detectProvider } from "../../providers";
import { regionFromEndpoint } from "../../utils/regionFromEndpoint";
import {
  accountNameMaxLength,
  clampAccountName,
  createAccountFormSchema,
  parseSchema,
} from "../../validation";

export function AddAccountForm(props: { onDone: () => void; onCancel: () => void; editing?: Account }) {
  const isEdit = () => !!props.editing;
  const providers = [...PICKABLE_PROVIDERS, PROVIDERS.find((p) => p.id === "s3")!];

  const initialProvider = (): ProviderDef => {
    if (props.editing) {
      const detected = detectProvider({ endpoint: props.editing.endpoint });
      return providers.find((p) => p.id === detected.id) ?? providers[0];
    }
    return providers[0];
  };
  const [provider, setProvider] = createSignal<ProviderDef>(initialProvider());
  const [form, setForm] = createSignal<AddAccountInput>(
    props.editing
      ? {
          name: props.editing.name,
          protocol: props.editing.protocol,
          region: props.editing.region,
          access_key_id: props.editing.access_key_id,
          secret_access_key: "",
          endpoint: props.editing.endpoint ?? undefined,
          addressing_style: props.editing.addressing_style,
        }
      : {
          name: "", protocol: "s3", region: providers[0].region,
          access_key_id: "", secret_access_key: "",
          endpoint: providers[0].endpoint || undefined,
          addressing_style: providers[0].addressing_style || undefined,
        }
  );
  const [busy, setBusy] = createSignal(false);
  const [errors, setErrors] = createSignal<Record<string, string>>({});

  function applyProvider(p: ProviderDef) {
    setProvider(p);
    setForm((f) => ({
      ...f,
      region: p.region || f.region,
      endpoint: p.endpoint || undefined,
      addressing_style: p.addressing_style || undefined,
    }));
    setErrors({});
  }

  function set<K extends keyof AddAccountInput>(k: K, v: AddAccountInput[K]) {
    setForm((p) => ({ ...p, [k]: v }));
    setErrors((current) => {
      if (!(k in current)) return current;
      const next = { ...current };
      delete next[k];
      return next;
    });
  }

  function schema() {
    return createAccountFormSchema({
      providerId: provider().id,
      isEdit: isEdit(),
      existingName: props.editing?.name,
    });
  }

  const nameMaxLength = () => accountNameMaxLength({
    isEdit: isEdit(),
    existingName: props.editing?.name,
  });

  function validateForm() {
    const result = parseSchema(schema(), form());
    if (!result.success) {
      setErrors(result.fieldErrors);
      return false;
    }
    setErrors({});
    return true;
  }

  const valid = () => schema().safeParse(form()).success;

  async function save() {
    if (!validateForm()) return;
    setBusy(true);
    const f = form();
    if (isEdit()) {
      const trimmedName = f.name.trim();
      try {
        await updateAccount(props.editing!.id, {
          name: trimmedName,
          region: f.region,
          access_key_id: f.access_key_id,
          endpoint: f.endpoint ?? null,
          addressing_style: f.addressing_style,
          secret_access_key: f.secret_access_key ? f.secret_access_key : undefined,
        });
        bumpAccountsRefresh();
        bumpBucketsRefresh();
        props.onDone();
        try {
          await testAccount(props.editing!.id);
          toast.ok("Account updated", `"${trimmedName}" saved and connection verified`);
        } catch {
          toast.warn(
            `"${trimmedName}" was saved but the connection test failed. Check credentials in Settings.`,
            "Account saved",
          );
        }
      } catch (e) {
        toast.err(e);
      } finally { setBusy(false); }
      return;
    }
    try {
      const acct = await addAccount({
        ...f,
        name: f.name.trim(),
        region: regionFromEndpoint(provider(), f.endpoint ?? ""),
      });
      bumpAccountsRefresh();
      bumpBucketsRefresh();
      props.onDone();
      try {
        await testAccount(acct.id);
        toast.ok("Account added", `"${acct.name}" connected successfully`);
      } catch {
        toast.warn(`"${acct.name}" was saved but the connection test failed. Check credentials in Settings.`, "Account saved");
      }
    } catch (e) {
      toast.err(e);
    } finally { setBusy(false); }
  }

  return (
    <div class="add-account-form">
      <div class="settings-section-title settings-section-title-flat">{isEdit() ? "Edit account" : "Add account"}</div>

      <div class="provider-picker">
        <For each={providers}>
          {(p) => (
            <button
              class={`provider-picker-tile ${provider().id === p.id ? "selected" : ""}`}
              onClick={() => applyProvider(p)}
              disabled={busy()}
            >
              <span class={`provider-picker-tile-icon${p.tile_fill ? " tile-fill" : ""}`} style={{ background: p.color }}>
                <img src={p.iconUrl} alt={p.label} class="provider-picker-tile-img" classList={{ "provider-picker-tile-img-mono": !!p.monochrome_icon }} />
              </span>
              <span class="provider-picker-tile-label">{p.label}</span>
            </button>
          )}
        </For>
      </div>

      <div class="fields">
        <div>
          <input
            class="field"
            classList={{ "field-error": !!errors().name }}
            placeholder="Name"
            value={form().name}
            maxlength={nameMaxLength()}
            onInput={(e) => set("name", clampAccountName(e.currentTarget.value, nameMaxLength()))}
            disabled={busy()}
          />
          <Show when={errors().name}><div class="field-hint">{errors().name}</div></Show>
        </div>
        <Show when={provider().id !== "aws"}>
          <div>
            <input
              class="field"
              classList={{ "field-error": !!errors().endpoint }}
              placeholder={provider().endpoint_placeholder ?? "Endpoint URL"}
              value={form().endpoint ?? ""}
              onInput={(e) => set("endpoint", e.currentTarget.value.trim() || undefined)}
              disabled={busy()}
            />
            <Show when={errors().endpoint}><div class="field-hint">{errors().endpoint}</div></Show>
          </div>
        </Show>
        <div>
          <input
            class="field"
            classList={{ "field-error": !!errors().access_key_id }}
            placeholder="Access Key ID"
            value={form().access_key_id}
            onInput={(e) => set("access_key_id", e.currentTarget.value.trim())}
            disabled={busy()}
          />
          <Show when={errors().access_key_id}><div class="field-hint">{errors().access_key_id}</div></Show>
        </div>
        <div>
          <input
            class="field"
            classList={{ "field-error": !!errors().secret_access_key }}
            type="password"
            placeholder={isEdit() ? "Secret Access Key (leave blank to keep)" : "Secret Access Key"}
            value={form().secret_access_key}
            onInput={(e) => set("secret_access_key", e.currentTarget.value.trim())}
            disabled={busy()}
          />
          <Show when={errors().secret_access_key}><div class="field-hint">{errors().secret_access_key}</div></Show>
        </div>
      </div>
      <div class="btn-row mt-2 add-account-btn-row">
        <button class="btn-secondary add-account-btn" onClick={props.onCancel}>Cancel</button>
        <button class="btn-primary add-account-btn" disabled={!valid() || busy()} onClick={save}>
          {busy() ? "Testing…" : (isEdit() ? "Update" : "Save")}
        </button>
      </div>
    </div>
  );
}
