import { createSignal, Show, For } from "solid-js";
import { PICKABLE_PROVIDERS as PROVIDERS, type ProviderDef } from "../../providers";
import { IconCheck } from "../../utils/icons";
import { ProviderIconTile } from "./shared";

// ── step 2: provider select ───────────────────────────────────────────────────

const PROVIDER_DESCS: Record<string, string> = {
  aws:          "s3.amazonaws.com",
  backblaze:    "S3-compatible · low cost",
  r2:           "Zero egress fees",
  wasabi:       "Hot cloud storage",
  digitalocean: "Managed object storage",
  minio:        "Self-hosted S3",
  s3:           "Any S3-compatible endpoint",
};

export function ProviderStep(props: {
  onBack: () => void;
  onNext: (p: ProviderDef) => void;
}) {
  const [selected, setSelected] = createSignal<string>(PROVIDERS[0].id);

  function submit(e: Event) {
    e.preventDefault();
    const p = PROVIDERS.find((x) => x.id === selected());
    if (p) props.onNext(p);
  }

  function handleListKeyDown(e: KeyboardEvent) {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "Enter") return;
    e.preventDefault();
    if (e.key === "Enter") { submit(e); return; }
    const ids = PROVIDERS.map((p) => p.id);
    const idx = ids.indexOf(selected());
    const next =
      e.key === "ArrowDown"
        ? ids[(idx + 1) % ids.length]
        : ids[(idx - 1 + ids.length) % ids.length];
    setSelected(next);
  }

  return (
    <form class="card" onSubmit={submit}>
      <button class="btn-back" type="button" onClick={props.onBack}>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="15" height="15">
          <path d="M19 12H5M12 5l-7 7 7 7"/>
        </svg>
        Back
      </button>
      <div>
        <div class="card-title">Choose a provider</div>
        <div style="font-size:12.5px;color:var(--text-muted);margin-top:4px">Connect any S3-compatible storage backend.</div>
      </div>
      <div
        class="provider-list"
        tabIndex={0}
        onKeyDown={handleListKeyDown}
        ref={(el) => setTimeout(() => el?.focus(), 0)}
      >
        <For each={PROVIDERS}>
          {(p) => (
            <button
              type="button"
              tabIndex={-1}
              class={`provider-row ${selected() === p.id ? "selected" : ""}`}
              onClick={() => setSelected(p.id)}
              onDblClick={() => { setSelected(p.id); submit(new Event("submit")); }}
            >
              <ProviderIconTile provider={p} size={32} />
              <div class="provider-row-info">
                <div class="provider-row-name">{p.label}</div>
                <Show when={PROVIDER_DESCS[p.id]}>
                  <div class="provider-row-desc">{PROVIDER_DESCS[p.id]}</div>
                </Show>
              </div>
              <Show when={selected() === p.id}>
                <span class="provider-checkmark">
                  <IconCheck size={15} />
                </span>
              </Show>
            </button>
          )}
        </For>
      </div>
      <button type="submit" class="btn-primary">
        Continue
      </button>
    </form>
  );
}
