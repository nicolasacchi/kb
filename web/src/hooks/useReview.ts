import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  addComment as apiAddComment,
  addReply as apiAddReply,
  deleteComment as apiDeleteComment,
  deleteReply as apiDeleteReply,
  detachAttachment as apiDetachAttachment,
  detachReplyAttachment as apiDetachReplyAttachment,
  editComment as apiEditComment,
  editReply as apiEditReply,
  emptyReview,
  fetchReview,
  resolveAll as apiResolveAll,
  resolveComment as apiResolveComment,
  setVerdict as apiSetVerdict,
  unresolveAll as apiUnresolveAll,
  unresolveComment as apiUnresolveComment,
  type Anchor,
  type Choice,
  type Comment,
  type Reply,
  type ReviewFile,
  type VerdictState,
} from "../api/client";
import { sse } from "../api/sse";

// Shared empty stale-id set used when resetting on an artifact change. It's
// never mutated in place (the SSE handlers always derive a fresh `new Set`),
// so a single module-level instance is safe and avoids a per-reset alloc.
const EMPTY_STALE = new Set<string>();

export type UseReview = {
  file: ReviewFile | null;
  loading: boolean;
  error: string | null;
  /// v0.5 P4 — Set of comment ids whose anchors are currently flagged
  /// "stale" (the indexer's fuzzy_resolve_anchor returned Stale on the
  /// last reindex). Cleared on a comment.anchor_resolved SSE for the
  /// same comment id. CommentsPanel renders a small badge per stale id.
  staleCommentIds: Set<string>;
  /// R7 — fine-grained mutations. Each posts a small delta to its
  /// dedicated endpoint; the daemon owns the load → mutate → save under
  /// its review_lock (no client ETag, no minted ids). `addComment` /
  /// `addReply` splice the server-returned entity into local state
  /// immediately (optimistic insert) so the panel feels instant; the
  /// `comments.updated` SSE refetch then reconciles. The status/edit/
  /// delete actions update local state optimistically too. All resolve
  /// with the API result (e.g. the created entity) or void; failures
  /// reject so the caller can surface them.
  addComment: (
    anchor: Anchor,
    body: string,
    opts?: {
      author?: "you" | "claude";
      choices?: Choice[];
      attachmentIds?: string[];
    },
  ) => Promise<Comment>;
  addReply: (
    commentId: string,
    author: "you" | "claude",
    body: string,
    opts?: { choices?: Choice[]; attachmentIds?: string[] },
  ) => Promise<Reply>;
  resolveComment: (commentId: string) => Promise<void>;
  unresolveComment: (commentId: string) => Promise<void>;
  resolveAll: () => Promise<void>;
  unresolveAll: () => Promise<void>;
  /// W2.15a — set (or, passing `null`, clear) the review-pass verdict.
  /// Distinct from any individual comment's status. Optimistic: patches
  /// `file.verdict` immediately, same slim pattern as the mutations above
  /// (commit + resync-on-failure). The server also mirrors the state onto
  /// the artifact's own kb-tags as a `status-*` display shortcut
  /// (`routes/comments.rs`) — that lands via the next `comments.updated`/
  /// reindex, not duplicated here.
  setVerdict: (
    verdict: { state: VerdictState; note?: string } | null,
  ) => Promise<void>;
  editComment: (commentId: string, body: string) => Promise<void>;
  editReply: (commentId: string, replyId: string, body: string) => Promise<void>;
  deleteComment: (commentId: string) => Promise<void>;
  deleteReply: (commentId: string, replyId: string) => Promise<void>;
  /// Y-track — detach an attachment from a posted comment/reply (DELETE +
  /// optimistic local removal; the server GC-reaps the orphaned blob).
  detachAttachment: (commentId: string, aid: string) => Promise<void>;
  detachReplyAttachment: (
    commentId: string,
    replyId: string,
    aid: string,
  ) => Promise<void>;
  refetch: () => void;
};

