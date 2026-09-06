import { useEffect, useRef, useState } from "react";
import {
  isAbortError,
  search,
  type SearchHit,
  type SearchMode,
} from "../api/client";

// Debounce is short enough to feel reactive but long enough to coalesce
// rapid typing into one fetch. Pairs with the daemon-side query-embedding
// LRU (kb-server::embed_cache): when typing visits the same prefix
// repeatedly (or the user retries an earlier query), the second hit
// returns in ~25 ms instead of ~125 ms. Pre-v0.14 this was 150 ms — half
// the perceived round-trip on localhost — with no daemon cache.
const DEBOUNCE_MS = 80;

export type SearchState = {
  hits: SearchHit[];
  ms: number;
  embedMs: number;
  cacheHit: boolean;
  loading: boolean;
  error: string | null;
};

// useSearch — debounces the query string, calls /api/search, and cancels
// in-flight requests when a new query arrives. Empty query → empty
// state (no API call). Errors are surfaced as readable strings; the
// hybrid/semantic 400 ("kb has no embedder") is the most common one.
export function useSearch(q: string, mode: SearchMode, kb?: string): SearchState {
  const [state, setState] = useState<SearchState>({
    hits: [],
    ms: 0,
    embedMs: 0,
    cacheHit: false,
    loading: false,
    error: null,
  });
  const generation = useRef(0);

  useEffect(() => {
    if (!q.trim()) {
      // Bump the generation so a search from a prior non-empty query that
      // resolves a tick AFTER this clear fails its gen check and can't
      // repopulate stale hits over the now-empty state. (The AbortController
      // cancels still-in-flight fetches, but one that already resolved isn't
      // abortable — this closes that window.)
      generation.current++;
      setState({
        hits: [],
        ms: 0,
        embedMs: 0,
        cacheHit: false,
        loading: false,
        error: null,
      });
      return;
    }
    const gen = ++generation.current;
    // Abort the in-flight fetch when a newer query supersedes this one (or
    // the hook unmounts) so the stale request doesn't tie up the network /
    // daemon. The generation ref still guards against late resolves; this
    // just stops wasting the round-trip.
    const ctl = new AbortController();
    const timer = setTimeout(() => {
      setState((s) => ({ ...s, loading: true, error: null }));
      search(q, { mode, kb, signal: ctl.signal })
        .then((resp) => {
          if (gen !== generation.current) return;
          setState({
            hits: resp.hits,
            ms: resp.ms,
            embedMs: resp.embed_ms,
            cacheHit: resp.cache_hit,
            loading: false,
            error: null,
          });
        })
        .catch((e) => {
          if (gen !== generation.current) return;
          // Superseded request — the abort is expected, not a failure.
          if (isAbortError(e)) return;
          setState({
            hits: [],
            ms: 0,
            embedMs: 0,
            cacheHit: false,
            loading: false,
            error: String(e),
          });
        });
    }, DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
      ctl.abort();
    };
  }, [q, mode, kb]);

  return state;
}
