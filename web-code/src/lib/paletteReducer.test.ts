import { describe, expect, it } from "vitest";
import {
  HEADER_ROW,
  initialPaletteState,
  paletteReducer,
  type PaletteSection,
  type PaletteState,
} from "./paletteReducer";

const SECTIONS: PaletteSection[] = [
  { lane: "files", rowCount: 3 },
  { lane: "symbols", rowCount: 0 },
  { lane: "text", rowCount: 2 },
];

function stateWith(cursor: PaletteState["cursor"]): PaletteState {
  return { sections: SECTIONS, cursor };
}

describe("initialPaletteState", () => {
  it("starts empty, cursor on section 0's header", () => {
    expect(initialPaletteState()).toEqual({ sections: [], cursor: { section: 0, row: HEADER_ROW } });
  });
});

describe("SET_SECTIONS", () => {
  it("clamps the row cursor when the new section list is smaller", () => {
    const state = stateWith({ section: 0, row: 2 });
    const next = paletteReducer(state, {
      type: "SET_SECTIONS",
      sections: [{ lane: "files", rowCount: 1 }],
    });
    expect(next.cursor).toEqual({ section: 0, row: 0 });
  });

  it("clamps the section index when the new section list is shorter", () => {
    const state = stateWith({ section: 2, row: 0 });
    const next = paletteReducer(state, {
      type: "SET_SECTIONS",
      sections: [{ lane: "files", rowCount: 5 }],
    });
    expect(next.cursor.section).toBe(0);
  });

  it("preserves the cursor's row when the new section list still fits it", () => {
    const state = stateWith({ section: 0, row: 1 });
    const next = paletteReducer(state, { type: "SET_SECTIONS", sections: SECTIONS });
    expect(next.cursor).toEqual({ section: 0, row: 1 });
  });
});

describe("MOVE_ROW", () => {
  it("moves down within a section's rows", () => {
    const next = paletteReducer(stateWith({ section: 0, row: HEADER_ROW }), {
      type: "MOVE_ROW",
      delta: 1,
    });
    expect(next.cursor).toEqual({ section: 0, row: 0 });
  });

  it("clamps at the section's last row (never overflows into the next section)", () => {
    const next = paletteReducer(stateWith({ section: 0, row: 2 }), { type: "MOVE_ROW", delta: 1 });
    expect(next.cursor).toEqual({ section: 0, row: 2 });
  });

  it("clamps at HEADER_ROW moving up from row 0 (never goes negative past the header)", () => {
    const next = paletteReducer(stateWith({ section: 0, row: 0 }), { type: "MOVE_ROW", delta: -1 });
    expect(next.cursor).toEqual({ section: 0, row: HEADER_ROW });
  });

  it("stays at HEADER_ROW for a zero-row section regardless of delta", () => {
    const next = paletteReducer(stateWith({ section: 1, row: HEADER_ROW }), {
      type: "MOVE_ROW",
      delta: 1,
    });
    expect(next.cursor).toEqual({ section: 1, row: HEADER_ROW });
  });

  it("is a no-op when there are no sections at all", () => {
    const empty = initialPaletteState();
    expect(paletteReducer(empty, { type: "MOVE_ROW", delta: 1 })).toBe(empty);
  });
});

describe("MOVE_SECTION", () => {
  it("moves forward and resets the row to HEADER_ROW", () => {
    const next = paletteReducer(stateWith({ section: 0, row: 2 }), {
      type: "MOVE_SECTION",
      delta: 1,
    });
    expect(next.cursor).toEqual({ section: 1, row: HEADER_ROW });
  });

  it("wraps forward past the last section back to the first", () => {
    const next = paletteReducer(stateWith({ section: 2, row: HEADER_ROW }), {
      type: "MOVE_SECTION",
      delta: 1,
    });
    expect(next.cursor.section).toBe(0);
  });

  it("wraps backward past the first section to the last (Shift+Tab)", () => {
    const next = paletteReducer(stateWith({ section: 0, row: HEADER_ROW }), {
      type: "MOVE_SECTION",
      delta: -1,
    });
    expect(next.cursor.section).toBe(2);
  });

  it("is a no-op when there are no sections at all", () => {
    const empty = initialPaletteState();
    expect(paletteReducer(empty, { type: "MOVE_SECTION", delta: 1 })).toBe(empty);
  });
});

describe("SET_CURSOR", () => {
  it("clamps an out-of-range explicit cursor (e.g. from a stale mouse hover)", () => {
    const next = paletteReducer(stateWith({ section: 0, row: HEADER_ROW }), {
      type: "SET_CURSOR",
      cursor: { section: 0, row: 99 },
    });
    expect(next.cursor).toEqual({ section: 0, row: 2 });
  });

  it("accepts a valid explicit cursor verbatim (mouse hover onto a specific row)", () => {
    const next = paletteReducer(stateWith({ section: 0, row: HEADER_ROW }), {
      type: "SET_CURSOR",
      cursor: { section: 2, row: 1 },
    });
    expect(next.cursor).toEqual({ section: 2, row: 1 });
  });
});

describe("RESET", () => {
  it("returns the cursor to section 0's header without touching sections", () => {
    const next = paletteReducer(stateWith({ section: 2, row: 1 }), { type: "RESET" });
    expect(next.cursor).toEqual({ section: 0, row: HEADER_ROW });
    expect(next.sections).toBe(SECTIONS);
  });
});
