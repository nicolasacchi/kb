import { useEffect, useMemo, useRef } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  applyAnnotationSuggestion,
  deleteFindingDisposition,
  createAnnotation,
  createManualFinding,
  deleteAnnotation,
  deleteAnnotationSuggestion,
  fetchReviewComments,
  patchAnnotation,
  putAnnotationSuggestion,
  putFindingDisposition,
} from "../api/client";
import type { AnnotationIntent, ReviewCommentsOut, ReviewFinding } from "../api/types";
import { annotationsQueryKey } from "./useAnnotations";
import { buildReplyPayload } from "../lib/annotations";
import {
  buildManualFindingPayload,
  findingsByAnnotationId,
  type FindingDisposition,
  type OverlayMode,
} from "../lib/diffFindings";
import { diffAgentReplies } from "../lib/questionState";
import {
  buildAskAgentPayload,
  buildReviewCommentPayload,
  EMPTY_INDEX,
  indexThreads,
  type DiffCommentsApi,
  type DiffSide,
} from "../lib/reviewComments";
import { toast } from "../lib/toast";
import { useReviewFindings } from "./useReviews";

/// Key convention: `repo` stays at index 1 so the `review.changed` prefix
/// `["reviews", repo]` and the `annotation.changed` review_id prefix
/// `["reviews", repo, "comments"]` both reach this query.
export function reviewCommentsKey(
  repo: string | undefined,
  id: number | undefined,
  ps: string | undefined,
  all: boolean,
) {
  return ["reviews", repo, "comments", id, ps ?? "latest", all] as const;
}

export function useReviewComments(
  repo: string | undefined,
  id: number | undefined,
  ps: string | undefined,
  all = true,
) {
  return useQuery({
    queryKey: reviewCommentsKey(repo, id, ps, all),
    queryFn: () => fetchReviewComments(id as number, ps, all),
    enabled: repo !== undefined && id !== undefined,
  });
}

function invalidateComments(
  qc: ReturnType<typeof useQueryClient>,
  repo: string,
  reviewId: number,
  path?: string,
) {
  void qc.invalidateQueries({ queryKey: ["reviews", repo, "comments", reviewId] });
  if (path) void qc.invalidateQueries({ queryKey: annotationsQueryKey(repo, path) });
}

export function useReviewCommentMutations(repo: string, reviewId: number, ps: string) {
  const qc = useQueryClient();

  const create = useMutation({
    mutationFn: (input: {
      path: string;
      side: DiffSide;
      line: number;
      lineEnd?: number;
      body: string;
      intent: string;
    }) => {
      const payload = buildReviewCommentPayload({
        repo,
        path: input.path,
        line: input.line,
        lineEnd: input.lineEnd,
        body: input.body,
        intent: input.intent as AnnotationIntent,
        reviewId,
        ps,
        side: input.side,
      });
      if (!payload) throw new Error("incomplete review comment");
      return createAnnotation(payload);
    },
    onSuccess: (_data, vars) => invalidateComments(qc, repo, reviewId, vars.path),
  });

  const reply = useMutation({
    mutationFn: (input: { path: string; parentId: string; body: string }) => {
      const payload = buildReplyPayload(repo, input.path, input.parentId, input.body);
      if (!payload) throw new Error("empty reply");
      return createAnnotation(payload);
    },
    onSuccess: (_data, vars) => invalidateComments(qc, repo, reviewId, vars.path),
  });

  const resolve = useMutation({
    mutationFn: (input: { id: string; path: string; resolved: boolean }) =>
      patchAnnotation(input.id, { resolved: input.resolved }),
    onSuccess: (_data, vars) => invalidateComments(qc, repo, reviewId, vars.path),
  });

  const remove = useMutation({
    mutationFn: (input: { id: string; path: string }) => deleteAnnotation(input.id),
    onSuccess: (_data, vars) => invalidateComments(qc, repo, reviewId, vars.path),
  });

  const setSuggestion = useMutation({
    mutationFn: (input: { id: string; path: string; replacement: string }) =>
      putAnnotationSuggestion(input.id, input.replacement),
    onSuccess: (_data, vars) => invalidateComments(qc, repo, reviewId, vars.path),
  });

  const clearSuggestion = useMutation({
    mutationFn: (input: { id: string; path: string }) => deleteAnnotationSuggestion(input.id),
    onSuccess: (_data, vars) => invalidateComments(qc, repo, reviewId, vars.path),
  });

  const applySuggestion = useMutation({
    mutationFn: (input: { id: string; path: string; resolve: boolean }) =>
      applyAnnotationSuggestion(input.id, input.resolve),
    onSuccess: (_data, vars) => {
      invalidateComments(qc, repo, reviewId, vars.path);
      void qc.invalidateQueries({ queryKey: ["file", repo, vars.path] });
    },
  });

  return { create, reply, resolve, remove, setSuggestion, clearSuggestion, applySuggestion };
}

