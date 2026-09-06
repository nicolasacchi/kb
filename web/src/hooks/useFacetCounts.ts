import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  fetchFacets,
  fetchFolders,
  fetchTags,
  type FolderNode,
} from "../api/client";

// v0.22 — corpus-wide facet COUNTS for the reader's "Explore from here" chip
// strip. One cached fetch each of /tags, /facets, /folders (the count-bearing
// facet endpoints), intersected CLIENT-SIDE with the open doc's own facets —
// so a doc with N tags doesn't fire N count queries. All three ride the SSE
// bridge (['tags'|'facets'|'folders', kb] invalidated on artifact.indexed —
// invariant #23/#24), so the counts stay fresh as the corpus changes.

export type FacetCounts = {
  tag: (name: string) => number | undefined;
  category: (value: string) => number | undefined;
  folder: (path: string) => number | undefined;
  /// True once at least one facet source has resolved (so the strip can hold
  /// its chips until counts are available rather than flashing count-less).
  ready: boolean;
};

function flattenFolders(
  nodes: FolderNode[],
  out: Map<string, number>,
): Map<string, number> {
  for (const n of nodes) {
    out.set(n.path, n.count);
    flattenFolders(n.children, out);
  }
  return out;
}

export function useFacetCounts(kb: string | null): FacetCounts {
  const enabled = !!kb;
  const tagsQ = useQuery({
    queryKey: ["tags", kb],
    enabled,
    queryFn: ({ signal }) => fetchTags(kb!, signal),
    staleTime: Infinity,
  });
  const facetsQ = useQuery({
    queryKey: ["facets", kb],
    enabled,
    queryFn: ({ signal }) => fetchFacets(kb!, signal),
    staleTime: Infinity,
  });
  const foldersQ = useQuery({
    queryKey: ["folders", kb],
    enabled,
    queryFn: ({ signal }) => fetchFolders(kb!, signal),
    staleTime: Infinity,
  });

  const tagMap = useMemo(() => {
    const m = new Map<string, number>();
    for (const t of tagsQ.data ?? []) m.set(t.name, t.count);
    return m;
  }, [tagsQ.data]);
  const catMap = useMemo(() => {
    const m = new Map<string, number>();
    for (const b of facetsQ.data?.categories ?? []) m.set(b.value, b.count);
    return m;
  }, [facetsQ.data]);
  const folderMap = useMemo(
    () => flattenFolders(foldersQ.data?.folders ?? [], new Map()),
    [foldersQ.data],
  );

  return {
    tag: (name) => tagMap.get(name),
    category: (value) => catMap.get(value),
    folder: (path) => folderMap.get(path),
    ready: !!tagsQ.data || !!facetsQ.data || !!foldersQ.data,
  };
}
