import { createSignal, Show } from "solid-js";
import { saveProfile } from "../state/profile";
import { type ProviderDef } from "../providers";
import { type Step, Stepper } from "./onboarding/shared";
import { UserInfoStep } from "./onboarding/UserInfoStep";
import { ProviderStep } from "./onboarding/ProviderStep";
import { AccountSetupStep } from "./onboarding/AccountSetupStep";

// ── root onboarding ───────────────────────────────────────────────────────────

export default function Onboarding(props: { onDone: () => void }) {
  const [step, setStep] = createSignal<Step>("user-info");
  const [provider, setProvider] = createSignal<ProviderDef | null>(null);

  return (
    <div class="onboarding">
      <Stepper step={step()} />

      <Show when={step() === "user-info"}>
        <UserInfoStep
          onNext={(name, email, org) => {
            saveProfile({ name, email, org });
            setStep("provider");
          }}
        />
      </Show>
      <Show when={step() === "provider"}>
        <ProviderStep
          onBack={() => setStep("user-info")}
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
        Cosmog runs locally
        <span>·</span>
        No telemetry
        <span>·</span>
        Single native binary
      </div>
    </div>
  );
}
