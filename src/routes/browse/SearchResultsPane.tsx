import { Show, For } from "solid-js";
import type { Resource } from "solid-js";
import {
  FileIcon, fileTypeLabel,
  IconDownload, IconLink, IconSearch,
} from "../../utils/icons";
import { formatBytes, formatDate } from "../../utils/fmt";
import { navigateToPrefix } from "../../state/app";
import type { CachedObjectMeta, BucketIndexStatus, SearchResult } from "../../types";

function escapeRegex(s: string) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function highlightText(text: string, query: string) {
  const terms = query.trim().split(/\s+/).filter(t => t.length >= 3);
  if (!terms.length) return <>{text}</>;
  const pattern = terms.map(escapeRegex).join("|");
  const re = new RegExp(`(${pattern})`, "gi");
  const parts = text.split(re);
  const matchRe = new RegExp(`^(?:${pattern})$`, "i");
  return (
    <>
      {parts.map((part) =>
        matchRe.test(part) ? <mark class="search-highlight">{part}</mark> : part
      )}
    </>
  );
}

export function SearchResultsPane(props: {
  searchQuery: string;
  searchResults: Resource<SearchResult | undefined>;
  indexStatus: Resource<BucketIndexStatus | undefined>;
  indexBusy: boolean;
  onEnableIndex: () => void;
  onSelectResult: (obj: CachedObjectMeta) => void;
  onCtxResult: (e: MouseEvent, obj: CachedObjectMeta) => void;
  onDownload: (obj: CachedObjectMeta) => void;
  onCopyLink: (obj: CachedObjectMeta) => void;
  onClearSearch: () => void;
}) {
  return (
    <div class="search-results-pane" style="position:relative">
      <Show when={props.searchResults.loading && !!props.searchResults.latest}>
        <span class="spinner" style="position:absolute;top:8px;right:8px;z-index:1" />
      </Show>
      {/* Latch: while a new query is fetching, keep the previous result set
          rendered underneath a small corner spinner instead of flashing the
          full "Searching…" placeholder on every keystroke. */}
      <Show when={props.searchResults.loading && !props.searchResults.latest}>
        <div class="loading-row"><span class="spinner" /> Searching…</div>
      </Show>
      <Show when={props.searchResults.latest}>
        {(r) => (
          <Show when={r().objects.length > 0}
                fallback={
                  <Show when={!props.indexStatus()?.enabled}
                        fallback={
                          <div class="empty-state">
                            <span class="empty-icon"><IconSearch size={32} /></span>
                            No results for "{props.searchQuery}"
                          </div>
                        }>
                    <div class="empty-state">
                      <span class="empty-icon"><IconSearch size={32} /></span>
                      <span>Bucket not indexed</span>
                      <button class="btn-primary" style="margin-top:12px;width:auto;padding:0 20px" disabled={props.indexBusy} onClick={props.onEnableIndex}>
                        Enable index
                      </button>
                    </div>
                  </Show>
                }>
            <div class="results-header">{r().total.toLocaleString()} matches</div>
            <div class="object-list" style="flex:1;overflow-y:auto">
              <For each={r().objects}>
                {(obj) => (
                  <div class="obj-row" style="cursor:pointer"
                       onClick={() => { navigateToPrefix(obj.key.includes("/") ? obj.key.slice(0, obj.key.lastIndexOf("/") + 1) : ""); props.onClearSearch(); props.onSelectResult(obj); }}
                       onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); props.onCtxResult(e, obj); }}>
                    <div class="obj-name-cell">
                      <span class="obj-checkbox-spacer" />
                      <FileIcon name={obj.basename} />
                      <span class="obj-name" title={obj.key}>{highlightText(obj.key, props.searchQuery)}</span>
                    </div>
                    <div class="obj-type">{fileTypeLabel(obj.basename)}</div>
                    <div class="obj-size">{formatBytes(obj.size)}</div>
                    <div class="obj-date">{obj.last_modified ? formatDate(obj.last_modified) : "-"}</div>
                    <div class="obj-actions" onClick={(e) => e.stopPropagation()}>
                      <button class="icon-btn" title="Download" onClick={() => props.onDownload(obj)}><IconDownload size={15} /></button>
                      <button class="icon-btn" title="Copy link" onClick={() => props.onCopyLink(obj)}><IconLink size={15} /></button>
                    </div>
                  </div>
                )}
              </For>
            </div>
          </Show>
        )}
      </Show>
    </div>
  );
}
