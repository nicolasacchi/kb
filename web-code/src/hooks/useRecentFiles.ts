import { useQuery } from "@tanstack/react-query";
import { fetchSearchFiles } from "../api/client";

/// F4 — Home dashboard's per-repo "Recent files" card section: `GET
/// /api/search/files?repo=&q=&limit=` with an EMPTY `q` (`FileIndex::
/// recent`'s opened-at-ordered list — see `api/client.ts`'s
/// `fetchSearchFiles` doc), never the unified `/api/search` endpoint. One
/// query per repo card (`components/home/RepoCard.tsx`), keyed on
/// `[repo, limit]` so cards never share a cache slot.
export function useRecentFiles(repo: string, limit: number) {
  return useQuery({
    queryKey: ["recentFiles", repo, limit],
    queryFn: () => fetchSearchFiles(repo, "", limit),
  });
}
