import { useCallback, useEffect, useMemo, useState } from "react";
import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query";
import {
  fetchDocsPage,
  type DocsQueryParams,
  type DocsPage,
  type DocSummary,
} from "../api/client";

// S-milestone S4 — paginated docs hook for the gallery; TQ1 moved the
// machinery onto TanStack Query (useInfiniteQuery) keeping the public
// surface identical.
//
// Behaviour:
//   * `query` describes the filter/sort/projection. Pagination state
//     lives in the query cache under ["docs", kb, params, pageSize];
//     the caller never threads it.
//   * The KEY is debounced 150 ms so a flurry of URL param flips
//     (toggling tags rapidly) coalesces into one request; previous rows
//     stay visible while the new key loads (placeholderData), matching
//     the old hook's keep-rows-until-replaced UX.
//   * `loadMore()` fetches the next page into the cache; abort-on-
//     supersede is react-query's stale-query cancellation.
//   * Invalidation arrives from the SSE bridge (api/queryClient.ts):
//     artifact.indexed/removed → invalidate ["docs", kb] (burst-gated),
//     gap → invalidate everything. All loaded pages refetch in place,
//     so scroll depth survives churn (the old hook reset to page 0).
//
// `total` is the server-side post-filter count (NOT `rows.length`) so
// the "12 of 288" badge stays accurate mid-pagination.
export type UseDocsResult = {
  rows: DocSummary[];
  total: number;
  isLoading: boolean;
  hasMore: boolean;
  error: string | null;
  loadMore: () => void;
  /// Force a refetch — invalidates every ["docs", kb] query. Mostly
  /// superseded by the SSE bridge; kept for explicit-refresh callers.
  refresh: () => void;
  /// v0.10 Q2 — route timing in milliseconds from the first page.
  ms?: number;
  /// v0.10 Q2 — query parser diagnostics surfaced through the route.
  queryWarnings: string[];
};

/// Debounce a serialisable value. Used on the query KEY: react-query
/// cancels the superseded key's fetch automatically, so debouncing the
/// key gives the old hook's request-coalescing without a timer around
/// the fetch itself.
function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setV(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return v;
}

// `kb` is null while the KB list is still loading; the hook stays idle
// until a concrete kb name is available.
export function useDocs(
  kb: string | null,
  query: Omit<DocsQueryParams, "offset" | "signal">,
  pageSize = 200,
): UseDocsResult {
  const queryClient = useQueryClient();
  // Serialised so a same-shape object doesn't re-key every render
  // (undefined fields drop out, giving a canonical form).
  const serialized = useDebounced(JSON.stringify(query), 150);
  const params = useMemo(
    () => JSON.parse(serialized) as Omit<DocsQueryParams, "offset" | "signal">,
    [serialized],
  );

  const q = useInfiniteQuery({
    queryKey: ["docs", kb, params, pageSize] as const,
    enabled: kb !== null,
    initialPageParam: 0,
    queryFn: ({ pageParam, signal }) =>
      fetchDocsPage(kb as string, {
        ...params,
        offset: pageParam,
        limit: pageSize,
        signal,
      }),
    getNextPageParam: (last: DocsPage) =>
      last.has_more ? last.offset + last.docs.length : undefined,
    placeholderData: (prev) => prev,
  });

  const rows = useMemo(
    () => q.data?.pages.flatMap((p) => p.docs) ?? [],
    [q.data],
  );
  const first = q.data?.pages[0];
  const lastPage = q.data?.pages[q.data.pages.length - 1];

  const { hasNextPage, isFetching, fetchNextPage } = q;
  const loadMore = useCallback(() => {
    if (hasNextPage && !isFetching) void fetchNextPage();
  }, [hasNextPage, isFetching, fetchNextPage]);

  const refresh = useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: ["docs", kb] });
  }, [queryClient, kb]);

  return {
    rows,
    total: lastPage?.total ?? 0,
    isLoading: q.isFetching,
    hasMore: q.hasNextPage ?? false,
    error: q.error ? String(q.error) : null,
    loadMore,
    refresh,
    ms: first?.ms,
    queryWarnings: first?.query_warnings ?? [],
  };
}
