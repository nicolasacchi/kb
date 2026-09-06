import { describe, expect, it } from "vitest";
import type { LaneSection } from "../api/types";
import { laneRowCount, orderSections } from "./searchLanes";
import { resolveSearchTarget } from "./searchTargets";
import { sectionsToRowCounts } from "./omniSearch";
import { buildPageView, refineSection, splitByGroups } from "./searchPage";
import { parseRefinement } from "./refine";

function filesSection(): LaneSection {
  return {
    lane: "files",
    truncated: false,
    results: [
      { repo: "kb", path: "app/models/order.rb", score: 3 },
      { repo: "kb", path: "app/models/user.rb", score: 2 },
      { repo: "kb", path: "README.md", score: 1 },
    ],
    groups: [
      { key: "app/models", label: "app/models", count: 2, indices: [0, 1] },
      { key: "", label: "(repo root)", count: 1, indices: [2] },
    ],
  };
}

function textSection(): LaneSection {
  return {
    lane: "text",
    truncated: false,
    results: [
      {
        path: "app/a.rb",
        matches: [
          { line_no: 1, line: "def call", byte_range: [0, 3] },
          { line_no: 9, line: "def perform", byte_range: [0, 3] },
        ],
      },
      { path: "spec/a_spec.rb", matches: [{ line_no: 4, line: "def call", byte_range: [0, 3] }] },
    ],
  };
}

describe("searchPage — server grouping, client refinement", () => {
  it("splits a lane into one view section per SERVER group, slicing by its indices", () => {
    const view = splitByGroups(filesSection());
    expect(view.map((v) => v.label)).toEqual(["Files · app/models", "Files · (repo root)"]);
    expect(view.map((v) => laneRowCount(v.section))).toEqual([2, 1]);
    expect(view[0].groupKey).toBe("app/models");
    // Every row of the lane survives the split — grouping partitions.
    const rows = view.flatMap((v) => v.section.results as { path: string }[]);
    expect(rows.map((r) => r.path)).toEqual([
      "app/models/order.rb",
      "app/models/user.rb",
      "README.md",
    ]);
  });

  it("leaves an ungrouped section exactly as it was", () => {
    const s: LaneSection = { lane: "files", truncated: false, results: [{ repo: "k", path: "a", score: 1 }] };
    const view = splitByGroups(s);
    expect(view).toHaveLength(1);
    expect(view[0].section).toBe(s);
    expect(view[0].label).toBe("Files");
  });

  /// A truncated lane's warning belongs on the lane, printed ONCE, not
  /// repeated on every group it was split into.
  it("carries a truncation warning on the last group only", () => {
    const s = { ...filesSection(), truncated: true };
    expect(splitByGroups(s).map((v) => v.section.truncated)).toEqual([false, true]);
  });

  it("refines the text lane per MATCH and drops files that lose all of theirs", () => {
    const refined = refineSection(textSection(), parseRefinement("perform"));
    const files = refined.results as { path: string; matches: unknown[] }[];
    expect(files).toHaveLength(1);
    expect(files[0].path).toBe("app/a.rb");
    expect(files[0].matches).toHaveLength(1);
  });

  it("drops server groups when it refines — indices address the unfiltered array", () => {
    const refined = refineSection(filesSection(), parseRefinement("order"));
    expect(refined.groups).toBeUndefined();
    expect(laneRowCount(refined)).toBe(1);
  });

  it("reports N-of-M rather than replacing the count", () => {
    const before = buildPageView([filesSection()], "");
    expect(before.total).toBe(3);
    expect(before.shown).toBe(3);
    expect(before.refined).toBe(false);

    const after = buildPageView([filesSection()], "order");
    expect(after.total).toBe(3);
    expect(after.shown).toBe(1);
    expect(after.refined).toBe(true);
  });

  /// The reason this module returns `LaneSection[]` at all: the cursor
  /// model and the Enter resolver must keep working over the VIEW, with a
  /// lane appearing more than once. `orderSections`' sort is stable, so
  /// groups stay adjacent and in order — if that ever stops holding, Enter
  /// opens the wrong file.
  it("keeps the shared cursor model and target resolver working over grouped view sections", () => {
    const { view } = buildPageView([filesSection()], "");
    const sections = view.map((v) => v.section);
    expect(orderSections(sections).map((s) => (s.results as { path: string }[])[0].path)).toEqual([
      "app/models/order.rb",
      "README.md",
    ]);
    expect(sectionsToRowCounts(sections)).toEqual([
      { lane: "files", rowCount: 2 },
      { lane: "files", rowCount: 1 },
    ]);
    expect(resolveSearchTarget(sections, { section: 1, row: 0 }, undefined, "")).toEqual({
      kind: "reader",
      repo: "kb",
      path: "README.md",
    });
  });

  it("orders lanes canonically before expanding them", () => {
    const sessions: LaneSection = { lane: "sessions", truncated: false, results: [] };
    const { view } = buildPageView([sessions, filesSection()], "");
    expect(view.map((v) => v.section.lane)).toEqual(["files", "files", "sessions"]);
  });
});
