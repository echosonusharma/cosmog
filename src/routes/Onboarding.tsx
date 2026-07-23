import { createSignal, Show } from "solid-js";
import { type ProviderDef } from "../providers";
import { type Step, Stepper } from "./onboarding/shared";
import { ProviderStep } from "./onboarding/ProviderStep";
import { AccountSetupStep } from "./onboarding/AccountSetupStep";

// ── root onboarding ───────────────────────────────────────────────────────────

export default function Onboarding(props: { onDone: () => void }) {
  const [step, setStep] = createSignal<Step>("provider");
  const [provider, setProvider] = createSignal<ProviderDef | null>(null);

  return (
    <div class="onboarding">
      <Stepper step={step()} />

      <Show when={step() === "provider"}>
        <ProviderStep
          onNext={(p) => {
            setProvider(p);
            setStep("account-setup");
          }}
        />
      </Show>
      <Show when={step() === "account-setup" && provider() !== null}>
        <AccountSetupStep
          provider={provider()!}
          onBack={() => setStep("provider")}
          onDone={props.onDone}
        />
      </Show>

      <div class="onboarding-footer">
        Multiple providers
        <span>·</span>
        No telemetry
        <span>·</span>
        Open source
      </div>
    </div>
  );
}
