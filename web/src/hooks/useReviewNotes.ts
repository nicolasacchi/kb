import { useQuery } from "@tanstack/react-query";
import {
  fetchReviewNotes,
  type ReviewNoteRow,
  type ReviewNotesQuery,
  type ReviewNoteStatus,
  type TagSummary,
} from "../api/reviewNotes";

// v0.40 TN — the private-note index, invariant #23 shaped: ONE query per
// filter combination under the `["review-notes", …]` prefix, refetched by
// the EXISTING `comments.updated` bridge in api/queryClient.ts (the one
// invalidate line added there). No subscription of its own, no `sse.on`,
// no new event kind — invariants #23/#24 stay intact.
//
// The key is a normalised TUPLE, not the raw filter object: TanStack hashes
// object keys by insertion order, and two `?tag=` orderings that mean the
// same filter must land on the same entry. `invalidate(["review-notes"])`
// prefix-matches every variant, so one line refreshes all of them.

const EMPTY_NOTES: ReviewNoteRow[] = [];
const EMPTY_TAGS: TagSummary[] = [];

export type UseReviewNotesResult = {
  notes: ReviewNoteRow[];
  /// Facet counts over the PRE-tag-filter, PRE-`q` set (so clicking a
  /// second tag narrows instead of emptying) — exactly what the server
  /// returns, never recomputed here.
  tags: TagSummary[];
  total: number;
  /// The server capped the note page (it has no paging; a flag instead).
  truncated: boolean;
  /// The server capped the facet list at 200.
  tagsTruncated: boolean;
  loading: boolean;
  error: string | null;
};

export function useReviewNotes(q: ReviewNotesQuery = {}): UseReviewNotesResult {
  // Normalise once, use for BOTH the key and the fetch — the key and the
  // request must never describe different filters.
  const kb = q.kb ?? "";
  const tags = (q.tags ?? []).filter((t) => t !== "");
  const text = (q.q ?? "").trim();
  const status: ReviewNoteStatus = q.status ?? "all";
  const query = useQuery({
    queryKey: ["review-notes", kb, tags.join(" "), text, status],
    queryFn: ({ signal }) =>
      fetchReviewNotes(
        { kb: kb || undefined, tags, q: text || undefined, status },
        signal,
      ),
  });
  return {
    notes: query.data?.notes ?? EMPTY_NOTES,
    tags: query.data?.tags ?? EMPTY_TAGS,
    total: query.data?.total ?? 0,
    truncated: query.data?.truncated ?? false,
    tagsTruncated: query.data?.tags_truncated ?? false,
    loading: query.isPending,
    error: query.error ? String(query.error) : null,
  };
}
