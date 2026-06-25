import { Show, For } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import {
  currentView, setCurrentView,
  accounts, browseState, selectAccount, navigateToBucket,
  sidebarBuckets,
  setOpenAddAccount,
} from "../../state/app";
import { providerLabel } from "../../providers";
import {
  IconBrowse, IconTransfer, IconSettings,
  IconSidebar, IconPlus, IconActivity, IconBucket,
} from "../../utils/icons";
import type { JSX } from "solid-js";
import type { View } from "../../state/app";
import type { Account } from "../../types";
import { ProviderTile } from "./ProviderTile";

// ── nav definition ────────────────────────────────────────────────────────────

const NAV: { view: View; label: string; icon: () => JSX.Element }[] = [
  { view: "browse",    label: "Browser",   icon: () => <IconBrowse size={16} /> },
  { view: "transfers", label: "Transfers", icon: () => <IconTransfer size={16} /> },
  { view: "logs",      label: "Logs",      icon: () => <IconActivity size={16} /> },
  { view: "settings",  label: "Settings",  icon: () => <IconSettings size={16} /> },
];

export function Sidebar(props: {
  collapsed: boolean;
  onCollapse: () => void;
  activeAccount: Account | null;
  activeCount: number;
}) {
  return (
    <aside class={`sidebar ${props.collapsed ? "collapsed" : ""}`}>

      {/* account header */}
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
            <button class="collapse-btn" onClick={props.onCollapse} title="Collapse sidebar">
              <IconSidebar size={15} />
            </button>
          </div>
        </Show>
        <Show when={props.collapsed}>
          <div class="sidebar-account-pill" style="justify-content:center">
            <Show when={props.activeAccount}
                  fallback={
                    <img src="/app-icon.svg" width="28" height="28" class="app-icon-img" alt="Cosmog" />
                  }>
              <ProviderTile account={props.activeAccount!} />
            </Show>
          </div>
        </Show>
      </div>

      <div class="sidebar-body">
        {/* nav */}
        <For each={NAV}>
          {(item) => (
            <button
              class={`sidebar-item ${currentView() === item.view ? "active" : ""}`}
              onClick={() => setCurrentView(item.view)}
              title={props.collapsed ? item.label : undefined}
            >
              <span class="sidebar-item-icon">{item.icon()}</span>
              <Show when={!props.collapsed}>
                <span class="sidebar-item-label">{item.label}</span>
                <Show when={item.view === "transfers" && props.activeCount > 0}>
                  <span class="sidebar-item-badge">{props.activeCount}</span>
                </Show>
              </Show>
            </button>
          )}
        </For>

        {/* buckets */}
        <Show when={!props.collapsed && sidebarBuckets().length > 0}>
          <div class="sidebar-group">
            <div class="sidebar-group-header">
              Buckets
              <span class="sidebar-group-count">{sidebarBuckets().length}</span>
            </div>
            <For each={sidebarBuckets()}>
              {(b) => (
                <button
                  class={`sidebar-bucket-item ${browseState.bucket === b.name && browseState.accountId === (props.activeAccount?.id ?? "") ? "active" : ""}`}
                  onClick={() => {
                    const id = browseState.accountId;
                    if (id) navigateToBucket(id, b.name);
                  }}
                  title={b.name}
                >
                  <span class="sidebar-bucket-icon">
                    <IconBucket size={13} />
                  </span>
                  <span class="sidebar-bucket-name">{b.name}</span>
                </button>
              )}
            </For>
          </div>
        </Show>

        {/* accounts */}
        <Show when={!props.collapsed && accounts().length > 0}>
          <div class="sidebar-group">
            <div class="sidebar-group-header">
              Accounts
              <span class="sidebar-group-count">{accounts().length}</span>
            </div>
            <For each={accounts()}>
              {(a) => (
                <button
                  class={`sidebar-account-item ${browseState.accountId === a.id ? "active" : ""}`}
                  onClick={() => selectAccount(a.id)}
                  title={a.name}
                >
                  <ProviderTile account={a} size="small" />
                  <span class="sidebar-account-item-name">{a.name}</span>
                  <Show when={browseState.accountId === a.id}>
                    <span class="sidebar-active-dot" />
                  </Show>
                </button>
              )}
            </For>
            <button class="sidebar-add-btn" onClick={() => { setOpenAddAccount(true); setCurrentView("settings"); }}>
              <IconPlus size={12} />
              Add account
            </button>
          </div>
        </Show>
      </div>

      {/* devtools button — dev builds only */}
      <Show when={import.meta.env.DEV}>
        <button
          class="sidebar-devtools-btn"
          title="Open DevTools (F12)"
          onClick={() => invoke("open_devtools").catch(() => {})}
        >
          {"{ }"}
        </button>
      </Show>
    </aside>
  );
}
