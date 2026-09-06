import { describe, it, expect } from "vitest";
import type { DocSummary } from "../api/client";
import {
  fnv1a,
  tagColor,
  pathToTags,
  slugifyTag,
  tagsFor,
  isIndexPage,
  relativeAge,
  isNew,
} from "./derive";

function makeDoc(p: Partial<DocSummary>): DocSummary {
  return {
    id: "id",
    title: "t",
    path: "a/b/c.html",
    folder: "",
    source_relative: "a/b/c.html",
    kb_category: null,
    ...p,
  };
}

describe("fnv1a", () => {
  it("returns the 32-bit offset basis for the empty string", () => {
    expect(fnv1a("")).toBe(2166136261);
  });
  it("matches canonical FNV-1a 32-bit vectors (pins the mixing step)", () => {
    // The empty-string case skips the mix loop entirely; these non-empty
    // vectors lock the actual hash arithmetic, so a transposed shift or
    // wrong constant in the mixing line is caught.
    expect(fnv1a("a")).toBe(3826002220);
    expect(fnv1a("foobar")).toBe(3214735720);
  });
  it("is deterministic", () => {
    expect(fnv1a("hello")).toBe(fnv1a("hello"));
  });
  it("distinguishes different inputs", () => {
    expect(fnv1a("a")).not.toBe(fnv1a("b"));
  });
  it("stays within unsigned 32-bit range", () => {
    for (const s of ["", "a", "kb", "a-very-long-tag-name-here"]) {
      const h = fnv1a(s);
      expect(h).toBeGreaterThanOrEqual(0);
      expect(h).toBeLessThanOrEqual(0xffffffff);
    }
  });
});

describe("tagColor", () => {
  it("formats an hsl string with hue in [0,360)", () => {
    const m = tagColor("rust").match(/^hsl\((\d+), 64%, 64%\)$/);
    expect(m).not.toBeNull();
    const hue = Number(m![1]);
    expect(hue).toBeGreaterThanOrEqual(0);
    expect(hue).toBeLessThan(360);
  });
  it("is stable for the same name", () => {
    expect(tagColor("design")).toBe(tagColor("design"));
  });
});

describe("pathToTags", () => {
  it("returns the two dir segments closest to the file, most-specific first", () => {
    // NB: pins ACTUAL behaviour — closer-to-file segment first. The inline
    // example in derive.ts's doc comment (`["incidents","checks"]`) is stale;
    // the code returns more-specific-first per its own trailing comment.
    expect(pathToTags("incidents/checks/foo.html")).toEqual(["checks", "incidents"]);
  });
  it("returns [] for null / undefined / empty", () => {
    expect(pathToTags(null)).toEqual([]);
    expect(pathToTags(undefined)).toEqual([]);
    expect(pathToTags("")).toEqual([]);
  });
  it("returns [] for a bare filename (no directory parts)", () => {
    expect(pathToTags("foo.html")).toEqual([]);
  });
  it("filters generic top-level dir names", () => {
    expect(pathToTags("docs/src/foo.html")).toEqual([]);
  });
  it("slugifies and dedupes repeated segments", () => {
    expect(pathToTags("My Dir/My Dir/foo.html")).toEqual(["my-dir"]);
  });
  it("caps at two tags", () => {
    expect(pathToTags("a/b/c/d/foo.html")).toEqual(["d", "c"]);
  });
  it("skips a dir that slugifies to nothing (the slug==='-' guard)", () => {
    // "é" → "-" after slugify → skipped, so only "checks" survives.
    expect(pathToTags("é/checks/foo.html")).toEqual(["checks"]);
  });
  it("drops the empty leading segment from an absolute path", () => {
    expect(pathToTags("/incidents/checks/foo.html")).toEqual(["checks", "incidents"]);
  });
});

