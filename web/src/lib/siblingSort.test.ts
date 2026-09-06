import { describe, it, expect } from "vitest";
import {
  parseSiblingSort,
  siblingComparator,
  siblingDateUnix,
  type SiblingSortable,
} from "./siblingSort";

function row(p: Partial<SiblingSortable> & { filename: string }): SiblingSortable {
  return {
    title: p.title ?? p.filename,
    mtime: p.mtime ?? null,
    created: p.created,
    firstIndexed: p.firstIndexed,
    filename: p.filename,
  };
}

describe("parseSiblingSort", () => {
  it("accepts known keys", () => {
    expect(parseSiblingSort("name")).toBe("name");
    expect(parseSiblingSort("title")).toBe("title");
    expect(parseSiblingSort("updated")).toBe("updated");
    expect(parseSiblingSort("created")).toBe("created");
  });
  it("falls back to updated for missing/unknown/legacy values", () => {
    expect(parseSiblingSort(undefined)).toBe("updated");
    expect(parseSiblingSort("recent")).toBe("updated");
    expect(parseSiblingSort("indexed")).toBe("updated");
    expect(parseSiblingSort(42)).toBe("updated");
  });
});

describe("siblingComparator", () => {
  it("orders by updated (mtime) desc", () => {
    const rows = [
      row({ filename: "old.html", mtime: 100 }),
      row({ filename: "new.html", mtime: 300 }),
      row({ filename: "mid.html", mtime: 200 }),
    ];
    const sorted = [...rows].sort(siblingComparator("updated"));
    expect(sorted.map((r) => r.filename)).toEqual([
      "new.html",
      "mid.html",
      "old.html",
    ]);
  });

  it("orders by created desc with per-row mtime fallback when created is null", () => {
    // a: created 100; b: no created, mtime 250 → b wins; c: created 200
    const rows = [
      row({ filename: "a.html", mtime: 999, created: 100 }),
      row({ filename: "b.html", mtime: 250, created: null }),
      row({ filename: "c.html", mtime: 1, created: 200 }),
      row({ filename: "d.html", mtime: null }), // missing both → last
    ];
    const sorted = [...rows].sort(siblingComparator("created"));
    expect(sorted.map((r) => r.filename)).toEqual([
      "b.html",
      "c.html",
      "a.html",
      "d.html",
    ]);
  });

  // v0.33 Y2 — firstIndexed sits between created and mtime.
  it("created sort chain: created > firstIndexed > mtime", () => {
    const rows = [
      // edit bumped mtime high, but firstIndexed is the stable anchor
      row({
        filename: "edited.html",
        mtime: 9999,
        firstIndexed: 100,
      }),
      // btime present wins over firstIndexed + mtime
      row({
        filename: "born.html",
        mtime: 1,
        created: 500,
        firstIndexed: 50,
      }),
      // no created, no firstIndexed → raw mtime
      row({ filename: "legacy.html", mtime: 300 }),
      // firstIndexed only
      row({
        filename: "seeded.html",
        mtime: 10,
        firstIndexed: 400,
      }),
    ];
    const sorted = [...rows].sort(siblingComparator("created"));
    expect(sorted.map((r) => r.filename)).toEqual([
      "born.html", // created 500
      "seeded.html", // firstIndexed 400
      "legacy.html", // mtime 300
      "edited.html", // firstIndexed 100 (mtime ignored)
    ]);
  });

  it("created sort is stable under mtime-only edits when firstIndexed is set", () => {
    const before = [
      row({ filename: "a.md", mtime: 100, firstIndexed: 100 }),
      row({ filename: "b.md", mtime: 200, firstIndexed: 200 }),
    ];
    const afterEdit = [
      row({ filename: "a.md", mtime: 9999, firstIndexed: 100 }), // edited
      row({ filename: "b.md", mtime: 200, firstIndexed: 200 }),
    ];
    const order = (rows: SiblingSortable[]) =>
      [...rows].sort(siblingComparator("created")).map((r) => r.filename);
    expect(order(before)).toEqual(["b.md", "a.md"]);
    expect(order(afterEdit)).toEqual(["b.md", "a.md"]); // same order
  });

  it("both-missing times fall through to the filename tiebreak (no NaN)", () => {
    const rows = [
      row({ filename: "b.html", mtime: null }),
      row({ filename: "a.html", mtime: null }),
    ];
    const sorted = [...rows].sort(siblingComparator("updated"));
    expect(sorted.map((r) => r.filename)).toEqual(["a.html", "b.html"]);
  });

  it("stable-tiebreaks equal times by filename localeCompare", () => {
    const rows = [
      row({ filename: "zeta.html", mtime: 100 }),
      row({ filename: "alpha.html", mtime: 100 }),
      row({ filename: "mid.html", mtime: 100 }),
    ];
    const sorted = [...rows].sort(siblingComparator("updated"));
    expect(sorted.map((r) => r.filename)).toEqual([
      "alpha.html",
      "mid.html",
      "zeta.html",
    ]);
  });

  it("name sort is filename asc (unchanged)", () => {
    const rows = [
      row({ filename: "zeta.html", mtime: 999 }),
      row({ filename: "alpha.html", mtime: 1 }),
    ];
    const sorted = [...rows].sort(siblingComparator("name"));
    expect(sorted.map((r) => r.filename)).toEqual([
      "alpha.html",
      "zeta.html",
    ]);
  });

  it("title sort is title||filename asc with base sensitivity (unchanged)", () => {
    const rows = [
      row({ filename: "z.html", title: "Banana" }),
      row({ filename: "a.html", title: "apple" }),
      row({ filename: "no-title.html", title: "" }),
    ];
    const sorted = [...rows].sort(siblingComparator("title"));
    // "apple" < "Banana" (base sensitivity); empty title → filename
    expect(sorted.map((r) => r.filename)).toEqual([
      "a.html",
      "z.html",
      "no-title.html",
    ]);
  });
});

describe("siblingDateUnix", () => {
  it("uses mtime for updated/name/title", () => {
    const r = row({ filename: "x.html", mtime: 50, created: 10 });
    expect(siblingDateUnix(r, "updated")).toBe(50);
    expect(siblingDateUnix(r, "name")).toBe(50);
    expect(siblingDateUnix(r, "title")).toBe(50);
  });
  it("uses created with mtime fallback for created sort", () => {
    expect(
      siblingDateUnix(
        row({ filename: "x.html", mtime: 50, created: 10 }),
        "created",
      ),
    ).toBe(10);
    expect(
      siblingDateUnix(row({ filename: "x.html", mtime: 50 }), "created"),
    ).toBe(50);
  });

  it("uses firstIndexed between created and mtime for created sort", () => {
    expect(
      siblingDateUnix(
        row({
          filename: "x.html",
          mtime: 50,
          firstIndexed: 30,
          created: 10,
        }),
        "created",
      ),
    ).toBe(10);
    expect(
      siblingDateUnix(
        row({ filename: "x.html", mtime: 50, firstIndexed: 30 }),
        "created",
      ),
    ).toBe(30);
    expect(
      siblingDateUnix(row({ filename: "x.html", mtime: 50 }), "created"),
    ).toBe(50);
  });
});
