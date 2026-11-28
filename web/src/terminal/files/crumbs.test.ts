import { describe, expect, it } from "vitest";
import { crumbsOf } from "./crumbs";

describe("crumbsOf", () => {
  it("lists every segment when not collapsing", () => {
    expect(crumbsOf("a/b/c/d", false).map((c) => c.label)).toEqual(["a", "b", "c", "d"]);
    expect(crumbsOf("", true)).toEqual([]);
  });
  it("leaves one or two segments untouched when collapsing", () => {
    expect(crumbsOf("a", true).map((c) => c.label)).toEqual(["a"]);
    expect(crumbsOf("a/b", true).map((c) => [c.label, c.to])).toEqual([["a", "a"], ["b", "a/b"]]);
  });
  it("folds everything before the last two into one crumb that goes to the deepest hidden directory", () => {
    const c = crumbsOf("a/b/c/d/e", true);
    expect(c.map((x) => x.label)).toEqual(["…", "d", "e"]);
    expect(c[0]).toEqual({ label: "…", to: "a/b/c", collapsed: true });
    expect(c[1].to).toBe("a/b/c/d");
    expect(c[2].to).toBe("a/b/c/d/e");
    expect(crumbsOf("a/b/c", true).map((x) => x.label)).toEqual(["…", "b", "c"]);
  });
});
