// V73-K2a — the review diff's per-HUNK identity and state.
//
// Diff v2 (design §D9: "content-addressed per-hunk reviewed state that
// survives rebases") needs a hunk to have a NAME that outlives a rebase.
// A hunk's `@@ -a,b +c,d @@` header does not: rebasing the branch onto a
// moved base shifts every line number in the file, so a positional id
// ("file X hunk 3") silently re-points at different code the moment
// anything above it changes. So the id here is CONTENT-ADDRESSED:
//
//   hunkId(path, hunk) = fnv1a64( path + "\n" + each CHANGED line, in
//                                 order, prefixed by its own sigil )
//
// Two deliberate exclusions, each load-bearing:
//
//   * **line numbers** — `oldStart`/`newStart`/`oldLines`/`newLines` are
//     exactly what a rebase perturbs, so hashing them would defeat the
//     purpose;
//   * **context lines** — git's `-U3` window around a hunk is a function of
//     what happens to sit NEAR the change. Edit an unrelated line four
//     rows above and the context shifts; the change itself did not. Only
//     `add`/`remove` lines are hashed.
//
// The consequence, stated rather than hidden: two IDENTICAL changes in the
// same file (the same one-line edit applied twice in different places)
// collide onto one id, and marking one viewed marks both. That is the
// price of surviving a rebase, and it is the same trade a content address
// always makes. It is not a correctness hazard for viewed state (the
// operator sees both hunks' chips flip together, which is honest about
// what was addressed), and it is why this id is NEVER used as an anchor
// for a comment or a finding — those keep the existing line-anchored
// carry-forward ladder (`review_comments::resolve_for_ps_with_content`).
//
// The SERVER stores these ids opaquely (`review_hunk_viewed`,
// migration V0031): the addressing scheme is this file's, the daemon only
// keeps a set. `kbc-hunkid/1` is the version string that pins it — bump it
// here AND in `crates/kb-code-server/src/reviews.rs` if the hashed input
// ever changes, since already-stored ids would otherwise silently stop
// matching (an old id simply never matches again — a viewed hunk reads
// unviewed, never the reverse).

import type { DiffHunk, ParsedDiff } from "./diff";

/// The hunk-addressing scheme's version. Rendered in the docs and mirrored
/// server-side as a comment; see this module's header for the bump rule.
export const HUNK_ID_SCHEMA = "kbc-hunkid/1";

/// FNV-1a, 64-bit, over UTF-16 code units, in BigInt so the arithmetic is
/// exact (a `number`-based 32-bit FNV collides far too readily across the
/// thousands of hunks a large review carries). Rendered as 16 lowercase
/// hex chars, zero-padded — a stable, URL-safe, `?hunk=`-able token.
function fnv1a64(text: string): string {
  const PRIME = 0x100000001b3n;
  const MASK = 0xffffffffffffffffn;
  let hash = 0xcbf29ce484222325n;
  for (let i = 0; i < text.length; i++) {
    hash ^= BigInt(text.charCodeAt(i));
    hash = (hash * PRIME) & MASK;
  }
  return hash.toString(16).padStart(16, "0");
}

/// The exact bytes `hunkId` hashes — exported so the golden test can pin
/// the INPUT as well as the output, which is what makes an accidental
/// change to the recipe read as itself rather than as an opaque hex diff.
export function hunkFingerprintInput(path: string, hunk: DiffHunk): string {
  const parts: string[] = [path];
  for (const line of hunk.lines) {
    if (line.kind === "add") parts.push(`+${line.text}`);
    else if (line.kind === "remove") parts.push(`-${line.text}`);
  }
  return parts.join("\n");
}

/// The content address for one hunk of one file. Stable across rebases,
/// across `?ctx=` widths (context lines are not hashed) and across
/// unified/split rendering.
export function hunkId(path: string, hunk: DiffHunk): string {
  return fnv1a64(hunkFingerprintInput(path, hunk));
}

/// Every hunk id in a parsed file diff, in hunk order.
export function hunkIds(path: string, parsed: ParsedDiff): string[] {
  return parsed.hunks.map((h) => hunkId(path, h));
}

/// How many `+`/`-` lines one hunk carries (the `large` noise rule's input
/// and the hunk strip's own "+N −M" pair).
export function hunkStats(hunk: DiffHunk): { additions: number; deletions: number } {
  let additions = 0;
  let deletions = 0;
  for (const line of hunk.lines) {
    if (line.kind === "add") additions += 1;
    else if (line.kind === "remove") deletions += 1;
  }
  return { additions, deletions };
}

/// The NEW-side line span a hunk covers, or `null` for a pure-deletion
/// hunk (nothing on the new side to span). Used by the context dial to ask
/// the file route for the right window, and by `hasThreadsAt` to decide
/// whether a thread lands inside this hunk.
export function hunkNewSpan(hunk: DiffHunk): { start: number; end: number } | null {
  let start: number | null = null;
  let end: number | null = null;
  for (const line of hunk.lines) {
    if (line.newLine === null) continue;
    if (start === null) start = line.newLine;
    end = line.newLine;
  }
  return start === null || end === null ? null : { start, end };
}

/// The OLD-side line span, same shape (`null` for a pure-addition hunk).
export function hunkOldSpan(hunk: DiffHunk): { start: number; end: number } | null {
  let start: number | null = null;
  let end: number | null = null;
  for (const line of hunk.lines) {
    if (line.oldLine === null) continue;
    if (start === null) start = line.oldLine;
    end = line.oldLine;
  }
  return start === null || end === null ? null : { start, end };
}

/// One line-anchored thread, reduced to what hunk membership needs. The
/// caller passes the RESOLVED position (`ResolvedForPs.line`) — an orphan
/// (`line: null`) belongs to no hunk, which is the honest answer: it is
/// rendered in the file's orphan section, exactly as before.
export interface HunkThreadRef {
  side: "old" | "new" | null;
  line: number | null;
}

/// Does any of `threads` land inside this hunk? A `side: null` thread is
/// matched against BOTH spans (the pre-side-aware rows the wire still
/// carries), which can only ever over-report "this hunk has threads" —
/// never hide one.
export function hunkHasThreads(hunk: DiffHunk, threads: readonly HunkThreadRef[]): boolean {
  if (threads.length === 0) return false;
  const oldSpan = hunkOldSpan(hunk);
  const newSpan = hunkNewSpan(hunk);
  for (const t of threads) {
    if (t.line === null) continue;
    if (t.side !== "new" && oldSpan && t.line >= oldSpan.start && t.line <= oldSpan.end) return true;
    if (t.side !== "old" && newSpan && t.line >= newSpan.start && t.line <= newSpan.end) return true;
  }
  return false;
}