describe("slugifyTag", () => {
  it("lowercases and dashes non-alphanumerics", () => {
    expect(slugifyTag("Hello World!")).toBe("hello-world");
  });
  it("collapses runs and trims leading/trailing dashes", () => {
    expect(slugifyTag("  --Foo__Bar--  ")).toBe("foo-bar");
  });
  it("drops non-ascii letters", () => {
    expect(slugifyTag("café")).toBe("caf");
  });
  it("returns empty for all-symbol input", () => {
    expect(slugifyTag("!!!")).toBe("");
  });
});

describe("tagsFor", () => {
  it("prefers server tags when present", () => {
    expect(tagsFor(makeDoc({ tags: ["explicit"], path: "x/y/z.html" }))).toEqual(["explicit"]);
  });
  it("falls back to path-derived tags when tags is empty or null", () => {
    expect(tagsFor(makeDoc({ tags: [], path: "incidents/checks/foo.html" }))).toEqual([
      "checks",
      "incidents",
    ]);
    expect(tagsFor(makeDoc({ tags: null, path: "incidents/checks/foo.html" }))).toEqual([
      "checks",
      "incidents",
    ]);
  });
});

describe("isIndexPage", () => {
  it("is true for a hand-authored index.html", () => {
    expect(isIndexPage(makeDoc({ path: "foo/index.html", kb_category: null }))).toBe(true);
  });
  it("is case-insensitive on the filename", () => {
    expect(isIndexPage(makeDoc({ path: "foo/INDEX.HTML" }))).toBe(true);
  });
  it("excludes the generated index-page category", () => {
    expect(isIndexPage(makeDoc({ path: "index.html", kb_category: "index-page" }))).toBe(false);
  });
  it("is false for a normal page", () => {
    expect(isIndexPage(makeDoc({ path: "foo/bar.html" }))).toBe(false);
  });
});

describe("relativeAge (re-export from lib/time — seconds→ms for now)", () => {
  // Canonical pins live in time.test.ts; this keeps derive callers' import
  // path covered. `now` is milliseconds (time.ts API).
  const nowMs = 1_000_000_000_000;
  const nowSecs = Math.floor(nowMs / 1000);
  it("returns '' for null / undefined", () => {
    expect(relativeAge(null, nowMs)).toBe("");
    expect(relativeAge(undefined, nowMs)).toBe("");
  });
  it("buckets sub-minute as 'now'", () => {
    expect(relativeAge(nowSecs - 30, nowMs)).toBe("now");
  });
  it("buckets minutes / hours / days / weeks / months / years", () => {
    expect(relativeAge(nowSecs - 90, nowMs)).toBe("1m");
    expect(relativeAge(nowSecs - 3700, nowMs)).toBe("1h");
    expect(relativeAge(nowSecs - 2 * 86400, nowMs)).toBe("2d");
    expect(relativeAge(nowSecs - 20 * 86400, nowMs)).toBe("2w");
    expect(relativeAge(nowSecs - 70 * 86400, nowMs)).toBe("2mo");
    expect(relativeAge(nowSecs - 400 * 86400, nowMs)).toBe("1y");
  });
});

describe("isNew", () => {
  const now = 1_000_000_000;
  it("is true when indexed within 24h", () => {
    expect(isNew(makeDoc({ indexed_at_unix: now - 3600 }), now)).toBe(true);
  });
  it("is false when older than 24h", () => {
    expect(isNew(makeDoc({ indexed_at_unix: now - 90_000 }), now)).toBe(false);
  });
  it("is false exactly at the 24h boundary (now - t < 86400 is strict)", () => {
    expect(isNew(makeDoc({ indexed_at_unix: now - 86_400 }), now)).toBe(false);
  });
  it("falls back to mtime when indexed is missing", () => {
    expect(isNew(makeDoc({ indexed_at_unix: null, mtime_unix: now - 100 }), now)).toBe(true);
  });
  it("is false when both timestamps are missing", () => {
    expect(isNew(makeDoc({ indexed_at_unix: null, mtime_unix: null }), now)).toBe(false);
  });
});
