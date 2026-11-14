import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { t } from "../../../i18n";

/** Native folder picker; null when dismissed. */
export async function pickFolder(): Promise<string | null> {
  const sel = await openDialog({ directory: true, multiple: false, title: t("folderUpload.entry.pickTitle") });
  return typeof sel === "string" && sel ? sel : null;
}
