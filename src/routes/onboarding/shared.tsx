import { Show, For } from "solid-js";
import { type ProviderDef } from "../../providers";
import { IconCheck } from "../../utils/icons";

// ── types ─────────────────────────────────────────────────────────────────────

export type Step = "provider" | "account-setup";

// ── provider color tile ───────────────────────────────────────────────────────

const PROVIDER_COLORS: Record<string, string> = {
  aws:          "#E8891C",
  backblaze:    "#CC0000",
  r2:           "#F38020",
  wasabi:       "#27A843",
  digitalocean: "#0080FF",
  minio:        "#C7264A",
  s3:           "#6B6F7A",
};

export function ProviderIconTile(props: { provider: ProviderDef; size?: number }) {
  const sz = props.size ?? 32;
  const color = PROVIDER_COLORS[props.provider.id] ?? "#6B6F7A";
  return (
    <span
      class="provider-icon-tile"
      style={{ background: color, width: `${sz}px`, height: `${sz}px`, "border-radius": `${Math.round(sz * 0.22)}px` }}

    >
      <img src={props.provider.iconUrl} alt={props.provider.label} />
    </span>
  );
}

// ── stepper ───────────────────────────────────────────────────────────────────

export function Stepper(props: { step: Step }) {
  const steps: { id: Step; label: string }[] = [
    { id: "provider", label: "Provider" },
    { id: "account-setup", label: "Account" },
  ];
  const idx = () => steps.findIndex((s) => s.id === props.step);

  return (
    <div class="onboarding-stepper">
      <For each={steps}>
        {(s, i) => (
          <>
            <div class="step-item">
              <div class={`step-circle ${i() < idx() ? "done" : i() === idx() ? "active" : ""}`}>
                <Show when={i() < idx()} fallback={i() + 1}>
                  <IconCheck size={12} />
                </Show>
              </div>
              <span class={`step-label ${i() < idx() ? "done" : i() === idx() ? "active" : ""}`}>
                {s.label}
              </span>
            </div>
            <Show when={i() < steps.length - 1}>
              <div class={`step-connector ${i() < idx() ? "done" : ""}`} />
            </Show>
          </>
        )}
      </For>
    </div>
  );
}

// ── labeled field ─────────────────────────────────────────────────────────────

export function LabeledField(props: {
  label: string;
  optional?: boolean;
  placeholder: string;
  value: string;
  onInput: (v: string) => void;
  type?: string;
  disabled?: boolean;
}) {
  return (
    <div>
      <div class="field-label">
        {props.label}
        <Show when={props.optional}>
          <span class="field-optional">(optional)</span>
        </Show>
      </div>
      <input
        class="field"
        type={props.type ?? "text"}
        placeholder={props.placeholder}
        value={props.value}
        disabled={props.disabled}
        onInput={(e) => props.onInput(e.currentTarget.value)}
      />
    </div>
  );
}

export function Field(props: {
  placeholder: string;
  value: string;
  onInput: (v: string) => void;
  type?: string;
  disabled?: boolean;
}) {
  return (
    <input
      class="field"
      type={props.type ?? "text"}
      placeholder={props.placeholder}
      value={props.value}
      disabled={props.disabled}
      onInput={(e) => props.onInput(e.currentTarget.value)}
    />
  );
}
