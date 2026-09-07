// `kbc-refs/1` — the SPA half of the review-document reference grammar
// (V73-K2b, design D9/D9-a).
//
// This is a MIRROR of the daemon's `crates/kb-code-server/src/review_doc/
// refs.rs`, not a derivation of it — neither side generates the other, and
// the only thing keeping them from drifting is ONE shared fixture,
// `crates/kb-code-server/grammar/kbcrefs.golden.json`, which BOTH sides'
// golden tests walk (`kbcRefs.golden.test.ts` here,
// `golden_corpus_matches_the_rust_parser` there). That is exactly the
// discipline `lib/kbcq.ts` carries for kbcq/1 (web-code/CLAUDE.md § Search
// grammar), applied to a second grammar.
//
// Why a TS parser exists at all: the Document tab replaces every `[[…]]`
// span in the rendered body with a LIVE card, and it must decide — before
// any round trip — which spans are kbc refs (ours), which are kb wikilinks
// (root invariant #29's, left as literal text) and which are typo'd kbc
// refs (shown as a malformed chip naming the reason). The server already
// sends resolved CARDS keyed by the ref body; this side's job is to find
// the spans and join on that key, never to invent a resolution.
//
// **A bare `[[X]]` is a kb wikilink and can never be a kbc ref.** Every kbc
// ref carries one of the seven CLOSED scheme prefixes below. A `[[…]]`
// naming a known scheme that does not parse is `malformed` with a reason —
// never silently degraded into a wikilink, which would make the failure
// invisible on both sides (kb-code-server/CLAUDE.md invariant 22(b)).

/**
 * The nine scheme prefixes, in the order `refs::SCHEMES` declares them.
 * V73-K5 added `ci` (a check-run snapshot citation) and `question` (a
 * document-local back-reference into this document's own `questions[]`).
 */
export const SCHEMES = [
  "code",
  "sym",
  "ent",
  "finding",
  "gh",
  "kb",
  "hunk",
  "ci",
  "question",
] as const;
export type Scheme = (typeof SCHEMES)[number];

/** `gh:` object kinds — closed, so a typo is a malformed ref. */
export const GH_KINDS = ["comment", "review", "issue", "pr"] as const;

/**
 * A parsed reference. The field names and the `scheme` tag are the serde
 * serialization of the Rust `Ref` enum verbatim — the golden's `parsed`
 * objects ARE those bytes, so this type is the wire shape, not a
 * convenience shape. Absent optionals are OMITTED (serde's
 * `skip_serializing_if = "Option::is_none"`), never `null`.
 */
export type KbcRef =
  | { scheme: "code"; raw: string; path: string; line?: number; line_end?: number; sha?: string }
  | { scheme: "sym"; raw: string; container?: string; name: string }
  | { scheme: "ent"; raw: string; fqn: string }
  | { scheme: "finding"; raw: string; slug: string }
  | { scheme: "gh"; raw: string; kind: string; id: string }
  | { scheme: "kb"; raw: string; kb: string; id: string }
  | { scheme: "hunk"; raw: string; path: string; ps: number; index: number }
  | { scheme: "ci"; raw: string; name: string }
  | { scheme: "question"; raw: string; index: number };

/** What a `[[…]]` body turned out to be — exhaustive and disjoint. */
export type RefClass =
  | { kind: "ref"; ref: KbcRef }
  | { kind: "wikilink"; body: string }
  | { kind: "malformed"; body: string; reason: string };

/** One `[[…]]` span found by {@link scanRefs}, with the author's position. */
export interface FoundRef {
  class: RefClass;
  /** 1-based line within the whole document text handed to `scanRefs`. */
  line: number;
  /** 1-based column, counted in code points (what an editor shows). */
  col: number;
  /** The body exactly as written, without the `[[`/`]]`. */
  body: string;
}

/** `Ok(None)` — this body is not ours. */
const NOT_OURS = Symbol("not-a-kbc-ref");

/** Rust's `{:?}` on a `&str`, close enough for a message. */
function dbg(s: string): string {
  return JSON.stringify(s);
}

/**
 * Rust's `str::parse::<u32>()` — decimal only, an optional leading `+`, no
 * whitespace, no empty string, and range-checked. `Number()` would happily
 * accept `" 12 "`, `0x10` and `1e3`, all of which the daemon rejects.
 */
