import { useCallback, useState } from "react";
import { RAIL_TABS, type RailTab } from "../desk/deskState";

// F3b — which sub-panel the reader's right-rail inspector is showing,
// persisted per surface (localStorage), mirroring kb's own
// `web/src/hooks/useInspectorTab.ts` (root CLAUDE.md invariant #30): the
// default is the LAST tab the operator chose, so re-opening the reader
// restores where they left off instead of resetting to a fixed tab.
//
// kb-code has exactly one inspector surface today (the reader's rail) —
// unlike kb's own multi-surface version, this doesn't need the
// storage/CustomEvent cross-instance sync (only one `InspectorRail` is ever
// mounted at a time), so it's trimmed to read-once-on-mount + write-through.
//
// V70-A4 — the six SOURCE tabs (outline · provenance · history ·
// annotations · bookmarks · entity) became the design's four TASK tabs
// plus a conditional fifth (docs/research/
// kb-code-v7-continuum-2026-09.html §P1: "tab one is All (everything
// stacked), then Understand …, History …, Review …, Notes"). The
// storage KEY is deliberately unchanged, so an upgrade reads the
// operator's last six-tab choice and lands on the four-tab equivalent
// rather than silently resetting everyone to tab one — `LEGACY_TAB_MAP`
// is that translation, and it is also what lets the imperative
// `openTab("provenance")` call sites (the blame gutter, `a`) and the
// `?shell=legacy` reader keep speaking the old vocabulary.

export type InspectorTab = RailTab;

export const INSPECTOR_TABS: readonly InspectorTab[] = RAIL_TABS;

/// The pre-V70-A4 tab ids. Kept as a type (not just data) so a caller
/// that still speaks the old vocabulary is checked, not coerced.
export type LegacyInspectorTab =
  | "outline"
  | "provenance"
  | "history"
  | "annotations"
  | "bookmarks"
  | "entity";

/// Old id → the tab that now CONTAINS that content. `history` is the one
/// name that survives with the same meaning; `provenance` folds into it
/// (blame and why are both "how did it get here?"), outline and entity
/// into Understand, annotations and bookmarks into Notes.
export const LEGACY_TAB_MAP: Record<LegacyInspectorTab, InspectorTab> = {
  outline: "understand",
  entity: "understand",
  provenance: "history",
  history: "history",
  annotations: "notes",
  bookmarks: "notes",
};

/// Total: a current id passes through, a legacy id translates, anything
/// else lands on the default. Never throws — a persisted value is
/// untrusted input.
export function normalizeInspectorTab(v: string | null | undefined): InspectorTab {
  if (v && (INSPECTOR_TABS as readonly string[]).includes(v)) return v as InspectorTab;
  if (v && v in LEGACY_TAB_MAP) return LEGACY_TAB_MAP[v as LegacyInspectorTab];
  return DEFAULT_TAB;
}

/// V70-A4: tab one is All — "everything stacked" is the honest default
/// for an operator who has not chosen, because it hides nothing.
const DEFAULT_TAB: InspectorTab = "all";

function storageKey(surface: string): string {
  return `kbc:inspector-tab.${surface}`;
}

function read(surface: string): InspectorTab {
  if (typeof localStorage === "undefined") return DEFAULT_TAB;
  try {
    return normalizeInspectorTab(localStorage.getItem(storageKey(surface)));
  } catch {
    return DEFAULT_TAB;
  }
}

function write(surface: string, value: InspectorTab): void {
  try {
    localStorage.setItem(storageKey(surface), value);
  } catch {
    // localStorage denied — state still updates this session; a reload loses it.
  }
}

export function useInspectorTab(surface: string): {
  tab: InspectorTab;
  setTab: (next: InspectorTab) => void;
} {
  const [tab, setLocal] = useState<InspectorTab>(() => read(surface));

  const setTab = useCallback(
    (next: InspectorTab) => {
      setLocal(next);
      write(surface, next);
    },
    [surface],
  );

  return { tab, setTab };
}
