import { createSignal, createEffect, onMount, For, Show } from "solid-js";
import { deleteAccount } from "../../api/accounts";
import { accounts, openAddAccount, setOpenAddAccount, bumpAccountsRefresh } from "../../state/app";
import { toast } from "../../state/toast";
import { confirmDialog } from "../../state/confirm";
import { ProviderIcon, providerLabel, IconX, IconEdit } from "../../utils/icons";
import type { Account } from "../../types";
import { AddAccountForm } from "./AddAccountForm";

export function AccountsList() {
  const [showAdd, setShowAdd] = createSignal(false);
  const [editing, setEditing] = createSignal<Account | null>(null);
  let formAnchor: HTMLDivElement | undefined;

  function scrollToForm() {
    queueMicrotask(() => formAnchor?.scrollIntoView({ behavior: "smooth", block: "nearest" }));
  }

  function openAdd() {
    setEditing(null);
    setShowAdd(true);
    scrollToForm();
  }

  function openEdit(account: Account) {
    setEditing(account);
    setShowAdd(true);
    scrollToForm();
  }

  function closeForm() {
    setShowAdd(false);
    setEditing(null);
  }

  // Sidebar "Add account" button sets this signal → auto-open the form
  createEffect(() => {
    if (openAddAccount()) {
      openAdd();
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
    try { await deleteAccount(id); bumpAccountsRefresh(); toast.ok("Account removed", `"${name}" and its cached data were deleted`); }
    catch (e) { toast.err(e); }
  }

  return (
    <div class="settings-section">
      <div class="settings-section-title">
        <span>Accounts</span>
        <button class="btn-ghost" onClick={() => (showAdd() ? closeForm() : openAdd())}>
          {showAdd() ? "Cancel" : "+ Add account"}
        </button>
      </div>

      <Show when={showAdd()}>
        <Show when={editing()?.id ?? "new"} keyed>
          {(_id) => (
            <div ref={formAnchor}>
              <AddAccountForm
                editing={editing() ?? undefined}
                onDone={() => { closeForm(); bumpAccountsRefresh(); }}
                onCancel={closeForm}
              />
            </div>
          )}
        </Show>
      </Show>

      <Show when={accounts().length > 0}
            fallback={<div class="empty-state empty-state-accounts">No accounts</div>}>
        <div class="account-rows">
          <For each={accounts()}>
            {(a) => (
              <div class="account-row-item">
                <ProviderIcon account={a} size={32} />
                <div class="account-row-info">
                  <span class="account-name">
                    {a.name}
                    <Show when={a.needs_reauth}>
                      <span class="account-reauth-badge">Reconnect</span>
                    </Show>
                  </span>
                  <span class="account-meta">
                    <Show when={a.needs_reauth}
                          fallback={<>{providerLabel(a)} · {a.region}{a.endpoint ? ` · ${a.endpoint}` : ""}</>}>
                      Credentials missing on this device. Edit to re-enter the secret key.
                    </Show>
                  </span>
                </div>
                <button type="button" class="icon-btn"
                        aria-label={`Edit ${a.name}`}
                        onClick={() => openEdit(a)}><IconEdit size={15} /></button>
                <button type="button" class="icon-btn danger"
                        onClick={() => handleDelete(a.id, a.name)}><IconX size={15} /></button>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
