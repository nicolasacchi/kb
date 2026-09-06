// V71-E2 — the Usages dock's PURE half: census, chips, grouping.
//
// docs/research/kb-code-v7-evidence/research/usages-browsing.md §4.1–4.2:
// "census → filters → grouped tree → preview", and the census is "the answer
// before the list" — for a Rails monolith the modal reference query returns
// hundreds of rows and the human's first question is not "show me row 1", it
// is "what am I dealing with".
//
// THE ONE RULE THIS MODULE EXISTS TO KEEP — the E2 brief's own words: *a
// count never changes without an on-screen reason.* Three things follow, and
// each is pinned by a test below:
//
//  1. **Server totals are never re-derived.** `Usages2Out.totals` and
//     `kind_totals` are the TRUE totals BEFORE the per-class cap;
//     `capped[]` names any class that hid rows. `censusOf` reports those
//     numbers verbatim. The one number this module computes itself is how
//     many of the RETURNED rows the chips currently hide — and that number
//     is always rendered beside the chip that hid them.
//  2. **Chips filter the PAGE, not the query.** `/api/usages/2` takes no
//     filter params (V71-E1 cut them deliberately: "a param the server
//     accepts and no caller sends is the same dead surface as a key with no
//     handler"). So a chip narrows what is on screen, `censusOf` says
//     `shown of total`, and when the page itself was capped the strip says
//     THAT too — a filtered view of a capped page is honest only if both
//     facts are visible.
//  3. **The mentions (grep) lane is a separate count, never merged.**
//     `/api/xrefs` answers a different question — a word-boundary regex over
//     the working tree — and the recon's §6.3 finding is that kb-code
//     already shipped three disagreeing "usages" numbers. The mentions count
//     therefore lives in its own field, is rendered as its own chip, and is
//     never added into `totals`.
//
// Pure: no React, no fetch, no clock. Everything here is a function of the
// wire body plus the chip state.

import type { UsageRow2, Usages2Out, UsagesCapped } from "../api/types";

/// The trust classes, in the ONE order every kb-code surface renders them:
/// exact ▷ likely ▷ candidate (Sourcegraph's "precise before fuzzy", mapped
/// onto kb-code's vocabulary). `observed` has no producer on this wire yet
/// and is deliberately absent rather than declared-and-empty.
export const TRUST_ORDER = ["exact", "likely", "candidate"] as const;
export type UsageTrust = (typeof TRUST_ORDER)[number];

/// The role bits a chip can exclude. Mirrors `usages2::roles` — PATH
/// heuristics, never claims about content, and never inputs to a trust
/// class (that module's own doc).
export const ROLE_TEST = 0x20;
export const ROLE_GENERATED = 0x10;
export const ROLE_VENDOR = 0x80;

export type ExcludeRole = "tests" | "vendor" | "generated";

export const EXCLUDE_BITS: Readonly<Record<ExcludeRole, number>> = {
  tests: ROLE_TEST,
  vendor: ROLE_VENDOR,
  generated: ROLE_GENERATED,
};

/// The chip state. Every field is ADDITIVE narrowing: an empty set means "no
/// chip of this kind is on", which is the unfiltered view.
export interface UsageChips {
  /// Kind names (`UsageKind`) to keep. Empty = every kind.
  kinds: readonly string[];
  /// Trust classes to keep. Empty = every class.
  trust: readonly UsageTrust[];
  /// Role bits to drop.
  exclude: readonly ExcludeRole[];
  /// A path prefix (a `[scopes]`-style narrowing done client-side over the
  /// returned page). Empty = the whole page.
  scope: string;
}

export const NO_CHIPS: UsageChips = { kinds: [], trust: [], exclude: [], scope: "" };

export function chipsAreEmpty(c: UsageChips): boolean {
  return c.kinds.length === 0 && c.trust.length === 0 && c.exclude.length === 0 && !c.scope;
}

/// Every returned row, in trust order, each tagged with the class it came
/// from. `UsageRow2.trust` already carries it per row (V71-E1 put it there
/// so a flattened list stays honest), but the ARRAY it arrived in is the
/// authority for which section it belongs to.
export function flattenRows(out: Usages2Out): UsageRow2[] {
  return [...out.exact, ...out.likely, ...out.candidate];
}