/// PRR-U3 — `PUT`/`DELETE /api/reviews/{id}/findings/{slug}/disposition`.
/// Invalidates the `["reviews", repo, "findings", reviewId]` prefix; the
/// `review.changed{reason:"disposition"}` SSE event also prefix-invalidates
/// the whole `["reviews", repo]` surface (`api/queryClient.ts`), so this is
/// belt-and-braces for the mutating tab itself (instant local feedback
/// without waiting on the SSE round-trip).
export function useReviewFindingDispositionMutations(repo: string, reviewId: number) {
  const qc = useQueryClient();
  function invalidate() {
    // Prefix-invalidate ["reviews", repo, "findings", reviewId] — matches
    // `reviewFindingsKey`'s own prefix (see `hooks/useReviews.ts`) without
    // needing a `ps` value here.
    void qc.invalidateQueries({ queryKey: ["reviews", repo, "findings", reviewId] });
  }
  const setDisposition = useMutation({
    mutationFn: ({ slug, disposition }: { slug: string; disposition: FindingDisposition }) =>
      putFindingDisposition(reviewId, slug, { disposition }),
    onSuccess: invalidate,
  });
  const clearDisposition = useMutation({
    mutationFn: (slug: string) => deleteFindingDisposition(reviewId, slug),
    onSuccess: invalidate,
  });
  return { setDisposition, clearDisposition };
}

/// PRR-U3 (addendum §E) — `POST /api/reviews/{id}/findings`, the composer's
/// finding mode. Invalidates BOTH the findings list (the new row) and the
/// comments list (a finding's annotation also renders as an ordinary
/// thread — see `lib/diffFindings.ts`'s module doc) so both queries agree
/// immediately, without waiting on the `review.changed` SSE round-trip.
export function useCreateManualFindingMutation(repo: string, reviewId: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (input: Parameters<typeof createManualFinding>[1]) =>
      createManualFinding(reviewId, input),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["reviews", repo, "findings", reviewId] });
      void qc.invalidateQueries({ queryKey: ["reviews", repo, "comments", reviewId] });
    },
  });
}

/// Per-file `DiffCommentsApi` over the shared review-comments query, PLUS
/// (PRR-U3) the review's findings joined by annotation id and the active
/// overlay filter — the ONE object `DiffFile`/`UnifiedHunks`/`SplitHunks`/
/// `DiffThread` read to render "one thread system, two voices."
export function useReviewDiffComments(
  repo: string | undefined,
  reviewId: number | undefined,
  ps: string | undefined,
  path: string,
  extras?: {
    compose?: { side: DiffSide; line: number; token?: number } | null;
    flashThreadId?: string | null;
    overlay?: OverlayMode;
  },
): DiffCommentsApi | null {
  const q = useReviewComments(repo, reviewId, ps, true);
  const findingsQ = useReviewFindings(repo, reviewId, { ps });
  const mut = useReviewCommentMutations(repo ?? "", reviewId ?? 0, ps ?? "latest");
  const dispositionMut = useReviewFindingDispositionMutations(repo ?? "", reviewId ?? 0);
  const createFindingMut = useCreateManualFindingMutation(repo ?? "", reviewId ?? 0);
  const indexed = useMemo(() => (q.data ? indexThreads(q.data) : EMPTY_INDEX), [q.data]);
  const findingsById = useMemo(
    () => findingsByAnnotationId(findingsQ.data?.findings ?? []),
    [findingsQ.data],
  );
  const compose = extras?.compose ?? null;
  const flashThreadId = extras?.flashThreadId ?? null;
  const overlay = extras?.overlay ?? "all";
  const createAsync = mut.create.mutateAsync;
  const replyAsync = mut.reply.mutateAsync;
  const resolveAsync = mut.resolve.mutateAsync;
  const deleteAsync = mut.remove.mutateAsync;
  const setSuggestionAsync = mut.setSuggestion.mutateAsync;
  const clearSuggestionAsync = mut.clearSuggestion.mutateAsync;
  const applySuggestionAsync = mut.applySuggestion.mutateAsync;
  const setDispositionAsync = dispositionMut.setDisposition.mutateAsync;
  const clearDispositionAsync = dispositionMut.clearDisposition.mutateAsync;
  const createFindingAsync = createFindingMut.mutateAsync;

  return useMemo(() => {
    if (!repo || reviewId == null) return null;
    return {
      reviewId,
      ps: ps ?? "latest",
      byLine: indexed.byLine,
      orphansByPath: indexed.orphansByPath,
      onCreate: (side, line, lineEnd, body, intent) =>
        createAsync({ path, side, line, lineEnd, body, intent }).then(() => undefined),
      onCreateFinding: (side, line, draft) => {
        const payload = buildManualFindingPayload({ path, side, line, ...draft });
        if (!payload) return Promise.reject(new Error("incomplete finding"));
        return createFindingAsync(payload as Parameters<typeof createManualFinding>[1]).then(() => undefined);
      },
      onReply: (id, body) => replyAsync({ path, parentId: id, body }).then(() => undefined),
      onResolve: (id, resolved) => resolveAsync({ id, path, resolved }).then(() => undefined),
      onDelete: (id) => deleteAsync({ id, path }).then(() => undefined),
      onSetSuggestion: (id, replacement) =>
        setSuggestionAsync({ id, path, replacement }).then(() => undefined),
      onClearSuggestion: (id) => clearSuggestionAsync({ id, path }).then(() => undefined),
      onApplySuggestion: (id, resolve) => applySuggestionAsync({ id, path, resolve }),
      compose,
      flashThreadId,
      findingsById,
      overlay,
      onSetDisposition: (slug, disposition) =>
        setDispositionAsync({ slug, disposition }).then(() => undefined),
      onClearDisposition: (slug) => clearDispositionAsync(slug).then(() => undefined),
    };
  }, [
    repo,
    reviewId,
    ps,
    path,
    indexed,
    createAsync,
    replyAsync,
    resolveAsync,
    deleteAsync,
    setSuggestionAsync,
    clearSuggestionAsync,
    applySuggestionAsync,
    compose,
    flashThreadId,
    findingsById,
    overlay,
    setDispositionAsync,
    clearDispositionAsync,
    createFindingAsync,
  ]);
}

