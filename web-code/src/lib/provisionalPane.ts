// V70-A6 — provisional panes, OFF BY DEFAULT (§P7).
//
// The design's ruling is narrow and deliberate: "Provisional panes are off by
// default (peek → pane promotion only, auto-pinned on the second
// interaction)". That is a direct read of the measured failure of VS Code's
// preview editors — the mechanism is right (a commitment gradient), the
// SIGNAL is too quiet (an italic tab title), and people lose a file they
// thought was open. So:
//
//   * NOTHING makes a pane provisional except promoting an inline peek into
//     one (`Enter` inside a peek). A tree click, a search hit, `Shift-Enter`
//     — none of them; those are deliberate opens and a deliberate open is
//     never provisional.
//   * The signal is LOUD: a coloured left edge plus a literal chip reading
//     "provisional — p to pin". Not italics.
//   * It auto-pins on the SECOND interaction. Reading is one interaction;
//     doing something in the pane is the second, and by then it is yours.
//   * A preference turns the whole mechanism off, in which case a promotion
//     produces an ordinary pinned pane.
//
// Pure and tiny, so the rule lives in one place instead of being re-derived
// by the reader.

/// Which pane is provisional right now, and how many interactions it has
/// seen. `pane: null` means none is — the default, and the state after a pin.
export interface ProvisionalPaneState {
  pane: 1 | 2 | null;
  interactions: number;
}

export const NO_PROVISIONAL: ProvisionalPaneState = { pane: null, interactions: 0 };

/// The number of interactions after which a provisional pane pins itself.
export const AUTO_PIN_AFTER = 2;

/// Mark `pane` provisional. `enabled: false` (the preference is off) returns
/// the pinned state — the caller still gets its pane, just without the
/// gradient.
export function markProvisional(pane: 1 | 2, enabled: boolean): ProvisionalPaneState {
  return enabled ? { pane, interactions: 0 } : NO_PROVISIONAL;
}

/// Record an interaction with `pane`. Auto-pins at `AUTO_PIN_AFTER`. An
/// interaction with a DIFFERENT pane leaves the state alone: reading
/// elsewhere is not a commitment to this one.
export function noteInteraction(state: ProvisionalPaneState, pane: 1 | 2): ProvisionalPaneState {
  if (state.pane === null || state.pane !== pane) return state;
  const interactions = state.interactions + 1;
  return interactions >= AUTO_PIN_AFTER ? NO_PROVISIONAL : { ...state, interactions };
}

/// Pin explicitly (`p`, or the chip's own button).
export function pin(state: ProvisionalPaneState, pane: 1 | 2): ProvisionalPaneState {
  return state.pane === pane ? NO_PROVISIONAL : state;
}

export function isProvisional(state: ProvisionalPaneState, pane: 1 | 2): boolean {
  return state.pane === pane;
}

/// The modifier chips a pane header may carry, in ONE row, capped at two
/// (§P7: "a pane carries at most two modifier chips in one row (frame ·
/// follow · trail)"). `frame` and `follow` are later units; the slot is
/// reserved here so the cap is enforced from the first chip rather than
/// discovered when the third one arrives.
export type PaneModifier = "frame" | "follow" | "trail" | "provisional";

export const PANE_MODIFIER_CAP = 2;

/// Order is fixed (not "whatever the caller passed") so a pane's chip row
/// never reshuffles as state changes — spatial stability is the whole reason
/// Patchworks beat Code Bubbles.
const MODIFIER_ORDER: readonly PaneModifier[] = ["provisional", "frame", "follow", "trail"];

export function paneModifiers(active: readonly PaneModifier[]): PaneModifier[] {
  return MODIFIER_ORDER.filter((m) => active.includes(m)).slice(0, PANE_MODIFIER_CAP);
}
