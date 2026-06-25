import { createSignal, For, Show } from "solid-js";
import {
  addAccount, deleteAccount, testAccount,
  type AddAccountInput,
} from "../../api/accounts";
import { toast } from "../../state/toast";
import { PROVIDERS, PICKABLE_PROVIDERS, type ProviderDef } from "../../providers";

// ── add account ───────────────────────────────────────────────────────────────

export function AddAccountForm(props: { onDone: () => void; onCancel: () => void }) {
  // All providers including the generic "s3" catch-all at the end
  const providers = [...PICKABLE_PROVIDERS, PROVIDERS.find((p) => p.id === "s3")!];

  const [provider, setProvider] = createSignal<ProviderDef>(providers[0]);
  const [form, setForm] = createSignal<AddAccountInput>({
    name: "", protocol: "s3", region: providers[0].region,
    access_key_id: "", secret_access_key: "",
    endpoint: providers[0].endpoint || undefined,
    addressing_style: providers[0].addressing_style as any || undefined,
  });
  const [busy, setBusy] = createSignal(false);

  function applyProvider(p: ProviderDef) {
    setProvider(p);
    setForm((f) => ({
      ...f,
      region: p.region || f.region,
      endpoint: p.endpoint || undefined,
      addressing_style: p.addressing_style as any || undefined,
    }));
  }

  function set<K extends keyof AddAccountInput>(k: K, v: AddAccountInput[K]) {
    setForm((p) => ({ ...p, [k]: v }));
  }

  const valid = () =>
    form().name.trim() && form().region.trim() &&
    form().access_key_id.trim() && form().secret_access_key.trim();

  async function save() {
    if (!valid()) return;
    setBusy(true);
    let id: string | null = null;
    try {
      const acct = await addAccount(form());
      id = acct.id;
      await testAccount(id);
      toast.ok(`Account "${acct.name}" added`);
      props.onDone();
    } catch (e) {
      if (id) await deleteAccount(id).catch(() => {});
      toast.err(e);
    } finally { setBusy(false); }
  }

  return (
    <div class="add-account-form">
      <div class="settings-section-title" style="border-bottom:none;padding:0">Add account</div>

      {/* provider picker */}
      <div class="provider-picker">
        <For each={providers}>
          {(p) => (
            <button
              class={`provider-picker-tile ${provider().id === p.id ? "selected" : ""}`}
              onClick={() => applyProvider(p)}
              disabled={busy()}
              title={p.label}
            >
              <span class="provider-picker-tile-icon" style={{ background: p.color }}>
                <img src={p.iconUrl} alt={p.label} style={`width:65%;height:65%;object-fit:contain;${p.monochrome_icon ? "filter:brightness(0) invert(1)" : ""}`} />
              </span>
              <span class="provider-picker-tile-label">{p.label}</span>
            </button>
          )}
        </For>
      </div>

      <div class="fields">
        <input class="field" placeholder="Name" value={form().name}
               onInput={(e) => set("name", e.currentTarget.value)} disabled={busy()} />
        <input class="field"
               placeholder={provider().region ? `Region (e.g. ${provider().region})` : "Region"}
               value={form().region}
               onInput={(e) => set("region", e.currentTarget.value)} disabled={busy()} />
        <Show when={provider().id !== "aws"}>
          <input class="field"
                 placeholder={provider().endpoint_placeholder ?? "Endpoint URL"}
                 value={form().endpoint ?? ""}
                 onInput={(e) => set("endpoint", e.currentTarget.value || undefined)}
                 disabled={busy()} />
        </Show>
        <input class="field" placeholder="Access Key ID" value={form().access_key_id}
               onInput={(e) => set("access_key_id", e.currentTarget.value)} disabled={busy()} />
        <input class="field" type="password" placeholder="Secret Access Key" value={form().secret_access_key}
               onInput={(e) => set("secret_access_key", e.currentTarget.value)} disabled={busy()} />
      </div>
      <div class="btn-row mt-2" style="justify-content:flex-end">
        <button class="btn-secondary" style="min-width:90px" onClick={props.onCancel}>Cancel</button>
        <button class="btn-primary" style="min-width:90px" disabled={!valid() || busy()} onClick={save}>
          {busy() ? "Testing…" : "Save"}
        </button>
      </div>
    </div>
  );
}
