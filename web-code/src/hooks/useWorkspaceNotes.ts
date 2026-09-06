import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  createAnnotation,
  deleteAnnotation,
  fetchWorkspaceNotes,
  patchAnnotation,
  type CreateAnnotationInput,
  type PatchAnnotationInput,
} from "../api/client";

/// V70-A10 ("Workspaces v0") — `GET /api/annotations?set_id=`'s query key.
/// The `["workspace-notes", …]` PREFIX is what `api/queryClient.ts`'s
/// `annotation.changed` SSE handler invalidates wholesale (a coarse,
/// unscoped refresh — see that handler's own doc for why it doesn't narrow
/// to one `setId`).
export function workspaceNotesQueryKey(setId: string | undefined) {
  return ["workspace-notes", setId] as const;
}

/// `GET /api/annotations?set_id=` — a workspace's notes, general path-less
/// notes AND code-anchored comments alike (parents and their replies).
export function useWorkspaceNotes(setId: string | undefined) {
  return useQuery({
    queryKey: workspaceNotesQueryKey(setId),
    queryFn: () => fetchWorkspaceNotes(setId as string),
    enabled: setId !== undefined,
  });
}

/// `POST /api/annotations` with `set_id` set — the composer's create path
/// (both the general path-less note and a code-anchored one route through
/// this SAME mutation; `lib/workspaceNotes.ts`'s `buildWorkspaceNotePayload`
/// picks the shape). Also invalidates `annotations-open`-style repo-wide
/// listings is NOT done here (a workspace note is not itself surfaced
/// there — `list_open_annotations`'s own doc scopes to top-level PATH
/// annotations, and a general note has no path); only this workspace's own
/// notes query is invalidated.
export function useCreateWorkspaceNote(setId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (input: CreateAnnotationInput) => createAnnotation(input),
    onSuccess: () => qc.invalidateQueries({ queryKey: workspaceNotesQueryKey(setId) }),
  });
}

/// `PATCH /api/annotations/{id}` — resolve/reopen/edit a workspace note.
/// Same `{id, input}` mutate-time shape as `hooks/useAnnotations.ts`'s
/// `usePatchAnnotation`.
export function usePatchWorkspaceNote(setId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: PatchAnnotationInput }) =>
      patchAnnotation(id, input),
    onSuccess: () => qc.invalidateQueries({ queryKey: workspaceNotesQueryKey(setId) }),
  });
}

/// `DELETE /api/annotations/{id}`.
export function useDeleteWorkspaceNote(setId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => deleteAnnotation(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: workspaceNotesQueryKey(setId) }),
  });
}
