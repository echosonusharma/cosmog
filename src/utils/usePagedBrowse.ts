import { createSignal, createEffect, onCleanup } from "solid-js";
import { createStore } from "solid-js/store";
import { browsePrefix } from "../api/browse";
import type { BrowseResult, CachedObjectMeta } from "../types";

export interface PagedBrowseState {
  objects: CachedObjectMeta[];
  subprefixes: string[];
  mode: "indexed" | "live";
  continuation: string | null;
  truncated: boolean;
  last_synced_at: number | null;
  loading: boolean;
  error: unknown | null;
  initialLoaded: boolean;
}

export function createPagedBrowse(getKey: () => {
  accountId: string;
  bucket: string;
  prefix: string;
  /** Bump to force a fresh first-page fetch. */
  refresh: number;
}) {
  const [state, setState] = createStore<PagedBrowseState>({
    objects: [],
    subprefixes: [],
    mode: "indexed",
    continuation: null,
    truncated: false,
    last_synced_at: null,
    loading: false,
    error: null,
    initialLoaded: false,
  });

  const [fetchTrigger, setFetchTrigger] = createSignal(0);
  let nextContinuation: string | null = null;

  // Refetch first page whenever the identity (account/bucket/prefix/refresh)
  // changes. Always stale-while-revalidate: previous rows stay visible (with
  // `loading` true) until the new page lands, so navigation and bucket/account
  // switches don't flash a blank content area. Bulk selection is reset by the
  // caller on bucket/account change, so acting on stale rows is safe — per-row
  // actions use each CachedObjectMeta's own account_id/bucket, not the
  // component's current props.
  createEffect(() => {
    const key = getKey();
    void key.prefix; void key.refresh;
    void key.accountId; void key.bucket;
    nextContinuation = null;
    setState({ continuation: null, truncated: false, error: null });
    setFetchTrigger((n) => n + 1);
  });

  createEffect(() => {
    fetchTrigger();
    const key = getKey();
    if (!key.accountId || !key.bucket) return;

    let cancelled = false;
    setState({ loading: true, error: null });
    const cont = nextContinuation ?? undefined;
    browsePrefix(key.accountId, key.bucket, key.prefix, cont)
      .then((res: BrowseResult) => {
        if (cancelled) return;
        nextContinuation = res.continuation;
        setState((prev) => ({
          objects: cont ? [...prev.objects, ...res.objects] : res.objects,
          subprefixes: cont ? mergeUnique(prev.subprefixes, res.subprefixes) : res.subprefixes,
          mode: res.mode,
          continuation: res.continuation,
          truncated: res.truncated,
          last_synced_at: res.last_synced_at,
          loading: false,
          error: null,
          initialLoaded: true,
        }));
      })
      .catch((err) => {
        if (cancelled) return;
        setState({ loading: false, error: err, initialLoaded: true });
      });

    onCleanup(() => { cancelled = true; });
  });

  const loadMore = () => {
    if (state.loading) return;
    if (!state.continuation) return;
    setFetchTrigger((n) => n + 1);
  };

  return { state, loadMore } as const;
}

function mergeUnique(a: string[], b: string[]): string[] {
  const seen = new Set(a);
  const out = [...a];
  for (const v of b) if (!seen.has(v)) { seen.add(v); out.push(v); }
  return out;
}
