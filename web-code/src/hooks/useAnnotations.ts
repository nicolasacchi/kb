import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  createAnnotation,
  deleteAnnotation,
  fetchAnnotations,
  fetchOpenAnnotations,
  patchAnnotation,
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
