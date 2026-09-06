// The blame gutter's disclosure ladder state machine (W4.4): dot → hover
// chip → click-to-open panel. Pure reducer, no CM6/DOM — `Reader.tsx` wires
// `editor/lineGutter.ts`'s hover/click callbacks straight onto `dispatch`.

export interface LadderState {
  /// The line currently under the pointer (drives the floating hover
  /// chip), or `null` when nothing is hovered.
  hoverLine: number | null;
  /// The line whose why-panel is open in the right rail, or `null` when
  /// the rail is showing something else (outline/annotations).
  openLine: number | null;
}

export type LadderAction =
  | { type: "hover"; line: number }
  | { type: "unhover"; line: number }
  | { type: "click"; line: number }
  | { type: "closePanel" };

export const initialLadderState: LadderState = { hoverLine: null, openLine: null };

/// A click OPENS the panel for that line and clears any hover chip (the
/// panel already shows everything the chip previewed, so a lingering chip
/// would be redundant clutter). `unhover` only clears the hover state if it
/// still names the SAME line — a fast pointer move that already fired a new
/// `hover` for a different line must not let a late, stale `unhover` for the
/// old line clobber it (mirrors the same "don't let a stale async event
/// undo a newer one" discipline the rest of this codebase uses for network
/// races).
export function ladderReducer(state: LadderState, action: LadderAction): LadderState {
  switch (action.type) {
    case "hover":
      return state.hoverLine === action.line ? state : { ...state, hoverLine: action.line };
    case "unhover":
      return state.hoverLine === action.line ? { ...state, hoverLine: null } : state;
    case "click":
      return state.hoverLine === null && state.openLine === action.line
        ? state
        : { hoverLine: null, openLine: action.line };
    case "closePanel":
      return state.openLine === null ? state : { ...state, openLine: null };
    default:
      return state;
  }
}
