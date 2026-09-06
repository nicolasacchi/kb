// DCB W3.B — pure "Cited by" helpers for `components/lens/CitedBy.tsx`. Same
// "pure helpers live in `lib/`, network I/O + JSX stay in the component"
// split `lib/docLensUrl.ts` established for the sibling lens page.
//
// The chip's headline number is DISTINCT CITING DOCS, not a raw claim count:
// `doc_refs` persists one row per source-ref ORDINAL (W3.A's schema,
// `sync.rs`), so a single doc can carry more than one row for the SAME
// resolved path — it cited the same file at two different ordinals (e.g.
// two different lines). Reporting the raw row count as "N docs" would
// overstate how many distinct documents actually mention this file; dedup by
// `(kb, doc_id)` for the headline (doc identity on this wire, not `doc_id`
// alone — see `citedByDocCount`'s own doc), then list every claim (one row
// per citation, possibly several per doc) in the expanded view. When the
// claim count and the dedup'd doc count diverge, `citedByLabel` appends the
// raw citation count too (m9, W3.B.R review) — the rows below the headline
// are per-claim, so a reader comparing "Cited by 2 docs" against 3 visible
// rows would otherwise wonder if one was a rendering bug.

import type { DocRefClaim } from "../api/types";

/// Distinct `(kb, doc_id)` pairs among `claims` — doc identity on this wire
/// is the PAIR, not `doc_id` alone: a `doc-refs/1` response can in
/// principle span more than one corpus, and two different kbs are free to
/// mint the same `doc_id`. Deduping on `doc_id` alone would silently
/// undercount that (rare but real) cross-kb collision.
export function citedByDocCount(claims: DocRefClaim[]): number {
  return new Set(claims.map((c) => `${c.kb} ${c.doc_id}`)).size;
}

/// The label to render for ONE claim's row. The server NEVER persists an
/// empty `doc_title`: `sync.rs`'s `doc_title_or_fallback` (DCB-W3.A.R fix 6)
/// falls back to the doc path's basename, then the doc id itself, at WRITE
/// time — a title-less kb doc can't reach the wire as `""` in the first
/// place. This client-side ladder mirrors that same fallback order and is
/// therefore redundant-but-safe defense-in-depth, not a real-world gap it's
/// covering for; `citedBy.test.ts` exercises it directly, and the e2e
/// `doc5` fixture (`doc-lens.spec.ts`) separately exercises the SERVER-side
/// fallback end to end.
export function citedByRowLabel(c: Pick<DocRefClaim, "doc_title" | "doc_path" | "doc_id">): string {
  if (c.doc_title.trim() !== "") return c.doc_title;
  const base = c.doc_path
    .split("/")
    .filter((s) => s !== "")
    .pop();
  return base && base.trim() !== "" ? base : c.doc_id;
}

/// The collapsed strip's label. `live === false` is the SOLE "rotted claim"
/// signal (`doc-refs/1`'s own doc: every claim in one response shares the
/// same path, so liveness is a property of the response, never of one row)
/// — appended once to the whole strip, never rendered per-claim.
///
/// m9 (W3.B.R review): the headline counts DISTINCT DOCS, but the expanded
/// list renders one row per CLAIM — when a doc cites the same file at more
/// than one ordinal, `claims.length` exceeds `n` and the two numbers
/// visibly disagree once expanded. Append `· M citations` (only when they
/// diverge) so the headline stays honest about what the list below it
/// actually shows.
export function citedByLabel(claims: DocRefClaim[], live: boolean): string {
  const n = citedByDocCount(claims);
  const noun = n === 1 ? "doc" : "docs";
  let base = `Cited by ${n} ${noun}`;
  if (claims.length !== n) {
    base += ` · ${claims.length} citations`;
  }
  return live ? base : `${base} — path no longer present`;
}
