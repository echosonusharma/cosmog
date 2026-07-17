import { Show } from "solid-js";
import type { Resource } from "solid-js";
import {
  IconBack, IconRefresh, IconUpload,
  IconPlus, IconX, IconColumns, IconList, IconSearch,
  IconLock, IconLockOpen,
} from "../../utils/icons";
import { setBrowseState, goUpPrefix } from "../../state/app";
import { PathBar } from "./PathBar";
import type { BucketIndexStatus } from "../../types";

export function Toolbar(props: {
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
  return (
    <div class="app-toolbar">
      <div class="toolbar-left">
        <div class="toolbar-nav">
          <button class="icon-btn" onClick={goUpPrefix}><IconBack size={16} /></button>
          <button class="icon-btn" onClick={props.onRefresh}><IconRefresh size={16} /></button>
        </div>
        <PathBar
          accountName={props.accountName}
          bucket={props.bucket}
          prefix={props.prefix}
          onAccountSelect={() => setBrowseState({ bucket: null, prefix: "" })}
          onBucketSelect={() => setBrowseState({ prefix: "" })}
        />
      </div>

      {/* search — center, takes flex space */}
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

      {/* index toggle */}
      <button
        class={`index-toggle-btn ${(props.indexStatus.latest ?? props.indexStatus())?.enabled ? "on" : "off"}`}

        disabled={props.indexBusy}
        onClick={props.onToggleIndex}
      >
        <span class="index-toggle-dot" />
        <Show when={(props.indexStatus.latest ?? props.indexStatus())?.enabled}>
          <span class="index-toggle-label">Indexed</span>
        </Show>
        <Show when={!(props.indexStatus.latest ?? props.indexStatus())?.enabled}>
          <span class="index-toggle-label">Not indexed</span>
        </Show>
      </button>

      <div class="toolbar-actions">
        <Show when={props.showSyncing}>
          <span class="sync-badge"><span class="spinner" /> syncing</span>
        </Show>
        <Show when={!props.showSyncing && props.mode === "live"}>
          <span class="mode-badge live">live</span>
        </Show>
        <button
          class="icon-btn"
          style={props.encryptionEnabled ? "color:var(--accent)" : ""}

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
        <button class="btn-secondary toolbar-btn" onClick={props.onNewFolder}>
          <IconPlus size={14} /> <span class="btn-label-desktop">New folder</span><span class="btn-label-mobile">Add</span>
        </button>
        <button class="btn-primary toolbar-btn" onClick={props.onUpload}>
          <IconUpload size={14} /> Upload
        </button>
      </div>
    </div>
  );
}
