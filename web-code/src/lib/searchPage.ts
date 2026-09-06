// V71-D2 — the results page's own pure layer: take the daemon's sections,
// apply the SERVER's grouping and the CLIENT's refinement, and hand back the
// section list the existing `components/search/SearchSection` renderer
// already knows how to draw.
//
// Design P1 says every surface is a LENS over the shell, not a new app, and
// that shows up here as a deliberate non-decision: this module does not
// render anything and does not fork `SearchSection`. It produces a
// `LaneSection[]` — the same shape the Omnibox and the pre-D2 search page
// pass around — so `lib/omniSearch.ts`'s cursor derivation and
// `lib/searchTargets.ts`'s Enter resolution keep working unchanged, over the
// VIEW sections instead of the raw ones. One lane split into three groups is
// three sections of the same lane, and `orderSections`' stable sort keeps
// them adjacent and in group order.
//
// Two rules, matching the server halves they consume:
//
//   - **Grouping comes from the server** (`section.groups`, computed by
//     `search::results::group_hits`). This module never re-derives a group
//     key; it slices `results` by the group's own `indices`. That is what
//     makes `kb-code search --json --group dir` and this page agree by
//     construction rather than by two implementations happening to match.
//   - **Refinement only REMOVES**, never re-orders and never re-queries
//     (`lib/refine.ts`). The counts it produces are reported as "N of M"
//     rather than replacing M, because a narrowed page is still a page of
//     the same search.

import type {
  ChunkHit,
  FileHit,
  LaneSection,
  SessionHit,
  SymbolHit,
  TextFileResult,
  TranscriptHit,
} from "../api/types";
import { parseRefinement, matchesRefinement, type Refinement } from "./refine";
import { LANE_LABELS, laneRowCount, orderSections } from "./searchLanes";

/// One rendered section of the results page.
export interface ViewSection {
  /// A stable key for React — `lane` alone is no longer unique once a lane
  /// splits into groups.
  key: string;
  section: LaneSection;
  /// What the header reads: the lane label, or `Lane · group` when grouped.
  label: string;
  /// The group this section came from, when grouped — so a caller can offer
  /// "narrow to this group" without re-deriving it.
  groupKey?: string;
}

/// The refinable text of one RESULT ROW, per lane. Not exported: the page
/// never needs a row's haystack, only the filtered section.
function haystacksFor(section: LaneSection): string[] {
  const rows = Array.isArray(section.results) ? section.results : [];
  switch (section.lane) {
    case "files":
      return (rows as FileHit[]).map((h) => `${h.repo} ${h.path}`);
    case "symbols":
      return (rows as SymbolHit[]).map(
        (h) => `${h.container ? `${h.container}::` : ""}${h.name} ${h.kind} ${h.repo} ${h.path}`,
      );
    case "semantic":
      return (rows as ChunkHit[]).map((h) => `${h.repo} ${h.path} ${h.snippet}`);
    case "sessions":
      return (rows as SessionHit[]).map((h) => `${h.title ?? ""} ${h.session_id}`);
    case "transcripts":
      return (rows as TranscriptHit[]).map(
        (h) => `${h.snippet} ${h.kind} ${h.tool_name ?? ""} ${h.project_dir}`,
      );
    // The text lane is refined per MATCH, not per file — see `refineSection`.
    default:
      return rows.map(() => "");
  }
}

/// Narrow one section's rows by `r`. The TEXT lane is special and stays
/// special on purpose: its result row is a FILE carrying N matches, while
/// the thing a human sees (and means to narrow) is a MATCH LINE. So a text
/// section is refined match-by-match and a file whose matches all fall away
/// is dropped — anything else would keep an empty file header on screen or
/// hide a matching line under a non-matching path.
export function refineSection(section: LaneSection, r: Refinement): LaneSection {
  if (r.empty || !Array.isArray(section.results)) return section;
  if (section.lane === "text") {
    const kept = (section.results as TextFileResult[])
      .map((f) => ({
        ...f,
        matches: f.matches.filter((m) => matchesRefinement(`${f.path} ${m.line}`, r)),
      }))
      .filter((f) => f.matches.length > 0);
    return { ...section, results: kept, groups: undefined };
  }
  const hay = haystacksFor(section);
  const kept = (section.results as unknown[]).filter((_, i) => matchesRefinement(hay[i] ?? "", r));
  // `groups` index into the ORIGINAL array, so they cannot survive a filter
  // — dropping them is honest; remapping them silently would be a second
  // grouping implementation on this side.
  return { ...section, results: kept, groups: undefined };
}

/// Split one section into one view section per SERVER group. A section with
/// no `groups` (ungrouped query, or a lane the grouper could not key) comes
/// back as itself.
export function splitByGroups(section: LaneSection): ViewSection[] {
  const label = LANE_LABELS[section.lane] ?? section.lane;
  const rows = Array.isArray(section.results) ? (section.results as unknown[]) : [];
  if (!section.groups || section.groups.length === 0 || rows.length === 0) {
    return [{ key: section.lane, section, label }];
  }
  return section.groups.map((g, i) => ({
    key: `${section.lane}#${i}`,
    // A group is a SLICE of the page, so it is never `truncated` on its own
    // — the lane is. Carrying the lane's flag onto every group would
    // repeat one honest warning N times.
    section: {
      ...section,
      results: g.indices.map((idx) => rows[idx]).filter((x) => x !== undefined),
      groups: undefined,
      truncated: i === section.groups!.length - 1 ? section.truncated : false,
    },
    label: `${label} · ${g.label}`,
    groupKey: g.key,
  }));
}

export interface PageView {
  view: ViewSection[];
  /// Navigable rows before refinement.
  total: number;
  /// Navigable rows after it.
  shown: number;
  /// `true` when a refinement is actually in force — the caption reads
  /// "N of M" only then.
  refined: boolean;
}

/// The whole pipeline: canonical lane order → server groups → refinement.
/// Grouping runs FIRST because the server's `indices` address the unfiltered
/// array; refining first would invalidate every index.
export function buildPageView(sections: LaneSection[], refineText: string): PageView {
  const r = parseRefinement(refineText);
  const ordered = orderSections(sections);
  const total = ordered.reduce((n, s) => n + laneRowCount(s), 0);
  const view: ViewSection[] = [];
  for (const s of ordered) {
    for (const vs of splitByGroups(s)) {
      const refined = refineSection(vs.section, r);
      view.push({ ...vs, section: refined });
    }
  }
  const shown = view.reduce((n, v) => n + laneRowCount(v.section), 0);
  return { view, total, shown, refined: !r.empty };
}
