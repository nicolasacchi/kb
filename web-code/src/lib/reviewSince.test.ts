// v0.44 F9 — pins the PatchsetStrip label and the "author changes only"
// interdiff filter. The failure these exist for: a rebase-only patchset read
// as author edits (every upstream file listed, "changed since your verdict").
import { describe, expect, it } from "vitest";
import type { SinceReport } from "../api/types";
import {
  authorChangeCount,
  authorChangedPaths,
  authorOnlyCaption,
  canReaffirm,
  filterAuthorFiles,
  rangeFileRows,
  reaffirmLabel,
  sinceApplies,
  sinceLabel,
} from "./reviewSince";

function report(over: Partial<SinceReport> = {}): SinceReport {
  return {
    schema: "kbc-review-since/1",
    review_id: 7,
    from: { ps: 2, base_sha: "a".repeat(40), tip_sha: "b".repeat(40) },
    to: { ps: 4, base_sha: "c".repeat(40), tip_sha: "d".repeat(40) },
    from_source: "verdict",
    paths: [{ path: "a.txt", carried: 1, new: 0, gone: 0 }],
    author_delta: { new_hunks: 0, gone_hunks: 0, paths_changed: 0 },
    rebase_only: true,
    bases: { from: "a".repeat(40), to: "c".repeat(40), moved: true },
    note: "",
    ...over,
  };
}

describe("sinceLabel", () => {
  it("names a rebase-only patchset with zero author changes", () => {
    expect(sinceLabel(report())).toBe("ps4 · rebase-only since your verdict (0 author changes)");
  });

  it("counts new and gone hunks as author changes", () => {
    const r = report({
      rebase_only: false,
      author_delta: { new_hunks: 2, gone_hunks: 1, paths_changed: 2 },
    });
    expect(authorChangeCount(r)).toBe(3);
    expect(sinceLabel(r)).toBe("ps4 · 3 author changes since your verdict (rebased)");
  });

  it("singular, same-base, explicit-from wording", () => {
    const r = report({
      from_source: "ps",
      rebase_only: false,
      author_delta: { new_hunks: 1, gone_hunks: 0, paths_changed: 1 },
      bases: { from: "a".repeat(40), to: "a".repeat(40), moved: false },
    });
    expect(sinceLabel(r)).toBe("ps4 · 1 author change since ps2");
  });

  it("an unmoved base with nothing changed is not called a rebase", () => {
    const r = report({ bases: { from: "a".repeat(40), to: "a".repeat(40), moved: false } });
    expect(sinceLabel(r)).toBe("ps4 · no changes since your verdict (0 author changes)");
  });

  it("never claims anything is fixed or approved", () => {
    for (const r of [report(), report({ rebase_only: false })]) {
      expect(sinceLabel(r)).not.toMatch(/fixed|resolved|approved|addressed/i);
    }
  });
});

describe("filterAuthorFiles", () => {
  const files = [
    { path: "a.txt", old_path: null },
    { path: "upstream.txt", old_path: null },
    { path: "new_name.txt", old_path: "old_name.txt" },
  ];

  it("keeps only the files the author changed, dropping upstream movement", () => {
    const r = report({
      paths: [
        { path: "a.txt", carried: 0, new: 1, gone: 0 },
        { path: "upstream.txt", carried: 3, new: 0, gone: 0 },
      ],
    });
    expect(authorChangedPaths(r)).toEqual(new Set(["a.txt"]));
    expect(filterAuthorFiles(files, r).map((f) => f.path)).toEqual(["a.txt"]);
  });

  it("a rename row matches on either name", () => {
    const r = report({ paths: [{ path: "old_name.txt", carried: 0, new: 0, gone: 2 }] });
    expect(filterAuthorFiles(files, r).map((f) => f.path)).toEqual(["new_name.txt"]);
  });

  it("without a report the list is untouched (no guessing)", () => {
    expect(filterAuthorFiles(files, undefined)).toEqual(files);
  });
});

describe("sinceApplies", () => {
  it("needs a verdict and a later patchset", () => {
    expect(sinceApplies(2, 4)).toBe(true);
    expect(sinceApplies(4, 4)).toBe(false);
    expect(sinceApplies(null, 4)).toBe(false);
    expect(sinceApplies(2, null)).toBe(false);
  });
});

// v0.44 F9b — the full-page review diff's `?ps=a..b` arm shares the switch.
describe("rangeFileRows (ReviewDiff ?ps=a..b arm)", () => {
  const files = [
    { path: "a.txt", old_path: null },
    { path: "upstream.txt", old_path: null },
  ];
  const r = report({ paths: [{ path: "a.txt", carried: 0, new: 1, gone: 0 }] });

  it("shows every interdiff file with the switch off", () => {
    expect(rangeFileRows(files, r, false)).toEqual(files);
  });

  it("drops upstream-only files with the switch on", () => {
    expect(rangeFileRows(files, r, true).map((f) => f.path)).toEqual(["a.txt"]);
  });

  it("shows everything while the delta is still loading, never an empty list", () => {
    expect(rangeFileRows(files, undefined, true)).toEqual(files);
  });

  it("captions the count and a moved base", () => {
    expect(authorOnlyCaption(report({ author_delta: { new_hunks: 1, gone_hunks: 0, paths_changed: 1 } }))).toBe(
      "1 author change · base moved",
    );
  });
});

// v0.44 F9b - the one-click re-affirm on a rebase-only patchset (D20: a
// human click, never automatic).
describe("canReaffirm", () => {
  // report() defaults: ps2 -> ps4, rebase_only, base moved.
  it("offers it for a stale verdict whose newer patchset is rebase-only with a moved base", () => {
    expect(canReaffirm(2, true, 4, report())).toBe(true);
  });

  it("not while the delta is unknown, or the verdict is not stale", () => {
    expect(canReaffirm(2, true, 4, undefined)).toBe(false);
    expect(canReaffirm(2, false, 4, report())).toBe(false);
    expect(canReaffirm(4, true, 4, report())).toBe(false);
  });

  it("never when the author changed anything since the verdict", () => {
    const edited = report({
      rebase_only: false,
      author_delta: { new_hunks: 1, gone_hunks: 0, paths_changed: 1 },
    });
    expect(canReaffirm(2, true, 4, edited)).toBe(false);
  });

  it("not when the base did not move (a plain newer patchset is not a rebase)", () => {
    const same = report({ bases: { from: "a".repeat(40), to: "a".repeat(40), moved: false } });
    expect(canReaffirm(2, true, 4, same)).toBe(false);
  });

  it("not for a report about a different pair than verdict -> latest", () => {
    expect(canReaffirm(3, true, 4, report())).toBe(false);
    expect(canReaffirm(2, true, 5, report())).toBe(false);
  });

  it("labels the action with the verdict it records", () => {
    expect(reaffirmLabel("approve", 4)).toBe("Re-affirm approval on ps4");
    expect(reaffirmLabel("request-changes", 4)).toBe("Re-affirm changes requested on ps4");
  });
});
