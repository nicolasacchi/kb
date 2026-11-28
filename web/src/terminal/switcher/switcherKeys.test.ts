import { describe, expect, it } from "vitest";
import { chordOf, nextId, nextNeedsId } from "./switcherKeys";

const ev = (code: string, o: Partial<Parameters<typeof chordOf>[0]> = {}) => ({
  altKey: true,
  ctrlKey: false,
  metaKey: false,
  shiftKey: false,
  code,
  ...o,
});

describe("nextId", () => {
  const ids = ["a", "b", "c"].map((id) => ({ id }));
  it("steps and wraps in both directions", () => {
    expect(nextId(ids, "a", 1)).toBe("b");
    expect(nextId(ids, "c", 1)).toBe("a");
    expect(nextId(ids, "a", -1)).toBe("c");
    expect(nextId(ids, "b", -1)).toBe("a");
  });
  it("an unknown current id starts at the first (next) or last (prev)", () => {
    expect(nextId(ids, "zzz", 1)).toBe("a");
    expect(nextId(ids, null, -1)).toBe("c");
  });
  it("a single row, or none, has nowhere to go", () => {
    expect(nextId([{ id: "a" }], "a", 1)).toBeNull();
    expect(nextId([{ id: "a" }], "a", -1)).toBeNull();
    expect(nextId([], "a", 1)).toBeNull();
    expect(nextId([{ id: "a" }], "zzz", 1)).toBe("a");
  });
});

describe("nextNeedsId", () => {
  const rows = [
    { id: "a", tier: "idle" },
    { id: "b", tier: "needs" },
    { id: "c", tier: "idle" },
    { id: "d", tier: "needs" },
  ];
  it("finds the next needing row after the current, wrapping", () => {
    expect(nextNeedsId(rows, "a")).toBe("b");
    expect(nextNeedsId(rows, "b")).toBe("d");
    expect(nextNeedsId(rows, "d")).toBe("b");
  });
  it("never returns the current row and is null when nothing else needs you", () => {
    expect(nextNeedsId([{ id: "a", tier: "needs" }], "a")).toBeNull();
    expect(nextNeedsId([{ id: "a", tier: "idle" }], "a")).toBeNull();
    expect(nextNeedsId([], null)).toBeNull();
  });
  it("an unknown current id starts from the top", () => {
    expect(nextNeedsId(rows, null)).toBe("b");
  });
});

describe("chordOf", () => {
  it("maps the three Alt chords by physical code", () => {
    expect(chordOf(ev("ArrowDown"))).toBe("next");
    expect(chordOf(ev("ArrowUp"))).toBe("prev");
    expect(chordOf(ev("KeyN"))).toBe("needs");
  });
  it("needs Alt alone: no bare key, no Ctrl/Meta/Shift, no composition", () => {
    expect(chordOf(ev("ArrowDown", { altKey: false }))).toBeNull();
    expect(chordOf(ev("KeyN", { altKey: false }))).toBeNull();
    expect(chordOf(ev("ArrowDown", { ctrlKey: true }))).toBeNull();
    expect(chordOf(ev("ArrowDown", { metaKey: true }))).toBeNull();
    expect(chordOf(ev("KeyN", { shiftKey: true }))).toBeNull();
    expect(chordOf(ev("KeyN", { isComposing: true }))).toBeNull();
    expect(chordOf(ev("KeyK"))).toBeNull();
  });
});
