import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";

let _permitted: boolean | null = null;

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

export async function notify(title: string, body?: string) {
  try {
    if (!(await permitted())) return;
    sendNotification({ title, body });
  } catch {
    // silently ignore — in-app toast is the fallback
  }
}
