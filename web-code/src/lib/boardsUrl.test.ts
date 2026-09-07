// `lib/boardsUrl.ts` — the board surface's URL grammar (V74-L2).
//
// Every parser is TOTAL: junk degrades to the documented default rather than
// throwing, and — the rule that matters for a walkthrough — an out-of-range
// `?step=` reads as "no step" rather than being clamped to one, because
// guessing which step an operator meant is the quiet repair this codebase does
// not do (`parseDiffPs`'s own refusal, same shape).
import { describe, expect, it } from "vitest";
import {
  boardHref,
  boardsHref,
  parseBoardFlag,
  parseBoardsStatus,
  parseStep,
} from "./boardsUrl";
import { BOARD_STATUSES } from "./boards";

describe("parseStep", () => {
  it("is 1-based on the wire and 0-based in hand", () => {
    expect(parseStep("1", 3)).toBe(0);
    expect(parseStep("3", 3)).toBe(2);
  });

  it("refuses junk, zero, negatives and out-of-range rather than clamping", () => {
    for (const raw of [null, "", "  ", "abc", "0", "-1", "1.5", "4", "99"]) {
      expect(parseStep(raw, 3), String(raw)).toBeNull();
    }
  });

  it("a board with NO steps has no step, whatever the URL says", () => {
    expect(parseStep("1", 0)).toBeNull();
  });
});

describe("parseBoardFlag", () => {
  it("accepts only the daemon's own truths", () => {
    for (const yes of ["1", "true", "yes"]) expect(parseBoardFlag(yes), yes).toBe(true);
    for (const no of [null, "", "0", "false", "TRUE", "on"]) {
      expect(parseBoardFlag(no), String(no)).toBe(false);
    }
  });
});

describe("parseBoardsStatus", () => {
  it("passes a real status and drops anything else", () => {
    for (const s of BOARD_STATUSES) {
      expect(parseBoardsStatus(s, BOARD_STATUSES), s).toBe(s);
    }
    // A junk status shows EVERYTHING — the honest superset, never an empty list
    // the operator would read as "there are no boards".
    expect(parseBoardsStatus("shipped", BOARD_STATUSES)).toBeNull();
    expect(parseBoardsStatus(null, BOARD_STATUSES)).toBeNull();
  });
});

describe("the hrefs", () => {
  it("omit every param at its default", () => {
    expect(boardHref("r", "b")).toBe("/r/r/~boards/b");
    expect(boardsHref("r")).toBe("/r/r/~boards");
  });

  it("append params in a FIXED order, so one view is one string", () => {
    expect(boardHref("r", "b", { step: 2, live: true, ctx: true })).toBe(
      "/r/r/~boards/b?step=3&live=1&ctx=1",
    );
    expect(boardHref("r", "b", { step: 2 })).toBe("/r/r/~boards/b?step=3");
    expect(boardHref("r", "b", { live: true })).toBe("/r/r/~boards/b?live=1");
  });

  it("a null/negative step emits nothing", () => {
    expect(boardHref("r", "b", { step: null })).toBe("/r/r/~boards/b");
    expect(boardHref("r", "b", { step: -1 })).toBe("/r/r/~boards/b");
  });

  it("percent-encodes a slug and a status", () => {
    expect(boardHref("my repo", "a b")).toBe("/r/my%20repo/~boards/a%20b");
    expect(boardsHref("r", "pending")).toBe("/r/r/~boards?status=pending");
  });

  it("round-trips: what a href emits is what the parsers read back", () => {
    const href = boardHref("r", "b", { step: 2, live: true, ctx: true });
    const q = new URLSearchParams(href.split("?")[1]);
    expect(parseStep(q.get("step"), 5)).toBe(2);
    expect(parseBoardFlag(q.get("live"))).toBe(true);
    expect(parseBoardFlag(q.get("ctx"))).toBe(true);
  });
});
