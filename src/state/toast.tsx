// Every user-visible event (ok/err/warn/info) is routed through the OS
// notification tray via utils/notify. The `toast` object keeps the same
// method surface so existing call sites compile unchanged, but no in-app
// popup renders anywhere: the user always gets a real system notification.

import { notify, CHANNEL_EVENTS, CHANNEL_ALERTS } from "../utils/notify";
import { errMsg } from "../utils/errors";

export { errMsg } from "../utils/errors";

const RECENT_WINDOW_MS = 1500;
const recent = new Map<string, number>();
function suppressed(key: string): boolean {
  const now = Date.now();
  const last = recent.get(key);
  recent.set(key, now);
  if (recent.size > 64) {
    for (const [k, t] of recent) {
      if (now - t > RECENT_WINDOW_MS) recent.delete(k);
    }
  }
  return last !== undefined && now - last < RECENT_WINDOW_MS;
}

// Notification titles render as a single line; keep them scannable and put
// the full text in the expanded body when it is long.
const TITLE_MAX = 60;
function short(msg: string): string {
  return msg.length <= TITLE_MAX ? msg : `${msg.slice(0, TITLE_MAX - 1)}…`;
}

/** Success and info messages read best as the headline itself ("Link
 *  copied", "Settings saved") instead of a generic app-name title. The
 *  optional detail line gives the notification a body so it doesn't render
 *  as a bare one-liner in the tray; long details land in the expanded view. */
function event(title: string, detail?: string) {
  if (suppressed(`${title}|${detail ?? ""}`)) return;
  notify(short(title), detail ? short(detail) : undefined, {
    channelId: CHANNEL_EVENTS,
    largeBody: detail ?? (title.length > TITLE_MAX ? title : undefined),
  });
}

function alert(title: string, msg: string) {
  if (suppressed(`${title}|${msg}`)) return;
  notify(title, short(msg), { channelId: CHANNEL_ALERTS, largeBody: msg });
}

export const toast = {
  info: (m: string, detail?: string) => event(m, detail),
  ok:   (m: string, detail?: string) => event(m, detail),
  warn: (m: string, title = "Warning") => alert(title, m),
  err:  (e: unknown) => alert("Error", errMsg(e)),
};
