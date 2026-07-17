import { createSignal, createResource, createEffect, onCleanup } from "solid-js";
import Browse from "./Browse";
import Transfers from "./Transfers";
import Settings from "./Settings";
import Logs from "./Logs";
import {
  currentView,
  setAccounts, accounts, browseState, setBrowseState, selectAccount,
  setSidebarBuckets, bucketsRefreshTick, accountsRefreshTick,
  bumpBucketsRefresh, bumpAccountsRefresh,
} from "../state/app";
import { listAccounts } from "../api/accounts";
import { listBuckets } from "../api/buckets";
import { listTransfers } from "../api/transfers";
import { getSettings } from "../api/settings";
import { setTheme } from "../state/theme";
import { notify, notifId } from "../utils/notify";
import { Sidebar } from "./mainapp/Sidebar";
import { MobileHeader } from "./mainapp/MobileHeader";
import { isMobile } from "../utils/breakpoint";
import { Show } from "solid-js";

// ── main app ──────────────────────────────────────────────────────────────────

export default function MainApp() {
  const [collapsed, setCollapsed] = createSignal(false);
  const [activeCount, setActiveCount] = createSignal(0);
  const [drawerOpen, setDrawerOpen] = createSignal(false);

  const openDrawer = () => setDrawerOpen(true);
  const closeDrawer = () => setDrawerOpen(false);

  const [accountsData] = createResource(accountsRefreshTick, listAccounts);
  const [settings] = createResource(getSettings);

  createEffect(() => {
    const list = accountsData();
    if (!list) return;
    setAccounts(list);
    const currentId = browseState.accountId;
    const stillExists = currentId && list.some((a) => a.id === currentId);
    if (!stillExists && list.length > 0) {
      selectAccount(list[0].id);
    } else if (!stillExists) {
      setBrowseState({ accountId: null, bucket: null, prefix: "" });
    }
  });

  createEffect(() => {
    const s = settings();
    if (s) setTheme(s.theme ?? "system");
  });

  // load buckets for active account; refetch on global bucket refresh tick
  createEffect(() => {
    const id = browseState.accountId;
    bucketsRefreshTick();
    setSidebarBuckets([]);
    if (!id) return;
    listBuckets(id)
      .then((b) => setSidebarBuckets(b))
      .catch(() => {});
  });

  // poll active transfer count + drive system notifications.
  // Two stages per transfer: "Downloading…" / "Uploading…" on start,
  // then "Download complete" / "Upload complete" (or failed) on finish.
  // Same stable id per transfer replaces the start notification.
  const notifiedStart = new Set<string>();
  const notifiedEnd   = new Set<string>();
  let firstLoad = true;
  async function refreshCount() {
    try {
      const list = await listTransfers();
      setActiveCount(list.filter((t) => t.status === "active" || t.status === "pending").length);
      if (firstLoad) {
        for (const t of list) {
          if (t.status === "done" || t.status === "failed") notifiedEnd.add(t.id);
          if (t.status === "active" || t.status === "pending") notifiedStart.add(t.id);
        }
        firstLoad = false;
        return;
      }
      for (const t of list) {
        const name = t.key.split("/").pop() || t.key;
        const nid  = notifId(t.id);
        const dir  = t.direction === "upload" ? "Upload" : "Download";
        const verbIng = t.direction === "upload" ? "Uploading" : "Downloading";

        if ((t.status === "active" || t.status === "pending") && !notifiedStart.has(t.id)) {
          notifiedStart.add(t.id);
          notify(`${verbIng}…`, name, { id: nid, ongoing: true, autoCancel: false, silent: true });
        }

        if ((t.status === "done" || t.status === "failed") && !notifiedEnd.has(t.id)) {
          notifiedEnd.add(t.id);
          const done = t.status === "done";
          notify(done ? `${dir} complete` : `${dir} failed`, name, { id: nid, ongoing: false, autoCancel: true });
        }
      }
    } catch { /* ignore */ }
  }
  refreshCount();
  const countTimer = setInterval(refreshCount, 3000);
  onCleanup(() => clearInterval(countTimer));

  // Android/WebView backgrounds the process when the screen locks or the
  // user switches apps. In-flight network requests can be killed mid-flight
  // and the AWS SDK surfaces this as "dispatch failure". On resume, re-run
  // the queries so the UI doesn't sit on a stale error indefinitely.
  const onVis = () => {
    if (document.visibilityState === "visible") {
      bumpAccountsRefresh();
      bumpBucketsRefresh();
      refreshCount();
    }
  };
  document.addEventListener("visibilitychange", onVis);
  onCleanup(() => document.removeEventListener("visibilitychange", onVis));

  const activeAccount = () => accounts().find((a) => a.id === browseState.accountId) ?? null;
  const defaultDownloadDir = () => settings()?.default_download_dir ?? "~/Downloads";

  return (
    <div class="app-shell">
      <Show
        when={isMobile()}
        fallback={
          <Sidebar
            collapsed={collapsed()}
            onCollapse={() => setCollapsed(true)}
            onExpand={() => setCollapsed(false)}
            activeAccount={activeAccount()}
            activeCount={activeCount()}
          />
        }
      >
        <div class="sidebar-drawer" classList={{ open: drawerOpen() }}>
          <Sidebar
            collapsed={false}
            onCollapse={closeDrawer}
            onExpand={() => openDrawer()}
            activeAccount={activeAccount()}
            activeCount={activeCount()}
            mobileOpen={drawerOpen()}
            onCloseMobile={closeDrawer}
          />
        </div>
        <Show when={drawerOpen()}>
          <div class="sidebar-drawer-backdrop" onClick={closeDrawer} />
        </Show>
      </Show>

      <main class="content-area">
        <Show when={isMobile()}>
          <MobileHeader onOpenSidebar={() => openDrawer()} />
        </Show>
        <div class="view-slot" classList={{ hidden: currentView() !== "browse" }}>
          <Browse defaultDownloadDir={defaultDownloadDir()} />
        </div>
        <div class="view-slot" classList={{ hidden: currentView() !== "transfers" }}>
          <Transfers />
        </div>
        <div class="view-slot" classList={{ hidden: currentView() !== "settings" }}>
          <Settings />
        </div>
        <div class="view-slot" classList={{ hidden: currentView() !== "logs" }}>
          <Logs />
        </div>
      </main>
    </div>
  );
}
