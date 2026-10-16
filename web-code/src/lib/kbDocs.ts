// OL S1 - pure helpers for the omnibox "Documents" lane (kb's own search,
// relayed by `GET /api/kb/search`). Kept apart from the hook and the
// component so the decisions - when to ask, what a row opens, what the
// caption says - are unit-testable without a DOM.

import type { KbDocHitOut, KbSearchOut } from "../api/types";

/// Minimum query length (chars, trimmed) before kb is asked - mirrors the
/// relay's own floor, so the lane never fires a request the server would
/// answer with an empty page anyway.
export const KB_DOCS_MIN_CHARS = 2;

/// Rows requested per query.
export const KB_DOCS_LIMIT = 8;

/// The omnibox's scoped-search prefixes (`@` symbols, `#` files, `/` text,
/// `~` sessions) and command mode (`>`). A prefixed query is a code-lane
/// question; sending `@resolve` to a document search would only add noise.
const SCOPED_PREFIXES = "@#/~>";

/// Should this omnibox query also search kb's documents?
export function shouldQueryKbDocs(q: string): boolean {
  const t = q.trim();
  if (t.length < KB_DOCS_MIN_CHARS) return false;
  return !SCOPED_PREFIXES.includes(t[0]);
}

/// Navigable row count (header excluded) - an unavailable response has none.
export function kbDocsRowCount(res: KbSearchOut | null): number {
  if (!res || res.unavailable) return 0;
  return res.hits.length;
}

/// The muted line for an `unavailable` caption. Closed vocabulary from
/// `kb_relay.rs`; anything else (a future caption) is shown verbatim rather
/// than guessed at.
export function kbDocsCaption(unavailable: string): string {
  switch (unavailable) {
    case "kb disabled":
      return "kb is not configured for this daemon";
    case "kb search timed out":
      return "kb search timed out - kb is up but slow to answer";
    case "kb unreachable":
      return "kb is unreachable right now";
    case "kb_sibling_mismatch":
      return "kb speaks a different sibling contract - update one of the two daemons";
    default:
      return unavailable;
  }
}

/// A permalink the box will open: http(s) only. The URL comes from the relay
/// (built on kb's public base), but it is still data from another daemon -
/// a `javascript:` URL must never reach `window.open`/`href`.
export function safeDocUrl(url: string): string | null {
  try {
    const u = new URL(url);
    return u.protocol === "http:" || u.protocol === "https:" ? u.toString() : null;
  } catch {
    return null;
  }
}

/// The URL for row `row` of the lane, or null (out of range / unsafe).
export function kbDocsRowHref(res: KbSearchOut | null, row: number): string | null {
  if (!res || res.unavailable) return null;
  const hit: KbDocHitOut | undefined = res.hits[row];
  return hit ? safeDocUrl(hit.url) : null;
}
