import { describe, expect, it } from "vitest";
import {
  emptyRecipeUrlState,
  parseRecipeSearch,
  recipeCliLine,
  recipeHomeUrl,
  recipeParamDefaultString,
  recipeParamToQueryValue,
  recipeParamValidate,
  recipeReplayUrl,
  recipeRunUrl,
  recipeValidateAll,
  type RecipeUrlState,
} from "./recipeUrl";
import type { KbcParamSpec } from "../api/types";

describe("recipeHomeUrl / recipeReplayUrl", () => {
  it("builds the bare home path", () => {
    expect(recipeHomeUrl("acme/widgets")).toBe("/r/acme%2Fwidgets/~recipes");
  });
  it("builds a materialised-run replay path", () => {
    expect(recipeReplayUrl("acme/widgets", "abc123")).toBe("/r/acme%2Fwidgets/~recipes/runs/abc123");
  });
  it("encodes a run id", () => {
    expect(recipeReplayUrl("r", "a b")).toBe("/r/r/~recipes/runs/a%20b");
  });
});

describe("recipeRunUrl — golden param order (slug, intent, scope, limit, view, p.*, ctx.*)", () => {
  it("is bare with no state", () => {
    expect(recipeRunUrl("r")).toBe("/r/r/~recipes");
    expect(recipeRunUrl("r", emptyRecipeUrlState())).toBe("/r/r/~recipes");
  });

  it("orders every field slug, intent, scope, limit, view regardless of insertion order", () => {
    const state: RecipeUrlState = {
      view: "hot",
      ctx: { path: "src/main.rs" },
      params: { top: "25" },
      limit: 50,
      scope: "orienting-scope",
      slug: "orient:hot-and-cold",
      intent: "orienting",
    };
    const url = recipeRunUrl("r", state);
    const order = ["slug=", "intent=", "scope=", "limit=", "view=", "p.top=", "ctx.path="].map((k) =>
      url.indexOf(k),
    );
    expect(order.every((i) => i >= 0)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
  });

  it("golden: full state round-trips through one deterministic string", () => {
    const state: RecipeUrlState = {
      slug: "hygiene:aged-todos",
      scope: "src/**",
      limit: 100,
      view: "rows",
      params: { top: "10", min_revisions: "3" },
      ctx: { path: "src/main.rs" },
    };
    const url = recipeRunUrl("acme", state);
    expect(url).toBe(
      "/r/acme/~recipes?slug=hygiene%3Aaged-todos&scope=src%2F**&limit=100&view=rows" +
        "&p.min_revisions=3&p.top=10&ctx.path=src%2Fmain.rs",
    );
  });

  it("sorts p.* and ctx.* keys alphabetically regardless of object order", () => {
    const a = recipeRunUrl("r", { ...emptyRecipeUrlState(), params: { zeta: "1", alpha: "2" } });
    const b = recipeRunUrl("r", { ...emptyRecipeUrlState(), params: { alpha: "2", zeta: "1" } });
    expect(a).toBe(b);
    expect(a).toBe("/r/r/~recipes?p.alpha=2&p.zeta=1");
  });

  it("omits an empty param/ctx value rather than sending an empty string", () => {
    const url = recipeRunUrl("r", { ...emptyRecipeUrlState(), params: { top: "" }, ctx: { path: "" } });
    expect(url).toBe("/r/r/~recipes");
  });

  it("drops a non-finite/zero/negative limit", () => {
    for (const bad of [0, -5, NaN, Infinity]) {
      expect(recipeRunUrl("r", { ...emptyRecipeUrlState(), limit: bad })).toBe("/r/r/~recipes");
    }
  });
});

describe("parseRecipeSearch", () => {
  it("is the inverse of recipeRunUrl for a full state", () => {
    const state: RecipeUrlState = {
      slug: "review:blast-radius",
      intent: "reviewing",
      scope: "src/**",
      limit: 250,
      view: "rows",
      params: { since: "2026-01-01" },
      ctx: { path: "a/b.rs", ref: "HEAD~1" },
    };
    const url = recipeRunUrl("r", state);
    const qs = url.slice(url.indexOf("?") + 1);
    expect(parseRecipeSearch(qs)).toEqual(state);
    expect(parseRecipeSearch(`?${qs}`)).toEqual(state);
    expect(parseRecipeSearch(new URLSearchParams(qs))).toEqual(state);
  });

  it("total: junk input never throws and yields an empty state", () => {
    expect(parseRecipeSearch("")).toEqual(emptyRecipeUrlState());
    expect(parseRecipeSearch("limit=not-a-number")).toEqual(emptyRecipeUrlState());
    expect(parseRecipeSearch("limit=-5")).toEqual(emptyRecipeUrlState());
    expect(parseRecipeSearch("limit=0")).toEqual(emptyRecipeUrlState());
  });

  it("ignores an unrelated query key and a bare p./ctx. prefix with no name", () => {
    expect(parseRecipeSearch("foo=bar&p.=1&ctx.=2")).toEqual(emptyRecipeUrlState());
  });
});

describe("recipeParamToQueryValue", () => {
  it("normalizes bool to the literal true/false string", () => {
    expect(recipeParamToQueryValue("bool", "true")).toBe("true");
    expect(recipeParamToQueryValue("bool", "anything-else")).toBe("false");
  });
  it("passes other types through verbatim", () => {
    for (const t of ["string", "int", "float", "enum", "path", "symbol", "ref"] as const) {
      expect(recipeParamToQueryValue(t, "25")).toBe("25");
    }
  });
});

describe("recipeParamValidate", () => {
  const intSpec: KbcParamSpec = { name: "top", type: "int", required: true, min: 1, max: 500 };
  const floatSpec: KbcParamSpec = { name: "thresh", type: "float", required: false, min: 0, max: 1 };
  const boolSpec: KbcParamSpec = { name: "flag", type: "bool", required: false };
  const enumSpec: KbcParamSpec = { name: "mode", type: "enum", required: true, values: ["a", "b"] };
  const stringSpec: KbcParamSpec = { name: "q", type: "string", required: false };

  it("required + empty ⇒ error naming the field", () => {
    expect(recipeParamValidate(intSpec, "")).toBe("top is required");
  });
  it("optional + empty ⇒ null (omit, don't send)", () => {
    expect(recipeParamValidate(floatSpec, "")).toBeNull();
  });
  it("int: non-integer, and each inclusive bound", () => {
    expect(recipeParamValidate(intSpec, "3.5")).toMatch(/whole number/);
    expect(recipeParamValidate(intSpec, "0")).toMatch(/≥ 1/);
    expect(recipeParamValidate(intSpec, "501")).toMatch(/≤ 500/);
    expect(recipeParamValidate(intSpec, "1")).toBeNull();
    expect(recipeParamValidate(intSpec, "500")).toBeNull();
  });
  it("float: NaN and bounds", () => {
    expect(recipeParamValidate(floatSpec, "nope")).toMatch(/must be a number/);
    expect(recipeParamValidate(floatSpec, "-0.1")).toMatch(/≥ 0/);
    expect(recipeParamValidate(floatSpec, "1.1")).toMatch(/≤ 1/);
    expect(recipeParamValidate(floatSpec, "0.5")).toBeNull();
  });
  it("bool: only true/false", () => {
    expect(recipeParamValidate(boolSpec, "true")).toBeNull();
    expect(recipeParamValidate(boolSpec, "false")).toBeNull();
    expect(recipeParamValidate(boolSpec, "yes")).toMatch(/true or false/);
  });
  it("enum: membership in values", () => {
    expect(recipeParamValidate(enumSpec, "a")).toBeNull();
    expect(recipeParamValidate(enumSpec, "c")).toMatch(/one of a, b/);
  });
  it("string/path/symbol/ref: any non-empty value is accepted client-side", () => {
    expect(recipeParamValidate(stringSpec, "anything")).toBeNull();
  });
});

describe("recipeValidateAll", () => {
  it("collects one error per failing field, none for a valid set", () => {
    const specs: KbcParamSpec[] = [
      { name: "top", type: "int", required: true, min: 1, max: 500 },
      { name: "q", type: "string", required: false },
    ];
    expect(recipeValidateAll(specs, { top: "", q: "x" })).toEqual([
      { name: "top", message: "top is required" },
    ]);
    expect(recipeValidateAll(specs, { top: "10", q: "" })).toEqual([]);
  });
});

describe("recipeParamDefaultString", () => {
  it("stringifies every default shape", () => {
    expect(recipeParamDefaultString({ name: "a", type: "int", required: false, default: 25 })).toBe(
      "25",
    );
    expect(
      recipeParamDefaultString({ name: "a", type: "bool", required: false, default: true }),
    ).toBe("true");
    expect(
      recipeParamDefaultString({ name: "a", type: "bool", required: false, default: false }),
    ).toBe("false");
    expect(
      recipeParamDefaultString({ name: "a", type: "string", required: false, default: "x" }),
    ).toBe("x");
    expect(
      recipeParamDefaultString({ name: "a", type: "string", required: false, default: ["x", "y"] }),
    ).toBe("x,y");
    expect(recipeParamDefaultString({ name: "a", type: "int", required: false })).toBe("");
  });
});

describe("recipeCliLine", () => {
  it("bare slug+repo", () => {
    expect(recipeCliLine({ slug: "orient:hot-and-cold", repo: "acme", params: {}, ctx: {} })).toBe(
      "kb-code recipe run orient:hot-and-cold --repo acme",
    );
  });
  it("includes scope, limit, sorted p./ctx., materialise and save-as-set", () => {
    const line = recipeCliLine({
      slug: "hygiene:aged-todos",
      repo: "acme",
      scope: "src/**",
      limit: 50,
      params: { top: "10", zeta: "1" },
      ctx: { ref: "HEAD~1", path: "a.rs" },
      materialise: true,
      saveAsSet: "my set",
    });
    expect(line).toBe(
      "kb-code recipe run hygiene:aged-todos --repo acme --scope 'src/**' --limit 50 " +
        "--p top=10 --p zeta=1 --ctx path=a.rs --ctx 'ref=HEAD~1' --materialise --save-as-set 'my set'",
    );
  });
  it("omits empty param/ctx values, absent scope, absent limit", () => {
    const line = recipeCliLine({
      slug: "s",
      repo: "r",
      params: { top: "" },
      ctx: { path: "" },
    });
    expect(line).toBe("kb-code recipe run s --repo r");
  });
  it("single-quotes a whole `name=value` arg containing whitespace or shell metacharacters", () => {
    const line = recipeCliLine({ slug: "s", repo: "r", params: { q: "a b" }, ctx: {} });
    expect(line).toBe("kb-code recipe run s --repo r --p 'q=a b'");
  });
  it("escapes an embedded single quote", () => {
    const line = recipeCliLine({ slug: "s", repo: "r", params: { q: "it's" }, ctx: {} });
    expect(line).toContain(String.raw`'q=it'\''s'`);
  });
  it("drops a non-finite/zero/negative limit the same way recipeRunUrl does", () => {
    for (const bad of [0, -5, NaN]) {
      expect(recipeCliLine({ slug: "s", repo: "r", limit: bad, params: {}, ctx: {} })).toBe(
        "kb-code recipe run s --repo r",
      );
    }
  });
});
