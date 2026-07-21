import { createSignal, createResource, createEffect, onCleanup } from "solid-js";
import Browse from "./Browse";
import Transfers from "./Transfers";
import Settings from "./Settings";
import Logs from "./Logs";
import {
  currentView, setCurrentView,
  setAccounts, accounts, browseState, setBrowseState, selectAccount,
  setSidebarBuckets, bucketsRefreshTick, accountsRefreshTick,
  bumpBucketsRefresh, bumpAccountsRefresh, setActiveTransfers, goUpPrefix,
} from "../state/app";
import { useBackHandler } from "../utils/androidBack";
import { listAccounts } from "../api/accounts";
import { listBuckets } from "../api/buckets";
import { listTransfers } from "../api/transfers";
import { getSettings } from "../api/settings";
import { setTheme } from "../state/theme";
import {
  notify,
  notifId,
  onNotificationAction,
  dismissNotification,
  ensureNotificationPermission,
  IS_MOBILE_OS,
  TRANSFER_ACTION_TYPE_ID,
  TRANSFER_CANCEL_ACTION_ID,
  NOTIFICATION_TAP_ACTION_ID,
  CHANNEL_PROGRESS,
  CHANNEL_EVENTS,
  CHANNEL_ALERTS,
} from "../utils/notify";
import { errMsg } from "../utils/errors";
import { cancelTransfer } from "../api/transfers";
import { finalizeSafDownload, takeSafFinalize, discardSafDownload } from "./browse/helpers";
import { invoke } from "@tauri-apps/api/core";
import { Sidebar } from "./mainapp/Sidebar";
import { MobileHeader } from "./mainapp/MobileHeader";
import { ActiveTransfersBar } from "./mainapp/ActiveTransfersBar";
import { isMobile } from "../utils/breakpoint";
import { Show } from "solid-js";

// ── main app ──────────────────────────────────────────────────────────────────

