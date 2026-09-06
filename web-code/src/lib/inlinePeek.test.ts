// V70-A6 — the inline peek's pure state.
import { describe, expect, it } from "vitest";
import {
  breadcrumb,
  clampContext,
  closedInlinePeek,
  CONTEXT_DEFAULT,
  CONTEXT_MAX,
  CONTEXT_MIN,
  CONTEXT_STEP,
  excerptWindow,
  inlinePeekReducer,
  INLINE_PEEK_MAX_DEPTH,
  isOpen,
  topFrame,
  type InlinePeekState,
} from "./inlinePeek";

function opened(): InlinePeekState {
  return inlinePeekReducer(closedInlinePeek, {
    type: "OPEN",
    hostLine: 20,
    scrollTop: 512,
    frame: { repo: "kb", path: "a.rs", line: 5, title: "Foo#bar", trust: "exact" },
  });
}

describe("open / close", () => {
  it("opens one frame and remembers the scroll to restore", () => {
    const s = opened();
    expect(isOpen(s)).toBe(true);
    expect(s.hostLine).toBe(20);
    expect(s.savedScrollTop).toBe(512);
    expect(topFrame(s)?.loading).toBe(true);
  });

  it("CLOSE empties the stack but keeps the saved offset for the restore", () => {
    const s = inlinePeekReducer(opened(), { type: "CLOSE" });
    expect(isOpen(s)).toBe(false);
    expect(s.savedScrollTop).toBe(512);
  });
});

describe("nesting", () => {
  const push = (s: InlinePeekState, n: number) =>
    inlinePeekReducer(s, {
      type: "PUSH",
      frame: { repo: "kb", path: `${n}.rs`, line: n, title: `f${n}` },
    });

  it("breadcrumbs outermost first", () => {
    const s = push(push(opened(), 2), 3);
    expect(breadcrumb(s)).toEqual(["Foo#bar", "f2", "f3"]);
  });

  it("REPLACES the deepest frame at the cap rather than going dead", () => {
    let s = opened();
    for (let i = 2; i <= 6; i++) s = push(s, i);
    expect(s.frames).toHaveLength(INLINE_PEEK_MAX_DEPTH);
    // …and it is the NEWEST hop that survives, not the one that happened to
    // be third: a `gd` at depth 3 has to go somewhere, and refusing the key
    // reads as broken.
    expect(topFrame(s)?.title).toBe("f6");
  });

  it("PUSH on a closed stack is a no-op — there is nothing to nest inside", () => {
    expect(isOpen(push(closedInlinePeek, 2))).toBe(false);
  });

  it("POP unwinds one frame, and closes at the last", () => {
    const s = push(opened(), 2);
    const one = inlinePeekReducer(s, { type: "POP" });
    expect(breadcrumb(one)).toEqual(["Foo#bar"]);
    expect(isOpen(inlinePeekReducer(one, { type: "POP" }))).toBe(false);
  });
});

describe("the context dial", () => {
  it("clamps to the design's floor and ceiling", () => {
    expect(clampContext(0)).toBe(CONTEXT_MIN);
    expect(clampContext(999)).toBe(CONTEXT_MAX);
    expect(clampContext(NaN)).toBe(CONTEXT_DEFAULT);
  });

  it("steps the TOP frame only", () => {
    let s = inlinePeekReducer(opened(), {
      type: "PUSH",
      frame: { repo: "kb", path: "b.rs", line: 2, title: "b" },
    });
    s = inlinePeekReducer(s, { type: "CONTEXT", delta: 1 });
    expect(s.frames[0].context).toBe(CONTEXT_DEFAULT);
    expect(s.frames[1].context).toBe(CONTEXT_DEFAULT + CONTEXT_STEP);
  });

  it("is a no-op at the ceiling (identity, so React does not re-render)", () => {
    let s = opened();
    for (let i = 0; i < 10; i++) s = inlinePeekReducer(s, { type: "CONTEXT", delta: 1 });
    const again = inlinePeekReducer(s, { type: "CONTEXT", delta: 1 });
    expect(again).toBe(s);
    expect(topFrame(s)?.context).toBe(CONTEXT_MAX);
  });
});

describe("content + errors land on the right frame", () => {
  it("by path", () => {
    const s = inlinePeekReducer(opened(), { type: "SET_CONTENT", path: "a.rs", content: "x\ny" });
    expect(topFrame(s)?.content).toBe("x\ny");
    expect(topFrame(s)?.loading).toBe(false);
  });
  it("an error clears loading and says why", () => {
    const s = inlinePeekReducer(opened(), { type: "SET_ERROR", path: "a.rs", message: "boom" });
    expect(topFrame(s)?.error).toBe("boom");
    expect(topFrame(s)?.loading).toBe(false);
  });
});

describe("excerptWindow", () => {
  it("biases UP so the signature is visible above the target line", () => {
    const w = excerptWindow(50, 12, 500);
    expect(w.end - w.start + 1).toBe(12);
    expect(w.start).toBeLessThan(50);
    expect(w.end).toBeGreaterThan(50);
    expect(50 - w.start).toBe(4); // ctx/3
  });
  it("clamps at the top of a file", () => {
    expect(excerptWindow(2, 12, 500)).toEqual({ start: 1, end: 12 });
  });
  it("re-anchors at the bottom of a file instead of returning a short window", () => {
    const w = excerptWindow(98, 12, 100);
    expect(w).toEqual({ start: 89, end: 100 });
  });
  it("is empty (not negative) before the content arrives", () => {
    const w = excerptWindow(5, 12, 0);
    expect(w.end).toBeLessThan(w.start);
  });
  it("never exceeds a short file", () => {
    expect(excerptWindow(2, 30, 4)).toEqual({ start: 1, end: 4 });
  });
});
