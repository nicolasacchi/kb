import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  bindAnnotationReview,
  createAnnotation,
  deleteAnnotation,
  fetchAnnotations,
  fetchOpenAnnotations,
  patchAnnotation,
  unbindAnnotationReview,
  type BindAnnotationReviewInput,
  type CreateAnnotationInput,
  type PatchAnnotationInput,
} from "../api/client";
import type { AnnotationIntent } from "../api/types";

export function annotationsQueryKey(repo: string | undefined, path: string | undefined) {
  return ["annotations", repo, path] as const;
}

/// `GET /api/annotations?repo=&path=` — every annotation for one file, each
/// carrying its live-resolved `line`/`stale`. Kept fresh by
/// `api/queryClient.ts`'s `annotation.changed` SSE handler (invalidates
/// this exact key), not by polling.
export function useAnnotations(repo: string | undefined, path: string | undefined) {
  return useQuery({
    queryKey: annotationsQueryKey(repo, path),
    queryFn: () => fetchAnnotations(repo as string, path as string),
    enabled: repo !== undefined && path !== undefined,
  });
}

export function useCreateAnnotation(repo: string, path: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (input: CreateAnnotationInput) => createAnnotation(input),
    onSuccess: () => qc.invalidateQueries({ queryKey: annotationsQueryKey(repo, path) }),
  });
}

export function usePatchAnnotation(repo: string, path: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: PatchAnnotationInput }) => patchAnnotation(id, input),
    onSuccess: () => qc.invalidateQueries({ queryKey: annotationsQueryKey(repo, path) }),
  });
}

export function useDeleteAnnotation(repo: string, path: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => deleteAnnotation(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: annotationsQueryKey(repo, path) }),
  });
}

/// V80-M2 — `PUT /api/annotations/{id}/review`: bind, or rebind onto a
/// different review, an EXISTING annotation. `annotation.changed`
/// (`api/queryClient.ts`) already prefix-invalidates `["reviews", repo,
/// "comments"]` on either event a bind/rebind emits (the NEW review, and —
/// on a rebind — a second one naming the OLD review; the prefix match
/// isn't scoped to one id, so either alone already covers both Rooms) —
/// this mutation ALSO invalidates it directly, belt-and-braces, so the
/// mutating tab itself never waits on that round trip.
export function useBindAnnotationReview(repo: string, path: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: BindAnnotationReviewInput }) =>
      bindAnnotationReview(id, input),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: annotationsQueryKey(repo, path) });
      void qc.invalidateQueries({ queryKey: ["reviews", repo, "comments"] });
    },
  });
}

/// V80-M2 — `DELETE /api/annotations/{id}/review`: unbind. Same
/// belt-and-braces invalidation as [`useBindAnnotationReview`] above.
export function useUnbindAnnotationReview(repo: string, path: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => unbindAnnotationReview(id),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: annotationsQueryKey(repo, path) });
      void qc.invalidateQueries({ queryKey: ["reviews", repo, "comments"] });
    },
  });
}

export function openAnnotationsQueryKey(
  repo: string | undefined,
  opts?: { intent?: AnnotationIntent; path_prefix?: string },
) {
  return ["annotations-open", repo, opts?.intent, opts?.path_prefix] as const;
}

/// `GET /api/annotations/open?repo=[&intent=][&path_prefix=]` (Phase D) —
/// every unresolved, top-level annotation across the WHOLE repo. No SPA
/// screen consumes this yet (see `fetchOpenAnnotations`'s own doc); kept
/// here so the next caller (a future cross-file "open questions" dashboard)
/// doesn't have to invent the query shape.
export function useOpenAnnotations(repo: string | undefined, opts?: { intent?: AnnotationIntent; path_prefix?: string }) {
  return useQuery({
    queryKey: openAnnotationsQueryKey(repo, opts),
    queryFn: () => fetchOpenAnnotations(repo as string, opts),
    enabled: repo !== undefined,
  });
}
