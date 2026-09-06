// DCB W2.B — the doc↔code lens's data hooks. `GET /api/doc-lens`/`GET
// /api/doc-lens/repos` are same-origin, plain `auth_bearer` reads — kb-code
// is THIS daemon's own frontend, so the CORS layer `router.rs`'s
// `doclens_read` sub-router adds (for kb's own, cross-origin reader) never
// applies here; `PUT /api/doc-lens/pin` is likewise a plain same-origin
// mutation for the same reason (12-w1c's D-A CORS design is entirely about
// the REVERSE direction, kb.example.com calling kb-code cross-origin).
//
// `staleTime: 30_000` on both read queries deliberately overrides
// `api/queryClient.ts`'s blanket `staleTime: Infinity` default: the result
// depends on kb's OWN content (a reindex) and the selected repo's live
// working tree, neither of which fires a kb-code SSE event this app
// subscribes to (kb-code's Wave-1 event vocabulary is `mirror.updated`/
// `repo.head_moved` only — see `api/queryClient.ts`'s header doc). No
// `refetchInterval` either — `Scorecard.tsx` renders a manual "Refresh"
// affordance next to `resolved_unix` instead, the same "structurally
// un-bridgeable, so a finite staleTime + a manual refresh" shape kb's own
// #23 fourth exception documents for its half of this same feature.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { fetchDocLens, fetchDocLensRepos, putDocLensPin } from "../api/client";
import { toast } from "../lib/toast";

const DOC_LENS_STALE_MS = 30_000;

export function docLensQueryKey(kb: string | undefined, doc: string | undefined, repo: string | undefined) {
  return ["doclens", kb, doc, repo] as const;
}

export function docLensRepoQueryKey(kb: string | undefined, doc: string | undefined) {
  return ["doclens-repos", kb, doc] as const;
}

/// `GET /api/doc-lens/repos?kb=&doc=` — the scorecard. Cheap-ish (per
/// amendment 8's cost contract: present/ambiguous/absent/external +
/// head_sha/dirty only, NOT confirmed counts) but still a live git+symbol
/// scan per configured repo.
export function useDocLensRepos(kb: string | undefined, doc: string | undefined) {
  return useQuery({
    queryKey: docLensRepoQueryKey(kb, doc),
    queryFn: () => fetchDocLensRepos(kb as string, doc as string),
    enabled: kb !== undefined && doc !== undefined,
    staleTime: DOC_LENS_STALE_MS,
  });
}

/// `GET /api/doc-lens?kb=&doc=&repo=` — full per-ref resolution against ONE
/// repo.
export function useDocLens(kb: string | undefined, doc: string | undefined, repo: string | undefined) {
  return useQuery({
    queryKey: docLensQueryKey(kb, doc, repo),
    queryFn: () => fetchDocLens(kb as string, doc as string, repo as string),
    enabled: kb !== undefined && doc !== undefined && repo !== undefined,
    staleTime: DOC_LENS_STALE_MS,
  });
}

/// `PUT /api/doc-lens/pin` — remember a checkout for this document
/// (Decision 1). Both this hook AND kb's own reader (13-w1d's
/// `useSetDocLensPin`) write through the SAME route independently — see the
/// W2.B spec §1/§3's ownership correction (R5): W2.A owns pin LIFECYCLE
/// only (boot prune, list, unpin), never the write itself.
export function useSetDocLensPin(kb: string | undefined, doc: string | undefined) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (args: { repo: string; docHash: string | null }) =>
      putDocLensPin(kb as string, doc as string, args.repo, args.docHash),
    onSuccess: () => qc.invalidateQueries({ queryKey: docLensRepoQueryKey(kb, doc) }),
    // W2.B.R fix 5 — was fire-and-forget: `Lens.tsx`'s `pickRepo` calls
    // `.mutate(...)` without awaiting or handling rejection, so a failed
    // pin write (network blip, kb-code down mid-session) previously left no
    // trace at all — the repo switch itself still works (a plain client
    // nav), but the "remembered for next time" write silently didn't
    // happen. `toast.err` mirrors `Reader.tsx`'s own convention for a
    // background action whose failure has no other visible surface (e.g.
    // its `gd`/bookmark handlers).
    onError: (err) => {
      const message = err instanceof Error ? err.message : String(err);
      toast.err(`pin failed: ${message}`);
    },
  });
}
