import { describe, expect, it } from "vitest";
import { repoOf } from "./useActiveRepo";

// `repoOf` is a pure function of (pathname, search) — `matchPath` needs no
// Router context, so this is testable directly without rendering a hook.
describe("repoOf (F1 — active-repo URL parsing)", () => {
  it("reads the repo from the reader's path param", () => {
    expect(repoOf("/r/fixture", new URLSearchParams())).toBe("fixture");
    expect(repoOf("/r/fixture/src/lib.rs", new URLSearchParams())).toBe("fixture");
  });

  it("falls back to ?repo= off the reader path", () => {
    expect(repoOf("/search", new URLSearchParams("repo=fixture"))).toBe("fixture");
  });

  it("path repo wins over a stray ?repo= on the reader route", () => {
    expect(repoOf("/r/fixture/x.rs", new URLSearchParams("repo=other"))).toBe("fixture");
  });

  it("is null on an unscoped view with no repo anywhere", () => {
    expect(repoOf("/", new URLSearchParams())).toBeNull();
    expect(repoOf("/search", new URLSearchParams())).toBeNull();
  });

  it("treats an empty ?repo= as absent", () => {
    expect(repoOf("/search", new URLSearchParams("repo="))).toBeNull();
  });

  it("doesn't match a non-reader path as a repo param", () => {
    expect(repoOf("/session/abc/diff", new URLSearchParams())).toBeNull();
  });
});
