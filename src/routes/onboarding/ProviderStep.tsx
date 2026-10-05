import { createSignal, Show, For } from "solid-js";
import { PICKABLE_PROVIDERS as PROVIDERS, type ProviderDef } from "../../providers";
import { IconCheck } from "../../utils/icons";
import { t, type TKey } from "../../i18n";
import { ProviderIconTile } from "./shared";

const PROVIDER_DESCS: Record<string, TKey> = {
  aws:          "onboarding.provider.desc.aws",
  backblaze:    "onboarding.provider.desc.backblaze",
  r2:           "onboarding.provider.desc.r2",
  wasabi:       "onboarding.provider.desc.wasabi",
  digitalocean: "onboarding.provider.desc.digitalocean",
  minio:        "onboarding.provider.desc.minio",
  s3:           "onboarding.provider.desc.s3",
};

export function ProviderStep(props: {
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
      <div>
        <div class="card-title">{t("onboarding.provider.title")}</div>
        <div class="card-subtitle">{t("onboarding.provider.subtitle")}</div>
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
                  <div class="provider-row-desc">{t(PROVIDER_DESCS[p.id])}</div>
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
        {t("onboarding.provider.continue")}
      </button>
    </form>
  );
}
