// FUMINPHONE (FUPHONELINKS follow-up) - the pressed-line slot is one-shot and
// expires after 3 s, so a Read view opened some other way never lands on a
// stale line.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setPressedLine, takePressedLine } from "./pressedLine";

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-01-01T00:00:00Z"));
  setPressedLine(null);
});
afterEach(() => {
  setPressedLine(null);
  vi.useRealTimers();
});

describe("pressedLine", () => {
  it("is taken once: a second take is null", () => {
    setPressedLine(42);
    expect(takePressedLine()).toBe(42);
    expect(takePressedLine()).toBeNull();
  });

  it("is still valid just under the TTL", () => {
    setPressedLine(7);
    vi.advanceTimersByTime(2_999);
    expect(takePressedLine()).toBe(7);
  });

  it("is stale at the TTL and is cleared by the take", () => {
    setPressedLine(7);
    vi.advanceTimersByTime(3_000);
    expect(takePressedLine()).toBeNull();
    vi.setSystemTime(new Date("2026-01-01T00:00:00Z"));
    expect(takePressedLine()).toBeNull();
  });

  it("a newer press restarts the clock, and null clears the slot", () => {
    setPressedLine(1);
    vi.advanceTimersByTime(2_000);
    setPressedLine(2);
    vi.advanceTimersByTime(2_000);
    expect(takePressedLine()).toBe(2);
    setPressedLine(3);
    setPressedLine(null);
    expect(takePressedLine()).toBeNull();
  });
});
