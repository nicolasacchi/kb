import { describe, expect, it } from "vitest";
import type { DiffHunk } from "../api/versions";
import { bucketHunksBySection, collapseWs } from "./galley";

// `extractHeadings` uses `DOMParser`, which the node-environment unit-test
// harness doesn't provide (see vitest.config.ts) — it's exercised through
// the real browser render path, not here. These goldens cover the two pure
// pieces the recon calls out: title matching and monotonic-forward
// bucketing.

function hunk(lines: string[], old_start = 1, new_start = 1): DiffHunk {
  return {
    old_start,
    new_start,
    lines: lines.map((text, i) => ({
      tag: "insert",
      old_lineno: null,
      new_lineno: new_start + i,
      text,
    })),
  };
}

describe("collapseWs", () => {
  it("collapses internal whitespace runs (incl. newlines) and trims", () => {
    expect(collapseWs("Title\n  Hello   world  ")).toBe("Title Hello world");
  });
  it("is a no-op on already-collapsed text", () => {
    expect(collapseWs("Introduction")).toBe("Introduction");
  });
});

describe("bucketHunksBySection", () => {
  const headings = [
    { id: "intro", text: "Introduction" },
    { id: "details", text: "The Details" },
    { id: "again", text: "The Details" }, // duplicate title, later in doc order
  ];

  it("buckets a hunk under the section whose title it contains (exact match)", () => {
    const h1 = hunk(["Introduction", "some added prose"]);
    const out = bucketHunksBySection([h1], headings);
    expect(out).toHaveLength(1);
    expect(out[0]).toMatchObject({ id: "intro", title: "Introduction" });
    expect(out[0].hunks).toEqual([h1]);
  });

  it("matches a heading whose title spans lines against a whitespace-collapsed diff line", () => {
    const wrapped = [{ id: "wrapped", text: "Title\nHello world" }];
    const h1 = hunk(["Title Hello world", "body"]);
    const out = bucketHunksBySection([h1], wrapped);
    expect(out).toHaveLength(1);
    expect(out[0].id).toBe("wrapped");
  });

  it("resolves duplicate titles by heading order, not by revisiting the first occurrence", () => {
    const h1 = hunk(["The Details", "change one"]);
    const h2 = hunk(["The Details", "change two"], 10, 10);
    const out = bucketHunksBySection([h1, h2], headings);
    // First "The Details" hunk binds to `details`; the second binds to the
    // NEXT occurrence (`again`), never back to `details`.
    expect(out.map((s) => s.id)).toEqual(["details", "again"]);
    expect(out[0].hunks).toEqual([h1]);
    expect(out[1].hunks).toEqual([h2]);
  });

  it("puts hunks before any match in an explicit unplaced-changes group", () => {
    const h1 = hunk(["stray change with no heading match"]);
    const out = bucketHunksBySection([h1], headings);
    expect(out).toHaveLength(1);
    expect(out[0].id).toBeNull();
    expect(out[0].title).toBeNull();
    expect(out[0].hunks).toEqual([h1]);
  });

  it("never reopens an already-passed heading (monotonic forward)", () => {
    const h1 = hunk(["The Details", "first change"]);
    // Merely echoes the earlier "Introduction" title — must NOT reopen the
    // intro section; it stays attached to the current (details) section.
    const h2 = hunk(["Introduction", "unrelated echo"], 20, 20);
    const out = bucketHunksBySection([h1, h2], headings);
    expect(out).toHaveLength(1);
    expect(out[0].id).toBe("details");
    expect(out[0].hunks).toEqual([h1, h2]);
  });

  it("drops empty buckets (a heading nothing changed under, or no hunks at all)", () => {
    expect(bucketHunksBySection([], headings)).toEqual([]);
  });

  it("handles a fully empty heading list — everything is unplaced", () => {
    const h1 = hunk(["anything"]);
    const out = bucketHunksBySection([h1], []);
    expect(out).toEqual([{ id: null, title: null, hunks: [h1] }]);
  });
});
