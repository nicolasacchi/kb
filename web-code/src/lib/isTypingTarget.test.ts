import { describe, expect, it } from "vitest";
import { isTypingTarget } from "./isTypingTarget";

// vitest.config.ts runs in `environment: "node"` (no jsdom/DOM globals —
// see hooks/useIsMobile.test.ts's own comment on this crate's convention),
// so every "element" here is a minimal duck-typed mock carrying only the
// properties `isTypingTarget` actually reads.

function mockEl(opts: {
  tagName?: string;
  isContentEditable?: boolean;
  closest?: (selector: string) => unknown;
}): EventTarget {
  return {
    tagName: opts.tagName ?? "DIV",
    isContentEditable: opts.isContentEditable ?? false,
    closest: opts.closest ?? (() => null),
  } as unknown as EventTarget;
}

function cmEditorStub(editable: boolean) {
  return {
    querySelector: (selector: string) =>
      editable && selector === '[contenteditable="true"]' ? {} : null,
  };
}

describe("isTypingTarget", () => {
  it("is false for null", () => {
    expect(isTypingTarget(null)).toBe(false);
  });

  it("is false for a plain, non-editable element", () => {
    expect(isTypingTarget(mockEl({ tagName: "DIV" }))).toBe(false);
  });

  it("is true for an <input>", () => {
    expect(isTypingTarget(mockEl({ tagName: "INPUT" }))).toBe(true);
  });

  it("is true for a <textarea>", () => {
    expect(isTypingTarget(mockEl({ tagName: "TEXTAREA" }))).toBe(true);
  });

  it("is true for any contenteditable element (native isContentEditable)", () => {
    expect(isTypingTarget(mockEl({ tagName: "SPAN", isContentEditable: true }))).toBe(true);
  });

  it("is false inside a READ-ONLY CM6 editor (the reader's own buffer)", () => {
    const cm = cmEditorStub(false);
    const target = mockEl({
      tagName: "SPAN",
      closest: (sel) => (sel === ".cm-editor" ? cm : null),
    });
    expect(isTypingTarget(target)).toBe(false);
  });

  it("is true inside an EDITABLE CM6 editor (the suggestion composer)", () => {
    const cm = cmEditorStub(true);
    const target = mockEl({
      tagName: "SPAN",
      closest: (sel) => (sel === ".cm-editor" ? cm : null),
    });
    expect(isTypingTarget(target)).toBe(true);
  });

  it("is false when not inside any .cm-editor at all", () => {
    const target = mockEl({ tagName: "SPAN", closest: () => null });
    expect(isTypingTarget(target)).toBe(false);
  });

  it("is false for a target with no closest() (e.g. window)", () => {
    const target = { tagName: undefined } as unknown as EventTarget;
    expect(isTypingTarget(target)).toBe(false);
  });
});
