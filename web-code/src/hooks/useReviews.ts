import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ApiError,
  createReview,
  deleteReview,
  deleteReviewViewed,
  deleteReviewHunkViewed,
  // ── PRR-U2 ──
  deleteFindingDisposition,
  fetchPrChecks,
  fetchPrReviews,
  fetchReview,
  fetchReviewAnnotations,
  fetchReviewFiles,
  fetchReviewFindings,
  fetchReviewInterdiff,
  fetchReviewDoc,
  fetchReviewDocLint,
  fetchReviewMap,
  fetchReviewReadingOrder,
  fetchReviewReport,
  fetchReviews,
  patchReview,
  putFindingDisposition,
  putReviewVerdict,
  putReviewViewed,
  putReviewHunkViewed,
  snapshotReview,
  type CreateReviewInput,
  type FetchReviewFindingsParams,
  type PatchReviewInput,
} from "../api/client";
import type {
  ReviewTimelineParams,
  ReviewVerdictState,
  SetFindingDispositionInput,
} from "../api/types";
import { BEHAVIORAL_STALE_MS } from "./useBehavioral";
// ── PRR-U1 ── kept as its OWN import statement (same PRR-U3 precedent
// `api/client.ts` documents) so this unit's diff never touches a line a
// sibling builder might also be editing.
import { createReviewPr, fetchReviewInbox } from "../api/client";
import type { CreateReviewPrInput } from "../api/types";

/// V3.R2 — query-key convention. List keys carry `state` so the open/closed
/// filter toggle doesn't thrash one shared cache; detail/files/annotations
/// share the `["reviews", repo]` PREFIX so the `review.changed` SSE handler
/// can invalidate the whole surface with one call (TanStack Query prefix
/// match).
export function reviewsListKey(repo: string | undefined, state?: string | null) {
  return ["reviews", repo, "list", state ?? "all"] as const;
}
export function reviewDetailKey(repo: string | undefined, id: number | undefined) {
  return ["reviews", repo, "detail", id] as const;
}
export function reviewFilesKey(
  repo: string | undefined,
  id: number | undefined,
  ps: string | undefined,
) {
  return ["reviews", repo, "files", id, ps ?? "latest"] as const;
}
export function reviewAnnotationsKey(repo: string | undefined, id: number | undefined) {
  return ["reviews", repo, "annotations", id] as const;
}
export function reviewInterdiffKey(
  repo: string | undefined,
  id: number | undefined,
  from: number | undefined,
  to: number | undefined,
) {
  return ["reviews", repo, "interdiff", id, from, to] as const;
}

function invalidateReviewSurface(qc: ReturnType<typeof useQueryClient>, repo: string) {
  return qc.invalidateQueries({ queryKey: ["reviews", repo] });
}

export function useReviews(repo: string | undefined, state?: "open" | "closed" | null) {
  return useQuery({
    queryKey: reviewsListKey(repo, state),
    queryFn: () => fetchReviews(repo as string, state ?? undefined),
    enabled: repo !== undefined,
  });
}

export function useReview(repo: string | undefined, id: number | undefined) {
  return useQuery({
    queryKey: reviewDetailKey(repo, id),
    queryFn: () => fetchReview(id as number),
    enabled: repo !== undefined && id !== undefined,
  });
}

export function useReviewFiles(
  repo: string | undefined,
  id: number | undefined,
  ps: string | undefined,
  enabled = true,
) {
  return useQuery({
    queryKey: reviewFilesKey(repo, id, ps),
    queryFn: () => fetchReviewFiles(id as number, ps),
    enabled: enabled && repo !== undefined && id !== undefined,
  });
}

export function useReviewAnnotations(repo: string | undefined, id: number | undefined) {
  return useQuery({
    queryKey: reviewAnnotationsKey(repo, id),
    queryFn: () => fetchReviewAnnotations(id as number),
    enabled: repo !== undefined && id !== undefined,
  });
}

export function useReviewInterdiff(
  repo: string | undefined,
  id: number | undefined,
  from: number | undefined,
  to: number | undefined,
  enabled = true,
) {
  return useQuery({
    queryKey: reviewInterdiffKey(repo, id, from, to),
    queryFn: () => fetchReviewInterdiff(id as number, from as number, to as number),
    enabled: enabled && repo !== undefined && id !== undefined && from !== undefined && to !== undefined,
  });
}

