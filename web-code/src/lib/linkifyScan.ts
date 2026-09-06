// Pure scanner: finds clickable tokens (URLs, repo-relative-looking paths,
// `Kb-Session:` values) inside a file's COMMENT/STRING highlight spans only
// — never inside code identifiers, keywords, or any other token class, so
// this can never turn a real symbol reference into a fake "path" link.
// `editor/linkify.ts` (the CM6 extension) decorates whatever this returns;
// this module does no DOM/CM6 work at all, same split as
// `lib/decorations.ts` vs. `editor/highlightField.ts`.
//
// Positions are UTF-16 offsets into `content` (CodeMirror's own convention —
// see `lib/decorations.ts`'s header comment on why a byte→UTF-16 mapper is
// needed at all), even though the underlying `Span[]` the server sends is
// UTF-8 byte offsets — this module reuses `decorations.ts`'s
// `makeByteToUtf16Mapper` + `spansToDecorationRanges` to do that conversion
// once, then regex-scans each already-UTF16 comment/string range's text.
//
// **Deliberately conservative** (false negatives over false positives, per
// the milestone brief): a path needs a short dotted extension, plus EITHER
// a `/` or a recognized source/doc extension (see `looksLikePath`'s doc for
// why a bare `lib.rs` still needs to qualify), so prose like "e.g." or a
// bare version number like "v1.2.3" never lights up. **Commit shas ARE
// linkified (Wave C)** — the earlier deferral here is resolved now that a
// real destination exists (`commitUrl`, the commit page hub): `SHA_RE`
// requires 7–40 LOWERCASE hex chars, word-bounded on both sides (see that
// pattern's own doc for why the bounding matters), which keeps the
// original false-positive worry (a hash-like constant, a color literal,
// part of a UUID) narrow — a session id (`SESSION_RE`, dashed UUID form)
// never collides, and an already-claimed URL/session span is checked the
// same way `looksLikePath` checks one for a path candidate.

import type { Span } from "../api/types";
import { makeByteToUtf16Mapper, spansToDecorationRanges } from "./decorations";

export type LinkKind = "url" | "path" | "session" | "sha";

export interface LinkToken {
  /// UTF-16 `[from, to)` into the file's `content` string.
  from: number;
  to: number;
  kind: LinkKind;
  /// The token text itself (for `session`, just the captured id — NOT
  /// including the `Kb-Session:` prefix).
  value: string;
}

/// Only these two highlight classes are ever scanned — a code identifier
/// that happens to look like a path (vanishingly rare, since identifiers
/// can't contain `/`) is still never a candidate, since it isn't a
/// comment/string span in the first place.
const SCANNED_CLASSES = new Set(["comment", "string"]);

