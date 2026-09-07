// V72-I2 — the `~rails` surface's data hooks (`rails/1`, `GET /api/rails/*`).
//
// Same "doclens-style" shape `useFrameworkEdges.ts` documents and for the
// same two reasons: `rails/1` is computed PER REQUEST out of reads that only
// change on a re-ingest pass (so a finite `staleTime` is the honest cache,
// not an SSE tie — `rails_edges` has no event of its own), and a miss is a
// repo-config fact ("not a Rails app", "the lens has not run here") rather
// than a transient failure, so `retry: false`.
//
// The list hook takes `limit`/`offset` and puts them IN THE QUERY KEY: paging
// is the server's (`RailsListOut.total` is the true post-filter size), so two
// pages are two cache entries, never one page sliced twice.
import { useQuery } from "@tanstack/react-query";
import {
  fetchActions,
  fetchRailsHome,
  fetchRailsNoun,
  fetchRailsOrphans,
} from "../api/client";

const RAILS_STALE_MS = 30_000;

export function railsHomeQueryKey(repo: string | undefined) {
  return ["rails", "home", repo] as const;
}

export function railsNounQueryKey(
  repo: string | undefined,
  noun: string,
  q: string,
  limit: number,
  offset: number,
) {
  return ["rails", "noun", repo, noun, q, limit, offset] as const;
}

export function railsOrphansQueryKey(repo: string | undefined) {
  return ["rails", "orphans", repo] as const;
}

export function useRailsHome(repo: string | undefined) {
  return useQuery({
    queryKey: railsHomeQueryKey(repo),
    queryFn: () => fetchRailsHome(repo as string),
    enabled: repo !== undefined && repo !== "",
    staleTime: RAILS_STALE_MS,
    retry: false,
  });
}

export interface RailsNounQuery {
  repo: string | undefined;
  noun: string;
  /// The SERVER's `q=` (a case-insensitive substring over name and path),
  /// applied before `total` is counted. Never a client-side filter.
  q?: string;
  limit: number;
  offset: number;
  enabled?: boolean;
}

export function useRailsNoun(args: RailsNounQuery) {
  const q = args.q ?? "";
  return useQuery({
    queryKey: railsNounQueryKey(args.repo, args.noun, q, args.limit, args.offset),
    queryFn: () =>
      fetchRailsNoun({
        repo: args.repo as string,
        noun: args.noun,
        q: q === "" ? undefined : q,
        limit: args.limit,
        offset: args.offset,
      }),
    enabled: args.repo !== undefined && args.repo !== "" && args.enabled !== false,
    staleTime: RAILS_STALE_MS,
    retry: false,
  });
}

export function useRailsOrphans(repo: string | undefined, enabled = true) {
  return useQuery({
    queryKey: railsOrphansQueryKey(repo),
    queryFn: () => fetchRailsOrphans(repo as string),
    enabled: enabled && repo !== undefined && repo !== "",
    staleTime: RAILS_STALE_MS,
    retry: false,
  });
}

/// `GET /api/actions` for ONE address — the server-rendered action list the
/// Rails atom card shows for a hovered atom's TARGET. Deliberately a query
/// (not `Reader.tsx`'s imperative `ActionMenuState` fetch): the atom card's
/// target is stable for as long as the card is open, so the ordinary cache
/// applies, where the popover's target changes per right-click.
///
/// Note the ROWS are the server's, whole and in its own order — the card
/// renders `menuOrder(out.groups)` and hand-picks none (`kbc-actions/1`'s
/// "nothing composes an action client-side" rule).
export function useAtomActions(
  repo: string | undefined,
  target: { path: string; line: number } | null,
  ref?: string,
) {
  return useQuery({
    queryKey: ["rails", "atom-actions", repo, target?.path ?? "", target?.line ?? 0, ref ?? ""],
    queryFn: () =>
      fetchActions({
        repo: repo as string,
        path: (target as { path: string }).path,
        line: (target as { line: number }).line,
        col: 1,
        ref,
      }),
    enabled: repo !== undefined && repo !== "" && target !== null,
    staleTime: RAILS_STALE_MS,
    retry: false,
  });
}