export default function MainApp() {
  const [collapsed, setCollapsed] = createSignal(false);
  const [activeCount, setActiveCount] = createSignal(0);
  const [drawerOpen, setDrawerOpen] = createSignal(false);

  const openDrawer = () => setDrawerOpen(true);
  const closeDrawer = () => setDrawerOpen(false);

  // Shell-level Android back handling. Registered first (parent mounts before
  // children), so it sits at the bottom of the back stack — overlays and the
  // object browser get first crack. Returning false at the browse root lets
  // the OS take the back press and background/exit the app.
  useBackHandler(() => true, () => {
    if (drawerOpen()) { closeDrawer(); return true; }
    if (currentView() !== "browse") { setCurrentView("browse"); return true; }
    if (browseState.prefix) { goUpPrefix(); return true; }
    if (browseState.bucket) { setBrowseState({ bucket: null, prefix: "" }); return true; }
    return false;
  });

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
  // null = unknown; forces one unconditional sync on the first poll so a
  // service orphaned by a previous process (start refused, app killed) is
  // stopped even though this session never saw a transition to active.
  let serviceOn: boolean | null = null;
  async function refreshCount() {
    try {
      const list = await listTransfers();
      const active = list.filter((t) => t.status === "active" || t.status === "pending").length;
      setActiveCount(active);
      // Publish the live list so the sticky ActiveTransfersBar (and anything
      // else that wants live progress) doesn't have to spin up a second poll.
      setActiveTransfers(list.filter((t) => t.status === "active" || t.status === "pending"));
      // Android: keep a foreground service running while transfers are in
      // flight. Without this, backgrounding the app lets Doze/cached-process
      // reap kill in-flight requests and progress restarts from 0 on retry.
      // Only mark the state applied once the invoke succeeds so a failed
      // start/stop is retried on the next tick.
      const wantService = active > 0;
      if (wantService !== serviceOn) {
        invoke("set_transfer_service", { active: wantService })
          .then(() => { serviceOn = wantService; })
          .catch(() => {});
      }
      if (firstLoad) {
        for (const t of list) {
          if (t.status === "done" || t.status === "failed" || t.status === "canceled") notifiedEnd.add(t.id);
          if (t.status === "active" || t.status === "pending") notifiedStart.add(t.id);
        }
        firstLoad = false;
        return;
      }
      // Cleared/history-pruned transfers never come back; drop their tracker
      // entries so the sets don't grow for the whole session.
      const liveIds = new Set(list.map((t) => t.id));
      for (const id of notifiedStart) if (!liveIds.has(id)) notifiedStart.delete(id);
      for (const id of notifiedEnd) if (!liveIds.has(id)) notifiedEnd.delete(id);
      const accs = accounts();
      const accountName = (id: string) =>
        accs.find((a) => a.id === id)?.name || accs.find((a) => a.id === id)?.id || "account";

      for (const t of list) {
        const name = t.key.split("/").pop() || t.key;
        const acct = accountName(t.account_id);
        const nid  = notifId(t.id);
        const up   = t.direction === "upload";
        const dir  = up ? "Upload" : "Download";
        // Title carries the human-relevant bits (verb + filename); body pins
        // the transfer to a specific account + bucket so the user can tell
        // parallel transfers apart at a glance in the notification tray.
        // summary puts the account name in the collapsed header line, and
        // largeBody spells out the whole story in the expanded view.
        const body = `${acct} · ${t.bucket}`;
        const where = up ? `to "${t.bucket}" on ${acct}` : `from "${t.bucket}" on ${acct}`;
        const rich = (opts: Parameters<typeof notify>[2]) => ({
          id: nid,
          summary: acct,
          ...opts,
        });

        if ((t.status === "active" || t.status === "pending") && !notifiedStart.has(t.id)) {
          notifiedStart.add(t.id);
          notify(`${up ? "Uploading" : "Downloading"} ${name}`, body, rich({
            largeBody: `${up ? "Uploading" : "Downloading"} ${where}`,
            channelId: CHANNEL_PROGRESS,
            ongoing: true,
            autoCancel: false,
            silent: true,
            actionTypeId: TRANSFER_ACTION_TYPE_ID,
            extra: { transfer_id: t.id },
          }));
        }

        if (t.status === "canceled" && !notifiedEnd.has(t.id)) {
          notifiedEnd.add(t.id);
          notifiedStart.delete(t.id);
          discardSafDownload(t.id);
          // Same nid replaces the ongoing "Uploading…" entry in place.
          notify(`${dir} canceled: ${name}`, body, rich({
            largeBody: `${dir} ${where} was canceled`,
            channelId: CHANNEL_EVENTS,
            ongoing: false,
            autoCancel: true,
          }));
          continue;
        }

        if ((t.status === "done" || t.status === "failed") && !notifiedEnd.has(t.id)) {
          notifiedEnd.add(t.id);
          const done = t.status === "done";
          // A done SAF download is only really "complete" once the bytes are
          // copied out of app cache into the user-picked location; that copy
          // can take minutes for multi-GB files and can fail (revoked grant,
          // disk full). Finalize first, notify after.
          if (done && t.direction === "download") {
            const pending = takeSafFinalize(t.id);
            if (pending) {
              try {
                await finalizeSafDownload(pending.cachePath, pending.safUri);
              } catch (e) {
                // Keep the cache copy (finalize only deletes it on success)
                // so the bytes are not lost; tell the user what happened.
                notify(`Download failed: ${name}`, body, rich({
                  largeBody: `Downloaded ${where} but saving to the chosen location failed: ${errMsg(e)}`,
                  channelId: CHANNEL_ALERTS,
                  ongoing: false,
                  autoCancel: true,
                }));
                continue;
              }
            }
          }
          const title = done ? `${dir} complete: ${name}` : `${dir} failed: ${name}`;
          const largeBody = done
            ? `${up ? "Uploaded" : "Downloaded"} ${where}`
            : `${dir} ${where} failed${t.error ? `: ${t.error}` : ""}`;
          notify(title, body, rich({
            largeBody,
            channelId: done ? CHANNEL_EVENTS : CHANNEL_ALERTS,
            ongoing: false,
            autoCancel: true,
          }));
          // Failed downloads intentionally keep their SAF finalize entry and
          // 0-byte placeholder: Retry re-enqueues under a new id and
          // Transfers.retry() moves the entry over so the retried download
          // still lands at the picked location. The placeholder is deleted
          // when the transfer is canceled or cleared instead.
        }
      }
    } catch { /* ignore */ }
  }
  refreshCount();
  // Poll fast enough that speed and ETA feel live; the queries are all
  // in-memory on the Rust side, so 1s is comfortable even on mobile.
  const countTimer = setInterval(refreshCount, 1000);
  onCleanup(() => clearInterval(countTimer));

  // Ask for notification permission and create channels immediately so the
  // prompt appears on first launch rather than mid-transfer.
  if (IS_MOBILE_OS) ensureNotificationPermission();

  // Route the notification "Cancel" button to the transfer cancel command.
  // Mobile only: the desktop plugin build does not register the listener
  // command and the registration itself would reject.
  let unlistenAction: (() => void) | null = null;
  if (IS_MOBILE_OS) {
    onNotificationAction(async (actionId, extra) => {
      const tid = extra.transfer_id;
      if (typeof tid !== "string" || !tid) return;
      // Tapping a transfer notification body jumps straight to the queue.
      if (actionId === NOTIFICATION_TAP_ACTION_ID) {
        setCurrentView("transfers");
        return;
      }
      if (actionId !== TRANSFER_CANCEL_ACTION_ID) return;
      // Let the backend confirm before touching anything: if the transfer
      // actually finished a moment ago, cancel rejects and the completion
      // path (SAF finalize included) proceeds untouched. On success the next
      // poll tick posts the proper "canceled" notification and deletes the
      // SAF placeholder.
      try {
        await cancelTransfer(tid);
        dismissNotification(notifId(tid));
      } catch { /* already terminal; nothing to cancel */ }
    })
      .then((h) => { unlistenAction = () => h.unregister(); })
      .catch(() => {});
  }
  onCleanup(() => { unlistenAction?.(); });

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
        <ActiveTransfersBar />
      </main>
    </div>
  );
}
