import { describe, expect, it } from "vitest";
import {
  parseRecipesSearch,
  recipeRequiresSince,
  recipesUrl,
} from "./recipesUrl";

describe("recipesUrl", () => {
  it("builds the bare sentinel", () => {
    expect(recipesUrl("kb")).toBe("/r/kb/~recipes");
  });

  it("includes recipe/since/limit in stable order", () => {
    expect(
      recipesUrl("kb", { recipe: "god-functions", since: "2024-01-01", limit: 50 }),
    ).toBe("/r/kb/~recipes?recipe=god-functions&since=2024-01-01&limit=50");
  });

  it("omits empty optional fields", () => {
    expect(recipesUrl("kb", { recipe: "god-functions" })).toBe(
      "/r/kb/~recipes?recipe=god-functions",
    );
  });

  it("percent-encodes the repo segment", () => {
    expect(recipesUrl("my repo", { recipe: "a" })).toBe(
      "/r/my%20repo/~recipes?recipe=a",
    );
  });
});

describe("parseRecipesSearch", () => {
  it("parses all fields", () => {
    expect(parseRecipesSearch("recipe=god-functions&since=main&limit=25")).toEqual({
      recipe: "god-functions",
      since: "main",
      limit: 25,
    });
  });

  it("accepts a leading ?", () => {
    expect(parseRecipesSearch("?recipe=x")).toEqual({ recipe: "x" });
  });

  it("drops non-positive limit", () => {
    expect(parseRecipesSearch("limit=0")).toEqual({});
    expect(parseRecipesSearch("limit=-3")).toEqual({});
    expect(parseRecipesSearch("limit=abc")).toEqual({});
  });
});

describe("recipeRequiresSince", () => {
  it("detects required since", () => {
    expect(recipeRequiresSince([{ name: "since", required: true }])).toBe(true);
    expect(recipeRequiresSince([{ name: "since", required: false }])).toBe(false);
    expect(recipeRequiresSince([])).toBe(false);
  });
});