const URL_RE = /https?:\/\/[^\s<>"'`)\]]+/g;

/// `[\w./-]+\.\w{1,8}` per the milestone brief; the "at least one slash"
/// half of the conservatism rule is applied as a post-filter (see
/// `looksLikePath`) rather than folded into the character class itself.
///
/// **Deviation from the brief's literal wording**: a bare filename with NO
/// directory prefix (`lib.rs`, `Cargo.toml`, `README.md` — extremely common
/// in real comments, and the exact shape the milestone's own e2e fixture
/// needs: a file at a repo's ROOT has no slash to offer) would otherwise
/// never linkify. `looksLikePath` accepts a slash-less candidate too, but
/// ONLY when its extension is in [`KNOWN_SOURCE_EXTENSIONS`] — still
/// conservative (an unrecognized slash-less token like `e.g.` or `v1.2.3`
/// stays plain text), just not gated on the slash alone.
const PATH_CANDIDATE_RE = /[\w./-]+\.\w{1,8}/g;

/// Gates the SLASH-LESS case in `looksLikePath` — a path WITH a `/` needs no
/// such allowlist (the directory separator is already enough evidence on
/// its own). Deliberately short: common source/doc/config extensions only,
/// so prose like "e.g." or "v1.2.3" (extensions `g`/`3`, neither here) never
/// qualifies.
const KNOWN_SOURCE_EXTENSIONS = new Set([
  "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rb", "go", "java", "kt", "kts",
  "swift", "c", "h", "cc", "cpp", "hpp", "cs", "php", "toml", "yaml", "yml", "json",
  "md", "txt", "sh", "bash", "zsh", "css", "scss", "html", "htm", "sql", "proto", "lock",
]);

function looksLikePath(candidate: string): boolean {
  if (candidate.includes("/")) return true;
  const dot = candidate.lastIndexOf(".");
  if (dot === -1) return false;
  return KNOWN_SOURCE_EXTENSIONS.has(candidate.slice(dot + 1).toLowerCase());
}

/// Case-insensitive: a capture hook could plausibly emit either casing for
/// the trailer key, and a session id is hex either way.
const SESSION_RE = /Kb-Session:\s*([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})/gi;

/// 7–40 lowercase hex chars, word-bounded on both sides (`\b` next to a
/// `[0-9a-f]` character class — hex digits are a subset of `\w`, so a
/// boundary can only land where the run of word characters actually ENDS,
/// meaning a longer alphanumeric identifier with a hex-looking PREFIX, or a
/// hex run longer than 40 chars, never partially matches — see the module
/// doc's "Commit shas ARE linkified" note for the false-positive reasoning
/// behind requiring all-lowercase). Every real sha this scanner will ever
/// see (git's own `%H`/`%h` output, or a comment quoting one) is already
/// lowercase, so a case-insensitive match would only buy MORE false
/// positives (e.g. `0xDEADBEEF`-style hex constants), never a real miss.
const SHA_RE = /\b[0-9a-f]{7,40}\b/g;

function overlaps(a: [number, number], b: [number, number]): boolean {
  return a[0] < b[1] && a[1] > b[0];
}

/// Scan one already-isolated comment/string range's text for tokens,
/// offsetting every match back into whole-file coordinates. URL, session,
/// and sha matches are found first and "claimed"; a path match overlapping
/// any of them (e.g. the path-shaped tail of a URL) is dropped rather than
/// double-linkifying the same characters.
function scanRange(text: string, offset: number): LinkToken[] {
  const claimed: Array<[number, number]> = [];
  const tokens: LinkToken[] = [];

  for (const m of text.matchAll(URL_RE)) {
    const from = offset + (m.index ?? 0);
    const to = from + m[0].length;
    claimed.push([from, to]);
    tokens.push({ from, to, kind: "url", value: m[0] });
  }

  for (const m of text.matchAll(SESSION_RE)) {
    const id = m[1];
    const idStartInMatch = m[0].lastIndexOf(id);
    const from = offset + (m.index ?? 0) + idStartInMatch;
    const to = from + id.length;
    claimed.push([from, to]);
    tokens.push({ from, to, kind: "session", value: id });
  }

  for (const m of text.matchAll(SHA_RE)) {
    const from = offset + (m.index ?? 0);
    const to = from + m[0].length;
    if (claimed.some((c) => overlaps(c, [from, to]))) continue;
    claimed.push([from, to]);
    tokens.push({ from, to, kind: "sha", value: m[0] });
  }

  for (const m of text.matchAll(PATH_CANDIDATE_RE)) {
    if (!looksLikePath(m[0])) continue;
    const from = offset + (m.index ?? 0);
    const to = from + m[0].length;
    if (claimed.some((c) => overlaps(c, [from, to]))) continue;
    tokens.push({ from, to, kind: "path", value: m[0] });
  }

  tokens.sort((a, b) => a.from - b.from);
  return tokens;
}

/// Entry point: `content` is the file's full text (same string CodeMirror
/// renders), `spans` its server-derived highlight spans (UTF-8 byte
/// offsets). Returns tokens in ascending `from` order, one pass per
/// comment/string span — trusts `spansToDecorationRanges`'s own ordering
/// guarantee (see that function's doc) rather than re-sorting the whole
/// output afterward.
export function scanLinkTokens(content: string, spans: Span[]): LinkToken[] {
  if (spans.length === 0) return [];
  const mapper = makeByteToUtf16Mapper(content);
  const ranges = spansToDecorationRanges(content, spans, mapper).filter((r) => SCANNED_CLASSES.has(r.class));
  const tokens: LinkToken[] = [];
  for (const range of ranges) {
    tokens.push(...scanRange(content.slice(range.from, range.to), range.from));
  }
  return tokens;
}