export function reviewMapKey(repo: string | undefined, id: number | undefined) {
  return ["reviews", repo, "map", id] as const;
}

export function reviewReadingOrderKey(repo: string | undefined, id: number | undefined) {
  return ["reviews", repo, "reading-order", id] as const;
}

export function reviewDocKey(
  repo: string | undefined,
  id: number | undefined,
  ps: string | undefined,
) {
  return ["reviews", repo, "doc", id, ps ?? "latest"] as const;
}

export function reviewDocLintKey(
  repo: string | undefined,
  id: number | undefined,
  ps: string | undefined,
) {
  return ["reviews", repo, "doc-lint", id, ps ?? "latest"] as const;
}

/**
 * `GET /api/reviews/{id}/doc?resolve=true` (V73-K1/K2b, `kbc-review/1`). On
 * 404 — the review has no composed document, or the daemon predates the
 * surface — returns `null` so the cockpit hides the Document tab rather than
 * showing empty chrome, exactly as `useReviewMap` does.
 *
 * Always asks for `resolve=true`: a Document tab without live cards is the
 * prose the CLI already prints, and D9-a's whole claim is that the refs
 * become cards. Fetched only when the tab is opened (`enabled`) so the cost
 * of resolving every ref is paid by the reader who asked for it.
 */