function parseU32(s: string): number | null {
  if (!/^\+?[0-9]+$/.test(s)) return null;
  const n = Number(s);
  return Number.isSafeInteger(n) && n >= 0 && n <= 4294967295 ? n : null;
}

/** Rust's `str::parse::<i64>()`, within JS's safe-integer range. */
function parseI64(s: string): number | null {
  if (!/^[+-]?[0-9]+$/.test(s)) return null;
  const n = Number(s);
  return Number.isSafeInteger(n) ? n : null;
}

/** Rust's `rsplit_once(sep)`. */
function rsplitOnce(s: string, sep: string): [string, string] | null {
  const i = s.lastIndexOf(sep);
  return i === -1 ? null : [s.slice(0, i), s.slice(i + sep.length)];
}

/** Rust's `split_once(sep)`. */
function splitOnce(s: string, sep: string): [string, string] | null {
  const i = s.indexOf(sep);
  return i === -1 ? null : [s.slice(0, i), s.slice(i + sep.length)];
}

/**
 * A plausible abbreviated git object name: 4–64 hex digits. Deliberately
 * permissive on LENGTH (git accepts any unambiguous abbreviation) and
 * strict on ALPHABET, so `a@b.rb` is never mistaken for a pinned sha.
 */
function isHexish(s: string): boolean {
  return s.length >= 4 && s.length <= 64 && /^[0-9a-fA-F]+$/.test(s);
}

/**
 * `f-[a-z0-9-]+` — the same shape
 * `review_findings::is_valid_finding_slug` enforces on the wire.
 */
export function isValidFindingSlug(s: string): boolean {
  return /^f-[a-z0-9-]+$/.test(s);
}

/**
 * `…:<line>` or `…:<lo>-<hi>` at the very END of `s`; `null` when the tail
 * is not a line suffix (so the whole string is a path).
 */
function splitLineSuffix(s: string): [string, number, number | null] | null {
  const parts = rsplitOnce(s, ":");
  if (!parts) return null;
  const [head, tail] = parts;
  if (head === "") return null;
  const dash = splitOnce(tail, "-");
  if (dash) {
    const lo = parseU32(dash[0]);
    const hi = parseU32(dash[1]);
    if (lo === null || hi === null) return null;
    return [head, lo, hi];
  }
  const lo = parseU32(tail);
  return lo === null ? null : [head, lo, null];
}

/**
 * `code:<path>[:<line>[-<end>]][@<sha>]`.
 *
 * Parsed RIGHT to LEFT, because only the tail is unambiguous: a path may
 * contain `@` and `:`, while `@<hex>` at the very end and `:<digits>`
 * immediately before it cannot be anything else.
 */
function parseCode(raw: string, rest: string): KbcRef {
  let head = rest;
  let sha: string | undefined;
  const at = rsplitOnce(rest, "@");
  if (at && isHexish(at[1]) && at[0] !== "") {
    head = at[0];
    sha = at[1];
  }
  const suffix = splitLineSuffix(head);
  const path = suffix ? suffix[0] : head;
  const line = suffix ? suffix[1] : undefined;
  const lineEnd = suffix && suffix[2] !== null ? suffix[2] : undefined;
  if (path === "") throw new RefError("code ref has an empty path");
  if (line !== undefined && lineEnd !== undefined && lineEnd < line) {
    throw new RefError(`code ref line range ${line}-${lineEnd} ends before it starts`);
  }
  const out: KbcRef = { scheme: "code", raw, path };
  if (line !== undefined) out.line = line;
  if (lineEnd !== undefined) out.line_end = lineEnd;
  if (sha !== undefined) out.sha = sha;
  return out;
}

/**
 * `sym:<Qualified>[#<member>]`. `#` splits container from name; failing
 * that, the LAST `::` does; failing that, the whole body is a bare name.
 * `::` is never a field boundary in its own right.
 */
function parseSym(raw: string, restIn: string): KbcRef {
  const rest = restIn.trim();
  if (rest === "") throw new RefError("sym ref has an empty symbol");
  const hash = splitOnce(rest, "#");
  if (hash) {
    if (hash[0] === "" || hash[1] === "") {
      throw new RefError(`sym ref ${dbg(rest)} has an empty side of '#'`);
    }
    return { scheme: "sym", raw, container: hash[0], name: hash[1] };
  }
  const colons = rsplitOnce(rest, "::");
  if (colons) {
    if (colons[0] === "" || colons[1] === "") {
      throw new RefError(`sym ref ${dbg(rest)} has an empty side of '::'`);
    }
    return { scheme: "sym", raw, container: colons[0], name: colons[1] };
  }
  return { scheme: "sym", raw, name: rest };
}

