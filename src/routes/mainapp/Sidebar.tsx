import { Show, For, createSignal, createMemo, createResource } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { getVersion, getTauriVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  currentView, setCurrentView,
  accounts, browseState, selectAccount, navigateToBucket,
  sidebarBuckets,
  setOpenAddAccount,
} from "../../state/app";
import { providerLabel } from "../../providers";
import {
  IconBrowse, IconTransfer, IconSettings,
  IconSidebar, IconPlus, IconActivity, IconBucket, IconSearch, IconX, IconBug, IconLock,
} from "../../utils/icons";
import { listEncryptedBuckets } from "../../api/encryption";
import type { JSX } from "solid-js";
import type { View } from "../../state/app";
import type { Account } from "../../types";
import { ProviderTile } from "./ProviderTile";

const GITHUB_ISSUES_URL = "https://github.com/echosonusharma/cosmog/issues";

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function loadBugInfo() {
  const appVersion = isTauri ? await getVersion() : "unknown";
  const tauriVersion = isTauri ? await getTauriVersion() : "unknown";
  const ua = navigator.userAgent;
  const platform = navigator.platform;
  const screen = `${window.screen.width}x${window.screen.height} @${window.devicePixelRatio}x`;
  const locale = navigator.language;
  return { appVersion, tauriVersion, ua, platform, screen, locale };
}

function BugReportModal(props: { onClose: () => void }) {
  const [info] = createResource(loadBugInfo);
  const [copied, setCopied] = createSignal(false);

  const infoText = () => {
    const d = info();
    if (!d) return "Loading…";
    return [
      `App Version:   ${d.appVersion}`,
      `Tauri Version: ${d.tauriVersion}`,
      `Platform:      ${d.platform}`,
      `Screen:        ${d.screen}`,
      `Locale:        ${d.locale}`,
      `User Agent:    ${d.ua}`,
    ].join("\n");
  };

  function copyInfo() {
    navigator.clipboard.writeText(infoText()).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  }

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="bug-modal" onClick={(e) => e.stopPropagation()}>
        <div class="bug-modal-header">
          <span class="bug-modal-title">
            <span class="bug-modal-icon-wrap"><IconBug size={15} /></span>
            Report a Bug
          </span>
          <button class="cd-modal-close" onClick={props.onClose} aria-label="Close">✕</button>
        </div>

        <div class="bug-modal-body">
          <p class="bug-modal-desc">
            Found something broken? Open an issue on GitHub and include the system info below so we can reproduce it faster.
          </p>

          <div class="bug-info-section">
            <div class="bug-info-header">
              <span class="bug-info-label">System Info</span>
              <button class="bug-copy-btn" onClick={copyInfo}>
                {copied() ? "Copied!" : "Copy"}
              </button>
            </div>
            <Show when={info()} fallback={<div class="bug-info-loading">Loading…</div>}>
              <dl class="bug-info-grid">
                <dt>App Version</dt><dd>{info()!.appVersion}</dd>
                <dt>Tauri Version</dt><dd>{info()!.tauriVersion}</dd>
                <dt>Platform</dt><dd>{info()!.platform}</dd>
                <dt>Screen</dt><dd>{info()!.screen}</dd>
                <dt>Locale</dt><dd>{info()!.locale}</dd>
                <dt>User Agent</dt><dd>{info()!.ua}</dd>
              </dl>
            </Show>
          </div>

          <div class="bug-steps">
            <div class="bug-steps-title">How to report</div>
            <ol class="bug-steps-list">
              <li>Click <strong>Copy</strong> above to copy system info</li>
              <li>Click <strong>Open Issue</strong> below to go to GitHub</li>
              <li>Describe the bug and paste the copied info in the <em>Additional context</em> section</li>
            </ol>
          </div>

          <button
            class="bug-open-btn"
            onClick={() => openUrl(GITHUB_ISSUES_URL).catch(() => {})}
          >
            Open Issue on GitHub
          </button>
        </div>
      </div>
    </div>
  );
}

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
        {/* nav */}
        <For each={NAV}>
          {(item) => (
            <button
              class={`sidebar-item ${currentView() === item.view ? "active" : ""}`}
              onClick={() => { setCurrentView(item.view); props.onCloseMobile?.(); }}

              aria-label={item.label}
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
          <div class="sidebar-group sidebar-group-flex">
            <div class="sidebar-group-header">
              Buckets
              <span class="sidebar-group-count">{filteredBuckets().length}{bucketFilter() ? `/${sidebarBuckets().length}` : ""}</span>
            </div>
            <Show when={sidebarBuckets().length > 8}>
              <div class="sidebar-bucket-search">
                <IconSearch size={11} class="sidebar-bucket-search-icon" />
                <input
                  class="sidebar-bucket-search-input"
                  placeholder="Filter buckets…"
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
            <button class="sidebar-add-btn" onClick={() => { setOpenAddAccount(true); setCurrentView("settings"); }}>
              <IconPlus size={12} />
              Add account
            </button>
          </div>
        </Show>
      </div>

      {/* bug report button */}
      <button
        class="sidebar-bug-btn"

        onClick={() => setShowBugModal(true)}
      >
        <span class="sidebar-bug-icon"><IconBug size={14} /></span>
        <Show when={!props.collapsed}>
          <span class="sidebar-bug-label">Report Bug</span>
        </Show>
      </button>

      {/* devtools button — dev builds only */}
      <Show when={import.meta.env.DEV}>
        <button
          class="sidebar-devtools-btn"

          onClick={() => invoke("open_devtools").catch(() => {})}
        >
          {"{ }"}
        </button>
      </Show>

      <Show when={showBugModal()}>
        <BugReportModal onClose={() => setShowBugModal(false)} />
      </Show>
    </aside>
  );
}
