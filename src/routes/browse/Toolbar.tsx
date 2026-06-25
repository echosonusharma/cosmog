import { Show } from "solid-js";
import type { Resource } from "solid-js";
import {
  IconBack, IconRefresh, IconUpload,
  IconPlus, IconX, IconColumns, IconList, IconSearch,
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
          <button class="icon-btn" title="Back" onClick={goUpPrefix}><IconBack size={16} /></button>
          <button class="icon-btn" title="Refresh" onClick={props.onRefresh}><IconRefresh size={16} /></button>
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
      <div class={`toolbar-search ${!props.indexStatus()?.enabled ? "toolbar-search-disabled" : ""}`}>
        <IconSearch size={13} class="toolbar-search-icon" />
        <input
          class="toolbar-search-input"
          placeholder={props.indexStatus()?.enabled ? "Search bucket…" : "Search (index required)"}
          value={props.searchQuery}
          disabled={!props.indexStatus()?.enabled}
          onInput={(e) => props.onSearchInput(e.currentTarget.value)}
        />
        <Show when={props.searchQuery}>
          <button class="toolbar-search-clear" onClick={props.onClearSearch}><IconX size={11} /></button>
        </Show>
      </div>

      {/* index toggle */}
      <button
        class={`index-toggle-btn ${props.indexStatus()?.enabled ? "on" : "off"}`}
        title={props.indexStatus()?.enabled ? "Indexing enabled — click to disable" : "Indexing disabled — click to enable"}
        disabled={props.indexBusy}
        onClick={props.onToggleIndex}
      >
        <span class="index-toggle-dot" />
        <Show when={props.indexStatus()?.enabled}>
          <span class="index-toggle-label">Indexed</span>
        </Show>
        <Show when={!props.indexStatus()?.enabled}>
          <span class="index-toggle-label">Not indexed</span>
        </Show>
      </button>

      <div class="toolbar-actions">
        <Show when={props.showSyncing}>
          <span class="sync-badge"><span class="spinner" /> syncing</span>
        </Show>
        <Show when={!props.showSyncing && props.mode === "live"}>
          <span class="mode-badge live" title="Live mode — pages fetched on demand. Enable indexing for search.">live</span>
        </Show>
        <div class="view-mode-toggle">
          <button class={`view-mode-btn ${props.viewMode === "columns" ? "active" : ""}`} onClick={() => props.onViewMode("columns")} title="Columns"><IconColumns size={14} /></button>
          <button class={`view-mode-btn ${props.viewMode === "list" ? "active" : ""}`} onClick={() => props.onViewMode("list")} title="List"><IconList size={14} /></button>
        </div>
        <button class="btn-secondary toolbar-btn" onClick={props.onNewFolder}>
          <IconPlus size={14} /> New folder
        </button>
        <button class="btn-primary toolbar-btn" onClick={props.onUpload}>
          <IconUpload size={14} /> Upload
        </button>
      </div>
    </div>
  );
}
