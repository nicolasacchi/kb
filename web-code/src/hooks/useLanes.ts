import { useQuery } from "@tanstack/react-query";
import { fetchLaneFacts, fetchLanes, fetchLanesSummary } from "../api/client";

export function lanesQueryKey(repo: string | undefined) {
  return ["lanes", repo ?? null] as const;
}

export function laneFactsQueryKey(repo: string | undefined, path: string | undefined) {
  return ["lane-facts", repo ?? null, path ?? null] as const;
}

export function lanesSummaryQueryKey(repo: string | undefined) {
  return ["lanes-summary", repo ?? null] as const;
}

/// `GET /api/lanes[?repo=]` — the registry dock and the rail's enablement.
export function useLanes(repo: string | undefined) {
  return useQuery({
    queryKey: lanesQueryKey(repo),
    queryFn: () => fetchLanes(repo),
    enabled: repo !== undefined && repo !== "",
  });
}

/// `GET /api/lanes/facts?repo=&path=` — per-request classing for the open file.
/// Always fetched when a file is open (same always-fetch discipline as
/// comments/1); the coverage-band toggle is a client filter over this
/// response, never a re-fetch.
export function useLaneFacts(repo: string | undefined, path: string | undefined) {
  return useQuery({
    queryKey: laneFactsQueryKey(repo, path),
    queryFn: () => fetchLaneFacts(repo as string, path as string),
    enabled: repo !== undefined && repo !== "" && path !== undefined && path !== "",
  });
}

export function useLanesSummary(repo: string | undefined) {
  return useQuery({
    queryKey: lanesSummaryQueryKey(repo),
    queryFn: () => fetchLanesSummary(repo as string),
    enabled: repo !== undefined && repo !== "",
  });
}
