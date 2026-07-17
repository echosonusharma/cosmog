import { isPermissionGranted, requestPermission, createChannel, Importance, Visibility } from "@tauri-apps/plugin-notification";
import { invoke } from "@tauri-apps/api/core";

let _permitted: boolean | null = null;
let _channelReady = false;

async function permitted(): Promise<boolean> {
  if (_permitted !== null) return _permitted;
  let granted = await isPermissionGranted();
  if (!granted) {
    const perm = await requestPermission();
    granted = perm === "granted";
  }
  _permitted = granted;
  return granted;
}

async function ensureChannel() {
  if (_channelReady) return;
  try {
    await createChannel({
      id: "cosmog-transfers",
      name: "Transfers",
      description: "Upload and download progress",
      importance: Importance.Default,
      visibility: Visibility.Public,
      vibration: false,
      lights: false,
    });
  } catch {
    // channel already exists or platform doesn't need it
  }
  _channelReady = true;
}

export interface NotifyOpts {
  id?: number;
  icon?: string;
  ongoing?: boolean;
  autoCancel?: boolean;
  silent?: boolean;
  channelId?: string;
}

export async function notify(title: string, body?: string, opts: NotifyOpts = {}) {
  try {
    if (!(await permitted())) return;
    await ensureChannel();
    await invoke("notify_ex", {
      id: opts.id ?? Math.floor(Math.random() * 2_000_000_000),
      title,
      body,
      icon: opts.icon ?? "ic_notification",
      ongoing: opts.ongoing ?? false,
      autoCancel: opts.autoCancel ?? true,
      silent: opts.silent ?? false,
      channelId: opts.channelId ?? "cosmog-transfers",
    });
  } catch {
    // silently ignore — in-app toast is the fallback
  }
}

/** Stable positive 31-bit int from a string, for reusing notification ids. */
export function notifId(key: string): number {
  let h = 5381;
  for (let i = 0; i < key.length; i++) h = ((h << 5) + h + key.charCodeAt(i)) | 0;
  return Math.abs(h);
}
