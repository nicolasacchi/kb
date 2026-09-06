import { describe, expect, it } from "vitest";
import {
  composeDraftExclusion,
  DRAFT_TAG,
  isDraftView,
  referencesDraftTag,
} from "./draft";

describe("DRAFT_TAG", () => {
  it("is the plain, stampable-at-capture tag", () => {
    expect(DRAFT_TAG).toBe("draft");
  });
});

describe("referencesDraftTag()", () => {
  it("matches a bare tag:draft atom", () => {
    expect(referencesDraftTag("tag:draft")).toBe(true);
  });

  it("matches a NOT-negated atom", () => {
    expect(referencesDraftTag("NOT tag:draft")).toBe(true);
    expect(referencesDraftTag("not tag:draft")).toBe(true);
  });

  it("matches case-insensitively on key and value", () => {
    expect(referencesDraftTag("Tag:Draft")).toBe(true);
    expect(referencesDraftTag("TAG:DRAFT")).toBe(true);
  });

  it("matches a quoted value", () => {
    expect(referencesDraftTag('tag:"draft"')).toBe(true);
  });

  it("matches embedded in a larger query", () => {
    expect(referencesDraftTag("tag:rust AND tag:draft")).toBe(true);
    expect(referencesDraftTag("tag:draft folder:capture")).toBe(true);
  });

  it("does not match a different tag, even one that starts with draft", () => {
    expect(referencesDraftTag("tag:drafted")).toBe(false);
    expect(referencesDraftTag("tag:rust")).toBe(false);
    expect(referencesDraftTag("")).toBe(false);
  });
});

describe("composeDraftExclusion()", () => {
  // invariant:35 — appends via the same `NOT tag:x` DSL atom `galleryUrl`'s
  // read facet / #35's grammar already relies on; never a new atom.
  it("appends the exclusion to an empty query", () => {
    expect(composeDraftExclusion("")).toBe("NOT tag:draft");
  });

  it("treats a whitespace-only query as empty", () => {
    expect(composeDraftExclusion("   ")).toBe("NOT tag:draft");
  });

  it("appends the exclusion to a plain user query", () => {
    expect(composeDraftExclusion("tag:rust")).toBe("tag:rust NOT tag:draft");
  });

  it("leaves whitespace-padded user queries alone but still appends", () => {
    expect(composeDraftExclusion("  tag:rust  ")).toBe(
      "tag:rust NOT tag:draft",
    );
  });

  it("does not double-append when the user already excludes drafts", () => {
    const q = "NOT tag:draft";
    expect(composeDraftExclusion(q)).toBe(q);
  });

  it("does not append when the user is explicitly querying for drafts", () => {
    const q = "tag:draft";
    expect(composeDraftExclusion(q)).toBe(q);
  });

  it("is case-insensitive when detecting an existing reference", () => {
    const q = "Tag:Draft";
    expect(composeDraftExclusion(q)).toBe(q);
  });

  it("leaves original whitespace untouched when already referenced", () => {
    const q = "  tag:draft  ";
    expect(composeDraftExclusion(q)).toBe(q);
  });

  it("parenthesizes a top-level OR so the exclusion binds to every branch", () => {
    expect(composeDraftExclusion("tag:a OR tag:b")).toBe(
      "(tag:a OR tag:b) NOT tag:draft",
    );
  });

  it("skips grouping+appending when an OR query already references draft", () => {
    const q = "tag:draft OR tag:b";
    expect(composeDraftExclusion(q)).toBe(q);
  });
});

describe("isDraftView()", () => {
  it("is true when ?tags= includes draft", () => {
    expect(isDraftView(new URLSearchParams("tags=draft"))).toBe(true);
    expect(isDraftView(new URLSearchParams("tags=draft,research"))).toBe(true);
  });

  it("is false when ?tags= has other tags only", () => {
    expect(isDraftView(new URLSearchParams("tags=research,rust"))).toBe(false);
  });

  it("is true when ?q= references tag:draft", () => {
    expect(isDraftView(new URLSearchParams("q=tag:draft"))).toBe(true);
    expect(isDraftView(new URLSearchParams("q=NOT tag:draft"))).toBe(true);
  });

  it("is false with no params at all", () => {
    expect(isDraftView(new URLSearchParams())).toBe(false);
  });
});
