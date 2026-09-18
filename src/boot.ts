import { listAccounts } from "./api/accounts";
import { listBuckets } from "./api/buckets";
import { getSettings } from "./api/settings";
import { initPrefs } from "./state/prefs";
import { initEditorTheme } from "./state/editorTheme";
import { setTheme } from "./state/theme";
import { errMsg } from "./utils/errors";
import {
  accounts,
  browseState,
  setAccounts,
  setBrowseState,
  seedBootBuckets,
  setSidebarBucketsError,
  restoreBrowseState,
} from "./state/app";

// Staged launch: each label names the real work after it. No timers.
// Seeds signals so App/MainApp mount with data, no duplicate invokes.
export async function boot(setStage: (s: string) => void) {
  setStage("Loading preferences…");
  await initPrefs();
  initEditorTheme();
  restoreBrowseState();

  setStage("Loading accounts…");
  let error: unknown = null;
  try {
    setAccounts(await listAccounts());
  } catch (e) {
    error = e;
  }

  if (!error) {
    // Settle selection; MainApp re-validates on refresh.
    const currentId = browseState.accountId;
    const stillValid = currentId && accounts().some((a) => a.id === currentId);
    if (!stillValid) {
      const first = accounts()[0]?.id ?? null;
      setBrowseState({ accountId: first, bucket: null, prefix: "" });
    }

    setStage("Loading settings…");
    try {
      const s = await getSettings();
      // Pre-paint apply avoids a theme flash.
      if (s) setTheme(s.theme ?? "system");
    } catch {
      // MainApp falls back to defaults.
    }

    const accountId = browseState.accountId;
    if (accountId) {
      setStage("Loading buckets…");
      try {
        seedBootBuckets(accountId, await listBuckets(accountId));
        setSidebarBucketsError(null);
      } catch (e) {
        // Display-only; MainApp retries on mount.
        setSidebarBucketsError(errMsg(e));
      }
    }
  }

  void import("./utils/CodeEditor");
  return { error };
}
