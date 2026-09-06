// The omnibox/search-page keyboard model (W4.3): up/down move a row cursor
// WITHIN the current section, Tab/Shift-Tab move to the next/previous
// section (resetting the row cursor to that section's own HEADER row) -
// see `components/Omnibox.tsx`'s doc for the full interaction contract.
// Pure reducer (`paletteReducer.test.ts` pins the arithmetic without
// mounting a component), driven by `useReducer` in both `Omnibox.tsx` and
// `routes/Search.tsx` so the two surfaces can never disagree on what
// Tab/arrow keys do.

export interface PaletteSection {
  lane: string;
  rowCount: number;
}

/// `row === HEADER_ROW` means the section's own HEADER is focused (Enter
/// there jumps to the full `/search?q=` page - see `lib/searchTargets.ts`);
/// `row` `0..rowCount-1` focuses that data row.
export const HEADER_ROW = -1;

export interface PaletteCursor {
  section: number;
  row: number;
}

export interface PaletteState {
  sections: PaletteSection[];
  cursor: PaletteCursor;
}

export type PaletteAction =
  | { type: "SET_SECTIONS"; sections: PaletteSection[] }
  | { type: "MOVE_ROW"; delta: number }
  | { type: "MOVE_SECTION"; delta: number }
  | { type: "SET_CURSOR"; cursor: PaletteCursor }
  | { type: "RESET" };

export function initialPaletteState(): PaletteState {
  return { sections: [], cursor: { section: 0, row: HEADER_ROW } };
}

/// Clamp a cursor against a (possibly just-changed) section list - section
/// index into range, row index into `[HEADER_ROW, rowCount - 1]` for
/// whichever section that resolves to. An empty section list always
/// resolves to `{section: 0, row: HEADER_ROW}`.
function clamp(cursor: PaletteCursor, sections: PaletteSection[]): PaletteCursor {
  if (sections.length === 0) return { section: 0, row: HEADER_ROW };
  const section = Math.min(Math.max(cursor.section, 0), sections.length - 1);
  const rowCount = sections[section].rowCount;
  const row = Math.min(Math.max(cursor.row, HEADER_ROW), rowCount - 1);
  return { section, row };
}

export function paletteReducer(state: PaletteState, action: PaletteAction): PaletteState {
  switch (action.type) {
    case "SET_SECTIONS": {
      // A fresh result set re-clamps the EXISTING cursor rather than
      // resetting to the top - typing a character that narrows (not
      // reorders) the current section's hits shouldn't visibly yank focus
      // back to row 0/section 0 every keystroke.
      return { sections: action.sections, cursor: clamp(state.cursor, action.sections) };
    }
    case "MOVE_ROW": {
      if (state.sections.length === 0) return state;
      const cursor = clamp(state.cursor, state.sections);
      const rowCount = state.sections[cursor.section].rowCount;
      const row = Math.min(Math.max(cursor.row + action.delta, HEADER_ROW), rowCount - 1);
      return { ...state, cursor: { section: cursor.section, row } };
    }
    case "MOVE_SECTION": {
      if (state.sections.length === 0) return state;
      const cursor = clamp(state.cursor, state.sections);
      const n = state.sections.length;
      const section = ((cursor.section + action.delta) % n + n) % n;
      return { ...state, cursor: { section, row: HEADER_ROW } };
    }
    case "SET_CURSOR": {
      if (state.sections.length === 0) return state;
      return { ...state, cursor: clamp(action.cursor, state.sections) };
    }
    case "RESET":
      return { ...state, cursor: { section: 0, row: HEADER_ROW } };
    default:
      return state;
  }
}
