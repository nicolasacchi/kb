// FXUI — one directory listing as a TanStack query. Key
// ["term","files",termId,root,path]; finite staleTime and a manual `refetch`:
// the documented no-SSE-tie kind of exception (#23), so no polling and no
// stream. The query function is the only read path of the Files tab.

import { useQuery } from "@tanstack/react-query";
import type { TermLsAnswer } from "../../api/generated/TermLsAnswer";
import { listDir, type FilesRoot } from "./filesApi";

export const FILES_STALE_MS = 30_000;

export const filesKey = (termId: string, root: FilesRoot, path: string) => ["term", "files", termId, root, path] as const;

export function useFilesListing(termId: string, root: FilesRoot, path: string) {
  return useQuery<TermLsAnswer>({
    queryKey: filesKey(termId, root, path),
    queryFn: ({ signal }) => listDir(termId, root, path, signal),
    enabled: termId !== "",
    staleTime: FILES_STALE_MS,
    retry: false,
    refetchOnWindowFocus: false,
  });
}