export function useReviewDoc(
  repo: string | undefined,
  id: number | undefined,
  ps?: string,
  enabled = true,
) {
  return useQuery({
    queryKey: reviewDocKey(repo, id, ps),
    queryFn: async () => {
      try {
        return await fetchReviewDoc(id as number, { ps, resolve: true });
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },
    enabled: enabled && repo !== undefined && id !== undefined,
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}

/**
 * `GET /api/reviews/{id}/doc/lint` (V73-K1/K2b). Same 404→`null` degrade:
 * a review with no document has nothing to lint, and the panel simply does
 * not render. A lint FAILURE is not an error state — rows are the point.
 */
export function useReviewDocLint(
  repo: string | undefined,
  id: number | undefined,
  ps?: string,
  enabled = true,
) {
  return useQuery({
    queryKey: reviewDocLintKey(repo, id, ps),
    queryFn: async () => {
      try {
        return await fetchReviewDocLint(id as number, { ps });
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },
    enabled: enabled && repo !== undefined && id !== undefined,
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}

/**
 * `GET /api/reviews/{id}/map` (V3.3-S1). On 404 (older server / absent surface)
 * returns `null` so the cockpit can hide the Map tab with no stub chrome.
 * Fetch only when the sub-view is opened (`enabled`).
 */
export function useReviewMap(
  repo: string | undefined,
  id: number | undefined,
  enabled = true,
) {
  return useQuery({
    queryKey: reviewMapKey(repo, id),
    queryFn: async () => {
      try {
        return await fetchReviewMap(id as number);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },
    enabled: enabled && repo !== undefined && id !== undefined,
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}

/**
 * `GET /api/reviews/{id}/reading-order` (V3.3-S1). Same 404 → null degrade.
 */
export function useReviewReadingOrder(
  repo: string | undefined,
  id: number | undefined,
  enabled = true,
) {
  return useQuery({
    queryKey: reviewReadingOrderKey(repo, id),
    queryFn: async () => {
      try {
        return await fetchReviewReadingOrder(id as number);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },
    enabled: enabled && repo !== undefined && id !== undefined,
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}

export function useCreateReview(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (input: CreateReviewInput) => createReview(input),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

export function useSnapshotReview(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: number) => snapshotReview(id),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

export function usePatchReview(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, input }: { id: number; input: PatchReviewInput }) => patchReview(id, input),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

export function useDeleteReview(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: number) => deleteReview(id),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

export function usePutReviewViewed(repo: string, id: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ path, blob_sha }: { path: string; blob_sha: string }) =>
      putReviewViewed(id, path, blob_sha),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

export function useDeleteReviewViewed(repo: string, id: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (path: string) => deleteReviewViewed(id, path),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

/// V73-K2a — the per-HUNK twins. Same `invalidateReviewSurface` prefix
/// invalidation as the per-file pair above, because `hunks_viewed` rides
/// the very same `GET .../files` response the file rows come from — one
/// refetch, one source of truth, no second cache to drift.
export function usePutReviewHunkViewed(repo: string, id: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ hunkId, path }: { hunkId: string; path: string }) =>
      putReviewHunkViewed(id, hunkId, path),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

export function useDeleteReviewHunkViewed(repo: string, id: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (hunkId: string) => deleteReviewHunkViewed(id, hunkId),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

/// `PUT /api/reviews/{id}/verdict` — LOOPBACK-ONLY. Prefix-invalidates
/// `["reviews", repo]` so list + detail chips refresh together.
export function usePutReviewVerdict(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({
      id,
      input,
    }: {
      id: number;
      input: { state: ReviewVerdictState; note?: string };
    }) => putReviewVerdict(id, input),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

// ── PRR-U2 ── kb v0.39 "The PR Room," unit U2 — Room cockpit + Report tab ──
// Key convention: see `api/queryClient.ts`'s own "── PRR-U2 ──" doc block.

export function reviewReportKey(repo: string | undefined, id: number | undefined) {
  return ["reviews", repo, "report", id] as const;
}

/// `GET /api/reviews/{id}/report` — bearer read. `report: null` (a present
/// `ReviewReportEmpty`) is a normal, successful result, not an error — the
/// caller (`ReportPanel`) branches on `hasReviewReport`, not on
/// `isError`/`isLoading` alone.
export function useReviewReport(repo: string | undefined, id: number | undefined) {
  return useQuery({
    queryKey: reviewReportKey(repo, id),
    queryFn: () => fetchReviewReport(id as number),
    enabled: repo !== undefined && id !== undefined,
  });
}

export function reviewFindingsKey(
  repo: string | undefined,
  id: number | undefined,
  params: FetchReviewFindingsParams = {},
) {
  return [
    "reviews",
    repo,
    "findings",
    id,
    params.ps ?? "latest",
    params.disposition ?? "any",
    params.include_superseded ?? false,
  ] as const;
}

/// `GET /api/reviews/{id}/findings?ps=&disposition=&include_superseded=`.
/// `disposition`/`include_superseded` are SERVER-side filters (part of the
/// query key, like `ps`) — callers that need client-side filtering AS WELL
/// (severity chips, the side panel's combined view) fetch the unfiltered set
/// once and filter locally, the same way `ReviewThreadsCard`'s existing
/// `ThreadFilter` already does over `useReviewComments`.
export function useReviewFindings(
  repo: string | undefined,
  id: number | undefined,
  params: FetchReviewFindingsParams = {},
) {
  return useQuery({
    queryKey: reviewFindingsKey(repo, id, params),
    queryFn: () => fetchReviewFindings(id as number, params),
    enabled: repo !== undefined && id !== undefined,
  });
}

export function reviewChecksKey(repo: string | undefined, id: number | undefined) {
  return ["reviews", repo, "checks", id] as const;
}

/// `GET /api/prs/{number}/checks?repo=` — CI checks for a PR-bound review's
/// current head. `prNumber` is `undefined` for a non-PR-bound (or not-yet-
/// R4-surfaced) review — the query simply stays disabled, and
/// `CiChecksCard` renders its own named-absence state instead of an error.
/// Deliberately keyed under the `["reviews", repo]` prefix (not `["prs",
/// …]`) — see `api/queryClient.ts`'s doc for why.
export function useReviewChecks(
  repo: string | undefined,
  id: number | undefined,
  prNumber: number | undefined,
) {
  return useQuery({
    queryKey: reviewChecksKey(repo, id),
    queryFn: () => fetchPrChecks(repo as string, prNumber as number),
    enabled: repo !== undefined && id !== undefined && prNumber !== undefined,
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}

export function reviewPrReviewsKey(repo: string | undefined, id: number | undefined) {
  return ["reviews", repo, "pr-reviews", id] as const;
}

/// `GET /api/prs/{number}/reviews?repo=` — reviewer states for
/// `GithubConversationCard`. GitHub-origin data: finite `staleTime` (not
/// `Infinity` + SSE) is the real freshness mechanism here, same "documented
/// exception" posture `["prs", repo]` already has — see `api/queryClient.ts`.
export function usePrReviews(
  repo: string | undefined,
  id: number | undefined,
  prNumber: number | undefined,
) {
  return useQuery({
    queryKey: reviewPrReviewsKey(repo, id),
    queryFn: () => fetchPrReviews(repo as string, prNumber as number),
    enabled: repo !== undefined && id !== undefined && prNumber !== undefined,
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}

/// `PUT`/`DELETE /api/reviews/{id}/findings/{slug}/disposition` —
/// LOOPBACK-ONLY. `input: null` clears (DELETE); otherwise sets (PUT).
/// Prefix-invalidates `["reviews", repo]` on success, same as every other
/// review mutation hook above (covers the findings list + any finding-row
/// caches in one call — `review.changed{reason:"disposition"}`'s own
/// prefix-invalidation on the SSE side, plus this tab's own optimistic
/// refresh).
export function useDispositionMutation(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({
      id,
      slug,
      input,
    }: {
      id: number;
      slug: string;
      input: SetFindingDispositionInput | null;
    }) => (input === null ? deleteFindingDisposition(id, slug) : putFindingDisposition(id, slug, input)),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

// ── PRR-U1 ── kb v0.39 "The PR Room," unit U1 (Review Room landing) ────────

export function reviewInboxKey(repo: string | undefined, state?: "open" | "closed" | "all" | null) {
  return ["reviews", repo, "inbox", state ?? "open"] as const;
}

/// `GET /api/reviews/inbox?repo=&state=` — the landing page's attention
/// queue (design doc §2 S1). Keyed under the `["reviews", repo]` PREFIX
/// (see `api/queryClient.ts`'s doc block) so `review.changed` SSE
/// invalidation covers it for free — no new bridge wiring needed.
export function useReviewInbox(
  repo: string | undefined,
  state: "open" | "closed" | "all" | null = "open",
) {
  return useQuery({
    queryKey: reviewInboxKey(repo, state),
    queryFn: () => fetchReviewInbox(repo as string, state ?? undefined),
    enabled: repo !== undefined,
  });
}

/// `POST /api/reviews/pr` — LOOPBACK-ONLY. The "Start review" action on
/// `routes/Prs.tsx` / `components/reviews/UnreviewedPrsStrip.tsx`.
/// Prefix-invalidates `["reviews", repo]` on success — covers both the
/// reviews list AND the inbox query above in one call, same convention as
/// every other review mutation hook in this file.
export function useCreateReviewPr(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (input: CreateReviewPrInput) => createReviewPr(input),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

// ── PRR-U56 ── kb v0.39 "The PR Room," combined unit U5+U6 — publish
// preview, suggestions batch apply, Timeline tab. Own import statement (ESM
// import declarations hoist regardless of position), so this unit's diff
// never touches the shared `import {...} from "../api/client"` block above
// — same precedent this file's own PRR-U2 block already established.
import {
  applySuggestionsBatch,
  fetchReviewExportGithub,
  fetchReviewTimeline,
  publishFinding,
  publishVerdict,
  type FetchReviewExportGithubParams,
} from "../api/client";
import type { PublishFindingInput, PublishVerdictInput } from "../api/types";

export function reviewExportGithubKey(
  repo: string | undefined,
  id: number | undefined,
  params: FetchReviewExportGithubParams = {},
) {
  return [
    "reviews",
    repo,
    "export-github",
    id,
    params.finding_slugs ?? "",
    params.include_waived ?? false,
    params.include_orphaned_as_general ?? false,
  ] as const;
}

/// `GET /api/reviews/{id}/export/github` — the publish-preview payload.
/// `enabled` is the caller's responsibility: an empty/absent `finding_slugs`
/// would return EVERY eligible finding rather than "what's marked," so
/// `PublishPreview.tsx` only enables this once at least one slug is marked.
export function useReviewExportGithub(
  repo: string | undefined,
  id: number | undefined,
  params: FetchReviewExportGithubParams,
  enabled: boolean,
) {
  return useQuery({
    queryKey: reviewExportGithubKey(repo, id, params),
    queryFn: () => fetchReviewExportGithub(id as number, params),
    enabled: enabled && repo !== undefined && id !== undefined,
  });
}

/// `POST /api/reviews/{id}/findings/{slug}/published` — LOOPBACK-ONLY.
export function usePublishFinding(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, slug, input }: { id: number; slug: string; input?: PublishFindingInput }) =>
      publishFinding(id, slug, input),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

/// `POST /api/reviews/{id}/verdict/published` — LOOPBACK-ONLY.
export function usePublishVerdict(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, input }: { id: number; input?: PublishVerdictInput }) => publishVerdict(id, input),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

export function reviewTimelineKey(
  repo: string | undefined,
  id: number | undefined,
  params?: ReviewTimelineParams,
) {
  return [
    "reviews",
    repo,
    "timeline",
    id,
    params?.kind ?? null,
    params?.author ?? null,
    params?.since ?? null,
    params?.until ?? null,
    params?.limit ?? null,
    params?.offset ?? null,
    params?.github ?? null,
    params?.hunk ?? null,
    params?.ps ?? null,
  ] as const;
}

/// `GET /api/reviews/{id}/timeline` (`review-timeline/2`, V73-K2c) — same
/// 404→null degrade convention as `useReviewMap`/`useReviewReadingOrder`
/// above (an older server without this route, or the surface genuinely
/// absent) so `CockpitTabs`' Timeline tab can hide itself with no stub
/// chrome rather than an error state. `params` round-trips to the server
/// (`kind`/`author`/`since`/`until`/`limit`/`offset`/`github`/`hunk`/`ps`);
/// lane VISIBILITY is a client-side filter over the returned `events[]`,
/// not a query param (the wire has no `?lane=`).
export function useReviewTimeline(
  repo: string | undefined,
  id: number | undefined,
  enabled = true,
  params: ReviewTimelineParams = {},
) {
  return useQuery({
    queryKey: reviewTimelineKey(repo, id, params),
    queryFn: async () => {
      try {
        return await fetchReviewTimeline(id as number, params);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },
    enabled: enabled && repo !== undefined && id !== undefined,
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}

/// `POST /api/annotations/apply-batch` — LOOPBACK-ONLY. Not a dedicated
/// query key of its own (no GET pairs with it) — prefix-invalidates
/// `["reviews", repo]` on success, covering both the comments surface
/// (`ReviewThreadsCard`/`SuggestionsBatchCard`'s own `useReviewComments`
/// call) and the findings surface in one call, same as every other review
/// mutation hook in this file.
export function useApplySuggestionsBatch(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ ids, resolveThreads }: { ids: string[]; resolveThreads: boolean }) =>
      applySuggestionsBatch(ids, resolveThreads),
    onSuccess: () => invalidateReviewSurface(qc, repo),
  });
}

// ── PRR-F ── kb v0.39 T2 frontier unit — GitHub threads, recurring-finding
// memory, reviewer X-ray. Own import statement, same precedent every prior
// PRR-* block in this file already established.
import { fetchFindingsRecurrence, fetchGithubThreads, fetchReviewImpact } from "../api/client";

export function reviewGithubThreadsKey(repo: string | undefined, id: number | undefined) {
  return ["reviews", repo, "github-threads", id] as const;
}

/// `GET /api/reviews/{id}/github-threads` (design-addendum-2 §A). Enabled
/// only once `prNumber` is known (mirrors `usePrReviews`/`useReviewChecks`
/// above — a non-PR-bound review 400s the route, so this hook simply never
/// fires for one). Same finite-`staleTime` GitHub-origin posture as
/// `usePrReviews` — `api/queryClient.ts`'s own PRR-F doc block. 404→`null`
/// degrade (older server without this route) so a consumer can hide the
/// GitHub lane entirely rather than show an error.
export function useGithubThreads(
  repo: string | undefined,
  id: number | undefined,
  prNumber: number | undefined,
) {
  return useQuery({
    queryKey: reviewGithubThreadsKey(repo, id),
    queryFn: async () => {
      try {
        return await fetchGithubThreads(id as number);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },
    enabled: repo !== undefined && id !== undefined && prNumber !== undefined,
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}

export function reviewFindingsRecurrenceKey(repo: string | undefined, id: number | undefined) {
  return ["reviews", repo, "findings-recurrence", id] as const;
}

/// `GET /api/reviews/{id}/findings/recurrence` (design-ui.md §12.4). Same
/// 404→`null` degrade convention as `useReviewMap`/`useReviewTimeline`
/// above (an older server, or the surface genuinely absent) — the
/// recurrence chip simply never renders rather than erroring.
export function useFindingsRecurrence(repo: string | undefined, id: number | undefined) {
  return useQuery({
    queryKey: reviewFindingsRecurrenceKey(repo, id),
    queryFn: async () => {
      try {
        return await fetchFindingsRecurrence(id as number);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },
    enabled: repo !== undefined && id !== undefined,
  });
}

export function reviewImpactKey(repo: string | undefined, id: number | undefined, path: string | undefined) {
  return ["reviews", repo, "impact", id, path] as const;
}

/// `GET /api/reviews/{id}/impact?path=` (design-ui.md §12.2, "Reviewer
/// X-ray"). Lazy: `enabled` is the caller's responsibility, same "fetched
/// only once the file section has actually expanded" contract `useDiagnostics`
/// documents for its own per-file fetch (`routes/ReviewDiff.tsx`'s
/// `FileDiffBody` only calls this once `LazyDiffSection`'s `useInViewOnce`
/// gate has fired). 404→`null` degrade (older server without this route).
export function useReviewImpact(
  repo: string | undefined,
  id: number | undefined,
  path: string | undefined,
  enabled: boolean,
) {
  return useQuery({
    queryKey: reviewImpactKey(repo, id, path),
    queryFn: async () => {
      try {
        return await fetchReviewImpact(id as number, path as string);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },
    enabled: enabled && repo !== undefined && id !== undefined && path !== undefined && path !== "",
    retry: false,
  });
}

// ── PRR-U8 (design-addendum-2.md §C) — the Review Room landing's collapsed
// Analytics section. Own import statement, same precedent every prior
// PRR-* block in this file already established.
import { fetchReviewAnalytics } from "../api/client";

export function reviewAnalyticsKey(repo: string | undefined) {
  return ["reviews", repo, "analytics"] as const;
}

/// `GET /api/reviews/analytics?repo=` — repo-scoped (this unit never opens
/// the corpus-wide, no-`repo` view). Kept under the `["reviews", repo]`
/// PREFIX (the SAME convention every other hook in this file documents) so
/// `review.changed`/`annotation.changed` SSE invalidation covers it for
/// free — a fresh disposition/import immediately invalidates the stat
/// tiles, same as every other review-derived query. `enabled` lets the
/// caller gate the fetch on the `<details>` actually being open (this
/// section starts collapsed, per the addendum's own UI spec) rather than
/// eagerly fetching on every landing-page load.
export function useReviewAnalytics(repo: string | undefined, enabled = true) {
  return useQuery({
    queryKey: reviewAnalyticsKey(repo),
    queryFn: () => fetchReviewAnalytics({ repo }),
    enabled: enabled && repo !== undefined,
  });
}

// ── V73-K2c — kbc-claim/1 (the claim register), kbc-pseudo/1 (chapter
// zero + the pseudo-file view) and kbc-hunk-turns/1 (the on-demand
// hunk↔turn chip). Own import statement, the same append-only precedent
// PRR-U8 above establishes.
import {
  fetchClaims,
  fetchHunkTurns,
  fetchReviewPseudoFile,
  fetchReviewPseudoList,
} from "../api/client";
import type { ClaimSubjectKind, FetchClaimsParams } from "../api/types";

export function claimsKey(params: FetchClaimsParams | undefined) {
  return [
    "claims",
    params?.repo,
    params?.subject ?? null,
    params?.subject_kind ?? null,
    params?.path ?? null,
    params?.review ?? null,
    params?.kind ?? null,
    params?.limit ?? null,
    params?.offset ?? null,
  ] as const;
}

/// `GET /api/claims?repo=&…` — surfaced, never scored: the wire order is
/// rendered verbatim, never re-sorted by confidence (`ClaimRegister.tsx`'s
/// own doc). `enabled` defaults to `repo` being known; every narrowing
/// param is optional, same "absent = the daemon's own default" posture
/// every other fetch in this file follows.
export function useClaims(params: FetchClaimsParams | undefined, enabled = true) {
  return useQuery({
    queryKey: claimsKey(params),
    queryFn: () => fetchClaims(params as FetchClaimsParams),
    enabled: enabled && params !== undefined && params.repo !== "",
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}

/// The reader inspector's Claims card — claims about exactly ONE file
/// (`subject_kind: "path"`), keyed on the subject address (root CLAUDE.md
/// invariant #30's "rail row, not a gutter slot" placement).
export function useFileClaims(repo: string | undefined, path: string | undefined) {
  return useClaims(
    repo !== undefined && path !== undefined && path !== ""
      ? { repo, subject_kind: "path" as ClaimSubjectKind, subject: path }
      : undefined,
    repo !== undefined && path !== undefined && path !== "",
  );
}

export function reviewPseudoListKey(repo: string | undefined, id: number | undefined, ps?: string) {
  return ["reviews", repo, "pseudo", id, ps ?? "latest"] as const;
}

/// `GET /api/reviews/{id}/pseudo?ps=` — the map column's chapter zero.
/// 404→null degrade (older server) mirrors every other optional review
/// surface in this file.
export function useReviewPseudoList(
  repo: string | undefined,
  id: number | undefined,
  ps: string | undefined,
  enabled = true,
) {
  return useQuery({
    queryKey: reviewPseudoListKey(repo, id, ps),
    queryFn: async () => {
      try {
        return await fetchReviewPseudoList(id as number, ps);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },
    enabled: enabled && repo !== undefined && id !== undefined,
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}

export function reviewPseudoFileKey(
  repo: string | undefined,
  id: number | undefined,
  name: string | undefined,
  ps?: string,
) {
  return ["reviews", repo, "pseudo-file", id, name, ps ?? "latest"] as const;
}

/// `GET /api/reviews/{id}/pseudo/{name}?ps=` — the single-file read (with
/// `content`), fetched only once a chapter-zero row is actually opened.
export function useReviewPseudoFile(
  repo: string | undefined,
  id: number | undefined,
  name: string | undefined,
  ps: string | undefined,
  enabled = true,
) {
  return useQuery({
    queryKey: reviewPseudoFileKey(repo, id, name, ps),
    queryFn: async () => {
      try {
        return await fetchReviewPseudoFile(id as number, name as string, ps);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },
    enabled: enabled && repo !== undefined && id !== undefined && name !== undefined,
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}

/// A refusal this hook DID reach the server for, vs never having asked
/// (`enabled: false`) — `HunkTurnsPanel.tsx` renders this branch as the
/// honest "loopback only" / "no match: <reason>" text rather than a blank
/// panel. The route has no dedicated bearer-visible refusal shape of its
/// own (LOOPBACK is enforced at the router, ahead of the handler), so any
/// non-2xx here is rendered as its own `ApiError.message` verbatim.
export interface HunkTurnsResult {
  ok: boolean;
  message?: string;
}

export function hunkTurnsKey(
  repo: string | undefined,
  id: number | undefined,
  hunk: string | undefined,
  ps?: string,
) {
  return ["reviews", repo, "hunk-turns", id, hunk, ps ?? "latest"] as const;
}

/// `GET /api/reviews/{id}/hunks/{hunk}/turns?ps=` — LOOPBACK-ONLY, and
/// deliberately ON DEMAND: `enabled` is `false` until the caller has
/// clicked the hunk's "turns" affordance (never auto-fetched per hunk).
export function useHunkTurns(
  repo: string | undefined,
  id: number | undefined,
  hunk: string | undefined,
  ps: string | undefined,
  enabled: boolean,
) {
  return useQuery({
    queryKey: hunkTurnsKey(repo, id, hunk, ps),
    queryFn: () => fetchHunkTurns(id as number, hunk as string, ps),
    enabled: enabled && repo !== undefined && id !== undefined && hunk !== undefined,
    staleTime: BEHAVIORAL_STALE_MS,
    retry: false,
  });
}
