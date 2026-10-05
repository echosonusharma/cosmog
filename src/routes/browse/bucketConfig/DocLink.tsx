import { openUrl } from "@tauri-apps/plugin-opener";
import { docUrl } from "./providerCaps";
import type { BucketConfigTab } from "./errors";
import { t } from "../../../i18n";

// Opens the provider-specific (or canonical S3) documentation for a config tab.
export function DocLink(props: { providerId: string; tab: BucketConfigTab }) {
  return (
    <button
      type="button"
      class="bcfg-doc-link"
      onClick={() => openUrl(docUrl(props.providerId, props.tab)).catch(() => {})}
    >
      {t("common.learnMore")} ↗
    </button>
  );
}
