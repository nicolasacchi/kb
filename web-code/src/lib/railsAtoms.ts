// V72-I2 — the Rails ATOM table behind the reader's hover card.
//
// A "Rails atom" is a token in the buffer that `rails-lens/1` already minted
// a convention edge FROM: an association macro, a `render`, a `t("…")`, a
// `perform_later`, an `include SomeConcern`. This module is the whole pure
// half of the card: given the file's edges (`GET /api/framework/edges`, one
// request the reader already makes for `FrameworkCard`) and a LINE, it
// answers "what did the lens see here, where does it point, and how much
// does it claim?".
//
// THREE THINGS IT DOES NOT DO, each on purpose:
//
//  * It does not RESOLVE anything. Every target below is an edge the daemon
//    wrote; this module picks the edges on one line and labels their kinds.
//    Picking rows by their own `src_line` is addressing, not re-deriving.
//  * It never AUTO-NAVIGATES. `rails-lens/1` has no `exact` tier by
//    construction, and D5's rule is that candidate/likely never auto-jump —
//    so an atom's target is an offered LINK, and `neverAutoNavigates` is the
//    assertion that keeps it one.
//  * It does not invent a ROUTE-HELPER edge. `frameworks::EdgeKind` has no
//    route-helper variant at all (checked against the closed grammar), so
//    `orders_path` under the cursor produces a SEARCH atom — a `route:`
//    query the reader can run — captioned as such, never a resolved target
//    the lens did not mint.
import type { FrameworkEdgeOut } from "../api/types";
import { codeUrl } from "./codeUrl";
import { trustClassOf, trustTierOf, type RailsTrustTier } from "./railsCards";

/// Edge kind → what a reader calls it. An unknown kind renders as ITSELF
/// (the tree's "render the note verbatim" rule): a daemon ahead of this
/// build must not produce a blank row.
const KIND_LABEL: Readonly<Record<string, string>> = {
  association: "association",
  render_partial: "renders partial",
  render_view: "renders view",
  view_component_render: "renders component",
  turbo_stream_target: "turbo stream target",
  stimulus_binding: "stimulus controller",
  i18n_key: "translation key",
  job_enqueue: "enqueues job",
  mailer_deliver: "delivers mail",
  concern_include: "includes concern",
  callback: "callback",
  validation: "validation",
  scope: "scope",
  delegate: "delegate",
  route_action: "route → action",
  route_file: "routes file",
  helper_for: "helper",
  spec_subject: "spec subject",
  devise_override: "devise override",
};

export function atomKindLabel(kind: string): string {
  return KIND_LABEL[kind] ?? kind;
}

export interface RailsAtomTarget {
  /// What the target is CALLED — the edge's `dst_symbol` when it has one
  /// (a locale key, a `controller#action`, a method), else its `dst_path`.
  label: string;
  path: string | null;
  symbol: string | null;
  /// The daemon's `dst_kind` (`dom_id`, `template`, …) or null.
  dstKind: string | null;
  trust: string;
  tier: RailsTrustTier;
  trustClass: string;
  /// The ONE address this target opens, or `null` when the edge resolved to
  /// a symbol with no file (a `scope`, a `dom_id`) — absent, never a guess.
  href: string | null;
}

export type RailsAtomSource = "lens" | "search";

export interface RailsAtom {
  /// A stable id for keyed rendering and for the "open the hovered atom"
  /// command: kind + line + the first target's label.
  id: string;
  kind: string;
  label: string;
  line: number;
  source: RailsAtomSource;
  targets: RailsAtomTarget[];
  /// Why this atom cannot say more than it does. Rendered VERBATIM.
  note?: string;
  /// For a `search` atom: the `kbcq/1` clause to run.
  clause?: string;
}

function targetOf(repo: string, e: FrameworkEdgeOut): RailsAtomTarget {
  const label = e.dst_symbol ?? e.dst_path ?? "(unresolved)";
  const href = e.dst_path ? codeUrl({ repo, path: e.dst_path }) : null;
  return {
    label,
    path: e.dst_path,
    symbol: e.dst_symbol,
    dstKind: e.dst_kind,
    trust: e.trust,
    tier: trustTierOf(e.trust),
    trustClass: trustClassOf(e.trust),
    href,
  };
}

/// The atoms on ONE line, grouped by edge kind in the daemon's own edge
/// order. Only edges this file PRODUCED (`direction: "src"`) count — an
/// inbound edge is a fact about some other file's line, and putting it under
/// this cursor would be a lie about where the reader is.
export function atomsForLine(
  repo: string,
  edges: readonly FrameworkEdgeOut[] | undefined,
  line: number,
): RailsAtom[] {
  if (!edges || line <= 0) return [];
  const byKind = new Map<string, RailsAtom>();
  for (const e of edges) {
    if (e.direction !== "src") continue;
    if (e.src_line !== line) continue;
    const target = targetOf(repo, e);
    const existing = byKind.get(e.kind);
    if (existing) {
      existing.targets.push(target);
      continue;
    }
    byKind.set(e.kind, {
      id: `${e.kind}@${line}`,
      kind: e.kind,
      label: atomKindLabel(e.kind),
      line,
      source: "lens",
      targets: [target],
    });
  }
  return [...byKind.values()];
}

/// A Rails route helper (`orders_path`, `edit_order_url`). `rails-lens/1`
/// mints NO edge for one, so this is a search, and the atom says so.
const ROUTE_HELPER_RE = /^[a-z_][A-Za-z0-9_]*_(?:path|url)$/;

export function isRouteHelper(word: string): boolean {
  return ROUTE_HELPER_RE.test(word);
}

/// The SEARCH atom for a route helper under the cursor, or `null`. Its
/// `clause` is a `kbcq/1` `route:` atom over the helper's stem — the same
/// query the `~rails` route cards' own chips build, so one grammar answers
/// both surfaces.
export function routeHelperAtom(word: string, line: number): RailsAtom | null {
  if (!isRouteHelper(word)) return null;
  const stem = word.replace(/_(?:path|url)$/, "");
  return {
    id: `route-helper@${line}`,
    kind: "route_helper",
    label: "route helper",
    line,
    source: "search",
    targets: [],
    clause: `route:${stem}`,
    note:
      "rails-lens/1 mints no route-helper edge — its EdgeKind vocabulary has no such variant — " +
      "so this is a search over the route index, not a resolved target",
  };
}

/// The address `GET /api/actions` should be asked about for an atom: the
/// first target that has a FILE. `null` when the atom resolved only to
/// symbols (a `scope`, a `dom_id`) or is a search — the card then shows no
/// action rows and says why, rather than asking about the wrong file.
export function actionsTargetOf(atom: RailsAtom): { path: string; line: number } | null {
  for (const t of atom.targets) {
    if (t.path) return { path: t.path, line: 1 };
  }
  return null;
}

/// D5, as a predicate rather than a habit: NOTHING on this card may
/// auto-navigate, because `rails-lens/1` cannot mint `exact`. Kept as a
/// function (not a comment) so `railsAtoms.test.ts` can assert it over every
/// tier the wire can send.
export function neverAutoNavigates(_atom: RailsAtom): false {
  return false;
}
