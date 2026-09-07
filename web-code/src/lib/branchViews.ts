// V75-M3 (D15) — the `~branches` view vocabulary and its URL grammar.
//
// Three rules, and each is why this is a MODULE rather than eight literals
// spread across the page:
//
//  1. **The vocabulary is CLOSED and mirrors the server's.** `facts::View`
//     is eight names in a fixed order; `BRANCH_VIEWS` is the same eight, in
//     the same order, and `branchViews.test.ts` walks it against the shared
//     facts golden so a ninth view on either side fails HERE rather than
//     rendering an empty tab.
//  2. **A view is a URL, not component state.** `?view=` is the whole
//     selection — root CLAUDE.md #35's one-URL-builder discipline applied to
//     this page, and the reason a reload, a Back and a shared link all show
//     the same rows. `parseBranchView` is TOTAL: an unknown `?view=` reads
//     as `all` CLIENT-side (the SERVER 400s a mistyped one, which is the
//     honest answer for an API; a page that blanked on a stale bookmark
//     would not be).
//  3. **Nothing here derives a COUNT or a MEMBERSHIP.** Both come off the
//     wire (`view_counts`, and each row's own `reasons[]`). A second,
//     client-side membership rule is exactly how a chip and a row-set start
//     disagreeing.

import type { BranchView } from "../api/types";
import { branchesUrl } from "./codeUrl";

/// The eight views, in `facts::View::ALL`'s own order.
export const BRANCH_VIEWS: readonly BranchView[] = [
  "current",
  "mine",
  "agent",
  "review",
  "active",
  "stale",
  "merged",
  "all",
] as const;

/// The default when `?view=` is absent — the server's own default too.
export const DEFAULT_BRANCH_VIEW: BranchView = "all";

/// Human labels. Deliberately NOT derived from the id (`fork-point` →
/// "Fork point" is a rule that works until it doesn't); one table, read by
/// the selector and by the `?` sheet alike.
export const BRANCH_VIEW_LABELS: Readonly<Record<BranchView, string>> = {
  current: "Current",
  mine: "Mine",
  agent: "Agent",
  review: "In review",
  active: "Active",
  stale: "Stale",
  merged: "Merged",
  all: "All",
};

/// One-line "what does this view mean". The AUTHORITATIVE text is the
/// response's own `rules.views`; these are the short form the selector
/// shows on hover, and the page renders the server's sentence beside them.
export const BRANCH_VIEW_HINTS: Readonly<Record<BranchView, string>> = {
  current: "checked out here or in a linked worktree",
  mine: "your git identity authored the tip",
  agent: "D18 provenance is exact or likely",
  review: "an open review names this branch",
  active: "not stale — see the stale rule",
  stale: "older than this repo's own 75th percentile, and no open review",
  merged: "a witness proved it (ancestry, or patch-id for a squash)",
  all: "every branch the one ref pass enumerated",
};

/// TOTAL — see rule 2. Anything unrecognised is `all`.
export function parseBranchView(raw: string | null | undefined): BranchView {
  if (!raw) return DEFAULT_BRANCH_VIEW;
  const found = BRANCH_VIEWS.find((v) => v === raw);
  return found ?? DEFAULT_BRANCH_VIEW;
}

/// Cycle by `delta` with wraparound — the `L`/`H` keys. Total for any
/// current value (an unrecognised one starts from `all`).
export function cycleBranchView(current: BranchView, delta: number): BranchView {
  const i = BRANCH_VIEWS.indexOf(current);
  const from = i === -1 ? BRANCH_VIEWS.indexOf(DEFAULT_BRANCH_VIEW) : i;
  const n = BRANCH_VIEWS.length;
  return BRANCH_VIEWS[(((from + delta) % n) + n) % n];
}

/// The two reading densities (D16's reading contract: "a comfortable/compact
/// density preference"). Browser-local by D16's own ruling — never a
/// daemon-side preference.
export type BranchDensity = "comfortable" | "compact";
export const BRANCH_DENSITIES: readonly BranchDensity[] = ["comfortable", "compact"] as const;
export const BRANCH_DENSITY_STORAGE_KEY = "kbc.branches.density";

export function parseBranchDensity(raw: string | null | undefined): BranchDensity {
  return raw === "compact" ? "compact" : "comfortable";
}

/// The ONE place a `~branches` query string is composed — root CLAUDE.md
/// #35's rule, this page's copy. Every atom is omitted when it is at its
/// default, so the bare page URL stays `?`-free and two equal selections
/// always produce the same string.
export interface BranchesUrlState {
  view?: BranchView;
  q?: string;
  prefix?: string;
  fav?: boolean;
  radar?: string;
  density?: BranchDensity;
}

export function branchesSearch(state: BranchesUrlState): string {
  const p = new URLSearchParams();
  if (state.view && state.view !== DEFAULT_BRANCH_VIEW) p.set("view", state.view);
  if (state.q) p.set("q", state.q);
  if (state.prefix) p.set("prefix", state.prefix);
  if (state.fav) p.set("fav", "1");
  if (state.radar) p.set("radar", state.radar);
  if (state.density && state.density !== "comfortable") p.set("density", state.density);
  const s = p.toString();
  return s ? `?${s}` : "";
}

/// The inverse. Total: every field falls back to its default.
export function parseBranchesSearch(search: string | URLSearchParams): Required<
  Omit<BranchesUrlState, "radar">
> & { radar: string | null } {
  const p = typeof search === "string" ? new URLSearchParams(search) : search;
  return {
    view: parseBranchView(p.get("view")),
    q: p.get("q") ?? "",
    prefix: p.get("prefix") ?? "",
    fav: p.get("fav") === "1",
    radar: p.get("radar"),
    density: parseBranchDensity(p.get("density")),
  };
}

/// The base ladder's four rungs, in the server's own order, for rendering a
/// class badge without hardcoding the vocabulary at the call site.
export const BASE_CLASS_LABELS: Readonly<Record<string, string>> = {
  upstream: "upstream",
  "fork-point": "fork point",
  "merge-base": "merge base",
  unknown: "unknown",
};

/// The ONE `~branches` URL builder — `codeUrl.ts`'s path plus this module's
/// query grammar. The dependency runs THIS way (composite → leaf) on
/// purpose: `codeUrl.ts` is import-free and stays that way, so its own
/// goldens can never be perturbed by a page module.
export function branchesPageUrl(repo: string, state: BranchesUrlState = {}): string {
  return `${branchesUrl(repo)}${branchesSearch(state)}`;
}
