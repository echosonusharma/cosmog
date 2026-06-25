import { createSignal, createResource, createEffect, onCleanup } from "solid-js";
import Browse from "./Browse";
import Transfers from "./Transfers";
import Settings from "./Settings";
import Logs from "./Logs";
import {
  currentView,
  setAccounts, accounts, browseState, selectAccount,
  setSidebarBuckets,
} from "../state/app";
import { listAccounts } from "../api/accounts";
import { listBuckets } from "../api/buckets";
import { listTransfers } from "../api/transfers";
import { getSettings } from "../api/settings";
import { setTheme } from "../state/theme";
import { Sidebar } from "./mainapp/Sidebar";

// ── main app ──────────────────────────────────────────────────────────────────

export default function MainApp() {
  const [collapsed, setCollapsed] = createSignal(false);
  const [activeCount, setActiveCount] = createSignal(0);

  const [accountsData] = createResource(listAccounts);
  const [settings] = createResource(getSettings);

  createEffect(() => {
    const list = accountsData();
    if (list) {
      setAccounts(list);
      // auto-select first account if none selected
      if (!browseState.accountId && list.length > 0) {
        selectAccount(list[0].id);
      }
    }
  });

  createEffect(() => {
    const s = settings();
    if (s) setTheme((s.theme as any) ?? "system");
  });

  // load buckets for active account
  createEffect(() => {
    const id = browseState.accountId;
    if (!id) { setSidebarBuckets([]); return; }
    listBuckets(id)
      .then((b) => setSidebarBuckets(b))
      .catch(() => setSidebarBuckets([]));
  });

  // poll active transfer count for badge
  async function refreshCount() {
    try {
      const list = await listTransfers();
      setActiveCount(list.filter((t) => t.status === "active" || t.status === "pending").length);
    } catch { /* ignore */ }
  }
  refreshCount();
  const countTimer = setInterval(refreshCount, 3000);
  onCleanup(() => clearInterval(countTimer));

  const activeAccount = () => accounts().find((a) => a.id === browseState.accountId) ?? null;
  const defaultDownloadDir = () => settings()?.default_download_dir ?? "~/Downloads";

  return (
    <div class="app-shell">
      <Sidebar
        collapsed={collapsed()}
        onCollapse={() => setCollapsed(true)}
        activeAccount={activeAccount()}
        activeCount={activeCount()}
      />

      <main class="content-area">
        <div class="view-slot" style={{ display: currentView() === "browse" ? "flex" : "none" }}>
          <Browse defaultDownloadDir={defaultDownloadDir()} />
        </div>
        <div class="view-slot" style={{ display: currentView() === "transfers" ? "flex" : "none" }}>
          <Transfers />
        </div>
        <div class="view-slot" style={{ display: currentView() === "settings" ? "flex" : "none" }}>
          <Settings />
        </div>
        <div class="view-slot" style={{ display: currentView() === "logs" ? "flex" : "none" }}>
          <Logs />
        </div>
      </main>
    </div>
  );
}
