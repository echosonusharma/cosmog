import { createSignal, createEffect, onMount, For, Show } from "solid-js";
import { deleteAccount } from "../../api/accounts";
import { accounts, openAddAccount, setOpenAddAccount, bumpAccountsRefresh } from "../../state/app";
import { toast } from "../../state/toast";
import { confirmDialog } from "../../state/confirm";
import { ProviderIcon, providerLabel, IconX, IconEdit } from "../../utils/icons";
import type { Account } from "../../types";
import { AddAccountForm } from "./AddAccountForm";

// ── accounts list ─────────────────────────────────────────────────────────────

export function AccountsList() {
  const [showAdd, setShowAdd] = createSignal(false);
  const [editing, setEditing] = createSignal<Account | null>(null);

  // Sidebar "Add account" button sets this signal → auto-open the form
  createEffect(() => {
    if (openAddAccount()) {
      setShowAdd(true);
      setOpenAddAccount(false);
    }
  });

  // Refresh whenever Settings tab opens so the list is never stale.
  onMount(bumpAccountsRefresh);

  async function handleDelete(id: string, name: string) {
    const ok = await confirmDialog({
      title: "Remove account?",
      body: `"${name}": cached objects, transfers, and credentials will be removed. This action is irreversible.`,
      confirmLabel: "Remove",
      danger: true,
    });
    if (!ok) return;
    try { await deleteAccount(id); bumpAccountsRefresh(); toast.ok("Account removed"); }
    catch (e) { toast.err(e); }
  }

  return (
    <div class="settings-section">
      <div class="settings-section-title">
        <span>Accounts</span>
        <button class="btn-ghost" onClick={() => { setEditing(null); setShowAdd((v) => !v); }}>
          {showAdd() && !editing() ? "Cancel" : "+ Add account"}
        </button>
      </div>

      <Show when={showAdd()}>
        <AddAccountForm
          editing={editing() ?? undefined}
          onDone={() => { setShowAdd(false); setEditing(null); bumpAccountsRefresh(); }}
          onCancel={() => { setShowAdd(false); setEditing(null); }}
        />
      </Show>

      <Show when={accounts().length > 0}
            fallback={<div class="empty-state empty-state-accounts">No accounts</div>}>
        <div class="account-rows">
          <For each={accounts()}>
            {(a) => (
              <div class="account-row-item">
                <ProviderIcon account={a} size={32} />
                <div class="account-row-info">
                  <span class="account-name">{a.name}</span>
                  <span class="account-meta">
                    {providerLabel(a)} · {a.region}
                    {a.endpoint ? ` · ${a.endpoint}` : ""}
                  </span>
                </div>
                <button class="icon-btn" title="Edit"
                        onClick={() => { setEditing(a); setShowAdd(true); }}><IconEdit size={15} /></button>
                <button class="icon-btn danger" title="Remove"
                        onClick={() => handleDelete(a.id, a.name)}><IconX size={15} /></button>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
