import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  createCanvas,
  deleteCanvas,
  fetchCanvas,
  fetchCanvasList,
  updateCanvas,
  type CreateCanvasInput,
  type UpdateCanvasInput,
} from "../api/client";

/// V3.4-C2 — canvas-set query keys. List is `["canvas", repo]`; detail is
/// `["canvas", repo, id]` so a prefix invalidate reaches both (mirrors
/// `useSets`'s `["sets", repo]` / `["sets", repo, id]` convention).
export function canvasListQueryKey(repo: string | undefined) {
  return ["canvas", repo] as const;
}

export function canvasQueryKey(repo: string | undefined, id: number | undefined) {
  return ["canvas", repo, id] as const;
}

/// `GET /api/canvas?repo=` — every canvas set in `repo` (summary, no body).
export function useCanvasList(repo: string | undefined) {
  return useQuery({
    queryKey: canvasListQueryKey(repo),
    queryFn: () => fetchCanvasList(repo as string),
    enabled: repo !== undefined && repo !== "",
  });
}

/// `GET /api/canvas/{id}` — one canvas's full opaque payload.
export function useCanvas(repo: string | undefined, id: number | undefined) {
  return useQuery({
    queryKey: canvasQueryKey(repo, id),
    queryFn: () => fetchCanvas(id as number),
    enabled: repo !== undefined && id !== undefined && Number.isFinite(id),
  });
}

/// `POST /api/canvas` — LOOPBACK-ONLY on the server.
export function useCreateCanvas(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (input: Omit<CreateCanvasInput, "repo"> & { repo?: string }) =>
      createCanvas({ ...input, repo: input.repo ?? repo }),
    onSuccess: () => qc.invalidateQueries({ queryKey: canvasListQueryKey(repo) }),
  });
}

/// `PUT /api/canvas/{id}` — LOOPBACK-ONLY.
export function useUpdateCanvas(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, input }: { id: number; input: UpdateCanvasInput }) =>
      updateCanvas(id, input),
    onSuccess: (_data, vars) => {
      qc.invalidateQueries({ queryKey: canvasListQueryKey(repo) });
      qc.invalidateQueries({ queryKey: canvasQueryKey(repo, vars.id) });
    },
  });
}

/// `DELETE /api/canvas/{id}` — LOOPBACK-ONLY.
export function useDeleteCanvas(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: number) => deleteCanvas(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: canvasListQueryKey(repo) }),
  });
}
