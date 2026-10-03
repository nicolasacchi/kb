// v0.44 F9 — pins the PatchsetStrip label and the "author changes only"
// interdiff filter. The failure these exist for: a rebase-only patchset read
// as author edits (every upstream file listed, "changed since your verdict").
import { describe, expect, it } from "vitest";
import type { SinceReport } from "../api/types";
import {
  authorChangeCount,
  authorChangedPaths,
  filterAuthorFiles,
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
