import { createSignal, Show } from "solid-js";
import { PolicyTab } from "./PolicyTab";
import { CorsTab } from "./CorsTab";
import { VersioningTab } from "./VersioningTab";
import { IconX } from "../../../utils/icons";
import type { BucketConfigTab } from "./errors";
import { t } from "../../../i18n";

const tabs = (): { id: BucketConfigTab; label: string }[] => [
  { id: "policy", label: t("bucketconfig.tab.policy") },
  { id: "cors", label: t("bucketconfig.tab.cors") },
  { id: "versioning", label: t("bucketconfig.tab.versioning") },
];

export function BucketConfigModal(props: {
  accountId: string;
  bucket: string;
  providerId: string;
  providerLabel: string;
  onClose: () => void;
  onChanged: () => void;
}) {
  const [tab, setTab] = createSignal<BucketConfigTab>("policy");

  // Tabs are mounted lazily on first visit, then kept mounted (hidden via CSS).
  // Switching back stays instant: no remount, no refetch spinner, no lost edits.
  const [visited, setVisited] = createSignal<Set<BucketConfigTab>>(new Set(["policy"]));
  const selectTab = (id: BucketConfigTab) => {
    setVisited((s) => (s.has(id) ? s : new Set(s).add(id)));
    setTab(id);
  };

  const tabProps = () => ({
    accountId: props.accountId,
    bucket: props.bucket,
    providerId: props.providerId,
    providerLabel: props.providerLabel,
    onChanged: props.onChanged,
  });

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="modal bcfg-modal" onClick={(e) => e.stopPropagation()}>
        <div class="bcfg-header">
          <div class="modal-title">{t("bucketconfig.title", { bucket: props.bucket })}</div>
          <button class="icon-btn bcfg-close-x" title={t("common.close")} onClick={props.onClose}>
            <IconX size={16} />
          </button>
        </div>

        <div class="bcfg-tabs" role="tablist">
          {tabs().map((tb) => (
            <button
              type="button"
              role="tab"
              class="bcfg-tab-btn"
              classList={{ active: tab() === tb.id }}
              aria-selected={tab() === tb.id}
              onClick={() => selectTab(tb.id)}
            >
              {tb.label}
            </button>
          ))}
        </div>

        <div class="bcfg-tab-body">
          <Show when={visited().has("policy")}>
            <div class="bcfg-pane" classList={{ hidden: tab() !== "policy" }}>
              <PolicyTab {...tabProps()} />
            </div>
          </Show>
          <Show when={visited().has("cors")}>
            <div class="bcfg-pane" classList={{ hidden: tab() !== "cors" }}>
              <CorsTab {...tabProps()} />
            </div>
          </Show>
          <Show when={visited().has("versioning")}>
            <div class="bcfg-pane" classList={{ hidden: tab() !== "versioning" }}>
              <VersioningTab {...tabProps()} />
            </div>
          </Show>
        </div>
      </div>
    </div>
  );
}
