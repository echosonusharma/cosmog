import {
  isPermissionGranted,
  requestPermission,
  createChannel,
  removeChannel,
  registerActionTypes,
  onAction,
  removeActive,
  Importance,
  Visibility,
} from "@tauri-apps/plugin-notification";
import { invoke } from "@tauri-apps/api/core";

export const TRANSFER_ACTION_TYPE_ID = "TRANSFER_ACTIONS";
export const TRANSFER_CANCEL_ACTION_ID = "cancel";
/** Action id the plugin reports when the user taps the notification body. */
export const NOTIFICATION_TAP_ACTION_ID = "tap";

// Android notification channels. Splitting by purpose lets the user mute
// noisy progress updates in system settings while keeping failure alerts.
export const CHANNEL_PROGRESS = "transfers-progress";
export const CHANNEL_EVENTS = "transfers-events";
export const CHANNEL_ALERTS = "alerts";

// Channels, action types, and removeActive are mobile-only plugin commands;
// the desktop plugin build does not register them and calls reject with
// "command not found". Gate on OS, not viewport width.
export const IS_MOBILE_OS = /android|iphone|ipad/i.test(navigator.userAgent);

let _granted = false;
let _prompted = false;
let _channelsReady = false;
let _actionsReady = false;

async function permitted(): Promise<boolean> {
  if (_granted) return true;
  // Re-check every call: the user may grant permission from system settings
  // mid-session. Only the interactive prompt is one-shot.
  _granted = await isPermissionGranted();
  if (!_granted && !_prompted) {
    _prompted = true;
    _granted = (await requestPermission()) === "granted";
  }
  return _granted;
}

async function ensureChannels() {
  if (!IS_MOBILE_OS || _channelsReady) return;
  await createChannel({
    id: CHANNEL_PROGRESS,
    name: "Transfer progress",
    description: "Ongoing upload and download progress",
    importance: Importance.Low,
    visibility: Visibility.Public,
    vibration: false,
    lights: false,
  });
  await createChannel({
    id: CHANNEL_EVENTS,
    name: "Transfer results",
    description: "Completed and canceled transfers, general updates",
    importance: Importance.Default,
    visibility: Visibility.Public,
    vibration: false,
    lights: false,
  });
  await createChannel({
    id: CHANNEL_ALERTS,
    name: "Alerts",
    description: "Failed transfers, errors, and warnings",
    importance: Importance.High,
    visibility: Visibility.Public,
    vibration: true,
    lights: false,
  });
  // Drop the pre-split channel from earlier builds so it does not linger in
  // the app's notification settings.
  await removeChannel("cosmog-transfers");
  _channelsReady = true;
}

async function ensureActionTypes() {
  if (!IS_MOBILE_OS || _actionsReady) return;
  await registerActionTypes([
    {
      id: TRANSFER_ACTION_TYPE_ID,
      actions: [
        { id: TRANSFER_CANCEL_ACTION_ID, title: "Cancel", destructive: true, foreground: false },
      ],
    },
  ]);
  _actionsReady = true;
}

export interface NotifyOpts {
  id?: number;
  icon?: string;
  ongoing?: boolean;
  autoCancel?: boolean;
  silent?: boolean;
  channelId?: string;
  actionTypeId?: string;
  /** Android: short line shown next to the app name in the tray header. */
  summary?: string;
  /** Android: expanded (BigTextStyle) body shown when the user pulls the
   *  notification open. Falls back to `body` when omitted. */
  largeBody?: string;
  extra?: Record<string, unknown>;
}

/** Call once at app startup to request notification permission and create
 *  channels before the first transfer. Mobile only; no-ops on desktop. */
export async function ensureNotificationPermission(): Promise<void> {
  if (!IS_MOBILE_OS) return;
  try {
    await permitted();
    await ensureChannels();
  } catch (e) {
    console.warn("[notify] startup permission request failed:", e);
  }
}

export async function notify(title: string, body?: string, opts: NotifyOpts = {}) {
  // Notifications are best-effort UX; callers fire-and-forget, so a rejection
  // here would surface as an unhandled promise rejection. Never throw.
  try {
    await notifyInner(title, body, opts);
  } catch (e) {
    console.warn("[notify] failed:", e);
  }
}

async function notifyInner(title: string, body: string | undefined, opts: NotifyOpts) {
  if (!(await permitted())) return;
  await ensureChannels();
  if (opts.actionTypeId) await ensureActionTypes();
  await invoke("notify_ex", {
    id: opts.id ?? Math.floor(Math.random() * 2_000_000_000),
    title,
    body,
    icon: opts.icon ?? "ic_notification",
    ongoing: opts.ongoing ?? false,
    autoCancel: opts.autoCancel ?? true,
    silent: opts.silent ?? false,
    channelId: opts.channelId ?? CHANNEL_EVENTS,
    actionTypeId: opts.actionTypeId ?? null,
    summary: opts.summary ?? null,
    largeBody: opts.largeBody ?? null,
    extra: opts.extra ?? null,
  });
}

/** Subscribe to notification action clicks (including body taps, reported as
 *  NOTIFICATION_TAP_ACTION_ID). `cb` receives the action id and the `extra`
 *  payload attached to the notification. Returns the unlisten handle. */
export async function onNotificationAction(
  cb: (actionId: string, extra: Record<string, unknown>) => void,
) {
  await ensureActionTypes();
  return onAction((n: unknown) => {
    const notif = n as { actionId?: string; extra?: Record<string, unknown> };
    if (!notif.actionId) return;
    cb(notif.actionId, notif.extra ?? {});
  });
}

/** Dismiss an active notification by its stable id. Used when the user
 *  cancels a transfer so the ongoing "Uploading…" entry disappears
 *  immediately instead of lingering until the next poll. Mobile only:
 *  desktop notifications are transient and cannot be recalled. */
export async function dismissNotification(id: number) {
  if (!IS_MOBILE_OS) return;
  try {
    await removeActive([{ id }]);
  } catch (e) {
    console.warn("[notify] dismiss failed:", e);
  }
}

/** Stable positive 31-bit int from a string, for reusing notification ids.
 *  Mask, not Math.abs: abs(-2^31) is 2^31, which overflows the i32 id on the
 *  Rust side. */
export function notifId(key: string): number {
  let h = 5381;
  for (let i = 0; i < key.length; i++) h = ((h << 5) + h + key.charCodeAt(i)) | 0;
  return (h & 0x7fffffff) || 1;
}