export function rowMatchesChips(row: UsageRow2, c: UsageChips): boolean {
  if (c.kinds.length > 0 && !c.kinds.includes(row.kind)) return false;
  if (c.trust.length > 0 && !(c.trust as readonly string[]).includes(row.trust)) return false;
  for (const e of c.exclude) {
    if ((row.roles & EXCLUDE_BITS[e]) !== 0) return false;
  }
  if (c.scope && !row.path.startsWith(c.scope)) return false;
  return true;
}

export function applyChips(rows: readonly UsageRow2[], c: UsageChips): UsageRow2[] {
  return chipsAreEmpty(c) ? [...rows] : rows.filter((r) => rowMatchesChips(r, c));
}

/// One "why is this number what it is" line. Every element of the census
/// strip is one of these, so the strip cannot grow a number with no reason
/// attached — the component renders the list, it never invents a figure.
export interface CensusReason {
  /// A stable id so a test (and a screenshot) can name the row.
  id: string;
  text: string;
  /// `true` when this line is reporting something HIDDEN (a cap, a chip) —
  /// the component renders those with the warning treatment.
  hiding: boolean;
}

export interface UsagesCensus {
  /// The server's true total across all three classes.
  total: number;
  /// Per class, the server's true totals.
  byTrust: { trust: UsageTrust; total: number }[];
  /// Per kind, the server's true totals — descending, then by name so the
  /// order is stable for equal counts.
  byKind: { kind: string; total: number }[];
  /// How many rows the server actually RETURNED (the page).
  returned: number;
  /// How many of those the chips currently show.
  shown: number;
  /// Classes the server capped, verbatim.
  capped: UsagesCapped[];
  /// The grep lane's own count, when it was fetched. `null` = not asked.
  /// NEVER folded into `total` (recon §6.3 — three disagreeing numbers).
  mentions: number | null;
  reasons: CensusReason[];
}

export function censusOf(
  out: Usages2Out,
  chips: UsageChips,
  mentions: number | null = null,
): UsagesCensus {
  const rows = flattenRows(out);
  const shown = applyChips(rows, chips).length;
  const byTrust = TRUST_ORDER.map((t) => ({ trust: t, total: out.totals[t] ?? 0 })).filter(
    (b) => b.total > 0,
  );
  const byKind = Object.entries(out.kind_totals ?? {})
    .map(([kind, total]) => ({ kind, total }))
    .filter((k) => k.total > 0)
    .sort((a, b) => b.total - a.total || a.kind.localeCompare(b.kind));

  const reasons: CensusReason[] = [];
  for (const cap of out.capped ?? []) {
    reasons.push({
      id: `capped:${cap.group}`,
      text: `${cap.group}: showing ${cap.returned} of ${cap.total} — capped by the ${cap.reason} limit`,
      hiding: true,
    });
  }
  const hiddenByChips = rows.length - shown;
  if (hiddenByChips > 0) {
    reasons.push({
      id: "chips",
      text: `${hiddenByChips} of ${rows.length} rows hidden by the chips above`,
      hiding: true,
    });
  }
  if (out.ruby_strict && !out.ruby_strict.exact) {
    reasons.push({
      id: "ruby-strict",
      text: `Ruby strict rule refused exact: ${out.ruby_strict.verdict}`,
      hiding: false,
    });
  }
  if (mentions !== null) {
    reasons.push({
      id: "mentions",
      text: `${mentions} text mentions — a word-boundary grep, a different question from the ${out.totals.all} classified usages`,
      hiding: false,
    });
  }
  return {
    total: out.totals.all,
    byTrust,
    byKind,
    returned: rows.length,
    shown,
    capped: out.capped ?? [],
    mentions,
    reasons,
  };
}

// --- grouping -------------------------------------------------------------

/// The grouping axes this unit ships. `session`/`author`/`age` are the
/// PROVENANCE lanes (research §4.3) and are NOT here: they need a blame join
/// the `usages/2` wire does not carry, and the plan file's own deferral list
/// puts provenance grouping in v7.2. Declaring an axis whose rows would all
/// read "unknown" is exactly the dead surface this milestone is about.
export const GROUP_AXES = ["dir", "file", "kind", "module", "enclosing", "trust"] as const;
export type GroupAxis = (typeof GROUP_AXES)[number];

