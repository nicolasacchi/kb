// The omnibox/search-page prefix-chip row (W4.3) - a subtle strip of
// buttons (@ # / ? ~ ~~) that INSERT a lane-selecting prefix into the
// query, mirroring `crates/kb-code-server/src/search/grammar.rs`'s
// documented prefix table 1:1. This is a small UI-only reimplementation
// (insert/strip only, no filter/regex parsing) - the daemon's
// `grammar::parse` stays the single source of truth for what a query
// actually MEANS; this only decides what the input box SHOWS.

export interface PrefixChip {
  /// What the chip button displays.
  label: string;
  /// What gets prepended to the query text.
  insert: string;
  /// The lane it selects - shown in the chip's title/aria-label.
  hint: string;
}

export const PREFIX_CHIPS: PrefixChip[] = [
  { label: "@", insert: "@", hint: "symbols" },
  { label: "#", insert: "#", hint: "files" },
  { label: "/", insert: "/", hint: "text" },
  // Trailing space: `?nl` is checked as a WHOLE-WORD prefix server-side
  // (grammar.rs's own doc - "?nlfoo" does NOT match), so the chip inserts
  // the space up front rather than leaving it to the next keystroke.
  { label: "?", insert: "?nl ", hint: "semantic" },
  { label: "~", insert: "~", hint: "sessions" },
  { label: "~~", insert: "~~", hint: "transcripts" },
];

// Longest-first, mirroring grammar.rs's own "~~ is itself prefixed by ~"
// ordering note - checked in this order so "~~foo" strips whole, not as a
// bare "~" leaving a stray leading "~foo".
const RECOGNIZED_PREFIXES = ["~~", "~", "@", "#", "/"];

/// Strip any lane-selecting prefix already at the start of `query` - used
/// before applying a NEW chip so repeated clicks stay idempotent instead of
/// stacking prefixes (e.g. "@#foo").
export function stripLeadingPrefix(query: string): string {
  if (query.startsWith("?nl ")) return query.slice(4);
  if (query === "?nl") return "";
  for (const p of RECOGNIZED_PREFIXES) {
    if (query.startsWith(p)) return query.slice(p.length);
  }
  return query;
}

export function applyPrefixChip(query: string, chip: PrefixChip): string {
  return chip.insert + stripLeadingPrefix(query);
}
