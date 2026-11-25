import { describe, expect, it } from "vitest";
import { badgeCount, boardAsOf, flattenAsks, freshness, isNoSlate, ownPosts, sessionShort, splitAsks } from "./boardRows";
import { SID, board, card, who } from "./boardRows.testkit";

describe("sessionShort mirrors the server rule", () => {
  it("takes the first four chars of an ordinary id", () => {
    expect(sessionShort(SID)).toBe("8f2a");
    expect(sessionShort("8f2a5b7c99")).toBe("8f2a");
  });
  it("takes the last four of a UUIDv7", () => {
    expect(sessionShort("01a025fd-51eb-7cd3-86f2-95261769cb14")).toBe("cb14");
    expect(sessionShort("01a025fd-6a00-7e11-9d0a-0123456789ab")).toBe("89ab");
  });
  it("a 7 in the version slot of a non-UUID shape is not v7, and empty is ?", () => {
    expect(sessionShort("01a025fd-51eb-7cd3-86f2-zzzz")).toBe("01a0");
    expect(sessionShort("")).toBe("?");
    expect(sessionShort(undefined)).toBe("?");
  });
});

describe("splitAsks", () => {
  const asks = [
    card(1, "ask", { to: "claude" }),
    card(2, "ask", { to: "you", answers: 1 }),
    card(3, "ask", { to: "you" }),
    card(4, "ask"),
    card(5, "ask", { to: "claude", answers: 2 }),
    card(6, "ask", { to: "you" }),
  ];
  it("puts open asks to the operator first, then other open, answered muted last", () => {
    const g = splitAsks(asks);
    expect(g.forYou.map((c) => c.seq)).toEqual([6, 3]);
    expect(g.other.map((c) => c.seq)).toEqual([4, 1]);
    expect(g.answered.map((c) => c.seq)).toEqual([2, 5]);
    expect(flattenAsks(g).map((c) => c.seq)).toEqual([6, 3, 4, 1, 2, 5]);
  });
});

describe("ownPosts", () => {
  const b = board({
    now: [card(1, "now", { who: who({ session_short: "8f2a" }) })],
    ask: [card(2, "ask", { who: who({ session_short: "8f2a", origin: "human", harness: "spa" }) })],
    take: [card(3, "take", { who: who({ session_short: "1111" }) }), card(4, "take", { who: who({ session_short: "8f2a" }) })],
  });
  it("keeps this session's agent posts, newest first, and drops human and other sessions", () => {
    expect(ownPosts(b, SID).map((c) => c.seq)).toEqual([4, 1]);
  });
  it("is empty without a sid", () => {
    expect(ownPosts(b, undefined)).toEqual([]);
  });
});

describe("badgeCount", () => {
  it("counts only open asks addressed to the operator", () => {
    const b = board({
      ask: [card(1, "ask", { to: "you" }), card(2, "ask", { to: "you", answers: 1 }), card(3, "ask", { to: "claude" }), card(4, "ask")],
      take: [card(5, "take", { to: "you" })],
      warn: [card(6, "warn")],
    });
    expect(badgeCount(b)).toBe(1);
    expect(badgeCount(undefined)).toBe(0);
    expect(badgeCount(board())).toBe(0);
  });
});

describe("freshness words", () => {
  it("names the newest card's age, or no posts", () => {
    expect(freshness([])).toBe("no posts");
    expect(freshness([card(1, "take", { age_secs: 600 }), card(2, "take", { age_secs: 240 })])).toBe("newest 4m ago");
    expect(freshness([card(1, "take", { age_secs: 30 })])).toBe("newest 30s ago");
  });
  it("words the head seq", () => {
    expect(boardAsOf(board({}, 42))).toBe("board as of seq 42");
  });
});

describe("isNoSlate", () => {
  it("recognises the 404 problem and nothing else", () => {
    expect(isNoSlate("SlateApiError: Not Found: slate-unknown: no slate \"x\"")).toBe(true);
    expect(isNoSlate("404 Not Found")).toBe(true);
    expect(isNoSlate("TypeError: Failed to fetch")).toBe(false);
    expect(isNoSlate(null)).toBe(false);
  });
});
