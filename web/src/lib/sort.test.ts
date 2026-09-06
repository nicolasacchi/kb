import { describe, it, expect } from "vitest";
import type { DocSummary } from "../api/client";
import { defaultDir, sortComparator, groupByFolder } from "./sort";

function makeDoc(p: Partial<DocSummary>): DocSummary {
  return {
    id: "id",
    title: "t",
    path: "p.html",
    folder: "",
    source_relative: "p.html",
    kb_category: null,
    ...p,
  };
}

describe("defaultDir", () => {
  it("ascends titles, descends everything else", () => {
    expect(defaultDir("title")).toBe("asc");
    expect(defaultDir("recent")).toBe("desc");
    expect(defaultDir("indexed")).toBe("desc");
    expect(defaultDir("created")).toBe("desc");
    expect(defaultDir("words")).toBe("desc");
  });
});

describe("sortComparator", () => {
  it("orders by recent desc (newest first), missing time last", () => {
    const docs = [
      makeDoc({ id: "old", mtime_unix: 100 }),
      makeDoc({ id: "new", mtime_unix: 300 }),
      makeDoc({ id: "none", mtime_unix: null }),
    ];
    const sorted = [...docs].sort(sortComparator("recent", "desc"));
    expect(sorted.map((d) => d.id)).toEqual(["new", "old", "none"]);
  });
  it("orders by title asc via localeCompare (case-insensitive collation)", () => {
    // Pins the full-ICU collation contract: "apple" < "Banana" despite 'B'
    // (66) < 'a' (97) in code-unit order. The official Node binary used in
    // CI (setup-node@v4) ships full ICU, so this holds; a small/no-ICU Node
    // would degrade localeCompare to code-unit order and flip only THIS pair.
    const docs = [makeDoc({ id: "b", title: "Banana" }), makeDoc({ id: "a", title: "apple" })];
    const sorted = [...docs].sort(sortComparator("title", "asc"));
    expect(sorted.map((d) => d.id)).toEqual(["a", "b"]);
  });
  it("orders by title asc for a collation-stable same-case pair", () => {
    // ICU-independent (locale order == code-unit order here), so this stays
    // green even on a stripped-ICU Node — isolating the regression signal.
    const docs = [makeDoc({ id: "z", title: "zeta" }), makeDoc({ id: "a", title: "alpha" })];
    const sorted = [...docs].sort(sortComparator("title", "asc"));
    expect(sorted.map((d) => d.id)).toEqual(["a", "z"]);
  });
  it("orders by indexed desc, reading indexed_at_unix (not mtime)", () => {
    // mtime is set to the INVERSE order so a wrong-field copy-paste in the
    // `indexed` branch (reading mtime_unix) would flip the result.
    const docs = [
      makeDoc({ id: "a", indexed_at_unix: 100, mtime_unix: 999 }),
      makeDoc({ id: "b", indexed_at_unix: 300, mtime_unix: 1 }),
    ];
    const sorted = [...docs].sort(sortComparator("indexed", "desc"));
    expect(sorted.map((d) => d.id)).toEqual(["b", "a"]);
  });
  it("orders by created desc, reading created_unix (not the other times)", () => {
    const docs = [
      makeDoc({ id: "a", created_unix: 100, indexed_at_unix: 999, mtime_unix: 999 }),
      makeDoc({ id: "b", created_unix: 300, indexed_at_unix: 1, mtime_unix: 1 }),
    ];
    const sorted = [...docs].sort(sortComparator("created", "desc"));
    expect(sorted.map((d) => d.id)).toEqual(["b", "a"]);
  });
  it("orders by words desc, missing word count as 0", () => {
    const docs = [
      makeDoc({ id: "x", word_count: 10 }),
      makeDoc({ id: "y", word_count: 500 }),
      makeDoc({ id: "z", word_count: null }),
    ];
    const sorted = [...docs].sort(sortComparator("words", "desc"));
    expect(sorted.map((d) => d.id)).toEqual(["y", "x", "z"]);
  });
});

describe("groupByFolder", () => {
  it("buckets by folder and orders sections by max time desc, preserving in-bucket order", () => {
    const docs = [
      makeDoc({ id: "a1", folder: "alpha", mtime_unix: 100 }),
      makeDoc({ id: "b1", folder: "beta", mtime_unix: 500 }),
      makeDoc({ id: "a2", folder: "alpha", mtime_unix: 200 }),
    ];
    const sorted = [...docs].sort(sortComparator("recent", "desc"));
    const sections = groupByFolder(sorted, "recent", "desc");
    expect(sections.map((s) => s.folder)).toEqual(["beta", "alpha"]);
    const alpha = sections.find((s) => s.folder === "alpha")!;
    // a2 (200) sorts before a1 (100) under recent-desc; the bucket keeps it.
    expect(alpha.docs.map((d) => d.id)).toEqual(["a2", "a1"]);
  });
  it("orders sections alphabetically for a title sort", () => {
    const docs = [makeDoc({ folder: "zeta" }), makeDoc({ folder: "alpha" })];
    const sections = groupByFolder(docs, "title", "asc");
    expect(sections.map((s) => s.folder)).toEqual(["alpha", "zeta"]);
  });
  it("ignores null times in a bucket's max (does not treat them as 0)", () => {
    // Bucket A has a null AND a 200; bucket B has 150. A must lead because
    // its max is 200 — a null treated as 0 (or -Infinity poisoning) would
    // wrongly sink A below B.
    const docs = [
      makeDoc({ id: "a1", folder: "alpha", mtime_unix: null }),
      makeDoc({ id: "a2", folder: "alpha", mtime_unix: 200 }),
      makeDoc({ id: "b1", folder: "beta", mtime_unix: 150 }),
    ];
    const sections = groupByFolder(docs, "recent", "desc");
    expect(sections.map((s) => s.folder)).toEqual(["alpha", "beta"]);
  });
  it("uses '' for a null/undefined folder (defensive — the API never sends one)", () => {
    // DocSummary.folder is non-null `string` per the API contract ("" at the
    // root), so this only exercises the defensive `?? \"\"` fallback.
    const docs = [makeDoc({ folder: undefined as unknown as string })];
    const sections = groupByFolder(docs, "recent", "desc");
    expect(sections[0].folder).toBe("");
  });
});
