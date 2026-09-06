import { describe, it, expect } from "vitest";
import { cmdkTotalRows, cmdkRowBase, cmdkRowAt } from "./cmdkRows";

const C = {
  recents: 2,
  commands: 3,
  hits: 1,
  notes: 2,
  memories: 0,
  sessions: 1,
}; // flat layout: r r c c c h n n s  (9 rows; memories empty → skipped)

describe("cmdkRows geometry", () => {
  it("totals every group", () => {
    expect(cmdkTotalRows(C)).toBe(9);
    expect(cmdkTotalRows({ recents: 0, commands: 0, hits: 0, notes: 0, memories: 0, sessions: 0 })).toBe(0);
  });

  it("bases each group after the preceding ones (empty groups contribute 0)", () => {
    expect(cmdkRowBase("recent", C)).toBe(0);
    expect(cmdkRowBase("command", C)).toBe(2);
    expect(cmdkRowBase("hit", C)).toBe(5);
    expect(cmdkRowBase("note", C)).toBe(6);
    expect(cmdkRowBase("memory", C)).toBe(8); // empty, but base is after notes
    expect(cmdkRowBase("session", C)).toBe(8); // memories=0 → session starts where memory would
  });

  it("maps each flat cursor to the right group + in-group index", () => {
    expect(cmdkRowAt(0, C)).toEqual({ kind: "recent", idx: 0 });
    expect(cmdkRowAt(1, C)).toEqual({ kind: "recent", idx: 1 });
    expect(cmdkRowAt(2, C)).toEqual({ kind: "command", idx: 0 });
    expect(cmdkRowAt(4, C)).toEqual({ kind: "command", idx: 2 });
    expect(cmdkRowAt(5, C)).toEqual({ kind: "hit", idx: 0 });
    expect(cmdkRowAt(6, C)).toEqual({ kind: "note", idx: 0 });
    expect(cmdkRowAt(7, C)).toEqual({ kind: "note", idx: 1 });
    expect(cmdkRowAt(8, C)).toEqual({ kind: "session", idx: 0 }); // memories skipped
  });

  it("returns null out of range", () => {
    expect(cmdkRowAt(9, C)).toBeNull();
    expect(cmdkRowAt(-1, C)).toBeNull();
  });

  it("round-trips base+idx ↔ cursor for every row", () => {
    for (let cursor = 0; cursor < cmdkTotalRows(C); cursor++) {
      const at = cmdkRowAt(cursor, C)!;
      expect(cmdkRowBase(at.kind, C) + at.idx).toBe(cursor);
    }
  });
});
