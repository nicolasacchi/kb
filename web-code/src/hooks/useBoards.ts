import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  acceptBoard,
  applyBoard,
  archiveBoard,
  fetchBoard,
  fetchBoardSweep,
  fetchBoards,
} from "../api/client";

/// `kbc-canvas/1` query keys (V74-L2). List is `["boards", repo]`; one board is
/// `["boards", repo, slug, ctx, live]` — the two view flags are part of the key
/// because they change what the DAEMON computes (whether the context range is
/// read, whether every query card is executed), not what this side hides. A
/// prefix invalidate on `["boards", repo]` therefore reaches every variant.
export function boardsListQueryKey(repo: string | undefined, status?: string | null) {
  return ["boards", repo, "list", status ?? null] as const;
}

export function boardQueryKey(
  repo: string | undefined,
  slug: string | undefined,
  opts: { ctx?: boolean; live?: boolean } = {},
) {
  return ["boards", repo, slug, Boolean(opts.ctx), Boolean(opts.live)] as const;
}

export function boardSweepQueryKey(repo: string | undefined, slug?: string) {
  return ["boards", repo, "sweep", slug ?? null] as const;
}

/// `GET /api/boards?repo=[&status=]`.
export function useBoards(repo: string | undefined, status?: string | null) {
  return useQuery({
    queryKey: boardsListQueryKey(repo, status),
    queryFn: () => fetchBoards(repo as string, status ?? undefined),
    enabled: Boolean(repo),
  });
}

/// `GET /api/boards/{slug}?repo=[&ctx=1][&live=1]` — the board, re-resolved NOW.
export function useBoard(
  repo: string | undefined,
  slug: string | undefined,
  opts: { ctx?: boolean; live?: boolean } = {},
) {
  return useQuery({
    queryKey: boardQueryKey(repo, slug, opts),
    queryFn: () => fetchBoard(repo as string, slug as string, opts),
    enabled: Boolean(repo && slug),
  });
}

/// `GET /api/boards/sweep` — the drift report, fetched ON DEMAND only.
///
/// `enabled` defaults to `false` because a sweep EXECUTES every query card on
/// every board (`boards::routes::sweep_boards`'s own doc: "a sweep is an
/// explicit, occasional check, not a page load"). Turning it into an ambient
/// query would put a full unified search per card behind every navigation.
export function useBoardSweep(
  repo: string | undefined,
  slug: string | undefined,
  enabled: boolean,
) {
  return useQuery({
    queryKey: boardSweepQueryKey(repo, slug),
    queryFn: () => fetchBoardSweep(repo as string, slug),
    enabled: Boolean(repo) && enabled,
    // A drift report is a measurement with a time on it, not a cache entry —
    // re-opening the panel re-measures rather than re-showing an old verdict.
    staleTime: 0,
    gcTime: 0,
  });
}

/// `POST /api/boards/apply` — LOOPBACK-ONLY. The whole document by slug.
export function useApplyBoard(repo: string | undefined) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (vars: { doc: unknown; dryRun?: boolean; allowDisconnected?: boolean }) =>
      applyBoard(vars.doc, { dryRun: vars.dryRun, allowDisconnected: vars.allowDisconnected }),
    onSuccess: (_out, vars) => {
      if (vars.dryRun) return;
      qc.invalidateQueries({ queryKey: ["boards", repo] });
    },
  });
}

/// `POST /api/boards/{slug}/accept` — LOOPBACK-ONLY (D21).
export function useAcceptBoard(repo: string | undefined) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (slug: string) => acceptBoard(repo as string, slug),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["boards", repo] }),
  });
}

/// `POST /api/boards/{slug}/archive` — LOOPBACK-ONLY.
export function useArchiveBoard(repo: string | undefined) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (slug: string) => archiveBoard(repo as string, slug),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["boards", repo] }),
  });
}