// ── PRR-U4 (§4 — "the question/answer loop") ──────────────────────────────

/// `AskAgentCard`'s mutation — `POST /api/annotations` for a review-level
/// question (`lib/reviewComments.ts`'s `buildAskAgentPayload`). Invalidates
/// the comments list the same way `useReviewCommentMutations.create` does;
/// no `path` to also invalidate (`invalidateComments`'s third arg is
/// optional precisely for this caller).
export function useAskAgentMutation(repo: string, reviewId: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: string) => {
      const payload = buildAskAgentPayload({ repo, reviewId, body });
      if (!payload) throw new Error("empty question");
      return createAnnotation(payload);
    },
    onSuccess: () => invalidateComments(qc, repo, reviewId),
  });
}

/// Design doc §4's "✳ claude replied" toast — mounted once per open review
/// room (`ReviewSidePanel`). Reactively diffs each new `comments` snapshot
/// against the previous one it saw (`lib/questionState.ts`'s pure
/// `diffAgentReplies`) and toasts every thread whose latest voice just
/// became agent-authored. The FIRST snapshot a mount sees is seed-only
/// (never toasts) so opening a review that already has agent replies
/// doesn't fire a toast storm; every snapshot after that is a real diff.
/// `findings` only needs to be an array (not the annotation-id map itself)
/// — the map rebuild is cheap and kept local to this hook so callers don't
/// have to.
export function useAgentReplyToast(
  repo: string,
  reviewId: number,
  comments: ReviewCommentsOut | undefined,
  findings: readonly ReviewFinding[],
): void {
  const prevByThreadRef = useRef<Map<string, number>>(new Map());
  const seededRef = useRef(false);
  // Reset the seed/watermark state when switching to a different review —
  // otherwise a stale watermark from review A could suppress (or a wiped
  // one could toast-storm) review B's first snapshot.
  const scopeRef = useRef<string | null>(null);
  const scopeKey = `${repo}:${reviewId}`;
  if (scopeRef.current !== scopeKey) {
    scopeRef.current = scopeKey;
    prevByThreadRef.current = new Map();
    seededRef.current = false;
  }

  useEffect(() => {
    if (!comments) return;
    const findingsById = findingsByAnnotationId(findings);
    const seedOnly = !seededRef.current;
    const { toasts, nextByThread } = diffAgentReplies(
      prevByThreadRef.current,
      comments,
      findingsById,
      seedOnly,
    );
    prevByThreadRef.current = nextByThread;
    seededRef.current = true;
    for (const t of toasts) {
      toast.ok(`✳ claude replied on ${t.label}`);
    }
  }, [comments, findings]);
}
