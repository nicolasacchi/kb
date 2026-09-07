import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ApiError, fetchTrail, fetchTrailState, fetchTrails, forkTrail, purgeTrails, setTrailState } from "../api/client";
import type { TrailsListOut } from "../api/types";

// `kbc-trail/1` (V74-L3b, design D17) — the SPA half of a ledger that is OFF
// by default.
//
// TWO fetch postures, and the split is the daemon's privacy posture rather
// than a caching decision:
//
//   * `useTrailState` is an ordinary bearer read and ALWAYS runs. The
//     indicator D17 requires is mandatory, and an indicator that cannot
//     render is not an indicator — so "off" is a state this hook returns,
//     never an error it throws.
//   * `useTrails`/`useTrail` are LOOPBACK-ONLY on the daemon. A non-loopback
//     browser gets a 404 from a route that is not there, which
//     `foldLoopbackAbsence` turns into an empty list with a stated reason
//     rather than a red error toast: the operator's own movement record not
//     leaving their box is the design, not a fault.

/// The `mode` values `trails::MODES` declares, in indicator order.
export const TRAIL_MODES = ["off", "recording", "paused"] as const;
export type TrailMode = (typeof TRAIL_MODES)[number];

/// Total: an unknown mode from a newer daemon reads as `off`, which is the
/// SAME fail-closed rule `trails::mode_static` applies server-side. An
/// indicator that said "recording" for a mode this build cannot interpret
/// would be the one lie this surface exists to prevent.
export function normalizeTrailMode(raw: string | null | undefined): TrailMode {
  return (TRAIL_MODES as readonly string[]).includes(raw ?? "") ? (raw as TrailMode) : "off";
}

export function trailStateQueryKey() {
  return ["trails", "state"] as const;
}

export function trailsListQueryKey(repo: string | undefined) {
  return ["trails", repo, "list"] as const;
}

export function trailQueryKey(repo: string | undefined, id: string | undefined, notes: boolean) {
  return ["trails", repo, id, notes] as const;
}

/// `GET /api/trails/state` — the indicator's source of truth.
///
/// A short `staleTime` rather than `Infinity`: the mode can change from the
/// CLI (`kb-code trail state --mode paused`) with no SSE event to hear, so
/// the indicator re-asks rather than showing a stale posture. This is the
/// documented "no-SSE-tie set carrying finite staleTime" carve-out kb's own
/// invariant #23 names, applied here for the same reason.
export function useTrailState() {
  return useQuery({
    queryKey: trailStateQueryKey(),
    queryFn: fetchTrailState,
    staleTime: 15_000,
    refetchOnWindowFocus: true,
  });
}

/// `POST /api/trails/state` — LOOPBACK-ONLY and audited.
export function useSetTrailState() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (mode: TrailMode) => setTrailState(mode),
    onSuccess: (out) => {
      qc.setQueryData(trailStateQueryKey(), out);
      qc.invalidateQueries({ queryKey: ["trails"] });
    },
  });
}

/// The empty, HONEST body a non-loopback browser gets for the two human
/// reads. Exported so the rail can render the same words the daemon would.
export const TRAILS_NOT_LOOPBACK: TrailsListOut = {
  schema: "kbc-trail/1",
  repo: "",
  enabled: false,
  mode: "off",
  origins_available: ["recorded", "authored"],
  trails: [],
  notes: [
    "your own trail is readable only over loopback — kbc-trail/1's movement record never leaves the operator's box (design D17). Reach kb-code at 127.0.0.1 to read it.",
  ],
};

function isLoopbackAbsence(e: unknown): boolean {
  return e instanceof ApiError && (e.status === 404 || e.status === 403);
}

/// `GET /api/trails?repo=` — LOOPBACK-ONLY. Never throws for the absence of
/// the route itself; see this module's header.
export function useTrails(repo: string | undefined, limit?: number) {
  return useQuery({
    queryKey: trailsListQueryKey(repo),
    queryFn: async () => {
      try {
        return await fetchTrails(repo as string, limit);
      } catch (e) {
        if (isLoopbackAbsence(e)) return { ...TRAILS_NOT_LOOPBACK, repo: repo ?? "" };
        throw e;
      }
    },
    enabled: Boolean(repo),
    staleTime: 15_000,
  });
}

/// `GET /api/trails/{id}?repo=[&notes=1]` — LOOPBACK-ONLY.
export function useTrail(
  repo: string | undefined,
  id: string | undefined,
  opts: { notes?: boolean } = {},
) {
  return useQuery({
    queryKey: trailQueryKey(repo, id, Boolean(opts.notes)),
    queryFn: () => fetchTrail(repo as string, id as string, opts),
    enabled: Boolean(repo && id),
    staleTime: 15_000,
  });
}

/// `POST /api/trails/purge` — LOOPBACK-ONLY, audited, and WHOLESALE unless
/// an id narrows it. Always behind the ONE confirm host at the call site.
export function usePurgeTrails(repo: string | undefined) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (vars: { id?: string } = {}) => purgeTrails(repo as string, vars),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["trails"] }),
  });
}

/// `POST /api/trails/{id}/fork` — LOOPBACK-ONLY.
export function useForkTrail(repo: string | undefined) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (vars: { id: string; fromOrdinal: number; title?: string }) =>
      forkTrail(repo as string, vars.id, vars.fromOrdinal, vars.title),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["trails"] }),
  });
}
