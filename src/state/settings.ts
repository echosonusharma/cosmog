import { createSignal } from "solid-js";
import { getSettings, updateSettings, resetSettings as resetSettingsApi, type SettingsPatch } from "../api/settings";
import { confirmDialog } from "./confirm";
import { basename } from "../utils/fmt";
import type { AppSettings } from "../types";

const [appSettings, setAppSettings] = createSignal<AppSettings | null>(null);
export { appSettings, setAppSettings };

// Bumped per write so a load that started earlier can't overwrite newer state.
let writeGen = 0;
// Writes run one at a time: backend update is read-modify-write, and rapid theme
// toggles must land in order.
let queue: Promise<unknown> = Promise.resolve();

function enqueue<T>(op: () => Promise<T>): Promise<T> {
  writeGen++;
  const run = queue.then(op, op);
  queue = run.catch(() => {});
  return run;
}

export async function loadSettings(): Promise<AppSettings | null> {
  const gen = writeGen;
  try {
    const s = await getSettings();
    if (gen !== writeGen) return appSettings();
    setAppSettings(s);
    return s;
  } catch {
    return appSettings();
  }
}

export function saveSettings(patch: SettingsPatch): Promise<AppSettings> {
  return enqueue(async () => {
    const s = await updateSettings(patch);
    setAppSettings(s);
    return s;
  });
}

export function resetAllSettings(): Promise<AppSettings> {
  return enqueue(async () => {
    const s = await resetSettingsApi();
    setAppSettings(s);
    return s;
  });
}

export const showHiddenFiles = () => appSettings()?.show_hidden ?? false;
export const autoPreview = () => appSettings()?.auto_preview ?? false;

export function confirmDestructive(opts: Parameters<typeof confirmDialog>[0]): Promise<boolean | null> {
  if (appSettings()?.confirm_destructive === false) return Promise.resolve(true);
  return confirmDialog(opts);
}

export function isShownKey(key: string): boolean {
  return showHiddenFiles() || !basename(key).startsWith(".");
}
