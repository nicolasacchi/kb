import { describe, it, expect } from "vitest";
import { EditorState, type TransactionSpec } from "@codemirror/state";
import { wrapSpec, continueListSpec } from "./commands";

// Pure (view-free) coverage of the tricky editor transforms. The full toolbar /
// keymap / live-preview integration is covered by tests/e2e/spa-editor.spec.ts;
// these pin the idempotent-wrap + smart-list edge cases a headless run can't.

function run(
  doc: string,
  anchor: number,
  head: number,
  fn: (s: EditorState) => TransactionSpec | null,
): string | null {
  const state = EditorState.create({ doc, selection: { anchor, head } });
  const spec = fn(state);
  return spec ? state.update(spec).state.doc.toString() : null;
}

describe("wrapSpec (bold/italic/code toggle)", () => {
  it("wraps a selection", () => {
    expect(run("hello", 0, 5, (s) => wrapSpec(s, "**"))).toBe("**hello**");
  });
  it("unwraps when markers sit just outside the selection", () => {
    // "**hello**", select the inner "hello" (2..7) → toggle off.
    expect(run("**hello**", 2, 7, (s) => wrapSpec(s, "**"))).toBe("hello");
  });
  it("unwraps when markers are captured inside the selection", () => {
    expect(run("**hello**", 0, 9, (s) => wrapSpec(s, "**"))).toBe("hello");
  });
  it("inserts empty markers at the caret", () => {
    expect(run("ab", 1, 1, (s) => wrapSpec(s, "**"))).toBe("a****b");
  });
});

describe("continueListSpec (smart Enter)", () => {
  it("continues a bullet", () => {
    expect(run("- one", 5, 5, continueListSpec)).toBe("- one\n- ");
  });
  it("continues a task as unchecked", () => {
    expect(run("- [ ] task", 10, 10, continueListSpec)).toBe("- [ ] task\n- [ ] ");
  });
  it("carries a checked task forward as unchecked", () => {
    expect(run("- [x] done", 10, 10, continueListSpec)).toBe("- [x] done\n- [ ] ");
  });
  it("increments an ordered list", () => {
    expect(run("1. first", 8, 8, continueListSpec)).toBe("1. first\n2. ");
  });
  it("terminates an empty item", () => {
    expect(run("- ", 2, 2, continueListSpec)).toBe("");
  });
  it("returns null on a non-list line (default newline runs)", () => {
    expect(run("plain text", 10, 10, continueListSpec)).toBeNull();
  });
});
