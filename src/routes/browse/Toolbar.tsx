import { Show, createSignal, lazy, Suspense } from "solid-js";
import type { Resource } from "solid-js";
import {
  IconBack, IconRefresh, IconUpload,
  IconPlus, IconX, IconColumns, IconList, IconSearch,
  IconLock, IconLockOpen, IconActivity, IconDatabase,
} from "../../utils/icons";
import { setBrowseState, goUpPrefix } from "../../state/app";
import { PathBar } from "./PathBar";
// uPlot chart lib loads only when the stats modal is opened.
const StatsModal = lazy(() => import("./StatsModal").then((m) => ({ default: m.StatsModal })));
import { ToolbarOverflow } from "./ToolbarOverflow";
import type { BucketIndexStatus } from "../../types";

export function Toolbar(props: {
  accountId: string;
  accountName: string;
  bucket: string;
  prefix: string;
  indexStatus: Resource<BucketIndexStatus | undefined>;
  indexBusy: boolean;
  onToggleIndex: () => void;
  encryptionEnabled: boolean;
  onOpenEncryption: () => void;
  searchQuery: string;
  onSearchInput: (v: string) => void;
  onClearSearch: () => void;
  showSyncing: boolean;
  mode: "indexed" | "live";
  viewMode: "list" | "columns";
  onViewMode: (m: "list" | "columns") => void;
  onRefresh: () => void;
  onNewFolder: () => void;
  onUpload: () => void;
}) {
  const [showStats, setShowStats] = createSignal(false);
  const indexed = () => (props.indexStatus.latest ?? props.indexStatus())?.enabled;
  return (
    <div class="app-toolbar browse-toolbar">
      <div class="toolbar-left">
        <div class="toolbar-nav">
          <button class="icon-btn" onClick={goUpPrefix}><IconBack size={16} /></button>
          <button
            class="icon-btn refresh-btn"
            classList={{ spinning: props.showSyncing }}
            title={props.showSyncing ? "Refreshing…" : "Refresh"}
            aria-busy={props.showSyncing}
            onClick={props.onRefresh}
          >
            <IconRefresh size={16} />
          </button>
        </div>
        <PathBar
          accountName={props.accountName}
          bucket={props.bucket}
          prefix={props.prefix}
          onAccountSelect={() => setBrowseState({ bucket: null, prefix: "" })}
          onBucketSelect={() => setBrowseState({ prefix: "" })}
        />
      </div>

      <div class={`toolbar-search ${!(props.indexStatus.latest ?? props.indexStatus())?.enabled ? "toolbar-search-disabled" : ""}`}>
        <IconSearch size={13} class="toolbar-search-icon" />
        <input
          class="toolbar-search-input"
          placeholder={(props.indexStatus.latest ?? props.indexStatus())?.enabled ? "Search bucket…" : "Search (index required)"}
          value={props.searchQuery}
          disabled={!(props.indexStatus.latest ?? props.indexStatus())?.enabled}
          onInput={(e) => props.onSearchInput(e.currentTarget.value)}
        />
        <Show when={props.searchQuery}>
          <button class="toolbar-search-clear" onClick={props.onClearSearch}><IconX size={11} /></button>
        </Show>
      </div>

      <button
        class={`index-toggle-btn ${(props.indexStatus.latest ?? props.indexStatus())?.enabled ? "on" : "off"}`}
        title={(props.indexStatus.latest ?? props.indexStatus())?.enabled ? "Indexed" : "Not indexed"}
        disabled={props.indexBusy}
        onClick={props.onToggleIndex}
      >
        <IconDatabase size={14} class="index-toggle-icon" />
        <Show when={(props.indexStatus.latest ?? props.indexStatus())?.enabled}>
          <span class="index-toggle-label">Indexed</span>
        </Show>
        <Show when={!(props.indexStatus.latest ?? props.indexStatus())?.enabled}>
          <span class="index-toggle-label">Not indexed</span>
        </Show>
      </button>

      <Show when={indexed()}>
        <button class="icon-btn analytics-btn" title="Storage analytics" onClick={() => setShowStats(true)}>
          <IconActivity size={16} />
        </button>
      </Show>

      <div class="toolbar-actions">
        <Show when={props.mode === "live"}>
          <span class="mode-badge live">live</span>
        </Show>
        <button
          class="icon-btn enc-btn"
          classList={{ "enc-active": props.encryptionEnabled }}
          onClick={props.onOpenEncryption}
        >
          <Show when={props.encryptionEnabled} fallback={<IconLockOpen size={15} />}>
            <IconLock size={15} />
          </Show>
        </button>
        <div class="view-mode-toggle">
          <button class={`view-mode-btn ${props.viewMode === "columns" ? "active" : ""}`} onClick={() => props.onViewMode("columns")}><IconColumns size={14} /></button>
          <button class={`view-mode-btn ${props.viewMode === "list" ? "active" : ""}`} onClick={() => props.onViewMode("list")}><IconList size={14} /></button>
        </div>
        {/* mobile: single toggle so it stays uniform with the other icon buttons */}
        <button class="icon-btn view-toggle-mobile" title="Toggle view"
                onClick={() => props.onViewMode(props.viewMode === "columns" ? "list" : "columns")}>
          <Show when={props.viewMode === "columns"} fallback={<IconColumns size={16} />}><IconList size={16} /></Show>
        </button>
        <button class="btn-secondary toolbar-btn newfolder-btn" onClick={props.onNewFolder}>
          <IconPlus size={14} /> <span class="btn-label-desktop">New folder</span><span class="btn-label-mobile">Add</span>
        </button>
        <button class="btn-primary toolbar-btn upload-btn" onClick={props.onUpload}>
          <IconUpload size={14} /> Upload
        </button>
        <ToolbarOverflow
          indexed={!!indexed()}
          encryptionEnabled={props.encryptionEnabled}
          onAnalytics={() => setShowStats(true)}
          onOpenEncryption={props.onOpenEncryption}
          onNewFolder={props.onNewFolder}
        />
      </div>

      <Show when={showStats()}>
        <Suspense>
          <StatsModal accountId={props.accountId} bucket={props.bucket} onClose={() => setShowStats(false)} />
        </Suspense>
      </Show>
    </div>
  );
}
