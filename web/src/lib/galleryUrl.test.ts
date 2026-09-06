import { describe, expect, it } from "vitest";
import { galleryUrl } from "./galleryUrl";

describe("galleryUrl", () => {
  // invariant:35
  it("pins the kb and omits empty axes", () => {
    expect(galleryUrl("research")).toBe("/?kb=research");
    expect(galleryUrl(null)).toBe("/");
  });

  it("joins tags csv and sets category/folder", () => {
    expect(galleryUrl("kb", { tags: ["rust", "atlas"] })).toBe(
      "/?kb=kb&tags=rust%2Catlas",
    );
    expect(galleryUrl("kb", { category: "research" })).toBe(
      "/?kb=kb&category=research",
    );
    expect(galleryUrl("kb", { folder: "changelog/daily" })).toBe(
      "/?kb=kb&folder=changelog%2Fdaily",
    );
  });

  it("drops an empty folder (root → unfiltered gallery)", () => {
    expect(galleryUrl("kb", { folder: "" })).toBe("/?kb=kb");
    expect(galleryUrl("kb", { folder: null })).toBe("/?kb=kb");
  });

  it("floors the mtime window bounds and allows open-ended", () => {
    expect(galleryUrl("kb", { from: 1000.9, to: 2000.1, sort: "recent" })).toBe(
      "/?kb=kb&from=1000&to=2000&sort=recent",
    );
    expect(galleryUrl("kb", { from: 500 })).toBe("/?kb=kb&from=500");
  });

  it("keeps a zero bound (a real timestamp, not 'unset')", () => {
    expect(galleryUrl("kb", { from: 0 })).toBe("/?kb=kb&from=0");
  });

  // invariant:35
  it("joins the read facet csv, order preserved, and omits when empty", () => {
    expect(galleryUrl("kb", { read: ["unread"] })).toBe("/?kb=kb&read=unread");
    expect(galleryUrl("kb", { read: ["never-opened", "in_progress"] })).toBe(
      "/?kb=kb&read=never-opened%2Cin_progress",
    );
    expect(galleryUrl("kb", { read: [] })).toBe("/?kb=kb");
  });

  // invariant:35
  it("joins the ids-set csv, order preserved, and omits when empty", () => {
    expect(galleryUrl("kb", { ids: ["a1b2c3"] })).toBe("/?kb=kb&ids=a1b2c3");
    expect(galleryUrl("kb", { ids: ["a1", "b2", "c3"] })).toBe(
      "/?kb=kb&ids=a1%2Cb2%2Cc3",
    );
    expect(galleryUrl("kb", { ids: [] })).toBe("/?kb=kb");
    expect(galleryUrl("kb", {})).toBe("/?kb=kb");
  });

  // v0.33 Y1 — folder_exact=1 only when true AND folder is set.
  it("serialises folder_exact=1 only when true and folder is set", () => {
    expect(galleryUrl("kb", { folder: "a/b", folderExact: true })).toBe(
      "/?kb=kb&folder=a%2Fb&folder_exact=1",
    );
    // false / absent leave the URL byte-identical to pre-Y1.
    expect(galleryUrl("kb", { folder: "a/b", folderExact: false })).toBe(
      "/?kb=kb&folder=a%2Fb",
    );
    expect(galleryUrl("kb", { folder: "a/b" })).toBe("/?kb=kb&folder=a%2Fb");
    // empty/null folder drops both axes (no orphan folder_exact).
    expect(galleryUrl("kb", { folder: "", folderExact: true })).toBe(
      "/?kb=kb",
    );
    expect(galleryUrl("kb", { folder: null, folderExact: true })).toBe(
      "/?kb=kb",
    );
    expect(galleryUrl("kb", { folderExact: true })).toBe("/?kb=kb");
  });
});
