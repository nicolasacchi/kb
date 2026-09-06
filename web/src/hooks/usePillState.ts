import { useState } from "react";

// Persisted layout state for the artifact-page command bar
// (`FloatingPill`): which bottom corner it sits in, and whether it's
// collapsed to a single icon. localStorage-backed under its own key —
// a per-device UI toggle, deliberately separate from the theme/accent
// prefs blob in api/prefs.ts.

type Side = "left" | "right";
type PillState = { side: Side; collapsed: boolean };

const KEY = "kb:pill";
const DEFAULT: PillState = { side: "left", collapsed: false };

function load(): PillState {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) return { ...DEFAULT, ...(JSON.parse(raw) as Partial<PillState>) };
  } catch {
    // fall through to default
  }
  return DEFAULT;
}

export function usePillState() {
  const [state, setState] = useState<PillState>(load);

  function update(next: PillState) {
    setState(next);
    try {
      localStorage.setItem(KEY, JSON.stringify(next));
    } catch {
      // best-effort — the in-memory state still updates this session
    }
  }

  return {
    ...state,
    toggleSide: () =>
      update({ ...state, side: state.side === "left" ? "right" : "left" }),
    toggleCollapsed: () => update({ ...state, collapsed: !state.collapsed }),
  };
}
