// V70-A6 — the ported `normalizeScrollKey` (root CLAUDE.md #31's carve-out).
//
// The key still derives PURELY from the URL; the ephemeral list only
// canonicalises params that toggle UI ON TOP of an unchanged scrollable list.
// kb-code's three are `focus` (a row focus / mobile sheet), `thread` and
// `finding` (two deep links that scroll-and-flash a row) — none of them
// changes what is scrollable, and without the carve-out each tap would
// fragment one list's scroll slot into N.
import { describe, expect, it } from "vitest";
import { LIST_EPHEMERAL_PARAMS, normalizeScrollKey } from "./useScrollRestoration";

describe("normalizeScrollKey", () => {
  it("is the identity with no ephemeral params declared", () => {
    expect(normalizeScrollKey("/r/kb/~reviews?focus=7", [])).toBe("/r/kb/~reviews?focus=7");
  });
  it("is the identity for a URL with no query at all", () => {
    expect(normalizeScrollKey("/r/kb/~todos", LIST_EPHEMERAL_PARAMS)).toBe("/r/kb/~todos");
  });
  it("strips each declared param, keeping the rest in place", () => {
    expect(normalizeScrollKey("/r/kb/~reviews?focus=7", LIST_EPHEMERAL_PARAMS)).toBe(
      "/r/kb/~reviews",
    );
    expect(
      normalizeScrollKey("/r/kb/~reviews?state=open&thread=c-1&finding=f-2", LIST_EPHEMERAL_PARAMS),
    ).toBe("/r/kb/~reviews?state=open");
  });
  it("collapses N taps of an ephemeral param onto ONE slot", () => {
    const a = normalizeScrollKey("/sessions?focus=s1", LIST_EPHEMERAL_PARAMS);
    const b = normalizeScrollKey("/sessions?focus=s2", LIST_EPHEMERAL_PARAMS);
    expect(a).toBe(b);
  });
  it("leaves a URL untouched when none of the declared params is present", () => {
    const url = "/search?q=refund&repo=kb";
    expect(normalizeScrollKey(url, LIST_EPHEMERAL_PARAMS)).toBe(url);
  });
  it("declares exactly the three kb-code params", () => {
    expect([...LIST_EPHEMERAL_PARAMS]).toEqual(["focus", "thread", "finding"]);
  });
});
