import { useQuery } from "@tanstack/react-query";
import { fetchFileHistory } from "../api/client";

/// `GET /api/file-history?repo=&path=` — one file's history, newest-first
/// (Phase C4). Gated on `enabled` (the reader's History inspector tab is
/// opt-in, same "don't fetch until the tab is actually opened" discipline
/// `useBlame` uses for the Provenance toggle).
export function useFileHistory(repo: string | undefined, path: string | undefined, enabled: boolean) {
  return useQuery({
    queryKey: ["file-history", repo, path],
    queryFn: () => fetchFileHistory(repo as string, path as string),
    enabled: enabled && repo !== undefined && path !== undefined,
  });
}
