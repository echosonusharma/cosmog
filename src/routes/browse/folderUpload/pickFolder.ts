import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { folderUploadPickTree } from "../../../api/folderUpload";
import { toast } from "../../../state/toast";
import { errCode } from "../../../utils/errors";
import { isAndroid } from "../../../utils/platform";
import { sourceDisplayName } from "../../../utils/sourceName";
import { t } from "../../../i18n";

export interface PickedFolder { root: string; name: string }

// Android hides some roots from the picker; explain once per session after a dismiss.
let hintShown = false;

async function pickTree(): Promise<PickedFolder | null> {
  try {
    const { uri, display_name } = await folderUploadPickTree();
    return { root: uri, name: display_name || sourceDisplayName(uri) };
  } catch (e) {
    if (errCode(e) !== "canceled") throw e;
    if (!hintShown) {
      hintShown = true;
      toast.info(t("folderUpload.android.pickHintTitle"), t("folderUpload.android.pickHint"));
    }
    return null;
  }
}

/** Native folder picker (SAF tree on Android); null when dismissed. */
export async function pickFolder(): Promise<PickedFolder | null> {
  if (isAndroid) return pickTree();
  const sel = await openDialog({ directory: true, multiple: false, title: t("folderUpload.entry.pickTitle") });
  return typeof sel === "string" && sel ? { root: sel, name: sourceDisplayName(sel) } : null;
}
