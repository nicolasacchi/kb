import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { applyTour, deleteTour, fetchTour, fetchTours } from "../api/client";

/// `kbc-tour/1` query keys (V74-L3b). Deliberately their own namespace rather
/// than a branch of `["boards", …]`: a tour IS a board on the SERVER, but a
/// cache key is about which REQUEST produced a body, and `/api/tours` and
/// `/api/boards` are different requests with different shapes. Sharing the
/// prefix would make a board invalidation silently refetch every tour.
export function toursListQueryKey(repo: string | undefined, status?: string | null) {
  return ["tours", repo, "list", status ?? null] as const;
}

/// `ctx` is part of the key because it changes what the DAEMON computes (it
/// reads each step's context range), not what this side hides.
export function tourQueryKey(
  repo: string | undefined,
  slug: string | undefined,
  opts: { ctx?: boolean } = {},
) {
  return ["tours", repo, slug, Boolean(opts.ctx)] as const;
}

/// `GET /api/tours?repo=[&status=]`.
export function useTours(repo: string | undefined, status?: string | null) {
  return useQuery({
    queryKey: toursListQueryKey(repo, status),
    queryFn: () => fetchTours(repo as string, status ?? undefined),
    enabled: Boolean(repo),
  });
}

/// `GET /api/tours/{slug}?repo=[&ctx=1]` — every step re-resolved NOW.
export function useTour(
  repo: string | undefined,
  slug: string | undefined,
  opts: { ctx?: boolean } = {},
) {
  return useQuery({
    queryKey: tourQueryKey(repo, slug, opts),
    queryFn: () => fetchTour(repo as string, slug as string, opts),
    enabled: Boolean(repo && slug),
  });
}

/// `POST /api/tours/apply` — LOOPBACK-ONLY. The whole document by slug.
export function useApplyTour(repo: string | undefined) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (vars: { doc: unknown; dryRun?: boolean }) =>
      applyTour(vars.doc, { dryRun: vars.dryRun }),
    onSuccess: (_out, vars) => {
      if (vars.dryRun) return;
      qc.invalidateQueries({ queryKey: ["tours", repo] });
    },
  });
}

/// `DELETE /api/tours/{slug}` — LOOPBACK-ONLY.
export function useDeleteTour(repo: string | undefined) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (slug: string) => deleteTour(repo as string, slug),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["tours", repo] }),
  });
}
