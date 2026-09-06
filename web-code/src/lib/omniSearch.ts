// Pure orchestration helpers for `hooks/useOmniSearch.ts` - kept apart from
// the hook itself so the "does this response need a semantic follow-up"
// and "merge the follow-up into the section list" decisions are testable
// without mounting a component or mocking `fetch` (web-code has no
// `@testing-library/react` dependency - see `vitest.config.ts`'s doc, pure
// `node`-environment tests only).

import type { ChunkHit, LaneSection } from "../api/types";
import { laneRowCount, orderSections } from "./searchLanes";
import type { PaletteSection } from "./paletteReducer";

/// `true` when the response's semantic section missed its staged budget
/// (`search::unified::SEMANTIC_STAGE_BUDGET` - see that module's "Semantic
/// staging" doc) and the caller should re-query `GET /api/search/semantic`
/// directly for this one lane.
export function needsSemanticFollowup(sections: LaneSection[]): boolean {
  return sections.some((s) => s.lane === "semantic" && s.pending === true);
}

/// Splice a semantic follow-up's hits into the ALREADY-RENDERED section
/// list, clearing `pending` - every other section is returned unchanged
/// (same array identity where possible isn't required here; React re-renders
/// off the new top-level array regardless).
export function mergeSemanticFollowup(sections: LaneSection[], hits: ChunkHit[]): LaneSection[] {
  return sections.map((s) =>
    s.lane === "semantic" ? { ...s, pending: false, truncated: false, results: hits } : s,
  );
}

/// Derive the keyboard reducer's `PaletteSection[]` from a unified search
/// response's `sections` - canonical lane order (`orderSections`) + each
/// lane's own navigable row count (`laneRowCount`).
export function sectionsToRowCounts(sections: LaneSection[]): PaletteSection[] {
  return orderSections(sections).map((s) => ({ lane: s.lane, rowCount: laneRowCount(s) }));
}
