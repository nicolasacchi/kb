// kb desk — GET /api/desk. Joins invariant #23's documented no-SSE-tie set
// (daycard / live tail / presence / doclens — see queryClient.ts): finite
// staleTime + refetchOnWindowFocus, NO SSE subscription of its own. The
// Header pill and the reader banner share this cache entry when called
// without a kb (fleet-wide); `useDesk(kb)` is a distinct scoped variant.

import { useQuery } from "@tanstack/react-query";
import { fetchDesk, type DeskItem } from "../api/desk";

export const DESK_KEY = ["desk"] as const;
export const DESK_STALE_MS = 60_000;

const EMPTY_ITEMS: DeskItem[] = [];

export function deskQueryKey(kb?: string) {
  return kb ? (["desk", kb] as const) : DESK_KEY;
}

export function useDesk(kb?: string): {
  items: DeskItem[];
  attention: number;
  loading: boolean;
  error: boolean;
} {
  const q = useQuery({
    queryKey: deskQueryKey(kb),
    queryFn: ({ signal }) => fetchDesk(kb, signal),
    staleTime: DESK_STALE_MS,
    refetchOnWindowFocus: true,
  });
  return {
    items: q.data?.items ?? EMPTY_ITEMS,
    attention: q.data?.attention ?? 0,
    loading: q.isPending,
    error: q.isError,
  };
}
