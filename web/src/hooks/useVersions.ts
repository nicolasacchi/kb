// Track V — load an artifact's version timeline. v0.22 — moved onto TanStack
// Query (keyed ["versions", kb, id]) so the VersionsPanel (the full timeline)
// and the rail's versions-count badge SHARE one fetch, and so the SSE bridge
// can refresh both on `artifact.indexed` (there is no version.* event). The
// timeline is a per-artifact read; the badge enables it eagerly for the open
// doc, the panel re-uses the cached result the moment it opens.

import { useQuery } from "@tanstack/react-query";
import { fetchVersions, type Version } from "../api/versions";

export type UseVersions = {
  versions: Version[];
  mode: string;
  loading: boolean;
  error: string | null;
};

export function useVersions(
  kb: string,
  id: string,
  enabled: boolean,
): UseVersions {
  const q = useQuery({
    queryKey: ["versions", kb, id],
    enabled: enabled && !!kb && !!id,
    queryFn: ({ signal }) => fetchVersions(kb, id, signal),
    staleTime: Infinity,
  });
  return {
    versions: q.data?.versions ?? [],
    mode: q.data?.mode ?? "",
    loading: q.isLoading,
    error: q.error
      ? q.error instanceof Error
        ? q.error.message
        : String(q.error)
      : null,
  };
}
