import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  createBookmark,
  deleteBookmark,
  fetchBookmarks,
  patchBookmark,
  type CreateBookmarkInput,
  type PatchBookmarkInput,
} from "../api/client";

/// Phase N — query-key convention: `["bookmarks", repo]`. The
/// `bookmark.changed` SSE handler invalidates by that key alone (prefix
/// match), same shape as `hooks/useSets.ts`.
export function bookmarksQueryKey(repo: string | undefined) {
  return ["bookmarks", repo] as const;
}

/// `GET /api/bookmarks?repo=`.
export function useBookmarks(repo: string | undefined) {
  return useQuery({
    queryKey: bookmarksQueryKey(repo),
    queryFn: () => fetchBookmarks(repo as string),
    enabled: repo !== undefined && repo !== "",
  });
}

export function useCreateBookmark(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (input: CreateBookmarkInput) => createBookmark(input),
    onSuccess: () => qc.invalidateQueries({ queryKey: bookmarksQueryKey(repo) }),
  });
}

export function usePatchBookmark(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, input }: { id: number; input: PatchBookmarkInput }) =>
      patchBookmark(id, input),
    onSuccess: () => qc.invalidateQueries({ queryKey: bookmarksQueryKey(repo) }),
  });
}

export function useDeleteBookmark(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: number) => deleteBookmark(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: bookmarksQueryKey(repo) }),
  });
}
