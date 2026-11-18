import { basename } from "./fmt";
import { t } from "../i18n";

export const isContentUri = (root: string) => root.startsWith("content://");

const safeDecode = (s: string) => { try { return decodeURIComponent(s); } catch { return s; } };

/** User-facing folder name for a local path or an Android SAF tree URI. */
export function sourceDisplayName(root: string): string {
  // "/" gives "", like basename, so a default dest never gets "//".
  if (!isContentUri(root)) return basename(root);
  // content://<authority>/tree/<docId>[/document/<docId>]; docId is "volume:path".
  const m = /\/tree\/([^/]+)/.exec(root);
  const docId = safeDecode(m ? m[1] : root.slice(root.lastIndexOf("/") + 1));
  const colon = docId.indexOf(":");
  const path = (colon >= 0 ? docId.slice(colon + 1) : docId).replace(/\/+$/, "");
  if (!path) {
    const vol = colon >= 0 ? docId.slice(0, colon) : docId;
    return vol === "primary" ? t("folderUpload.source.internalStorage") : vol;
  }
  return path.slice(path.lastIndexOf("/") + 1);
}
