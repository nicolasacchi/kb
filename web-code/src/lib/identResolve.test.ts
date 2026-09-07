// The READER-LINK GOLDEN (V74-L2, D10).
//
// D10 asks for a property: *"cards carry the reader's link affordances
// (golden: an identifier in a card resolves exactly as in the reader)."* This
// file is that golden, and it is deliberately not a tautology — it drives the
// two paths through their OWN coordinate arithmetic and compares the requests
// that come out:
//
// - the READER half runs CM6's real `EditorState` (`doc.lineAt(head)`,
//   `head - line.from`) — literally `editor/vimReader.ts`'s `wordAtCursor`
//   body, with the same `identAtColumn` call at the end;
// - the BOARD half runs `identInSnippet` over a SNIPPET of the same file,
//   which has its own `snippet_start + lineIndex` arithmetic.
//
// That second arithmetic is the whole point. A card shows a WINDOW on a file;
// getting the window's offset wrong produces a perfectly well-formed
// `/api/resolve` request about the wrong line — which resolves, which
// navigates, and which is silently wrong. Every case below would catch it.
import { EditorState } from "@codemirror/state";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { columnFromLineClick, identAtColumn, identInSnippet, resolveQueryFor } from "./identResolve";

const REPO = "fixture";
const PATH = "app/models/order.rb";

/// A file with the three things that break naive column arithmetic: a tab
/// indent, an astral character before an identifier, and identifiers that
/// repeat on different lines.
const FILE = [
  "class Order",
  "\tdef place",
  "    total = compute_total",
  "    # 🚀 ship it — compute_total again",
  "    notify(total)",
  "  end",
  "end",
].join("\n");

/// The reader's own path, run through CM6's real document model — this IS
/// `wordAtCursor`, with the view's cursor supplied by the test.
function readerRequest(absOffset: number) {
  const state = EditorState.create({ doc: FILE });
  const line = state.doc.lineAt(absOffset);
  const pos = identAtColumn(line.number, line.text, absOffset - line.from);
  return pos ? resolveQueryFor(REPO, PATH, pos) : null;
}

/// A card's path: the daemon sent lines `[start, end]` of the same file as one
/// snippet, and the human clicked line `lineIndex` of it at column `col`.
function boardRequest(start: number, end: number, lineIndex: number, col: number) {
  const snippet = FILE.split("\n").slice(start - 1, end).join("\n");
  const lineText = snippet.split("\n")[lineIndex];
  const pos = identInSnippet({ snippetStart: start, lineIndex, lineText, col });
  return pos ? resolveQueryFor(REPO, PATH, pos) : null;
}

/// The absolute offset of `col` on 1-based `line` — the fixture's own index,
/// used only to drive the reader half.
function offsetOf(line: number, col: number): number {
  const lines = FILE.split("\n");
  let at = 0;
  for (let i = 0; i < line - 1; i += 1) at += lines[i].length + 1;
  return at + col;
}

describe("an identifier in a card resolves exactly as in the reader", () => {
  const cases: { name: string; line: number; col: number; window: [number, number] }[] = [
    { name: "an identifier mid-line", line: 3, col: 12, window: [1, 7] },
    { name: "the first line of the window", line: 3, col: 4, window: [3, 5] },
    { name: "the last line of the window", line: 5, col: 11, window: [3, 5] },
    { name: "a TAB-indented line", line: 2, col: 5, window: [2, 4] },
    { name: "after an ASTRAL character", line: 4, col: 20, window: [4, 5] },
    { name: "a window that starts at line 1", line: 1, col: 6, window: [1, 3] },
    { name: "a window deep in the file", line: 6, col: 2, window: [5, 7] },
  ];

  for (const c of cases) {
    it(c.name, () => {
      const reader = readerRequest(offsetOf(c.line, c.col));
      const board = boardRequest(c.window[0], c.window[1], c.line - c.window[0], c.col);
      expect(reader, `${c.name}: the reader must resolve something`).not.toBeNull();
      expect(board).toEqual(reader);
      expect(board!.line).toBe(c.line);
    });
  }

  it("a column with no word under it resolves to NOTHING on both paths", () => {
    // Column 0 of line 3 is leading whitespace with no word starting at it.
    expect(readerRequest(offsetOf(3, 0))).toBeNull();
    expect(boardRequest(3, 5, 0, 0)).toBeNull();
  });

  it("the SAME name on two lines produces two DIFFERENT requests", () => {
    // `compute_total` appears on lines 3 and 4. If the snippet offset were
    // dropped, both would resolve to the window's first line — this is the
    // failure the golden exists to catch.
    const a = boardRequest(3, 5, 0, 12);
    const b = boardRequest(3, 5, 1, 20);
    expect(a!.line).toBe(3);
    expect(b!.line).toBe(4);
    expect(a).not.toEqual(b);
  });

  it("a request carries exactly the four params `/api/resolve` requires", () => {
    expect(Object.keys(boardRequest(3, 5, 0, 12)!).sort()).toEqual([
      "col",
      "line",
      "path",
      "repo",
    ]);
    // `ref` rides along only when the caller has one — never as an empty
    // string, which the route would read as a revspec.
    expect(
      resolveQueryFor(REPO, PATH, { line: 1, col: 0, word: "x" }, "abc123"),
    ).toMatchObject({ ref: "abc123" });
  });
});

describe("the reader really does go through this module", () => {
  it("`vimReader.ts`'s cursor path calls `identAtColumn`, not its own copy", () => {
    // The `deadRows.test.ts` shape: a claim about the SOURCE, so "the same
    // function on both paths" is checkable rather than promised. If the vim
    // layer ever re-derives the word boundary itself, the golden above would
    // still pass while the two paths quietly diverged.
    const src = readFileSync(
      fileURLToPath(new URL("../editor/vimReader.ts", import.meta.url)),
      "utf-8",
    );
    expect(src).toContain('from "../lib/identResolve"');
    expect(src).toContain("return identAtColumn(line.number, line.text, head - line.from);");
    expect(src).not.toContain("wordAt(line.text");
  });
});

describe("columnFromLineClick", () => {
  it("an event that did not land on text is an honest MISS, never column 0", () => {
    // The DOM branch (summing the text of every preceding painted segment) is
    // exercised by the Playwright spec, which clicks a real painted snippet;
    // what the node environment can pin is the refusal, which is the half that
    // decides whether a wrong identifier gets resolved.
    expect(columnFromLineClick({ ownerDocument: null } as unknown as Element, null, 0)).toBeNull();
  });
});
