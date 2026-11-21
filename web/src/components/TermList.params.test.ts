// FUTERMLIST — the `?term=` URL grammar of the /sessions page: one id grammar
// (the same one every kb surface accepts), every other param untouched.
import { describe, expect, it } from "vitest";
import { openTermParam, withTermParam } from "./TermList";

describe("?term= grammar", () => {
  it.each([
    ["abc123", "abc123"],
    ["a", "a"],
    ["z".repeat(32), "z".repeat(32)],
    ["t-e2e", "t-e2e"],
  ])("%s is an open terminal id", (v, want) => {
    expect(openTermParam(v)).toBe(want);
  });
  it.each([null, "", "ABC", "-a", "a_b", "a/b", "a b", "a".repeat(33), "../x"])("%s opens nothing", (v) => {
    expect(openTermParam(v)).toBeNull();
  });
  it("withTermParam sets and removes only term, keeping every other param and its order", () => {
    const base = new URLSearchParams("project=p&q=x");
    const on = withTermParam(base, "t9");
    expect(on.get("term")).toBe("t9");
    expect(on.get("project")).toBe("p");
    expect(on.get("q")).toBe("x");
    expect(base.has("term")).toBe(false);
    expect(withTermParam(on, null).toString()).toBe("project=p&q=x");
  });
  it("replaces an existing term instead of adding a second", () => {
    expect(withTermParam(new URLSearchParams("term=a1"), "b2").getAll("term")).toEqual(["b2"]);
  });
});
