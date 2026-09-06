// V71-D2 — refine-within-results: Emacs `consult`'s two-level filtering,
// which the search research names as "the highest value-per-line feature in
// the whole proposal" (research/search-understands-codebase.md §3.3).
//
// The expensive engine runs ONCE and returns a page; the human then narrows
// that page for free. Refinement is:
//
//   - **orderless** — space-separated needles, matched in ANY order,
//   - **literal** — no regex, no fuzzy, no scoring,
//   - **smartcase** — an all-lowercase needle is case-insensitive; a needle
//     with any uppercase character is matched case-sensitively (vim's rule,
//     and the one `search/text.rs`'s `case:` already follows),
//   - **subtractive** — a leading `!` on a needle EXCLUDES.
//
// It is emphatically NOT a matcher. It assigns no score, produces no
// ranking, and preserves the server's own order exactly — which is why it
// lives here rather than in `lib/speedSearch.ts` (deprecated as a matcher,
// kb-code-server/CLAUDE.md #16b) and why nothing in this file may ever grow
// a `score` field. The one matcher is nucleo, server-side.
//
// Because it only ever REMOVES rows from a page the server already chose, a
// refinement can never surface something the query did not return — which
// is the honest thing for it to be, and is why the UI captions it "N of M
// shown" rather than presenting it as a second search.

/// One parsed refinement: the needles to keep by and the needles to drop
/// by. Both are already case-folded when their own smartcase rule says so.
export interface Refinement {
  include: Needle[];
  exclude: Needle[];
  /// `true` when the raw text held nothing to filter on — the caller then
  /// shows the whole page rather than an empty one.
  empty: boolean;
}

export interface Needle {
  text: string;
  /// `false` = fold both sides to lower case before comparing.
  caseSensitive: boolean;
}

function needle(raw: string): Needle {
  const caseSensitive = raw !== raw.toLowerCase();
  return { text: caseSensitive ? raw : raw.toLowerCase(), caseSensitive };
}

/// Parse the refine input. Total: any string, including `""` and pure
/// punctuation, yields a `Refinement`.
export function parseRefinement(raw: string): Refinement {
  const include: Needle[] = [];
  const exclude: Needle[] = [];
  for (const tok of raw.split(/\s+/)) {
    if (tok === "" || tok === "!") continue;
    if (tok.startsWith("!")) exclude.push(needle(tok.slice(1)));
    else include.push(needle(tok));
  }
  return { include, exclude, empty: include.length === 0 && exclude.length === 0 };
}

function hit(haystack: string, n: Needle): boolean {
  return n.caseSensitive ? haystack.includes(n.text) : haystack.toLowerCase().includes(n.text);
}

/// Does `haystack` survive `r`? Every include needle must appear somewhere
/// in it (order-free), and no exclude needle may.
export function matchesRefinement(haystack: string, r: Refinement): boolean {
  if (r.empty) return true;
  for (const n of r.exclude) if (hit(haystack, n)) return false;
  for (const n of r.include) if (!hit(haystack, n)) return false;
  return true;
}

/// Narrow `rows` by `raw`, keeping the server's order. `haystackOf` builds
/// the text one row is matched against — the CALLER decides what a row's
/// searchable text is (path + line + symbol name …), so this module never
/// has to know the six lanes' shapes.
export function refineRows<T>(
  rows: T[],
  raw: string,
  haystackOf: (row: T) => string,
): { kept: T[]; refinement: Refinement } {
  const refinement = parseRefinement(raw);
  if (refinement.empty) return { kept: rows, refinement };
  return { kept: rows.filter((r) => matchesRefinement(haystackOf(r), refinement)), refinement };
}
