// DCB W3.B — the reverse "cited by" index's data hook
// (`components/lens/CitedBy.tsx`). `GET /api/doc-refs` is a same-origin,
// plain `auth_bearer` read (`api/client.ts`'s own doc on `fetchDocRefs`) —
// kb-code is THIS daemon's own frontend, so the CORS layer never applies
// here, same reasoning `hooks/useDocLens.ts`'s header doc already gives for
// its sibling `/api/doc-lens*` routes.
//
// `staleTime: 30_000` — same finite-staleTime, no-SSE-tie shape
// `useDocLens.ts` uses (invariant #23's fourth-exception category:
// structurally un-bridgeable, since the result depends on kb's OWN content —
// a `doc_refs.synced` event IS emitted per sync pass, `doclens/sync.rs`'s own
// doc, but nothing subscribes to it yet). `retry: false` is a SEPARATE
// convention `useDocLens.ts` does NOT use (it sets only `staleTime`) —
// this hook instead follows the sibling shape `usePrs`/`useReviews`/
// `useSessionDiff`/`useStacks` share: an error here is a caller/repo-config
// fact (no pin, no sync pass yet, …), not a transient failure worth React
// Query's default retry. One coincidence worth naming:
// `docRefsQueryKey`'s position [1] is `repo` (unlike doclens' `kb`), so
// `queryClient.ts`'s existing per-repo `mirror.updated`/`repo.head_moved`
// bridge WILL incidentally invalidate this query too — a bonus for the
// `live` flag specifically (a file's own repo just changed), not a
// substitute for the finite `staleTime` above (which is what actually covers
// the kb-side "a doc's citations changed since the last sync pass" case the
// bridge can't see).
import { useQuery } from "@tanstack/react-query";
import { fetchDocRefs } from "../api/client";

const DOC_REFS_STALE_MS = 30_000;

export function docRefsQueryKey(repo: string | undefined, path: string | undefined) {
  return ["docRefs", repo, path] as const;
}

/// `GET /api/doc-refs?repo=&path=` — every kb doc that cites `path` in
/// `repo`, live-revalidated (`DocRefsOut.live`) against the file's current
/// existence. `enabled` only once both `repo`/`path` are known (mirrors
/// `useDocLens`'s own gate) — there is deliberately no third "is a file
/// open" flag: a caller with `path === undefined` (no file open) already
/// gets `enabled: false` for free.
export function useDocRefs(repo: string | undefined, path: string | undefined) {
  return useQuery({
    queryKey: docRefsQueryKey(repo, path),
    queryFn: () => fetchDocRefs(repo as string, path as string),
    enabled: repo !== undefined && path !== undefined && path !== "",
    staleTime: DOC_REFS_STALE_MS,
    retry: false,
  });
}
