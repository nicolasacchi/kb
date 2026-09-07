import { useQuery } from "@tanstack/react-query";
import {
  fetchCommentKeywords,
  fetchComments,
  fetchCommentsFile,
  fetchCommentsSummary,
  type FetchCommentsParams,
} from "../api/client";

// V72-J2 (D8) — comments/1's SPA reads. `GET /api/comments/file` (the
// gutter's feed) is cheap enough to fetch unconditionally whenever a file is
// open, same "always-fetch discipline" `useAnnotations`/blame already use —
// the gutter mode toggle is a pure CLIENT filter over one response, never a
// re-fetch. `GET /api/comments` (paged) and `/summary` back the `~comments`
// dashboard; `/keywords` is daemon-wide and effectively static per session.

export function commentsFileQueryKey(repo: string | undefined, path: string | undefined) {
  return ["comments-file", repo, path] as const;
}

/// The per-file comment gutter's feed. `blobHash` is deliberately NOT part
/// of the key (same reasoning `useAnnotations` gives none either): a fresh
/// scan is keyed on `(repo, path)` server-side and this app has no SSE tie
/// for a source edit made outside the reader — a manual refetch (file
/// switch, or the daemon's own reindex-triggered invalidation elsewhere)
/// is what refreshes it, not a blob comparison here.
export function useCommentsFile(repo: string | undefined, path: string | undefined) {
  return useQuery({
    queryKey: commentsFileQueryKey(repo, path),
    queryFn: () => fetchCommentsFile(repo as string, path as string),
    enabled: repo !== undefined && path !== undefined && path !== "",
  });
}

export function commentsQueryKey(params: FetchCommentsParams | undefined) {
  return [
    "comments",
    params?.repo,
    params?.path ?? null,
    params?.kind ?? null,
    params?.keyword ?? null,
    params?.state ?? null,
    params?.limit ?? null,
    params?.offset ?? null,
  ] as const;
}

/// `GET /api/comments` — one PAGED lane. The `~comments` dashboard's default
/// view calls this once per `ACTIONABLE_STATES` entry (mirroring `kb-code
/// comments audit`'s own three-request shape, `lib/comments.ts`'s doc); the
/// "show everything" toggle calls it once with `state: undefined`.
export function useComments(params: FetchCommentsParams | undefined) {
  return useQuery({
    queryKey: commentsQueryKey(params),
    queryFn: () => fetchComments(params as FetchCommentsParams),
    enabled: params !== undefined && params.repo !== "",
  });
}

export function commentsSummaryQueryKey(repo: string | undefined) {
  return ["comments-summary", repo] as const;
}

/// `GET /api/comments/summary` — the dashboard's exact per-kind/per-keyword
/// counts and the two blame-free state lanes, for the facet chip counts and
/// the "N of M" bounds captions.
export function useCommentsSummary(repo: string | undefined) {
  return useQuery({
    queryKey: commentsSummaryQueryKey(repo),
    queryFn: () => fetchCommentsSummary(repo as string),
    enabled: repo !== undefined && repo !== "",
  });
}

/// `GET /api/comments/keywords` — daemon-wide, no `repo` param; a long
/// `staleTime` since `[comments] keywords` never changes without a daemon
/// restart, same posture other daemon-config reads in this crate take.
export function useCommentKeywords() {
  return useQuery({
    queryKey: ["comments-keywords"] as const,
    queryFn: () => fetchCommentKeywords(),
    staleTime: 5 * 60_000,
  });
}
