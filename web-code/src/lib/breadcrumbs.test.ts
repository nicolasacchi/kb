import { describe, expect, it } from "vitest";
import { buildBreadcrumbs, crumbHref, diffUrl, readerUrl } from "./breadcrumbs";

describe("buildBreadcrumbs", () => {
  it("is just the repo crumb (marked current) at the repo root", () => {
    expect(buildBreadcrumbs("kb", "")).toEqual([{ label: "kb", path: "", isCurrent: true }]);
  });

  it("builds one crumb per path segment with accumulating paths", () => {
    expect(buildBreadcrumbs("kb", "src/lib.rs")).toEqual([
      { label: "kb", path: "", isCurrent: false },
      { label: "src", path: "src", isCurrent: false },
      { label: "lib.rs", path: "src/lib.rs", isCurrent: true },
    ]);
  });

  it("drops empty segments from stray slashes", () => {
    expect(buildBreadcrumbs("kb", "/src//lib.rs/")).toEqual([
      { label: "kb", path: "", isCurrent: false },
      { label: "src", path: "src", isCurrent: false },
      { label: "lib.rs", path: "src/lib.rs", isCurrent: true },
    ]);
  });
});

describe("readerUrl", () => {
  it("builds the repo-root URL with no path", () => {
    expect(readerUrl("kb", "")).toBe("/r/kb");
  });

  it("builds a nested-path URL", () => {
    expect(readerUrl("kb", "src/lib.rs")).toBe("/r/kb/src/lib.rs");
  });

  it("appends ?ref= when a ref is given", () => {
    expect(readerUrl("kb", "src/lib.rs", "main")).toBe("/r/kb/src/lib.rs?ref=main");
  });

  it("percent-encodes repo, path segments, and ref", () => {
    expect(readerUrl("my repo", "a b/c.rs", "feature/x")).toBe(
      "/r/my%20repo/a%20b/c.rs?ref=feature%2Fx",
    );
  });

  it("omits the query string when ref is undefined", () => {
    expect(readerUrl("kb", "a.rs", undefined)).toBe("/r/kb/a.rs");
  });

  it("appends ?line= when a line is given with no ref", () => {
    expect(readerUrl("kb", "src/lib.rs", undefined, 42)).toBe("/r/kb/src/lib.rs?line=42");
  });

  it("combines ?ref= and &line= when both are given", () => {
    expect(readerUrl("kb", "src/lib.rs", "main", 7)).toBe("/r/kb/src/lib.rs?ref=main&line=7");
  });

  it("omits a non-positive or undefined line", () => {
    expect(readerUrl("kb", "a.rs", undefined, 0)).toBe("/r/kb/a.rs");
    expect(readerUrl("kb", "a.rs", undefined, undefined)).toBe("/r/kb/a.rs");
  });
});

// V70-A3S — `crumbHref` is what `Breadcrumbs.tsx` actually calls now
// (instead of `readerUrl`, whose narrower 4-arg signature has no `pane2`
// slot at all): it carries the current line + a live split through so a
// breadcrumb click doesn't silently drop them.
describe("crumbHref", () => {
  it("mirrors readerUrl's output when only ref is given", () => {
    expect(crumbHref("kb", "src/lib.rs", { ref: "main" })).toBe("/r/kb/src/lib.rs?ref=main");
  });

  it("omits the query string when opts is undefined", () => {
    expect(crumbHref("kb", "src/lib.rs")).toBe("/r/kb/src/lib.rs");
  });

  it("carries the current line through", () => {
    expect(crumbHref("kb", "src", { ref: "main", line: 42 })).toBe("/r/kb/src?ref=main&line=42");
  });

  it("omits a non-positive or undefined line, same as codeUrl", () => {
    expect(crumbHref("kb", "a.rs", { line: 0 })).toBe("/r/kb/a.rs");
    expect(crumbHref("kb", "a.rs", { line: undefined })).toBe("/r/kb/a.rs");
  });

  it("carries a live pane2 split through", () => {
    expect(
      crumbHref("kb", "src/lib.rs", {
        ref: "main",
        pane2: { path: "src/other.rs", ref: "main", line: 10 },
      }),
    ).toBe("/r/kb/src/lib.rs?ref=main&pane2=src%2Fother.rs%40main%3A10");
  });

  it("omits pane2= when pane2 is null or undefined", () => {
    expect(crumbHref("kb", "src/lib.rs", { pane2: null })).toBe("/r/kb/src/lib.rs");
    expect(crumbHref("kb", "src/lib.rs", { pane2: undefined })).toBe("/r/kb/src/lib.rs");
  });

  it("combines ref, line, and pane2 in codeUrl's param order", () => {
    expect(
      crumbHref("kb", "src", {
        ref: "main",
        line: 7,
        pane2: { path: "other.rs" },
      }),
    ).toBe("/r/kb/src?ref=main&line=7&pane2=other.rs%40%3A");
  });

  it("encodes repo/path segments like readerUrl", () => {
    expect(crumbHref("my repo", "a b/c.rs")).toBe("/r/my%20repo/a%20b/c.rs");
  });
});

describe("diffUrl", () => {
  it("builds a diff URL with from only (working tree default)", () => {
    expect(diffUrl("kb", "src/lib.rs", "main")).toBe("/r/kb/src/lib.rs/~diff?from=main");
  });

  it("builds a diff URL with both from and to", () => {
    expect(diffUrl("kb", "src/lib.rs", "v1", "v2")).toBe(
      "/r/kb/src/lib.rs/~diff?from=v1&to=v2",
    );
  });
});
