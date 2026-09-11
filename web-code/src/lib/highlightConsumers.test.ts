// V76-C1 — each consumer's rendered class sequence over a fixture.
// The painter is `paintSpans`; consumers must not grow a second class table.

import { describe, expect, it } from "vitest";
import type { HighlightSpan, Span } from "../api/types";
import { byteSpansToHighlightSpans, classSequence, paintSpans } from "./paintSpans";

const RUBY = "def greet(name)\n  # hi\nend\n";
const RUBY_SPANS: HighlightSpan[] = [
  { line: 1, start: 0, end: 3, role: "keyword" },
  { line: 1, start: 4, end: 9, role: "function" },
  { line: 2, start: 2, end: 6, role: "comment" },
  { line: 3, start: 0, end: 3, role: "keyword" },
];

describe("consumer class sequences (V76-C1)", () => {
  it("(a) review-diff hunk / new-file snippet", () => {
    expect(classSequence(RUBY, RUBY_SPANS).filter((s) => s.startsWith("kbc-hl-"))).toEqual([
      "kbc-hl-keyword:def",
      "kbc-hl-function:greet",
      "kbc-hl-comment:# hi",
      "kbc-hl-keyword:end",
    ]);
  });

  it("(b) suggestion old/new read-only (same painter as the hunk)", () => {
    const oldSeq = classSequence("def x\n", [
      { line: 1, start: 0, end: 3, role: "keyword" },
      { line: 1, start: 4, end: 5, role: "function" },
    ]);
    expect(oldSeq).toEqual(["kbc-hl-keyword:def", ": ", "kbc-hl-function:x"]);
  });

  it("(c) markdown fence — language from the info string", () => {
    const fenceBody = "class A\nend";
    const seq = classSequence(fenceBody, [
      { line: 1, start: 0, end: 5, role: "keyword" },
      { line: 2, start: 0, end: 3, role: "keyword" },
    ]);
    expect(seq).toEqual(["kbc-hl-keyword:class", ": A", "kbc-hl-keyword:end"]);
  });

  it("(d) LiveRefCard / RefCard — file byte spans unify onto paintSpans", () => {
    const snippet = "fn add() {}\n";
    const fileSpans: Span[] = [{ byte_start: 0, byte_len: 2, class: "keyword" }];
    const wire = byteSpansToHighlightSpans(snippet, fileSpans);
    expect(paintSpans(snippet, wire)[0]).toEqual([
      { text: "fn", cls: "kbc-hl-keyword" },
      { text: " add() {}" },
    ]);
  });

  it("(e) board code node — same sequence as a ref card", () => {
    const snippet = "fn add() {}\n";
    const fileSpans: Span[] = [{ byte_start: 0, byte_len: 2, class: "keyword" }];
    expect(classSequence(snippet, byteSpansToHighlightSpans(snippet, fileSpans))[0]).toBe(
      "kbc-hl-keyword:fn",
    );
  });

  it("tier none never emits a kbc-hl-* class", () => {
    expect(classSequence(RUBY, []).every((s) => s.startsWith(":"))).toBe(true);
  });
});
