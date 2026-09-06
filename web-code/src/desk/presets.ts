// V70-A4 — the four named desks.
//
// docs/research/kb-code-v7-continuum-2026-09.html §P1: "Presets Read /
// Review / Explore / Present plus named desks; a human drag marks the
// desk dirty and a preset never overrides it silently."
//
// A preset is a whole `DeskState`, not a patch: reading the table below
// tells you the ENTIRE geometry it produces, with nothing inherited from
// whatever the desk happened to be. That is also what makes `reset`
// (and double-click-a-separator, which the library resolves against each
// Panel's `defaultSize`) exactly reproducible.
//
// Sizes are percentages of the resizable group's own axis. The group is
// the viewport minus the two 40px stripes, so at the 1280×720 reference
// viewport the Read preset's `dock: 18` / `rail: 18` leave main ~64% ≈
// 768px — comfortably over the viewport golden's 80-column floor at the
// default 13px reader font (`e2e/desk-viewport.spec.ts` measures the real
// thing from CM6's own char width rather than trusting this arithmetic).

import type { DeskState, DeskPanesState, DeskRegionState, RailTab } from "./deskState";

export type DeskPresetName = "read" | "review" | "explore" | "present";

export const DEFAULT_PRESET: DeskPresetName = "read";

export interface DeskPresetDef {
  name: DeskPresetName;
  label: string;
  /// One line, shown in the preset chip's menu. Says what the preset is
  /// FOR, never what it does mechanically.
  hint: string;
  /// `true` for presets kb-code may apply on its own (never over a dirty
  /// desk). Present is opt-in ONLY — it hides the stripes and enlarges
  /// the type, which is a mode you ask for, never one you are given.
  autoApplicable: boolean;
  regions: Record<"dock" | "rail" | "drawer", DeskRegionState>;
  panes: Pick<DeskPanesState, "count" | "split">;
  railTab: RailTab;
  /// Present's larger reading type — a multiplier over the operator's own
  /// `readerFontSize` pref, never a replacement for it (the pref is still
  /// what the A−/A+ stepper writes; Present just reads it bigger).
  fontScale: number;
}

export const DESK_PRESETS: Record<DeskPresetName, DeskPresetDef> = {
  read: {
    name: "read",
    label: "Read",
    hint: "one wide pane · rail on All · drawer away",
    autoApplicable: true,
    regions: {
      dock: { size: 18, collapsed: false },
      rail: { size: 18, collapsed: false },
      drawer: { size: 26, collapsed: true },
    },
    panes: { count: 1, split: 50 },
    railTab: "all",
    fontScale: 1,
  },
  review: {
    name: "review",
    label: "Review",
    hint: "rail on Review · drawer open for findings",
    autoApplicable: true,
    regions: {
      dock: { size: 18, collapsed: false },
      rail: { size: 22, collapsed: false },
      drawer: { size: 30, collapsed: false },
    },
    panes: { count: 1, split: 50 },
    railTab: "review",
    fontScale: 1,
  },
  explore: {
    name: "explore",
    label: "Explore",
    hint: "two panes · rail on Understand",
    autoApplicable: true,
    regions: {
      dock: { size: 16, collapsed: false },
      rail: { size: 20, collapsed: false },
      drawer: { size: 26, collapsed: true },
    },
    panes: { count: 2, split: 50 },
    railTab: "understand",
    fontScale: 1,
  },
  present: {
    name: "present",
    label: "Present",
    hint: "everything to the stripes · larger type",
    autoApplicable: false,
    regions: {
      dock: { size: 18, collapsed: true },
      rail: { size: 18, collapsed: true },
      drawer: { size: 26, collapsed: true },
    },
    panes: { count: 1, split: 50 },
    railTab: "all",
    fontScale: 1.25,
  },
};

export const DESK_PRESET_NAMES: readonly DeskPresetName[] = ["read", "review", "explore", "present"];

/// The preset as a complete, freshly-allocated `DeskState`. Always a new
/// object graph — a caller mutating what it gets back can never poison
/// the table above.
export function presetState(name: DeskPresetName): DeskState {
  const p = DESK_PRESETS[name];
  return {
    v: 1,
    preset: name,
    regions: {
      dock: { ...p.regions.dock },
      rail: { ...p.regions.rail },
      drawer: { ...p.regions.drawer },
    },
    panes: { count: p.panes.count, split: p.panes.split, focused: 1, pinned: [] },
    railTab: p.railTab,
    dirty: false,
  };
}

export function presetFontScale(name: DeskPresetName): number {
  return DESK_PRESETS[name].fontScale;
}
