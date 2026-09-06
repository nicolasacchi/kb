// CT-E3 tier (a) — the gallery's per-kb code-ref count join.
//
// The gallery list response (`DocSummary`) carries no `ref_count`, and it
// must not grow one for a badge (#35 — the filter/wire grammar doesn't
// change for display chrome). The cheapest HONEST source is the existing
// coderef/1 corpus feed (`GET /api/kb/{kb}/code-refs?refs=0` — headers
// only: `ref_count`/`extracted_at`/`never_scanned`, no ref bodies),
// fetched once per gallery kb view and joined client-side by doc id.
//
// The feed is cursor-paged (100 docs/page max, server-clamped), so the
// walk here is HARD-CAPPED at `CODEREF_FEED_MAX_PAGES` requests. A corpus
// whose header count exceeds the cap yields `complete: false`, and the
// gallery then renders NO ref chips at all — all-or-nothing, because a
// partial map can't distinguish "this doc cites nothing" from "this doc's
// header fell past the cap", and a silently missing chip on a doc that
// DOES cite code would be exactly the dishonest gap CT-E3 exists to
// avoid. Absence of every chip claims nothing; a partial set would.
import type { CodeRefsFeedResponse } from "../api/generated/CodeRefsFeedResponse";

/// Mirrors the server's `FEED_MAX_LIMIT` clamp (routes/coderefs.rs) — ask
/// for the biggest page the server will serve so the walk is fewest trips.
export const CODEREF_FEED_PAGE_LIMIT = 100;
/// 10 pages × 100 headers = a 1000-doc corpus fully joined in ≤10 small
/// sequential requests; beyond that the walk stops and reports itself
/// incomplete rather than becoming an unbounded per-gallery-view crawl.
export const CODEREF_FEED_MAX_PAGES = 10;

export type CodeRefCounts = {
  /// doc_id → ref_count, only for scanned docs with ref_count > 0 (a
  /// zero-ref or never-scanned doc gets no chip, so it isn't carried).
  counts: Record<string, number>;
  /// False iff the page cap was hit with a `next_cursor` still pending —
  /// the map above is then a prefix of the corpus, not the corpus.
  complete: boolean;
};

/// Walk the coderef/1 feed to a doc_id → ref_count map. Pure over the
/// injected page fetcher (the hook passes the real HTTP pager; tests pass
/// arrays). `next_cursor` is absent (not null) on the last page — the
/// server omits it via `skip_serializing_if`, so `undefined` is the one
/// terminal signal and an explicit cursor string always means "more".
export async function collectCodeRefCounts(
  fetchPage: (cursor: string | undefined) => Promise<CodeRefsFeedResponse>,
  maxPages: number = CODEREF_FEED_MAX_PAGES,
): Promise<CodeRefCounts> {
  const counts: Record<string, number> = {};
  let cursor: string | undefined;
  for (let page = 0; page < maxPages; page++) {
    const res = await fetchPage(cursor);
    for (const d of res.docs) {
      if (!d.never_scanned && d.ref_count > 0) counts[d.doc_id] = d.ref_count;
    }
    if (res.next_cursor === undefined) return { counts, complete: true };
    cursor = res.next_cursor;
  }
  return { counts, complete: false };
}

/// The one chip gate: a card shows "N refs" iff the walk COMPLETED and
/// this doc's count is positive. An incomplete walk returns 0 for every
/// doc — no chip anywhere, never a partial set (see the module comment).
export function galleryRefCount(
  data: CodeRefCounts | undefined,
  docId: string,
): number {
  if (!data || !data.complete) return 0;
  return data.counts[docId] ?? 0;
}
