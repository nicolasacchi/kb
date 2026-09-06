import { useQuery } from "@tanstack/react-query";
import {
  ApiError,
  fetchAge,
  fetchCoupling,
  fetchHotspots,
  fetchOwnership,
  fetchReviewRisk,
  fetchTimeseries,
  type FetchHotspotsParams,
  type FetchTimeseriesParams,
} from "../api/client";

/// History-derived; not live. Generous staleTime so tree/entity reuse one
/// fetch per repo without hammering the daemon on every row paint.
export const BEHAVIORAL_STALE_MS = 10 * 60 * 1000; // 10 min

export function hotspotsQueryKey(params: FetchHotspotsParams | undefined) {
  return [
    "behavioral",
    "hotspots",
    params?.repo,
    params?.limit ?? null,
    params?.scope ?? null,
    params?.weight ?? null,
  ] as const;
}

export function useHotspots(params: FetchHotspotsParams | undefined, enabled = true) {
  return useQuery({
    queryKey: hotspotsQueryKey(params),
    queryFn: () => fetchHotspots(params as FetchHotspotsParams),
    enabled: enabled && params !== undefined && params.repo !== "",
    staleTime: BEHAVIORAL_STALE_MS,
  });
}

/// Tree overlay / entity map: one generous fetch per repo (default limit).
export function useHotspotsMap(repo: string | undefined, enabled = true) {
  return useQuery({
    queryKey: ["behavioral", "hotspots", repo, "map"] as const,
    queryFn: async () => {
      const out = await fetchHotspots({ repo: repo as string, limit: 500 });
      const map = new Map<string, (typeof out.items)[number]>();
      for (const row of out.items) map.set(row.path, row);
      return map;
    },
    enabled: enabled && !!repo,
    staleTime: BEHAVIORAL_STALE_MS,
  });
}

export function useCoupling(
  repo: string | undefined,
  path: string | undefined,
  enabled = true,
) {
  return useQuery({
    queryKey: ["behavioral", "coupling", repo, path] as const,
    queryFn: () => fetchCoupling(repo as string, path as string),
    enabled: enabled && !!repo && !!path,
    staleTime: BEHAVIORAL_STALE_MS,
  });
}

export function useOwnership(
  repo: string | undefined,
  path: string | undefined,
  enabled = true,
) {
  return useQuery({
    queryKey: ["behavioral", "ownership", repo, path] as const,
    queryFn: () => fetchOwnership(repo as string, path as string),
    enabled: enabled && !!repo && !!path,
    staleTime: BEHAVIORAL_STALE_MS,
  });
}

export function useAge(repo: string | undefined, path: string | undefined, enabled = true) {
  return useQuery({
    queryKey: ["behavioral", "age", repo, path] as const,
    queryFn: () => fetchAge(repo as string, path as string),
    enabled: enabled && !!repo && !!path,
    staleTime: BEHAVIORAL_STALE_MS,
  });
}

export function timeseriesQueryKey(params: FetchTimeseriesParams | undefined) {
  return [
    "behavioral",
    "timeseries",
    params?.repo,
    params?.path ?? null,
    params?.weeks ?? null,
  ] as const;
}

/**
 * `GET /api/behavioral/timeseries`. Lazy callers should pass `enabled`
 * only when the sparkline is visible / expander is open — never N eager
 * fetches for a whole table. Generous staleTime (same as other behavioral).
 */
export function useTimeseries(
  params: FetchTimeseriesParams | undefined,
  enabled = true,
) {
  return useQuery({
    queryKey: timeseriesQueryKey(params),
    queryFn: () => fetchTimeseries(params as FetchTimeseriesParams),
    enabled: enabled && params !== undefined && params.repo !== "",
    staleTime: BEHAVIORAL_STALE_MS,
  });
}

export function reviewRiskKey(repo: string | undefined, id: number | undefined) {
  return ["reviews", repo, "risk", id] as const;
}

/**
 * `GET /api/reviews/{id}/risk`. On 404 (B2 not landed) the query succeeds
 * with `null` so the UI can hide sort/badges with no stub chrome. Other
 * errors still surface as query errors.
 */
export function useReviewRisk(
  repo: string | undefined,
  id: number | undefined,
  enabled = true,
) {
  return useQuery({
    queryKey: reviewRiskKey(repo, id),
    queryFn: async () => {
      try {
        return await fetchReviewRisk(id as number);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },
    enabled: enabled && repo !== undefined && id !== undefined,
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}
