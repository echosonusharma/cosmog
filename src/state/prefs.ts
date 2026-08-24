import { load, type Store } from "@tauri-apps/plugin-store";

// Frontend-only UI prefs; backend config stays in the Rust settings DB.
// Async store, sync cache so Solid signals can read at creation time.
let store: Store | null = null;
const cache = new Map<string, unknown>();

// Load once at boot before render; populates the cache for sync reads.
export async function initPrefs() {
  try {
    store = await load("prefs.json", { autoSave: true });
    for (const [k, v] of await store.entries()) cache.set(k, v);
  } catch {
    // Store unavailable; fall back to in-memory only.
  }
}

export function getPref<T>(key: string, fallback: T): T {
  return cache.has(key) ? (cache.get(key) as T) : fallback;
}

export function setPref<T>(key: string, value: T) {
  cache.set(key, value);
  store?.set(key, value).catch(() => {});
}
