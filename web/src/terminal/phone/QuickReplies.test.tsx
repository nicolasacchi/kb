// @vitest-environment jsdom
//
// FUMINPHONE (FUKEYBAR follow-up) - the quick-reply list: what `normalizeReplies`
// keeps and what `loadReplies` falls back to when storage is empty, corrupt or
// the wrong shape. Pure helpers; the chip UI is pinned in Composer.test.tsx.
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  DEFAULT_REPLIES,
  REPLIES_KEY,
  REPLIES_MAX,
  REPLY_MAX_CHARS,
  loadReplies,
  normalizeReplies,
} from "./QuickReplies";

beforeEach(() => localStorage.clear());
afterEach(() => localStorage.clear());

describe("normalizeReplies", () => {
  it("trims, drops blanks and non-strings", () => {
    expect(normalizeReplies(["  go on  ", "", "   ", 7, null, { a: 1 }, "ship it"])).toEqual(["go on", "ship it"]);
  });

  it("drops a reply longer than the cap (after trimming) and keeps one exactly at the cap", () => {
    const atCap = "x".repeat(REPLY_MAX_CHARS);
    const over = "y".repeat(REPLY_MAX_CHARS + 1);
    expect(normalizeReplies([over, `  ${atCap}  `])).toEqual([atCap]);
  });

  it("drops a multi-line reply: a chip is exactly one line", () => {
    expect(normalizeReplies(["a\nb", "c\r\nd", "ok"])).toEqual(["ok"]);
  });

  it("keeps the first REPLIES_MAX valid replies in order", () => {
    const many = Array.from({ length: REPLIES_MAX + 4 }, (_, i) => `reply ${i}`);
    expect(normalizeReplies(many)).toEqual(many.slice(0, REPLIES_MAX));
  });

  it("the count cap counts kept replies, not input entries", () => {
    const input = ["", "", ...Array.from({ length: REPLIES_MAX }, (_, i) => `r${i}`)];
    expect(normalizeReplies(input)).toHaveLength(REPLIES_MAX);
  });
});

describe("loadReplies", () => {
  it("returns the defaults (a copy) when nothing is stored", () => {
    const a = loadReplies();
    expect(a).toEqual([...DEFAULT_REPLIES]);
    a.push("mutated");
    expect(loadReplies()).toEqual([...DEFAULT_REPLIES]);
  });

  it("falls back to the defaults on corrupt JSON", () => {
    localStorage.setItem(REPLIES_KEY, "{not json");
    expect(loadReplies()).toEqual([...DEFAULT_REPLIES]);
  });

  it("falls back to the defaults when the stored value is not an array", () => {
    for (const raw of ['{"a":1}', '"text"', "42", "null", "true"]) {
      localStorage.setItem(REPLIES_KEY, raw);
      expect(loadReplies(), raw).toEqual([...DEFAULT_REPLIES]);
    }
  });

  it("normalises a stored array and keeps an emptied list empty (the operator removed them all)", () => {
    localStorage.setItem(REPLIES_KEY, JSON.stringify(["  yes  ", 3, "", "a\nb"]));
    expect(loadReplies()).toEqual(["yes"]);
    localStorage.setItem(REPLIES_KEY, "[]");
    expect(loadReplies()).toEqual([]);
  });
});