/**
 * `ent:<Fqn>` — validated only for SHAPE here (non-empty, no whitespace).
 * Whether it is a legal constant path is the entity index's call, made at
 * RESOLUTION time so the grammar and the address rules stay one rule.
 */
function parseEnt(raw: string, restIn: string): KbcRef {
  const fqn = restIn.trim();
  if (fqn === "") throw new RefError("ent ref has an empty constant path");
  if (/\s/.test(fqn)) throw new RefError(`ent ref ${dbg(fqn)} contains whitespace`);
  return { scheme: "ent", raw, fqn };
}

/** `finding:<slug>` — `f-` prefixed, the wire's own slug shape. */
function parseFinding(raw: string, restIn: string): KbcRef {
  const slug = restIn.trim();
  if (!isValidFindingSlug(slug)) {
    throw new RefError(`finding ref ${dbg(slug)} is not a valid finding slug (f-[a-z0-9-]+)`);
  }
  return { scheme: "finding", raw, slug };
}

/** `gh:<kind>/<id>` with `kind` from {@link GH_KINDS}. */
function parseGh(raw: string, rest: string): KbcRef {
  const parts = splitOnce(rest, "/");
  if (!parts) {
    throw new RefError(`gh ref ${dbg(rest)} must be <kind>/<id>, kind one of ${GH_KINDS.join("|")}`);
  }
  const [kind, id] = parts;
  if (!(GH_KINDS as readonly string[]).includes(kind)) {
    throw new RefError(`gh ref kind ${dbg(kind)} is not one of ${GH_KINDS.join("|")}`);
  }
  if (id === "" || /\s/.test(id)) {
    throw new RefError(`gh ref ${dbg(rest)} has an empty or spaced id`);
  }
  return { scheme: "gh", raw, kind, id };
}

/** `kb:<kb>/<id>` — split on the FIRST `/`; a kb name never contains one. */
function parseKb(raw: string, rest: string): KbcRef {
  const parts = splitOnce(rest, "/");
  if (!parts) throw new RefError(`kb ref ${dbg(rest)} must be <kb>/<id>`);
  const [kb, id] = parts;
  if (kb === "" || id === "") throw new RefError(`kb ref ${dbg(rest)} has an empty kb or id`);
  if (/\s/.test(rest)) throw new RefError(`kb ref ${dbg(rest)} contains whitespace`);
  return { scheme: "kb", raw, kb, id };
}

/** `hunk:<path>@<ps>#<n>`. Parsed right to left, like `code:`. */
function parseHunk(raw: string, rest: string): KbcRef {
  const hash = rsplitOnce(rest, "#");
  if (!hash) throw new RefError(`hunk ref ${dbg(rest)} must end in #<n>`);
  const index = parseU32(hash[1]);
  if (index === null) throw new RefError(`hunk ref index ${dbg(hash[1])} is not a number`);
  const at = rsplitOnce(hash[0], "@");
  if (!at) throw new RefError(`hunk ref ${dbg(rest)} must name a patchset (@<ps>)`);
  const [path, psRaw] = at;
  const psDigits = psRaw.startsWith("ps") ? psRaw.slice(2) : psRaw;
  const ps = parseI64(psDigits);
  if (ps === null) throw new RefError(`hunk ref patchset ${dbg(psRaw)} is not a number`);
  if (path === "") throw new RefError("hunk ref has an empty path");
  if (ps < 1) throw new RefError(`hunk ref patchset ${ps} must be >= 1`);
  return { scheme: "hunk", raw, path, ps, index };
}

/**
 * `ci:<check-name>`. The name is whatever GitHub's Checks API calls the
 * run — free text, routinely containing spaces/slashes/parens — so the
 * only refusal is an empty name.
 */
function parseCi(raw: string, rest: string): KbcRef {
  const name = rest.trim();
  if (name === "") throw new RefError("ci ref has an empty check name");
  return { scheme: "ci", raw, name };
}

/**
 * `question:<n>`, `n >= 1` — a 1-based ordinal into the CURRENT document's
 * own `questions[]`.
 */
