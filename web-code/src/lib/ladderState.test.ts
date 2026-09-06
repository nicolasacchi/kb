import { describe, expect, it } from "vitest";
import { initialLadderState, ladderReducer, type LadderState } from "./ladderState";

describe("ladderReducer", () => {
  it("starts idle: no hover, no open panel", () => {
    expect(initialLadderState).toEqual({ hoverLine: null, openLine: null });
  });

  it("hover sets hoverLine", () => {
    const next = ladderReducer(initialLadderState, { type: "hover", line: 5 });
    expect(next).toEqual({ hoverLine: 5, openLine: null });
  });

  it("hover is a no-op (same reference) when already hovering that line", () => {
    const state: LadderState = { hoverLine: 5, openLine: null };
    expect(ladderReducer(state, { type: "hover", line: 5 })).toBe(state);
  });

  it("hover moving to a new line replaces the old one", () => {
    const state: LadderState = { hoverLine: 5, openLine: null };
    expect(ladderReducer(state, { type: "hover", line: 6 })).toEqual({ hoverLine: 6, openLine: null });
  });

  it("unhover for the currently-hovered line clears it", () => {
    const state: LadderState = { hoverLine: 5, openLine: null };
    expect(ladderReducer(state, { type: "unhover", line: 5 })).toEqual({ hoverLine: null, openLine: null });
  });

  it("a stale unhover for a DIFFERENT line than the current hover is ignored", () => {
    // Pointer moved 5 -> 6 (hover:6 already dispatched); a late unhover:5
    // from the old marker must not clobber the new hover.
    const state: LadderState = { hoverLine: 6, openLine: null };
    expect(ladderReducer(state, { type: "unhover", line: 5 })).toBe(state);
  });

  it("click opens the panel for that line and clears any hover", () => {
    const state: LadderState = { hoverLine: 5, openLine: null };
    expect(ladderReducer(state, { type: "click", line: 5 })).toEqual({ hoverLine: null, openLine: 5 });
  });

  it("click on a different line while a panel is already open switches it", () => {
    const state: LadderState = { hoverLine: null, openLine: 5 };
    expect(ladderReducer(state, { type: "click", line: 9 })).toEqual({ hoverLine: null, openLine: 9 });
  });

  it("closePanel clears openLine but leaves hover untouched", () => {
    const state: LadderState = { hoverLine: 3, openLine: 5 };
    expect(ladderReducer(state, { type: "closePanel" })).toEqual({ hoverLine: 3, openLine: null });
  });

  it("closePanel is a no-op (same reference) when nothing is open", () => {
    const state: LadderState = { hoverLine: null, openLine: null };
    expect(ladderReducer(state, { type: "closePanel" })).toBe(state);
  });
});
