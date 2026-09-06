// The corpus-wide `kind = 'link'` edge list, keyed ["edges", kb] so every
// consumer (the reader rail's Links badge + neighbor lists) SHARES one fetch
// and it is cached across reader→reader navigation within a kb (the list is
// identical for every doc in the corpus). Rides the SSE bridge: the docsGate
// invalidates ["edges", kb] on artifact.indexed/removed (links shift on
// reindex; there is no dedicated edge.* event) — invariant #23/#24, no manual
// EventSource. Corpus-local (invariant #29); resolution is unchanged, only
// fetch/caching moves here.

import { useQuery } from "@tanstack/react-query";
import { fetchAtlasEdges, type AtlasEdge } from "../api/client";

export function useEdges(kb: string | null): { edges: AtlasEdge[] } {
  const q = useQuery({
    queryKey: ["edges", kb ?? ""],
    enabled: !!kb,
    queryFn: ({ signal }) => fetchAtlasEdges(kb!, signal),
    staleTime: Infinity,
  });
  return { edges: q.data ?? [] };
}
