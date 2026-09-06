import { useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  search,
  type SearchHit,
  type SearchMode,
  type SearchScope,
  type SearchSort,
} from "../api/client";

export type UseSearchArgs = {
  q: string;
  mode: SearchMode;
  scope: SearchScope;
  kb?: string;
  category?: string;
  folder?: string;
  // Q-track facets — see SearchOptions; every axis is part of the query key.
  tags?: string[];
  excludeTags?: string[];
  status?: string[];
  severity?: string[];
  caps?: string[];
  since?: string;
  sinceField?: "created" | "modified";
  read?: string[];
  session?: string;
  list?: string;
  // W1.search — "read during" window (unix seconds), a history-opens
  // window distinct from `since`/`sinceField` (mtime/created recency).
  readFrom?: number;
  readTo?: number;
  sort?: SearchSort;
  dir?: "asc" | "desc";
  limit: number;
};

// 150 ms debounce on a serialisable value — mirrors useDocs so a flurry of
// facet/chip toggles coalesces into one request (react-query cancels the
// superseded key's fetch via its signal). Debouncing the KEY, not a timer
// around the fetch, is the gallery's request-coalescing idiom.
function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setV(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return v;
}

export type SearchResultsState = {
  hits: SearchHit[];
  ms?: number;
  embedMs?: number;
  cacheHit: boolean;
  loading: boolean;
  error: string | null;
};

// Track F — the full search page's data hook. TanStack Query keyed on every
// search axis; `rich: true` requests the widened per-hit payload
// (detail=full) so cards render in one round-trip. Disabled on an empty
// query, and (in single-kb scope) until a kb is resolved. `placeholderData`
// keeps the prior hits on screen while a new key loads, so refining the
// query / bumping the limit doesn't flash empty — matching useDocs. The SSE
// bridge invalidates the ["search"] prefix on artifact.indexed/removed.
export function useSearchResults(a: UseSearchArgs): SearchResultsState {
  // The serialised args ARE the query key (every axis included) and the
  // debounced snapshot the queryFn reads — one place, no drift. Default
  // (undefined) fields drop out of JSON, keeping the key canonical. The
  // SSE bridge still prefix-matches ["search"] for invalidation.
  const serialized = useDebounced(JSON.stringify(a), 150);
  const d = useMemo(() => JSON.parse(serialized) as UseSearchArgs, [serialized]);
  // FS6 — browse mode: a query is no longer required; an active filter or a
  // non-relevance sort runs an empty-query browse (the daemon lists +
  // filters + sorts, no relevance ranking).
  const hasFilters = !!(
    d.category ||
    d.folder ||
    d.session ||
    d.list ||
    d.since ||
    d.read?.length ||
    d.tags?.length ||
    d.excludeTags?.length ||
    d.status?.length ||
    d.severity?.length ||
    d.caps?.length ||
    d.readFrom != null ||
    d.readTo != null
  );
  const hasSort = !!(d.sort && d.sort !== "relevance");
  const enabled =
    (d.q.trim().length > 0 || hasFilters || hasSort) &&
    (d.scope === "all" || !!d.kb);
  const query = useQuery({
    queryKey: ["search", serialized] as const,
    enabled,
    queryFn: ({ signal }) =>
      search(d.q, {
        mode: d.mode,
        scope: d.scope,
        kb: d.kb,
        category: d.category,
        folder: d.folder,
        tags: d.tags,
        excludeTags: d.excludeTags,
        status: d.status,
        severity: d.severity,
        caps: d.caps,
        since: d.since,
        sinceField: d.sinceField,
        read: d.read,
        session: d.session,
        list: d.list,
        readFrom: d.readFrom,
        readTo: d.readTo,
        sort: d.sort,
        dir: d.dir,
        limit: d.limit,
        rich: true,
        signal,
      }),
    placeholderData: (prev) => prev,
  });
  return useMemo(
    () => ({
      hits: query.data?.hits ?? [],
      ms: query.data?.ms,
      embedMs: query.data?.embed_ms,
      cacheHit: query.data?.cache_hit ?? false,
      loading: query.isFetching,
      error: query.error
        ? query.error instanceof Error
          ? query.error.message
          : String(query.error)
        : null,
    }),
    [query.data, query.isFetching, query.error],
  );
}
