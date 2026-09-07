// ONE answer to "what does this identifier resolve to" (V74-L2, D10's golden).
//
// D10 asks for a property, not a feature: *"cards carry the reader's link
// affordances (golden: an identifier in a card resolves exactly as in the
// reader)."* The only way to make that testable rather than aspirational is
// for both surfaces to compute the request through the SAME function — so this
// module owns the two steps between "the human pointed at a character" and
// "the daemon is asked a question":
//
//   1. `identAtColumn` — the word under a column, ON a known FILE line.
//      `editor/vimReader.ts`'s `wordAtCursor` calls it with the CM6 line it
//      already has; `identInSnippet` calls it with the line a card's snippet
//      shows. Both go through `editor/vimKeys.ts`'s `wordAt`, which is the one
//      identifier-boundary rule in this SPA and is already unit-pinned.
//   2. `resolveQueryFor` — the `GET /api/resolve` params. There is no second
//      request builder, so a card cannot ask a subtly different question.
//
// The trap this exists to close is the SECOND step of the first function. A
// card shows a WINDOW on a file: line `i` of the snippet is line
// `snippet_start + i` of the file, and getting that arithmetic wrong sends a
// perfectly well-formed request about the wrong line — which resolves, which
// navigates, and which is silently wrong. `identResolve.test.ts` feeds one
// fixture through both entry points and asserts the two requests are
// identical, which is exactly the failure it would catch.

import { wordAt } from "../editor/vimKeys";

/// Structurally identical to `editor/vimReader.ts`'s `WordPos` — declared here
/// so a `lib/` module never has to import the CM6-bearing editor module.
export interface IdentPos {
  line: number;
  /// 0-based UTF-16 column of the word's FIRST character on its line, which is
  /// what `/api/resolve`'s `col` means (`routes::ResolveQuery`).
  col: number;
  word: string;
}

/// The identifier under `col` on FILE line `fileLine`. `null` when there is no
/// word either side of the column — never a guess at a nearby one.
export function identAtColumn(
  fileLine: number,
  lineText: string,
  col: number,
): IdentPos | null {
  const found = wordAt(lineText, col);
  if (!found) return null;
  return { line: fileLine, col: found.start, word: found.word };
}

/// The identifier under `col` on the `lineIndex`-th line of a SNIPPET whose
/// first line is file line `snippetStart`.
///
/// `snippetStart` is 1-based and `lineIndex` is 0-based, matching
/// `ReviewDocCard.snippet_start` and the order `snippetLines` returns — the
/// two values a card actually has in hand.
export function identInSnippet(opts: {
  snippetStart: number;
  lineIndex: number;
  lineText: string;
  col: number;
}): IdentPos | null {
  return identAtColumn(opts.snippetStart + opts.lineIndex, opts.lineText, opts.col);
}

/// The `GET /api/resolve` params for a position. Deliberately a plain object
/// rather than a fetch: the caller decides what to do with the answer (the
/// reader opens an inline peek, a board card opens the peek panel), but they
/// must ask the same question.
export interface ResolveRequest {
  repo: string;
  path: string;
  line: number;
  col: number;
  ref?: string;
}

export function resolveQueryFor(
  repo: string,
  path: string,
  pos: IdentPos,
  ref?: string,
): ResolveRequest {
  return { repo, path, line: pos.line, col: pos.col, ...(ref ? { ref } : {}) };
}

/// The 0-based UTF-16 column of a click inside a rendered snippet line.
///
/// A painted line is a run of `<span>`s (`lib/diffHighlight.ts`'s segments), so
/// the character offset is the length of every preceding sibling's text plus
/// the offset inside the clicked one. Returns `null` when the event did not
/// land on text — an honest miss, never column 0.
export function columnFromLineClick(
  lineEl: Element,
  target: Node | null,
  offsetInTarget: number,
): number | null {
  if (!target) return null;
  const walker = (lineEl.ownerDocument ?? document).createTreeWalker(
    lineEl,
    // NodeFilter.SHOW_TEXT — spelled numerically so this module needs no DOM
    // lib types beyond `Element`/`Node`.
    0x4,
  );
  let col = 0;
  let node = walker.nextNode();
  while (node) {
    if (node === target) return col + offsetInTarget;
    col += node.textContent?.length ?? 0;
    node = walker.nextNode();
  }
  return null;
}
