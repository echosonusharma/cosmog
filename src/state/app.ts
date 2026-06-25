import { createSignal } from "solid-js";
import { createStore } from "solid-js/store";
import type { Account, Bucket, CachedObjectMeta } from "../types";

export type View = "browse" | "transfers" | "settings" | "logs";

export interface BrowseState {
  accountId: string | null;
  bucket: string | null;
  prefix: string;
}

export const [currentView, setCurrentView] = createSignal<View>("browse");

export const [browseState, setBrowseState] = createStore<BrowseState>({
  accountId: null,
  bucket: null,
  prefix: "",
});

export const [accounts, setAccounts] = createSignal<Account[]>([]);
export const [sidebarBuckets, setSidebarBuckets] = createSignal<Bucket[]>([]);

const [bucketsRefreshTick, setBucketsRefreshTick] = createSignal(0);
export { bucketsRefreshTick };
export function bumpBucketsRefresh() { setBucketsRefreshTick((n) => n + 1); }

export const [pendingPreview, setPendingPreview] = createSignal<CachedObjectMeta | null>(null);

export const [openAddAccount, setOpenAddAccount] = createSignal(false);

export function navigateToBucket(accountId: string, bucket: string) {
  setBrowseState({ accountId, bucket, prefix: "" });
  setCurrentView("browse");
}

export function navigateToObject(obj: CachedObjectMeta) {
  const slash = obj.key.lastIndexOf("/");
  const parentPrefix = slash >= 0 ? obj.key.slice(0, slash + 1) : "";
  setPendingPreview(obj);
  setBrowseState({ accountId: obj.account_id, bucket: obj.bucket, prefix: parentPrefix });
  setCurrentView("browse");
}

export function navigateToPrefix(prefix: string) {
  setBrowseState("prefix", prefix);
}

export function goUpPrefix() {
  const p = browseState.prefix;
  if (!p) {
    setBrowseState({ bucket: null, prefix: "" });
    return;
  }
  const trimmed = p.endsWith("/") ? p.slice(0, -1) : p;
  const idx = trimmed.lastIndexOf("/");
  setBrowseState("prefix", idx >= 0 ? trimmed.slice(0, idx + 1) : "");
}

export function selectAccount(accountId: string) {
  setBrowseState({ accountId, bucket: null, prefix: "" });
  setCurrentView("browse");
}
