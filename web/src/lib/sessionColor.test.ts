import { describe, it, expect, vi } from "vitest";
import { sessionColorFor } from "./sessionColor";

describe("sessionColorFor", () => {
  it("formats hsl(<hue>deg 65% 60%) with hue in [0,360)", () => {
    const m = sessionColorFor("sess-1").match(/^hsl\((\d+)deg 65% 60%\)$/);
    expect(m).not.toBeNull();
    const hue = Number(m![1]);
    expect(hue).toBeGreaterThanOrEqual(0);
    expect(hue).toBeLessThan(360);
  });
  it("is deterministic per session id", () => {
    expect(sessionColorFor("abc")).toBe(sessionColorFor("abc"));
  });
  it("actually memoizes: the second lookup performs no cache write", () => {
    // A `toBe` on the two return values can't distinguish a memoized impl
    // from one that recomputes the same string. Instead observe the side
    // effect: a miss writes to the cache Map (one `.set`), a hit does not.
    // Deltas are measured around synchronous calls, so they're attributable
    // solely to sessionColorFor (nothing else runs between them).
    const setSpy = vi.spyOn(Map.prototype, "set");
    const id = "cold-id-not-used-elsewhere";
    const before = setSpy.mock.calls.length;
    const a = sessionColorFor(id); // miss → computes + caches
    const afterFirst = setSpy.mock.calls.length;
    const b = sessionColorFor(id); // hit → returns cached, no write
    const afterSecond = setSpy.mock.calls.length;
    setSpy.mockRestore();
    expect(a).toBe(b);
    expect(afterFirst).toBe(before + 1); // exactly one cache write on the miss
    expect(afterSecond).toBe(afterFirst); // no write on the hit
  });
  it("spreads ids across more than one hue (not a constant)", () => {
    const colors = new Set(
      Array.from({ length: 10 }, (_, i) => sessionColorFor(`session-${i}`)),
    );
    expect(colors.size).toBeGreaterThan(1);
  });
});
