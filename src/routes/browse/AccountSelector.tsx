import { For } from "solid-js";
import { accounts, setBrowseState } from "../../state/app";
import { ProviderIcon, providerLabel } from "../../utils/icons";

// ── account picker ────────────────────────────────────────────────────────────

export function AccountSelector() {
  return (
    <div class="account-selector">
      <div class="section-title" style="margin-bottom:12px">Select account</div>
      <div class="account-list">
        <For each={accounts()}>
          {(acct) => (
            <button class="account-card"
                    onClick={() => setBrowseState({ accountId: acct.id, bucket: null, prefix: "" })}>
              <ProviderIcon account={acct} size={36} />
              <div class="account-info">
                <span class="account-name">{acct.name}</span>
                <span class="account-meta">{providerLabel(acct)} · {acct.region}</span>
              </div>
            </button>
          )}
        </For>
      </div>
    </div>
  );
}
