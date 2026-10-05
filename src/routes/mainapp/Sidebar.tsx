import { Show, For, createSignal, createMemo, createResource } from "solid-js";
import { Portal } from "solid-js/web";
import { invoke } from "@tauri-apps/api/core";
import {
  currentView, setCurrentView,
  accounts, browseState, selectAccount, navigateToBucket,
  sidebarBuckets, sidebarBucketsError,
  setOpenAddAccount,
} from "../../state/app";
import { providerLabel } from "../../providers";
import {
  IconDashboard, IconBrowse, IconTransfer, IconSettings,
  IconSidebar, IconPlus, IconActivity, IconBucket, IconSearch, IconX, IconBug, IconLock, IconEye, IconMcp,
} from "../../utils/icons";
import { listEncryptedBuckets } from "../../api/encryption";
import { IS_MOBILE_OS } from "../../utils/notify";
import type { JSX } from "solid-js";
import type { View } from "../../state/app";
import type { Account } from "../../types";
import { ProviderTile } from "./ProviderTile";
import { BugReportModal } from "./BugReportModal";
import { t } from "../../i18n";

const NAV: { view: View; label: () => string; icon: () => JSX.Element; desktopOnly?: boolean; beta?: boolean }[] = [
  { view: "dashboard", label: () => t("shell.nav.dashboard"), icon: () => <IconDashboard size={16} /> },
  { view: "browse",    label: () => t("shell.nav.browse"),    icon: () => <IconBrowse size={16} /> },
  { view: "transfers", label: () => t("shell.nav.transfers"), icon: () => <IconTransfer size={16} /> },
  { view: "night-watcher", label: () => t("shell.nav.nightWatcher"), icon: () => <IconEye size={16} />, beta: true },
  { view: "mcp",       label: () => t("shell.nav.mcp"),       icon: () => <IconMcp size={16} />, desktopOnly: true, beta: true },
  { view: "logs",      label: () => t("shell.nav.logs"),      icon: () => <IconActivity size={16} /> },
  { view: "settings",  label: () => t("shell.nav.settings"),  icon: () => <IconSettings size={16} /> },
];

