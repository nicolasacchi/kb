// V70-A4 — the placement rule table: content kind → home.
//
// docs/research/kb-code-v7-evidence/research/panel-layout-system.md §3.2
// calls this "the core differentiator": "**no browser code tool has an
// answer to 'where will this open?' other than 'try it.'** Making it a
// printed table … is the thing that lets a human travel at speed without
// surprise."
//
// This file is that table, as DATA. Today exactly one rule is wired
// (`file-open`, consumed by the reader's own open path); every other row
// carries `shipped: false` and is here so the table is complete and
// honest from the first commit rather than growing silently. A future
// unit flips a row to `shipped: true` when its consumer lands — and the
// `placement.test.ts` census asserts the count, so flipping one is a
// deliberate, reviewed act.
//
// Deliberately NOT a config file yet. §P1 says the table is "user-
// editable and printable"; editing it belongs with the command registry
// (unit A4's `cmd/1`) and the CLI (`kb-code desk rules`), neither of
// which exists yet. Shipping the data shape first is what lets both land
// without re-deciding the vocabulary.

import type { RailTab } from "./deskState";

/// The closed vocabulary of things that can be OPENED. One name per kind
/// of content, matching the glossary in the design doc — a second name
/// for the same thing is exactly the drift the Glossary's CI lint
/// (§Glossary) exists to stop.
export type ContentKind =
  | "file-open"
  | "definition-single"
  | "definition-multi"
  | "usages"
  | "search-results"
  | "diagnostics"
  | "recipe-output"
  | "peek"
  | "blame"
  | "review-thread"
  | "annotation"
  | "symbol-info"
  | "companion"
  | "pr-diff"
  | "canvas";

/// Where a content kind lands. `float` is the transient overlay class
/// (peek, hover) — deliberately its own home rather than "no home", so
/// the table can say that a peek is NOT a pane and NOT a drawer set
/// until the human promotes it. `takeover` is the one sanctioned hard
/// stop (§3.2: "canvas | full-screen takeover | the one sanctioned hard
/// stop, explicitly modal").
export type PlacementHome =
  | { region: "main"; pane: "focused" | "other" }
  | { region: "rail"; tab: RailTab }
  | { region: "drawer"; set: string }
  | { region: "float" }
  | { region: "takeover" };

export interface PlacementRule {
  kind: ContentKind;
  home: PlacementHome;
  /// Why this home — the printable column. One line, human-facing.
  note: string;
  /// Whether a consumer actually reads this row TODAY. `false` is not a
  /// TODO marker, it is the honest statement that the rule is declared
  /// but nothing routes through it yet.
  shipped: boolean;
}

export const PLACEMENT_RULES: readonly PlacementRule[] = [
  {
    kind: "file-open",
    home: { region: "main", pane: "focused" },
    note: "replaces the focused pane unless it is pinned",
    shipped: true,
  },
  {
    kind: "definition-single",
    home: { region: "main", pane: "focused" },
    note: "one answer goes straight into the code, with a came-from chip",
    shipped: true,
  },
  {
    kind: "peek",
    home: { region: "float" },
    note: "transient over the code; Enter promotes it into main",
    shipped: true,
  },
  {
    kind: "definition-multi",
    home: { region: "drawer", set: "definitions" },
    note: "several answers are a result set, not a navigation",
    shipped: false,
  },
  {
    kind: "usages",
    home: { region: "drawer", set: "usages" },
    note: "grouped by trust class; outlives the popup",
    shipped: false,
  },
  {
    kind: "search-results",
    home: { region: "drawer", set: "search" },
    note: "the code stays on screen beside the hits",
    shipped: false,
  },
  {
    kind: "diagnostics",
    home: { region: "drawer", set: "diagnostics" },
    note: "a standing list, not a per-file card",
    shipped: false,
  },
  {
    kind: "recipe-output",
    home: { region: "drawer", set: "recipe" },
    note: "one tab per named run",
    shipped: false,
  },
  {
    kind: "blame",
    home: { region: "rail", tab: "history" },
    note: "never steals main",
    shipped: true,
  },
  {
    kind: "review-thread",
    home: { region: "rail", tab: "review" },
    note: "rail plus the inline gutter marker",
    shipped: true,
  },
  {
    kind: "annotation",
    home: { region: "rail", tab: "notes" },
    note: "compose beside the line, not over it",
    shipped: true,
  },
  {
    kind: "symbol-info",
    home: { region: "rail", tab: "understand" },
    note: "identity and framework facts for the caret's subject",
    shipped: true,
  },
  {
    kind: "companion",
    home: { region: "main", pane: "other" },
    note: "the counterpart file rides pane B",
    shipped: false,
  },
  {
    kind: "pr-diff",
    home: { region: "main", pane: "focused" },
    note: "a center mode, not a route",
    shipped: false,
  },
  {
    kind: "canvas",
    home: { region: "takeover" },
    note: "the one sanctioned hard stop, explicitly modal",
    shipped: false,
  },
];

const BY_KIND: ReadonlyMap<ContentKind, PlacementRule> = new Map(
  PLACEMENT_RULES.map((r) => [r.kind, r]),
);

export interface PlacementCtx {
  /// Panes locked against navigation (`DeskState.panes.pinned`).
  pinned: readonly (1 | 2)[];
  focused: 1 | 2;
  /// How many panes the reader is actually rendering RIGHT NOW — derived
  /// from the URL, never from `DeskState.panes.count` (kb-code's URL-
  /// purity ruling). With one pane there is no "other" to fall back to.
  paneCount: 1 | 2;
}

/// Resolve a content kind to its home, applying the one contextual rule
/// the table itself cannot express: a `main.focused` target whose focused
/// pane is PINNED goes to the other pane instead — and, when there is no
/// other pane, the pin loses (opening into the only pane there is beats
/// refusing to open at all, and the pin chip stays visible so the human
/// can see why nothing moved).
export function placementFor(kind: ContentKind, ctx?: PlacementCtx): PlacementHome {
  const rule = BY_KIND.get(kind);
  // An unknown kind is a programming error, not a user-facing state: the
  // union is closed and exhaustively covered by `PLACEMENT_RULES` (the
  // census test proves it), so this branch is unreachable at runtime and
  // exists only so the function is total.
  if (!rule) return { region: "main", pane: "focused" };
  if (!ctx) return rule.home;
  if (rule.home.region !== "main") return rule.home;
  if (rule.home.pane !== "focused") return rule.home;
  if (!ctx.pinned.includes(ctx.focused)) return rule.home;
  if (ctx.paneCount === 1) return rule.home;
  return { region: "main", pane: "other" };
}

/// The printable form — one line per rule, stable order. Backs a future
/// `kb-code desk rules` and the Settings table; kept here so the CLI and
/// the SPA can never print two different tables.
export function placementTableLines(): string[] {
  return PLACEMENT_RULES.map((r) => {
    const home =
      r.home.region === "main"
        ? `main.${r.home.pane}`
        : r.home.region === "rail"
          ? `rail:${r.home.tab}`
          : r.home.region === "drawer"
            ? `drawer:${r.home.set}`
            : r.home.region;
    return `${r.kind} → ${home}${r.shipped ? "" : " (declared, not yet routed)"} — ${r.note}`;
  });
}
