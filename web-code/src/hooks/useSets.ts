import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  appendSetSpan,
  createSet,
  deleteSet,
  fetchSet,
  fetchSets,
  fetchWorkspaceGroups,
  fromDocSet,
  fromSessionSet,
  patchSet,
  type CreateSetInput,
  type FromDocInput,
  type FromSessionInput,
  type PatchSetInput,
  type SetSpanInput,
} from "../api/client";

/// Phase E4 ("kb-code v2 — The Operable Reader") — query-key convention:
/// `["sets", repo]` (the list) and `["sets", repo, id]` (one set's detail)
/// share the `["sets", repo]` PREFIX on purpose — `api/queryClient.ts`'s
/// `set.changed` SSE handler invalidates by that shorter key alone
/// (TanStack Query's default `invalidateQueries` match IS a prefix match),
/// which reaches every open set's detail query for that repo without the
/// bridge needing to know which ids currently exist.
export function setsQueryKey(repo: string | undefined) {
  return ["sets", repo] as const;
}
export function setQueryKey(repo: string | undefined, id: string | undefined) {
  return ["sets", repo, id] as const;
}

/// `GET /api/sets?repo=` — every reading set in `repo`, alphabetical by name.
export function useSets(repo: string | undefined) {
  return useQuery({
    queryKey: setsQueryKey(repo),
    queryFn: () => fetchSets(repo as string),
    enabled: repo !== undefined,
  });
}

/// `GET /api/sets/{id}` — one set's full ordered spans. Keyed on `repo` too
/// even though the route itself takes no `repo` param — purely so this
/// query shares the `["sets", repo]` invalidation prefix above; `repo` here
/// is the CALLER's own route param (`SetDetail`/`Tour`), never sent over the
/// wire (see `fetchSet`).
export function useSet(repo: string | undefined, id: string | undefined) {
  return useQuery({
    queryKey: setQueryKey(repo, id),
    queryFn: () => fetchSet(id as string),
    enabled: repo !== undefined && id !== undefined,
  });
}

/// `POST /api/sets`.
export function useCreateSet(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (input: CreateSetInput) => createSet(input),
    onSuccess: () => qc.invalidateQueries({ queryKey: setsQueryKey(repo) }),
  });
}

/// `PATCH /api/sets/{id}` — the mutate-time `{id, input}` shape mirrors
/// `hooks/useAnnotations.ts`'s `usePatchAnnotation`, since the target id
/// isn't known until the caller acts (a rename, a reorder, a row removal —
/// all full-replacement `spans` PATCHes, see `patchSet`'s own doc).
export function usePatchSet(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: PatchSetInput }) => patchSet(id, input),
    onSuccess: () => qc.invalidateQueries({ queryKey: setsQueryKey(repo) }),
  });
}

/// `POST /api/sets/{id}/spans` — append one span. Same `{id, input}`
/// mutate-time shape as `usePatchSet` above; the reader's "+ Set" menu
/// (`components/sets/AddToSetMenu.tsx`) is the sole caller that picks `id`
/// from a small list rather than a fixed route param.
export function useAppendSetSpan(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: SetSpanInput }) => appendSetSpan(id, input),
    onSuccess: () => qc.invalidateQueries({ queryKey: setsQueryKey(repo) }),
  });
}

/// `DELETE /api/sets/{id}`.
export function useDeleteSet(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => deleteSet(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: setsQueryKey(repo) }),
  });
}

/// `POST /api/sets/from-session` — LOOPBACK-ONLY, see `fromSessionSet`'s own
/// doc. `repo` is fixed at hook-creation (mirrors every other per-repo
/// mutation above); a caller that doesn't yet know which repo to target (an
/// ambiguous multi-repo session — `components/sets/SaveAsSetButton.tsx`)
/// simply never calls `.mutate()`/`.mutateAsync()` until it's resolved.
export function useFromSessionSet(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (input: Omit<FromSessionInput, "repo">) => fromSessionSet({ ...input, repo }),
    onSuccess: () => qc.invalidateQueries({ queryKey: setsQueryKey(repo) }),
  });
}

/// `POST /api/sets/from-doc` — LOOPBACK-ONLY, see `fromDocSet`'s own doc
/// (DCB W3.C/R24). `repo` fixed at hook-creation, same `Omit<…, "repo">`
/// mutate-time shape and `["sets", repo]` invalidation as
/// `useFromSessionSet` above — no naming drift from that sibling hook.
export function useFromDocSet(repo: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (input: Omit<FromDocInput, "repo">) => fromDocSet({ ...input, repo }),
    onSuccess: () => qc.invalidateQueries({ queryKey: setsQueryKey(repo) }),
  });
}

// --- V70-A10 ("Workspaces v0") --------------------------------------------
//
// `kind=workspace` rides the SAME `/api/sets` route as plain reading sets —
// `useSet`/`useCreateSet`/`usePatchSet`/`useDeleteSet` above are reused
// AS-IS for a workspace's detail/create/update/delete (pass `kind:
// "workspace"` in a `CreateSetInput`; `GET /api/sets/{id}` has no `kind`
// param to begin with). Their own `["sets", repo]`-prefix invalidation
// already reaches the two list queries below (`["sets", repo,
// "workspaces"]`/`["sets", repo, "workspaces", "grouped"]` both start with
// `["sets", repo]`) and `set.changed`'s SSE handler needs no changes either
// — only the LIST queries (scoped to `kind=workspace`, unlike the plain
// `useSets` above) are new.

export function workspacesQueryKey(repo: string | undefined) {
  return ["sets", repo, "workspaces"] as const;
}
export function workspaceGroupsQueryKey(repo: string | undefined) {
  return ["sets", repo, "workspaces", "grouped"] as const;
}

/// `GET /api/sets?repo=&kind=workspace` — every workspace in `repo`,
/// alphabetical by name.
export function useWorkspaces(repo: string | undefined) {
  return useQuery({
    queryKey: workspacesQueryKey(repo),
    queryFn: () => fetchSets(repo as string, "workspace"),
    enabled: repo !== undefined,
  });
}

/// `GET /api/sets?repo=&kind=workspace&group=ref` — workspaces grouped by
/// their own `ref` label (a `~workspaces` branch-view landing).
export function useWorkspaceGroups(repo: string | undefined) {
  return useQuery({
    queryKey: workspaceGroupsQueryKey(repo),
    queryFn: () => fetchWorkspaceGroups(repo as string),
    enabled: repo !== undefined,
  });
}