function parseQuestion(raw: string, restIn: string): KbcRef {
  const rest = restIn.trim();
  const index = parseU32(rest);
  if (index === null) {
    throw new RefError(`question ref ${dbg(rest)} is not a positive whole number`);
  }
  if (index === 0) {
    throw new RefError("question ref index must be >= 1 (questions are numbered from 1)");
  }
  return { scheme: "question", raw, index };
}

/** The `Err(reason)` arm of the Rust `Result`, as a throw. */
class RefError extends Error {}

/**
 * Parse ONE ref body (no `[[`/`]]`). Returns the ref, `NOT_OURS` for a body
 * with no known scheme prefix (a kb wikilink — never this module's
 * business), and throws a {@link RefError} for a body that names a known
 * scheme but does not parse.
 */
function parseRefInner(body: string): KbcRef | typeof NOT_OURS {
  const raw = body.trim();
  if (raw === "") return NOT_OURS;
  const parts = splitOnce(raw, ":");
  if (!parts) return NOT_OURS;
  const [scheme, rest] = parts;
  if (!(SCHEMES as readonly string[]).includes(scheme)) return NOT_OURS;
  switch (scheme as Scheme) {
    case "code":
      return parseCode(raw, rest);
    case "sym":
      return parseSym(raw, rest);
    case "ent":
      return parseEnt(raw, rest);
    case "finding":
      return parseFinding(raw, rest);
    case "gh":
      return parseGh(raw, rest);
    case "kb":
      return parseKb(raw, rest);
    case "hunk":
      return parseHunk(raw, rest);
    case "ci":
      return parseCi(raw, rest);
    case "question":
      return parseQuestion(raw, rest);
  }
}

/**
 * Parse ONE ref body. `null` means "not a kbc ref at all" (a kb wikilink);
 * a throw is impossible — a malformed body comes back through
 * {@link classify}.
 */
export function parseRef(body: string): KbcRef | null {
  const got = classify(body);
  return got.kind === "ref" ? got.ref : null;
}

/**
 * One `[[…]]` body → its class. The three outcomes are exhaustive and
 * disjoint: a kbc ref, a kb wikilink, or a typo'd kbc ref.
 */
export function classify(body: string): RefClass {
  try {
    const got = parseRefInner(body);
    if (got === NOT_OURS) return { kind: "wikilink", body };
    return { kind: "ref", ref: got };
  } catch (e) {
    return { kind: "malformed", body, reason: e instanceof Error ? e.message : String(e) };
  }
}

/**
 * `true` for the schemes this daemon deliberately does not resolve LIVE:
 * `gh`/`kb` address something on the far side of a call kb-code never
 * makes or a corpus it does not own; `ci` reads a snapshot rather than
 * calling GitHub's Checks API again.
 */
export function isInert(r: KbcRef): boolean {
  return r.scheme === "gh" || r.scheme === "kb" || r.scheme === "ci";
}

// --- the document scanner --------------------------------------------------
//
// Mirrors `refs::scan_refs`: fenced code blocks (matching fence length),
// inline code spans (matching backtick runs) and a leading YAML front-matter
// region are skipped. A ref written inside a fence is a ref being TALKED
// ABOUT, not a ref being MADE. Four-space indented code blocks are
// deliberately NOT skipped (indistinguishable from a nested list item
// without a full block parse).

function openingFence(trimmed: string): [string, number] | null {
  for (const ch of ["`", "~"]) {
    let run = 0;
    while (run < trimmed.length && trimmed[run] === ch) run += 1;
    if (run >= 3) return [ch, run];
  }
  return null;
}

function isClosingFence(trimmed: string, ch: string, len: number): boolean {
  let run = 0;
  while (run < trimmed.length && trimmed[run] === ch) run += 1;
  return run >= len && trimmed.slice(run).trim() === "";
}

/** The first `]]` at or after `from`, or `null` for an unterminated `[[`. */
function findClose(chars: string[], from: number): number | null {
  for (let k = from; k + 1 < chars.length; k += 1) {
    if (chars[k] === "]" && chars[k + 1] === "]") return k;
  }
  return null;
}

/** The end index (exclusive) of the inline code span opening at `i`, or null. */
function codeSpanEnd(chars: string[], i: number): number | null {
  let run = 0;
  while (i + run < chars.length && chars[i + run] === "`") run += 1;
  let j = i + run;
  while (j < chars.length) {
    if (chars[j] === "`") {
      let r = 0;
      while (j + r < chars.length && chars[j + r] === "`") r += 1;
      if (r === run) return j + r;
      j += r;
    } else {
      j += 1;
    }
  }
  return null;
}

