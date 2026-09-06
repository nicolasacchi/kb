import { ApiError, fetchDoc, type DocSummary } from "./client";

// Link flow — resolving an artifact addressed by ID.
//
// An artifact-subdomain URL (`<id>.artifacts.<root>/`) names an artifact but
// NOT the corpus it lives in, and `GET /api/kb/{kb}/docs/{id}` is per-kb. So
// an id-shaped link is resolved by a short ladder: the observing pane's own
// kb first (overwhelmingly the answer — cross-kb links are the exception),
// then the daemon's other corpora in the `["kbs"]` query's stable order.
//
// ONE home for the ladder + its cache key, shared by the hover path
// (`PeekCard`'s `useQuery`) and the click path (`ArtifactPane`'s
// `queryClient.fetchQuery` on `kb:link-open`), so both populate/read the
// SAME cache entry and a click straight after a hover is free.
//
// The key stays under the `["doc", kb]` prefix the SSE bridge already
// invalidates (invariant #23) — no new invalidation plumbing.

export const docIdQueryKey = (kb: string | null, id: string) =>
  ["doc", kb, "by-id", id] as const;

/// The corpora to try, in order, for an id-shaped link: the kb the link
/// itself named (a `/a/<kb>/<12hex>` permalink) if any — otherwise the
/// observing pane's own kb first, then every other kb the daemon serves.
/// Pure; deduped; skips empties.
export function artifactKbLadder(
  linkKb: string | null,
  paneKb: string | null,
  kbIds: readonly string[],
): string[] {
  if (linkKb) return [linkKb];
  const out: string[] = [];
  for (const k of [paneKb, ...kbIds]) {
    if (k && !out.includes(k)) out.push(k);
  }
  return out;
}

/// Walk `kbs` in order looking for artifact `id`. A 404 means "not that
/// corpus" and moves on; any OTHER error aborts immediately (a 500 or a
/// dead daemon must not be retried against every kb). `fetchDoc` follows the
/// moves 301 chain (#27), so a relocated artifact still resolves.
export async function fetchDocByIdLadder(
  kbs: readonly string[],
  id: string,
  signal?: AbortSignal,
): Promise<{ kb: string; doc: DocSummary }> {
  for (const kb of kbs) {
    try {
      return { kb, doc: await fetchDoc(kb, id, signal) };
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) continue;
      throw err;
    }
  }
  throw new ApiError(`artifact ${id} is not indexed in any kb`, 404);
}