export const GROUP_AXIS_LABEL: Readonly<Record<GroupAxis, string>> = {
  dir: "directory",
  file: "file",
  kind: "kind",
  module: "module",
  enclosing: "enclosing def",
  trust: "trust",
};

export interface UsageGroup {
  /// Stable within one grouping — the tree's React key and the e2e handle.
  key: string;
  label: string;
  rows: UsageRow2[];
}

function dirOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i === -1 ? "." : path.slice(0, i);
}

/// The `module` axis: the enclosing symbol's own CONTAINER (`Order` for
/// `Order#total`), which for Ruby is the class/module the row sits in. Falls
/// back to the directory — captioned in the label, never silently — because
/// a row with no symbol table still has a place in the tree.
function moduleOf(row: UsageRow2): string {
  const c = row.enclosing?.container;
  if (c) return c;
  if (row.enclosing?.kind === "class" || row.enclosing?.kind === "module") {
    return row.enclosing.name;
  }
  return `${dirOf(row.path)}/ (no module)`;
}

function keyFor(axis: GroupAxis, row: UsageRow2): string {
  switch (axis) {
    case "dir":
      return dirOf(row.path);
    case "file":
      return row.path;
    case "kind":
      return row.kind;
    case "module":
      return moduleOf(row);
    case "enclosing":
      return row.enclosing
        ? row.enclosing.container
          ? `${row.enclosing.container}#${row.enclosing.name}`
          : row.enclosing.name
        : `${row.path} (top level)`;
    case "trust":
      return row.trust;
    default: {
      const never: never = axis;
      return never;
    }
  }
}

/// Group the rows. Ordering rules, all deliberate:
///
///  * the `trust` axis renders in TRUST_ORDER, never by size — exact must
///    never sort below likely because likely happens to be bigger;
///  * every other axis renders by group size descending, ties broken by the
///    key, so the order is stable across re-runs of the same query;
///  * rows WITHIN a group keep the server's own `(path, line, col)` order.
export function groupRows(rows: readonly UsageRow2[], axis: GroupAxis): UsageGroup[] {
  const buckets = new Map<string, UsageRow2[]>();
  for (const r of rows) {
    const k = keyFor(axis, r);
    const b = buckets.get(k);
    if (b) b.push(r);
    else buckets.set(k, [r]);
  }
  const groups: UsageGroup[] = [...buckets.entries()].map(([key, rs]) => ({
    key,
    label: key,
    rows: rs,
  }));
  if (axis === "trust") {
    groups.sort((a, b) => {
      const ia = TRUST_ORDER.indexOf(a.key as UsageTrust);
      const ib = TRUST_ORDER.indexOf(b.key as UsageTrust);
      return (ia === -1 ? 99 : ia) - (ib === -1 ? 99 : ib);
    });
    return groups;
  }
  groups.sort((a, b) => b.rows.length - a.rows.length || a.key.localeCompare(b.key));
  return groups;
}

/// The flat walking order `]u`/`[u` step through — the grouped tree read
/// top to bottom. One function, so the keyboard walk and the rendered tree
/// can never disagree about what "next" means.
export function walkOrder(groups: readonly UsageGroup[]): UsageRow2[] {
  return groups.flatMap((g) => g.rows);
}

/// Step the active-set cursor. Wraps in BOTH directions (a result set is a
/// ring, like vim's quickfix with `wrapscan`), and returns `-1` for an empty
/// set rather than 0 — "there is nothing to step to" is not "row zero".
export function stepCursor(length: number, cursor: number, delta: number): number {
  if (length <= 0) return -1;
  const base = cursor < 0 ? (delta > 0 ? -1 : 0) : cursor;
  return ((base + delta) % length + length) % length;
}

/// `Order#total` — the dock's title, from the wire's own symbol block.
export function usagesTitle(out: Usages2Out): string {
  const s = out.symbol;
  return s.container ? `${s.container}#${s.name}` : s.name;
}
