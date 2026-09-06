// V72-G1.2 — the entity dossier's PURE half.
//
// `components/entity/DossierView.tsx` renders rows it did not compute, the
// same posture `FileTree.tsx` takes over kbc-tree/1 (web-code/CLAUDE.md § The
// file tree) and `lib/usages2.ts` takes over the usages dock. Everything in
// this file is either (a) a re-ordering of rows the wire already sent, or
// (b) a caption DERIVED FROM THE WIRE'S OWN NUMBERS. Nothing here counts.
//
// THE ONE RULE THIS MODULE EXISTS TO ENFORCE: a count on screen comes from
// the response, never from `rows.length`. `entity/1` sends every list's TRUE
// total beside a `truncated` flag precisely because the rows in hand are a
// cut of it (`usages_per_kind` cut the caller asked for, or the row budget
// cut the caller did not choose where to land). `usageGroupCensus` below
// therefore reports `group.total` verbatim and states how many of it are on
// screen as a SEPARATE number — the `lib/usages2.ts` precedent, where the one
// number this side computes is "how many of the RETURNED rows are hidden" and
// it is emitted with its own reason. If a `.length` ever becomes the headline
// count in this file, that rule has been broken.
//
// Sorting is the other half. The server sends members in ITS order
// (visibility rank, then name — `ruby_body::visibility_rank`), and the three
// sort modes here are a VIEW over the rows already received, never a re-query:
// re-sorting cannot change which rows exist, so it needs no round trip and
// must not pretend the set changed.

import type {
  DossierDropped,
  DossierHonesty,
  DossierOut,
  MemberRow,
  UsageGroup,
  Visibility,
} from "../api/types";

// ── the member table's sort ────────────────────────────────────────────────

/// The three sort keys D6 names, in cycle order. `visibility` is the default
/// because it is the server's own order — landing on anything else would make
/// the first paint disagree with `kb-code entity`'s printed table for no
/// reason a reader asked for.
export type MemberSort = "visibility" | "name" | "defining_type";

export const MEMBER_SORTS: readonly MemberSort[] = ["visibility", "name", "defining_type"];

export const DEFAULT_MEMBER_SORT: MemberSort = "visibility";

export const MEMBER_SORT_LABEL: Readonly<Record<MemberSort, string>> = {
  visibility: "visibility",
  name: "name",
  defining_type: "defining type",
};

/// `ruby_body::visibility_rank`, mirrored. `unknown` sorts LAST for the reason
/// that module's own doc gives: a row whose visibility the scanner REFUSED to
/// claim must not be mixed into the public block a reader skims first.
/// Kept in lock-step with the Rust by `dossier.test.ts`'s golden walk.
export function visibilityRank(v: Visibility | string): number {
  switch (v) {
    case "public":
      return 0;
    case "protected":
      return 1;
    case "private":
      return 2;
    case "module_function":
      return 3;
    default:
      return 4;
  }
}

/// Cycle to the next sort key. Total over a junk value (an older persisted
/// choice, a hand-edited URL): anything unrecognised restarts the cycle at
/// the default rather than throwing or sticking.
export function cycleMemberSort(current: MemberSort | string): MemberSort {
  const i = MEMBER_SORTS.indexOf(current as MemberSort);
  if (i === -1) return DEFAULT_MEMBER_SORT;
  return MEMBER_SORTS[(i + 1) % MEMBER_SORTS.length];
}

