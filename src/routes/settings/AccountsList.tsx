import { createSignal, createEffect, onMount, For, Show } from "solid-js";
import { deleteAccount } from "../../api/accounts";
import { accounts, openAddAccount, setOpenAddAccount, bumpAccountsRefresh } from "../../state/app";
import { toast } from "../../state/toast";
import { t } from "../../i18n";
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
    // Always prompt: removal also destroys encryption keys, which is unrecoverable.
    const ok = await confirmDialog({
      title: t("settings.accounts.removeTitle"),
      body: t("settings.accounts.removeBody", { name }),
      confirmLabel: t("common.remove"),
      danger: true,
    });
    if (!ok) return;
    try { await deleteAccount(id); bumpAccountsRefresh(); toast.ok(t("settings.accounts.removed"), t("settings.accounts.removedDetail", { name })); }
    catch (e) { toast.err(e); }
  }

  return (
    <div class="settings-section">
      <div class="settings-section-title">
        <span>{t("settings.accounts.title")}</span>
        <button class="btn-ghost" onClick={() => (showAdd() ? closeForm() : openAdd())}>
          {showAdd() ? t("common.cancel") : t("settings.accounts.add")}
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
            fallback={<div class="empty-state empty-state-accounts">{t("settings.accounts.empty")}</div>}>
        <div class="account-rows">
          <For each={accounts()}>
            {(a) => (
              <div class="account-row-item">
                <ProviderIcon account={a} size={32} />
                <div class="account-row-info">
                  <span class="account-name">
                    {a.name}
                    <Show when={a.needs_reauth}>
                      <span class="account-reauth-badge">{t("settings.accounts.reconnect")}</span>
                    </Show>
                  </span>
                  <span class="account-meta">
                    <Show when={a.needs_reauth}
                          fallback={<>{providerLabel(a)} · {a.region}{a.endpoint ? ` · ${a.endpoint}` : ""}</>}>
                      {t("settings.accounts.missingCreds")}
                    </Show>
                  </span>
                </div>
                <button type="button" class="icon-btn"
                        aria-label={t("settings.accounts.edit", { name: a.name })}
                        onClick={() => openEdit(a)}><IconEdit size={15} /></button>
                <button type="button" class="icon-btn danger"
                        aria-label={t("common.remove")}
                        onClick={() => handleDelete(a.id, a.name)}><IconX size={15} /></button>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
