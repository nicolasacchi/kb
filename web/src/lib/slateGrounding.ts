// D29 (v0.42) — groundedness captions, the PURE half.
//
// A slate `found`/`tried` post may cite `path:crates/foo/src/bar.rs:141`.
// When the current kb has a `code_url`, the board asks kb-code whether that
// path (and line) is really there and captions the chip. The mapping from
// kb-code's `codelens/1` vocabulary to the three caption words lives HERE,
// alone, as a total function — the fetch (hooks/useSlateGrounding.ts) adds
// no interpretation of its own.
//
// THE POSTURE (invariant #2, D29): kb extracts hints, kb-code mints classes,
// and NOTHING is cached. This caption is computed per render, lives in the
// TanStack cache for 30 s like every other cross-daemon doclens read, is
// never persisted, is never a score, is never in the digest, and is NEVER
// sent to the kb daemon — the kb daemon still knows kb-code only as an inert
// URL. If the answer is not certain the caption says `unknown`; no arm below
// ever guesses `grounded`.

import type { LineState, PathState } from "../api/doclens";
import type { SlateKind } from "../api/slateTypes";

/// The three caption words. `unknown` is a first-class answer, not an error
/// state: kb-code down, CORS-blocked, an ambiguous path and a line kb-code
/// declined to verify are all honestly "I cannot say".
export type Groundedness = "grounded" | "ungrounded" | "unknown";

/// Only knowledge cards carry captions (D29: "each `path:` ref on found/
/// tried cards"). A `take`'s subject path is a lease label, not a claim
/// about the tree, and captioning it would read as a verdict on the take.
export const CAPTIONED_KINDS: readonly SlateKind[] = ["found", "tried"];

export function captionsOn(kind: SlateKind): boolean {
  return CAPTIONED_KINDS.includes(kind);
}

/// One `path:` ref, split. `null` for anything that is not a `path:` ref.
export type PathRef = {
  /// Repo-relative, exactly as authored (never rewritten, never resolved).
  path: string;
  /// The FIRST line the ref names — `:141`, `:141-160` and `:141,150,160`
  /// all give 141 — or null for a bare path. kb-code answers about one
  /// line; asking about the first named one is the honest reduction, and
  /// `unknown` covers everything a single line cannot settle.
  line: number | null;
};

/// Parse `path:<p>[:<line>[-<end>|,<more>]]`. TOTAL: any shape it cannot
/// read confidently comes back `null` (or as a bare path) and is simply not
/// line-checked. A string with no `path:` prefix is not this function's
/// business.
export function parsePathRef(raw: string): PathRef | null {
  if (!raw.startsWith("path:")) return null;
  const rest = raw.slice(5).trim();
  if (!rest) return null;
  const colon = rest.lastIndexOf(":");
  if (colon <= 0) return { path: rest, line: null };
  const head = rest.slice(0, colon);
  const tail = rest.slice(colon + 1);
  // `141`, `141-160`, `141,150` — the closed grammar kb's own extractor
  // (invariant #2) already accepts. A tail that is not a line list means the
  // colon belonged to the path, so the whole string is the path.
  const m = /^(\d+)(?:[-,]\d+)*$/.exec(tail);
  if (!m) return { path: rest, line: null };
  const line = Number(m[1]);
  if (!Number.isSafeInteger(line) || line <= 0) return { path: head, line: null };
  return { path: head, line };
}

/// What kb-code answered about one path — the `codelens/1` fields this
/// caption reads, all optional so a partial or older payload degrades to
/// `unknown` instead of throwing. `line_hint` (SL7f) echoes back the `?line=`
/// this call asked about; it is what tells "no line was asked" (`undefined`/
/// `null`) apart from "a line WAS asked and kb-code sent back no verdict" —
/// two different facts a plain `line_state` reading cannot distinguish on
/// its own.
export type PathLens = {
  path_state?: PathState | null;
  line_hint?: number | null;
  line_state?: LineState | null;
};

/// codelens/1 -> caption. The whole mapping, in one table:
///
/// | `path_state`                              | `line_hint` | `line_state`            | caption      |
/// |-------------------------------------------|-------------|-------------------------|--------------|
/// | `present`                                 | absent      | (irrelevant)            | `grounded`   |
/// | `present`                                 | present     | `confirmed`             | `grounded`   |
/// | `present`                                 | present     | `drifted`               | `ungrounded` |
/// | `present`                                 | present     | `absent`                | `ungrounded` |
/// | `present`                                 | present     | `unverifiable` / absent | `unknown`    |
/// | `absent`                                  | anything    | anything                | `ungrounded` |
/// | `ambiguous` / `external` / null / unknown | anything    | anything                | `unknown`    |
///
/// SL7f (v0.42 amendment) flips `drifted` to `ungrounded`: kb-code found the
/// cited line's own confirm token only ELSEWHERE in the file, which means
/// the post's literal claim — "the cited content is at THIS line" — is no
/// longer accurate, even though the code still exists somewhere in the
/// file. A caption is a verdict on the CITATION as written, not on whether
/// the file still contains the token anywhere. `unverifiable` (kb-code
/// declined to check — no usable confirm token, ambiguous window, or no
/// `?context=` at all) and a bare `line_hint` with no `line_state` at all
/// (a shape no route mints today, kept as a defensive default rather than
/// an assumed pass) are both `unknown` — never optimistically grounded from
/// a missing or declined verdict.
export function groundednessOf(lens: PathLens | null | undefined): Groundedness {
  if (!lens) return "unknown";
  switch (lens.path_state) {
    case "absent":
      return "ungrounded";
    case "present":
      break;
    default:
      // `ambiguous` (many candidates — a guess is not an answer),
      // `external` (a vendored/gem path this checkout does not own), null,
      // or a value this SPA does not know yet.
      return "unknown";
  }
  if (lens.line_hint === undefined || lens.line_hint === null) {
    // No line was asked about (a bare `path:` ref): the PATH is the whole
    // claim, and kb-code says it is present.
    return "grounded";
  }
  switch (lens.line_state) {
    case "confirmed":
      return "grounded";
    case "drifted":
      return "ungrounded";
    case "absent":
      return "ungrounded";
    case "unverifiable":
    default:
      // `unverifiable`, or a line kb-code declined to (or could not) give
      // any verdict on — never assume grounded from a missing one.
      return "unknown";
  }
}

/// The caption's hover text — always names the daemon that answered and what
/// the word means, so `unknown` never reads as a verdict on the post.
export function groundingTitle(g: Groundedness): string {
  switch (g) {
    case "grounded":
      return "kb-code resolved this path in the checkout";
    case "ungrounded":
      return "kb-code did not find this path (or line) in the checkout";
    default:
      return "kb-code could not answer — unverified, not wrong";
  }
}