/// Rust's `str::cmp`, mirrored: a CODE-UNIT comparison, NOT
/// `localeCompare`. The distinction is load-bearing rather than pedantic —
/// `dossier.rs` sorts members with `a.name.cmp(&b.name)` (byte order, so
/// `TAX_RATE` precedes `total`), while `localeCompare` collates
/// case-insensitively and puts them the other way round. Since the default
/// sort must be a NO-OP over the order the server already sent, a different
/// comparator here would silently REORDER the response — which is exactly
/// what `dossier.test.ts`'s golden walk caught.
///
/// Honest limit: JS compares UTF-16 code units and Rust compares UTF-8
/// bytes. These agree for the whole BMP and disagree only for supplementary
/// characters vs. U+E000–U+FFFF — a case Ruby constant and method names do
/// not reach, and one this mirror does not pretend to handle.
function cmpStr(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

/// A STABLE re-ordering of the rows already in hand. Never filters — the
/// inherited cut is `?inherited=1`'s job on the wire (a re-fetch), not a
/// client-side hide, because rows the server did not send are rows this side
/// cannot reveal. Every comparator falls back to the same last-resort chain
/// (name, then defining type, then path/line) so the order is total and two
/// renders of one response can never disagree.
export function sortMembers(members: readonly MemberRow[], sort: MemberSort): MemberRow[] {
  // The exact tie-break chain `dossier.rs`'s `members.sort_by` uses:
  // name → defining_type → path → line.
  const byName = (a: MemberRow, b: MemberRow) =>
    cmpStr(a.name, b.name) ||
    cmpStr(a.defining_type, b.defining_type) ||
    cmpStr(a.path, b.path) ||
    a.line - b.line;
  const rows = [...members];
  switch (sort) {
    case "name":
      rows.sort(byName);
      break;
    case "defining_type":
      rows.sort((a, b) => cmpStr(a.defining_type, b.defining_type) || byName(a, b));
      break;
    case "visibility":
    default:
      rows.sort(
        (a, b) => visibilityRank(a.visibility) - visibilityRank(b.visibility) || byName(a, b),
      );
      break;
  }
  return rows;
}

// ── the section spine ──────────────────────────────────────────────────────

/// D6's snippet-list order, declared as DATA so the center, the rail's jump
/// list and the `]s`/`[s` section motions all walk one list. A section that
/// is EMPTY still has an id and still renders (with its own empty caption) —
/// dropping it would make the motions skip unpredictably and would hide the
/// fact that the answer to "what mixes into this?" is "nothing".
export type DossierSectionId =
  | "definitions"
  | "members"
  | "hierarchy"
  | "usages"
  | "unknown-members"
  | "namespace";

export interface DossierSection {
  id: DossierSectionId;
  label: string;
  /// The DOM id the center renders and the motions scroll to.
  domId: string;
}

export const DOSSIER_SECTIONS: readonly DossierSection[] = [
  { id: "definitions", label: "Definitions", domId: "kbc-dossier-definitions" },
  { id: "members", label: "Members", domId: "kbc-dossier-members" },
  { id: "hierarchy", label: "Hierarchy", domId: "kbc-dossier-hierarchy" },
  { id: "usages", label: "Usages", domId: "kbc-dossier-usages" },
  { id: "unknown-members", label: "Unknown members", domId: "kbc-dossier-unknown-members" },
  { id: "namespace", label: "Namespace tree", domId: "kbc-dossier-namespace" },
];

/// Step the section cursor by `dir`, WRAPPING. Total: an unknown current id
/// (a section removed between renders) restarts at the first section going
/// forward and the last going back, rather than returning `null` and leaving
/// the key looking broken.
export function stepSection(current: DossierSectionId | null, dir: 1 | -1): DossierSectionId {
  const ids = DOSSIER_SECTIONS.map((s) => s.id);
  const i = current === null ? -1 : ids.indexOf(current);
  if (i === -1) return dir === 1 ? ids[0] : ids[ids.length - 1];
  return ids[(i + dir + ids.length) % ids.length];
}

// ── honesty ────────────────────────────────────────────────────────────────

/// The four READ states this page renders, named once. `loading` and `error`
/// are the CLIENT's two (a request in flight, a request that failed); `empty`
/// and `partial` are the SERVER's own `honesty.state`, which also carries
/// `ok`. Four separate renderings, because a spinner, an honest refusal, a
/// failed fetch and a truncated answer are four different facts and a page
/// that showed one of them for another would be lying.
export type DossierReadState = "loading" | "error" | "empty" | "partial" | "ok";

export function readStateOf(q: {
  isLoading: boolean;
  error: unknown;
  data: DossierOut | undefined;
}): DossierReadState {
  if (q.error) return "error";
  if (q.isLoading || !q.data) return "loading";
  const s = q.data.honesty.state;
  return s === "empty" || s === "partial" ? s : "ok";
}

/// The lanes the ROW BUDGET dropped, in `LANE_PRIORITY` order, with their
/// counts — the wire's own numbers, never re-derived. Zero-count lanes are
/// omitted: naming a lane that lost nothing would bury the ones that did.
///
/// `budget.order` is the server's declared priority list and is used AS the
/// iteration order so the caption reads in the order the budget was spent;
/// any dropped lane the order does not mention (a lane added server-side
/// before this file learns about it) is appended rather than silently lost.
export function droppedLanes(h: DossierHonesty): Array<{ lane: string; n: number }> {
  const dropped = h.budget.dropped as unknown as Record<string, number>;
  const out: Array<{ lane: string; n: number }> = [];
  const seen = new Set<string>();
  for (const lane of h.budget.order) {
    seen.add(lane);
    const n = dropped[lane];
    if (typeof n === "number" && n > 0) out.push({ lane, n });
  }
  for (const lane of Object.keys(dropped)) {
    if (seen.has(lane)) continue;
    const n = dropped[lane];
    if (typeof n === "number" && n > 0) out.push({ lane, n });
  }
  return out;
}

/// Total rows the budget dropped — the sum of the lane counts the wire sent.
/// This IS a client-side arithmetic, and it is allowed for the one reason the
/// module header carves out: it is a sum OF WIRE NUMBERS, not a count of rows
/// in hand, so it cannot disagree with the server about what was cut.
export function droppedTotal(d: DossierDropped): number {
  return Object.values(d as unknown as Record<string, number>).reduce(
    (a, b) => a + (typeof b === "number" ? b : 0),
    0,
  );
}

/// The header caption a `partial`/`empty` response owes its reader. Returns
/// `null` for `ok` — an "everything is fine" banner is noise, and its absence
/// is what makes the caption's PRESENCE meaningful.
///
/// A `partial` caption always names the budget (`spent of requested`) and,
/// when the budget actually cut rows, exactly which lanes lost how many. When
/// `partial` was raised for a reason that is NOT the budget (a missing
/// definition file, a degraded Zeitwerk read, a usages lane that could not
/// run — `honesty.notes`), the reason rides through unchanged and the budget
/// line still states what it spent, so the two never get conflated.
export function honestyCaption(h: DossierHonesty): string | null {
  if (h.state === "empty") {
    return h.reason ?? "no answer for this address, and no reason given";
  }
  if (h.state !== "partial") return null;
  const lanes = droppedLanes(h);
  const budget = `budget ${h.budget.spent} of ${h.budget.requested} rows`;
  if (lanes.length === 0) {
    return `${h.reason ?? "partial answer"} — ${budget}`;
  }
  const dropped = lanes.map((l) => `${l.n} ${l.lane}`).join(", ");
  return `${h.reason ?? "partial answer"} — ${budget}; dropped ${dropped}`;
}

// ── usages ─────────────────────────────────────────────────────────────────

/// One usage group's caption. `total` is the group's TRUE total, straight off
/// the wire; `shown` is how many rows arrived. They differ exactly when
/// `truncated` is set, and the caption says so — a "show more" that re-asks
/// with a higher `usages_per_kind` is the ONLY way to close the gap, because
/// this side never held the missing rows.
export interface UsageCensus {
  kind: string;
  total: number;
  shown: number;
  truncated: boolean;
  /// `exact`/`likely`/`candidate` over the RETURNED rows — the wire's
  /// `census_basis` says which set was counted, and it is rendered verbatim
  /// rather than being restated as an assumption.
  census: { exact: number; likely: number; candidate: number };
  basis: string;
  caption: string;
}

export function usageGroupCensus(g: UsageGroup): UsageCensus {
  const shown = g.rows.length;
  const census = g.trust_census;
  const trust = `${census.exact} exact · ${census.likely} likely · ${census.candidate} candidate`;
  const caption = g.truncated
    ? `${shown} of ${g.total} shown — ${trust} (${g.census_basis})`
    : `${g.total} total — ${trust} (${g.census_basis})`;
  return {
    kind: g.kind,
    total: g.total,
    shown,
    truncated: g.truncated,
    census: { exact: census.exact, likely: census.likely, candidate: census.candidate },
    basis: g.census_basis,
    caption,
  };
}

/// The next `usages_per_kind` a "show more" should ask for: double the
/// current cut, clamped to the route's own `MAX_USAGES_PER_KIND`. Returns
/// `null` when the cap is already reached, so the button can be ABSENT rather
/// than present-and-inert (the "a disabled row is a map of the surface"
/// posture kbc-actions/1 takes).
export const MAX_USAGES_PER_KIND = 200;
export const DEFAULT_USAGES_PER_KIND = 20;

export function nextUsagesPerKind(current: number): number | null {
  if (current >= MAX_USAGES_PER_KIND) return null;
  return Math.min(MAX_USAGES_PER_KIND, current * 2);
}