/**
 * Every `[[…]]` span on ONE line, honouring inline code spans. `lineno` is
 * the 1-based document line; columns are 1-based code points, the same
 * numbers `refs::scan_line` reports.
 */
export function scanLine(line: string, lineno: number): FoundRef[] {
  const out: FoundRef[] = [];
  const chars = Array.from(line);
  let i = 0;
  while (i < chars.length) {
    if (chars[i] === "`") {
      const end = codeSpanEnd(chars, i);
      if (end !== null) {
        i = end;
      } else {
        let run = 0;
        while (i + run < chars.length && chars[i + run] === "`") run += 1;
        i += run;
      }
      continue;
    }
    if (chars[i] === "[" && i + 1 < chars.length && chars[i + 1] === "[") {
      const close = findClose(chars, i + 2);
      if (close !== null) {
        const body = chars.slice(i + 2, close).join("");
        out.push({ class: classify(body), line: lineno, col: i + 1, body });
        i = close + 2;
        continue;
      }
    }
    i += 1;
  }
  return out;
}

/**
 * Walk a whole Markdown document and return every `[[…]]` span, classified.
 * The SPA's inline card replacement uses {@link scanLine} per rendered text
 * node instead; this whole-document walk exists for the rail's jump list
 * and for the golden's own document-level cases.
 */
export function scanRefs(doc: string): FoundRef[] {
  const out: FoundRef[] = [];
  let fence: [string, number] | null = null;
  let inFrontMatter = false;
  const lines = doc.split("\n");
  // `str::lines()` drops a trailing empty segment after a final newline.
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  for (let idx = 0; idx < lines.length; idx += 1) {
    const line = lines[idx].replace(/\r$/, "");
    const lineno = idx + 1;
    const trimmed = line.replace(/^[ \t]+/, "");
    if (idx === 0 && line.replace(/[ \t]+$/, "") === "---") {
      inFrontMatter = true;
      continue;
    }
    if (inFrontMatter) {
      if (line === "---" || line === "...") inFrontMatter = false;
      continue;
    }
    if (fence) {
      if (isClosingFence(trimmed, fence[0], fence[1])) fence = null;
      continue;
    }
    const open = openingFence(trimmed);
    if (open) {
      fence = open;
      continue;
    }
    out.push(...scanLine(line, lineno));
  }
  return out;
}

/**
 * Every well-formed ref in `doc`, deduplicated by `raw`, in document order —
 * `refs::refs_in`'s mirror. Wikilinks and malformed spans are dropped (they
 * have no card); {@link scanRefs} is what a surface that must SHOW them
 * reads.
 */
export function refsIn(doc: string): KbcRef[] {
  const seen = new Set<string>();
  const out: KbcRef[] = [];
  for (const f of scanRefs(doc)) {
    if (f.class.kind !== "ref") continue;
    if (seen.has(f.class.ref.raw)) continue;
    seen.add(f.class.ref.raw);
    out.push(f.class.ref);
  }
  return out;
}

/**
 * The document BODY — everything after the front matter's closing delimiter.
 * Mirrors `review_doc::frontmatter::split`'s body arm: an opening `---` on
 * the very first line, closed by a bare `---` or `...` line. `GET …/doc`
 * ships `doc_md` (the lossless record) and the typed fields, but not the
 * body on its own, so this side splits it exactly as the daemon does rather
 * than rendering the YAML as prose.
 *
 * TOTAL: a document with no front matter, or one whose front matter never
 * closes, is returned verbatim — the daemon would have refused to store an
 * unclosed one, so treating it as body here is the honest degrade.
 */
export function docBody(docMd: string): string {
  let rest: string;
  if (docMd.startsWith("---\n")) rest = docMd.slice(4);
  else if (docMd.startsWith("---\r\n")) rest = docMd.slice(5);
  else if (docMd.trimEnd() === "---") return "";
  else return docMd;
  let i = 0;
  while (i < rest.length) {
    const nl = rest.indexOf("\n", i);
    const end = nl === -1 ? rest.length : nl;
    const bare = rest.slice(i, end).replace(/\r$/, "");
    const next = nl === -1 ? rest.length : nl + 1;
    if (bare === "---" || bare === "...") return rest.slice(next);
    i = next;
  }
  return docMd;
}
