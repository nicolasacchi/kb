// W2.8 — capture-to-draft, the honest zero-daemon version.
//
// The synthesis's original ask (a real `kb-status: draft` stamped at
// capture, filterable via a server-side gate) needs daemon changes twice
// over (see /tmp/w2-recon-folio-capture.md §4): a `status`/`exclude_status`
// gate in `DocsQuery`/`ListParams` to make it gallery-visible, AND a
// widened invariant #12 meta-patch scope to make it clearable from the
// SPA. Both are refused here — this is the zero-daemon-change draft tag
// grammar instead:
//
//   - CaptureSheet stamps the plain tag `draft` (DRAFT_TAG) alongside the
//     user's own tags — no new wire field, no new stamping code.
//   - The gallery excludes it BY DEFAULT via the existing `NOT tag:x` `?q=`
//     DSL atom (kb_core::query, invariant #35's grammar) — `matches`
//     lowers straight to `DocsQuery::exclude_tags`.
//   - Clearing draft status is the existing tags editor (PATCH .../meta
//     already accepts `tags`, invariant #12) — no widening needed.
//   - Drilling into `?tags=draft` (the Sidebar's "Drafts (N)" chip) is the
//     draft VIEW: the exclusion is skipped there so drafts are visible,
//     framed as a staging shelf, not filtered out of existence.

export const DRAFT_TAG = "draft";

// Matches a `tag:draft` atom (positive or `NOT`-negated, quoted or bare) in
// the gallery's `?q=` DSL string — used both to avoid double-appending the
// exclusion and to detect "the user is already looking at/for drafts".
// Deliberately loose (doesn't parse the full DSL): a false-positive match
// only means we skip auto-appending the exclusion, which is harmless.
const DRAFT_ATOM_RE = /\btag\s*:\s*"?draft"?(?=\s|\)|$)/i;

/// True when `q` already mentions a `tag:draft` atom, in either polarity.
export function referencesDraftTag(q: string): boolean {
  return DRAFT_ATOM_RE.test(q);
}

/// Compose the gallery's `?q=` DSL string with the default draft
/// exclusion appended, unless the caller's own query already references
/// `tag:draft` (in which case it's left untouched — the user is either
/// already excluding it themselves or explicitly querying for it).
///
/// A top-level `OR` in the user's query is parenthesized before the NOT
/// is appended: `kb_core::docs_query::matches_conjunct` applies
/// `exclude_tags` per-conjunct (unlike the route-injected category/mtime
/// gates, which apply across every OR alternative — see
/// `crates/kb-core/src/docs_query.rs`), so a bare
/// `tag:a OR tag:b NOT tag:draft` would only exclude drafts from the
/// `tag:b` branch. Grouping first (`(tag:a OR tag:b) NOT tag:draft`)
/// distributes the exclusion into both DNF conjuncts.
export function composeDraftExclusion(userQ: string): string {
  const trimmed = userQ.trim();
  if (trimmed.length === 0) return `NOT tag:${DRAFT_TAG}`;
  if (referencesDraftTag(trimmed)) return userQ;
  const needsGroup = /\bor\b/i.test(trimmed);
  const base = needsGroup ? `(${trimmed})` : trimmed;
  return `${base} NOT tag:${DRAFT_TAG}`;
}

/// True when the current URL params already put the user in "the draft
/// view" — either the tags facet is pinned to `draft` (the Sidebar chip,
/// or a folder/tag deep-link) or their own `?q=` references `tag:draft` —
/// in which case the gallery must NOT auto-append the exclusion (that
/// would hide the very drafts the view is meant to show).
export function isDraftView(params: Pick<URLSearchParams, "get">): boolean {
  const tags = (params.get("tags") ?? "")
    .split(",")
    .map((t) => t.trim())
    .filter(Boolean);
  if (tags.includes(DRAFT_TAG)) return true;
  return referencesDraftTag(params.get("q") ?? "");
}