export function Sidebar(props: {
  collapsed: boolean;
  onCollapse: () => void;
  onExpand: () => void;
  activeAccount: Account | null;
  activeCount: number;
  mobileOpen?: boolean;
  onCloseMobile?: () => void;
}) {
  const [bucketFilter, setBucketFilter] = createSignal("");
  const [showBugModal, setShowBugModal] = createSignal(false);
  const filteredBuckets = createMemo(() => {
    const q = bucketFilter().trim().toLowerCase();
    const all = sidebarBuckets();
    if (!q) return all;
    return all.filter((b) => b.name.toLowerCase().includes(q));
  });
  // Encrypted bucket names for the active account. Refetches when the account
  // switches. Errors swallowed so a keychain hiccup can never crash the sidebar.
  const [encSet] = createResource<Set<string>, string | null>(
    () => props.activeAccount?.id ?? null,
    async (id) => (id ? new Set(await listEncryptedBuckets(id).catch(() => [] as string[])) : new Set<string>()),
  );
  return (
    <aside class={`sidebar ${props.collapsed ? "collapsed" : ""}`}>

      <div class="sidebar-account-header">
        <Show when={!props.collapsed}>
          <div class="sidebar-account-pill">
            <Show when={props.activeAccount}
                  fallback={
                    <img src="/app-icon.svg" width="28" height="28" class="app-icon-img" alt="Cosmog" />
                  }>
              <ProviderTile account={props.activeAccount!} />
            </Show>
            <div class="sidebar-account-info">
              <div class="sidebar-account-name">
                {props.activeAccount?.name ?? "Cosmog"}
              </div>
              <Show when={props.activeAccount}>
                <div class="sidebar-account-provider">
                  {providerLabel(props.activeAccount!)}
                </div>
              </Show>
            </div>
            <button class="collapse-btn" onClick={props.onCollapse}>
              <IconSidebar size={15} />
            </button>
          </div>
        </Show>
        <Show when={props.collapsed}>
          <button
            class="sidebar-account-pill collapsed-expand"
            onClick={props.onExpand}

          >
            <Show when={props.activeAccount}
                  fallback={
                    <img src="/app-icon.svg" width="28" height="28" class="app-icon-img" alt="Cosmog" />
                  }>
              <ProviderTile account={props.activeAccount!} />
            </Show>
          </button>
        </Show>
      </div>

      <div class="sidebar-body">
        <For each={NAV.filter((n) => !n.desktopOnly || !IS_MOBILE_OS)}>
          {(item) => (
            <button
              class={`sidebar-item ${currentView() === item.view ? "active" : ""}`}
              onClick={() => { setCurrentView(item.view); props.onCloseMobile?.(); }}

              aria-label={item.label()}
            >
              <span class="sidebar-item-icon">{item.icon()}</span>
              <Show when={!props.collapsed}>
                <span class="sidebar-item-label">{item.label()}</span>
                <Show when={item.beta}>
                  <span class="sidebar-item-beta">{t("shell.nav.beta")}</span>
                </Show>
                <Show when={item.view === "transfers" && props.activeCount > 0}>
                  <span class="sidebar-item-badge">{props.activeCount}</span>
                </Show>
              </Show>
            </button>
          )}
        </For>

        <Show when={!props.collapsed && sidebarBuckets().length > 0}>
          <div class="sidebar-group sidebar-group-flex">
            <div class="sidebar-group-header">
              {t("shell.sidebar.buckets")}
              <span class="sidebar-group-count">{filteredBuckets().length}{bucketFilter() ? `/${sidebarBuckets().length}` : ""}</span>
            </div>
            <Show when={sidebarBuckets().length > 8}>
              <div class="sidebar-bucket-search">
                <IconSearch size={11} class="sidebar-bucket-search-icon" />
                <input
                  class="sidebar-bucket-search-input"
                  placeholder={t("shell.sidebar.filterBuckets")}
                  value={bucketFilter()}
                  onInput={(e) => setBucketFilter(e.currentTarget.value)}
                />
                <Show when={bucketFilter()}>
                  <button class="sidebar-bucket-search-clear" onClick={() => setBucketFilter("")}><IconX size={10} /></button>
                </Show>
              </div>
            </Show>
            <div class="sidebar-group-list">
              <For each={filteredBuckets()}>
                {(b) => (
                  <button
                    class={`sidebar-bucket-item ${browseState.bucket === b.name && browseState.accountId === (props.activeAccount?.id ?? "") ? "active" : ""}`}
                    onClick={() => {
                      const id = browseState.accountId;
                      if (id) navigateToBucket(id, b.name);
                      props.onCloseMobile?.();
                    }}
                    aria-label={b.name}

                  >
                    <span class="sidebar-bucket-icon" classList={{ "is-encrypted": !!encSet()?.has(b.name) }}>
                      <Show when={encSet()?.has(b.name)} fallback={<IconBucket size={13} />}>
                        <IconLock size={13} />
                      </Show>
                    </span>
                    <span class="sidebar-bucket-name">{b.name}</span>
                  </button>
                )}
              </For>
            </div>
          </div>
        </Show>
        <Show when={!props.collapsed && sidebarBucketsError()}>
          <div class="sidebar-buckets-error" title={sidebarBucketsError() ?? ""}>
            {t("shell.sidebar.bucketsError", { error: sidebarBucketsError() ?? "" })}
          </div>
        </Show>
      </div>

      <Show when={!props.collapsed && accounts().length > 0}>
        <div class="sidebar-accounts">
          <div class="sidebar-group-header">
            {t("shell.sidebar.accounts")}
            <span class="sidebar-group-count">{accounts().length}</span>
          </div>
          <For each={accounts()}>
            {(a) => (
              <button
                class={`sidebar-account-item ${browseState.accountId === a.id ? "active" : ""}`}
                onClick={() => { selectAccount(a.id); props.onCloseMobile?.(); }}
                aria-label={a.name}

              >
                <ProviderTile account={a} size="small" />
                <span class="sidebar-account-item-name">{a.name}</span>
                <Show when={browseState.accountId === a.id}>
                  <span class="sidebar-active-dot" />
                </Show>
              </button>
            )}
          </For>
          <button class="sidebar-add-btn" onClick={() => { setOpenAddAccount(true); setCurrentView("settings"); props.onCloseMobile?.(); }}>
            <IconPlus size={12} />
            {t("shell.sidebar.addAccount")}
          </button>
        </div>
      </Show>

      <button
        class="sidebar-bug-btn"

        onClick={() => setShowBugModal(true)}
      >
        <span class="sidebar-bug-icon"><IconBug size={14} /></span>
        <Show when={!props.collapsed}>
          <span class="sidebar-bug-label">{t("shell.sidebar.reportBug")}</span>
        </Show>
      </button>

      <Show when={import.meta.env.DEV}>
        <button
          class="sidebar-devtools-btn"

          onClick={() => invoke("open_devtools").catch(() => {})}
        >
          {"{ }"}
        </button>
      </Show>

      <Show when={showBugModal()}>
        <Portal>
          <BugReportModal onClose={() => setShowBugModal(false)} />
        </Portal>
      </Show>
    </aside>
  );
}
