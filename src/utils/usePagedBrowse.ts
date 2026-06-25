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

  // Reset + fetch first page whenever the identity (account/bucket/prefix/refresh)
  // changes. Tracks the inputs explicitly so the effect re-runs on each.
  createEffect(() => {
    const key = getKey();
    void key.accountId; void key.bucket; void key.prefix; void key.refresh;
    nextContinuation = null;
    setState({
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