/// Reads /api/kb/{kb}/review/{id} through the query cache under
/// ["review", kb, artifactId]; the SSE bridge (api/queryClient.ts)
/// invalidates that key on comments.updated and on gap resync, so this
/// hook carries no subscription of its own. Returns an empty skeleton
/// when the file doesn't exist yet (so callers can render against
/// `file` unconditionally).
///
/// Mutations stay the slim optimistic pattern (patch the cached file,
/// fire the call, invalidate-to-resync + rethrow on failure) — the
/// cache entry is the single source of truth, which is the structural
/// safety useMutation would buy; its onMutate/onError ceremony would
/// just re-spell `commit`.
export function useReview(kb: string, artifactId: string): UseReview {
  const queryClient = useQueryClient();
  const [staleCommentIds, setStaleCommentIds] = useState<Set<string>>(
    () => new Set(),
  );

  // Reset the stale-anchor badge set the instant the target changes
  // (render-time, same one-frame argument as before). The FILE needs no
  // manual reset anymore: it lives under the (kb, artifactId) query key,
  // so a target change swaps to the new key's data — undefined while
  // loading — within the same render.
  const target = `${kb} ${artifactId}`;
  const lastTarget = useRef(target);
  if (lastTarget.current !== target) {
    lastTarget.current = target;
    setStaleCommentIds(EMPTY_STALE);
  }

  const reviewQuery = useQuery({
    queryKey: ["review", kb, artifactId] as const,
    enabled: !!kb && !!artifactId,
    queryFn: async ({ signal }) => {
      const got = await fetchReview(kb, artifactId, signal);
      // 404 → no comments yet: cache the empty skeleton so mutations
      // (which patch the cached file) have a base to splice into.
      return got ? got.file : emptyReview(kb, artifactId);
    },
  });
  const file = reviewQuery.data ?? null;
  const loading = reviewQuery.isPending;
  const error = reviewQuery.error ? String(reviewQuery.error) : null;

  const load = useCallback(() => {
    void queryClient.invalidateQueries({
      queryKey: ["review", kb, artifactId],
    });
  }, [queryClient, kb, artifactId]);

  // v0.5 P4 — anchor lifecycle. comment.anchor_stale flags a comment
  // as stale (indexer's fuzzy_resolve_anchor returned Stale); the
  // counterpart comment.anchor_resolved clears it. The map is keyed
  // on comment_id only (matching the indexer's tracker shape).
  useEffect(() => {
    const offStale = sse.subscribeEvent("comment.anchor_stale", (payload) => {
      if (payload.kb !== kb || payload.artifact_id !== artifactId) return;
      const id = payload.comment_id as string | undefined;
      if (!id) return;
      setStaleCommentIds((prev) => {
        if (prev.has(id)) return prev;
        const next = new Set(prev);
        next.add(id);
        return next;
      });
    });
    const offResolved = sse.subscribeEvent("comment.anchor_resolved", (payload) => {
      if (payload.kb !== kb || payload.artifact_id !== artifactId) return;
      const id = payload.comment_id as string | undefined;
      if (!id) return;
      setStaleCommentIds((prev) => {
        if (!prev.has(id)) return prev;
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    });
    return () => {
      offStale();
      offResolved();
    };
  }, [kb, artifactId]);

  // Cache patch helper — maps over the cached file's comments under
  // this hook's query key. No-op when there's no cached file yet.
  const patchComments = useCallback(
    (fn: (cs: Comment[]) => Comment[]) => {
      queryClient.setQueryData<ReviewFile>(
        ["review", kb, artifactId],
        (prev) => (prev ? { ...prev, comments: fn(prev.comments) } : prev),
      );
    },
    [queryClient, kb, artifactId],
  );

  // Patch ONE comment by id, leaving the rest untouched — the optimistic
  // bodies below reduce to "how does this comment change".
  const patchOne = useCallback(
    (commentId: string, fn: (c: Comment) => Comment) => {
      patchComments((cs) => cs.map((c) => (c.id === commentId ? fn(c) : c)));
    },
    [patchComments],
  );

  // Same, for one reply within one comment.
  const patchReply = useCallback(
    (commentId: string, replyId: string, fn: (r: Reply) => Reply) => {
      patchOne(commentId, (c) => ({
        ...c,
        replies: c.replies.map((r) => (r.id === replyId ? fn(r) : r)),
      }));
    },
    [patchOne],
  );

  const addComment = useCallback<UseReview["addComment"]>(
    async (anchor, body, opts) => {
      const text = body.trim();
      const author = opts?.author ?? "you";
      const created = await apiAddComment(kb, artifactId, {
        body: text,
        anchor,
        author,
        choices: opts?.choices,
        attachment_ids: opts?.attachmentIds,
      });
      // Optimistic splice — show the server-assigned comment immediately
      // (dedup-guarded so the SSE refetch can't double it).
      patchComments((cs) =>
        cs.some((c) => c.id === created.id) ? cs : [...cs, created],
      );
      return created;
    },
    [kb, artifactId, patchComments],
  );

  const addReply = useCallback<UseReview["addReply"]>(
    async (commentId, author, body, opts) => {
      const text = body.trim();
      const created = await apiAddReply(kb, artifactId, commentId, {
        author,
        body: text,
        choices: opts?.choices,
        attachment_ids: opts?.attachmentIds,
      });
      patchOne(commentId, (c) =>
        c.replies.some((r) => r.id === created.id)
          ? c
          : { ...c, replies: [...c.replies, created] },
      );
      return created;
    },
    [kb, artifactId, patchOne],
  );

  // Run an optimistic mutation: the caller has already patched local
  // state; await the API call and, on failure, refetch to resync. A
  // rejected mutation fires NO `comments.updated` SSE (the server didn't
  // change anything), so without this the optimistic change would stick.
  // Rethrows so the caller still sees the error.
  const commit = useCallback(
    async (p: Promise<unknown>) => {
      try {
        await p;
      } catch (e) {
        void load();
        throw e;
      }
    },
    [load],
  );

  const resolveComment = useCallback<UseReview["resolveComment"]>(
    async (commentId) => {
      patchOne(commentId, (c) => ({ ...c, status: "resolved" }));
      await commit(apiResolveComment(kb, artifactId, commentId));
    },
    [kb, artifactId, patchOne, commit],
  );

  const unresolveComment = useCallback<UseReview["unresolveComment"]>(
    async (commentId) => {
      patchOne(commentId, (c) => ({ ...c, status: "open" }));
      await commit(apiUnresolveComment(kb, artifactId, commentId));
    },
    [kb, artifactId, patchOne, commit],
  );

  const resolveAll = useCallback<UseReview["resolveAll"]>(async () => {
    patchComments((cs) => cs.map((c) => ({ ...c, status: "resolved" as const })));
    await commit(apiResolveAll(kb, artifactId));
  }, [kb, artifactId, patchComments, commit]);

  const unresolveAll = useCallback<UseReview["unresolveAll"]>(async () => {
    patchComments((cs) => cs.map((c) => ({ ...c, status: "open" as const })));
    await commit(apiUnresolveAll(kb, artifactId));
  }, [kb, artifactId, patchComments, commit]);

  const setVerdict = useCallback<UseReview["setVerdict"]>(
    async (verdict) => {
      queryClient.setQueryData<ReviewFile>(["review", kb, artifactId], (f) =>
        f
          ? {
              ...f,
              verdict: verdict
                ? {
                    state: verdict.state,
                    at: new Date().toISOString(),
                    by: "you" as const,
                    note: verdict.note,
                  }
                : undefined,
            }
          : f,
      );
      await commit(apiSetVerdict(kb, artifactId, verdict));
    },
    [kb, artifactId, commit, queryClient],
  );

  const editComment = useCallback<UseReview["editComment"]>(
    async (commentId, body) => {
      const text = body.trim();
      const now = new Date().toISOString();
      patchOne(commentId, (c) => ({ ...c, body: text, editedAt: now }));
      await commit(apiEditComment(kb, artifactId, commentId, text));
    },
    [kb, artifactId, patchOne, commit],
  );

  const editReply = useCallback<UseReview["editReply"]>(
    async (commentId, replyId, body) => {
      const text = body.trim();
      const now = new Date().toISOString();
      patchReply(commentId, replyId, (r) => ({
        ...r,
        body: text,
        editedAt: now,
      }));
      await commit(apiEditReply(kb, artifactId, commentId, replyId, text));
    },
    [kb, artifactId, patchReply, commit],
  );

  const deleteComment = useCallback<UseReview["deleteComment"]>(
    async (commentId) => {
      patchComments((cs) => cs.filter((c) => c.id !== commentId));
      await commit(apiDeleteComment(kb, artifactId, commentId));
    },
    [kb, artifactId, patchComments, commit],
  );

  const deleteReply = useCallback<UseReview["deleteReply"]>(
    async (commentId, replyId) => {
      patchOne(commentId, (c) => ({
        ...c,
        replies: c.replies.filter((r) => r.id !== replyId),
      }));
      await commit(apiDeleteReply(kb, artifactId, commentId, replyId));
    },
    [kb, artifactId, patchOne, commit],
  );

  const detachAttachment = useCallback<UseReview["detachAttachment"]>(
    async (commentId, aid) => {
      patchOne(commentId, (c) => ({
        ...c,
        attachments: (c.attachments ?? []).filter((a) => a.id !== aid),
      }));
      await commit(apiDetachAttachment(kb, artifactId, commentId, aid));
    },
    [kb, artifactId, patchOne, commit],
  );

  const detachReplyAttachment = useCallback<UseReview["detachReplyAttachment"]>(
    async (commentId, replyId, aid) => {
      patchReply(commentId, replyId, (r) => ({
        ...r,
        attachments: (r.attachments ?? []).filter((a) => a.id !== aid),
      }));
      await commit(
        apiDetachReplyAttachment(kb, artifactId, commentId, replyId, aid),
      );
    },
    [kb, artifactId, patchReply, commit],
  );

  return {
    file,
    loading,
    error,
    staleCommentIds,
    addComment,
    addReply,
    resolveComment,
    unresolveComment,
    resolveAll,
    unresolveAll,
    setVerdict,
    editComment,
    editReply,
    deleteComment,
    deleteReply,
    detachAttachment,
    detachReplyAttachment,
    refetch: load,
  };
}
