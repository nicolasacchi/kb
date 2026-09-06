import { useQuery } from "@tanstack/react-query";
import {
  fetchStacks,
  fetchStacksLayerDiff,
  type FetchStacksParams,
} from "../api/client";
import { BEHAVIORAL_STALE_MS } from "./useBehavioral";

export function stacksQueryKey(params: FetchStacksParams | undefined) {
  return ["stacks", params?.repo, params?.all ? "all" : "multi"] as const;
}

/// `GET /api/stacks` — ref-derived, generous staleTime.
export function useStacks(params: FetchStacksParams | undefined, enabled = true) {
  return useQuery({
    queryKey: stacksQueryKey(params),
    queryFn: () => fetchStacks(params as FetchStacksParams),
    enabled: enabled && params !== undefined && params.repo !== "",
    staleTime: BEHAVIORAL_STALE_MS,
  });
}

export function stacksLayerDiffKey(repo: string | undefined, branch: string | undefined) {
  return ["stacks", "layer-diff", repo, branch] as const;
}

/// `GET /api/stacks/layer-diff` — only when a layer is selected.
export function useStacksLayerDiff(
  repo: string | undefined,
  branch: string | undefined,
  enabled = true,
) {
  return useQuery({
    queryKey: stacksLayerDiffKey(repo, branch),
    queryFn: () => fetchStacksLayerDiff(repo as string, branch as string),
    enabled: enabled && !!repo && !!branch,
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}
