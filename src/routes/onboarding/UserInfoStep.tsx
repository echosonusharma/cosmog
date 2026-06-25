import { createSignal } from "solid-js";
import { LabeledField } from "./shared";

// ── step 1: user info ─────────────────────────────────────────────────────────

export function UserInfoStep(props: { onNext: (name: string, email: string, org: string) => void }) {
  const [name, setName] = createSignal("");
  const [email, setEmail] = createSignal("");
  const [org, setOrg] = createSignal("");

  function submit(e: Event) {
    e.preventDefault();
    if (name().trim()) props.onNext(name().trim(), email().trim(), org().trim());
  }

  return (
    <form class="card" onSubmit={submit}>
      <div>
        <div class="card-welcome-title">Welcome to Cosmog</div>
        <div class="card-welcome-sub">A fast, native client for S3-compatible storage. Everything stays on your device.</div>
      </div>
      <div class="fields">
        <LabeledField label="Your name" placeholder="Ada Lovelace" value={name()} onInput={setName} />
        <LabeledField label="Email" optional placeholder="ada@example.com" value={email()} onInput={setEmail} type="email" />
        <LabeledField label="Organization" optional placeholder="Analytical Engine Co." value={org()} onInput={setOrg} />
      </div>
      <button type="submit" class="btn-primary" disabled={!name().trim()}>
        Continue
      </button>
    </form>
  );
}
