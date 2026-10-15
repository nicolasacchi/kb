import { describe, expect, it } from "vitest";
import { HERO_SNIPPET_LINES, HERO_SNIPPET_MAX_CHARS, heroSnippet } from "./homeSnippet";

describe("heroSnippet (OL-C7)", () => {
  it("keeps only the first HERO_SNIPPET_LINES lines", () => {
    const content = Array.from({ length: 30 }, (_, i) => `line ${i + 1}`).join("\n");
    const out = heroSnippet({ encoding: "utf8", content });
    expect(out?.split("\n")).toHaveLength(HERO_SNIPPET_LINES);
    expect(out?.endsWith(`line ${HERO_SNIPPET_LINES}`)).toBe(true);
  });

  it("refuses base64 (binary) and empty files", () => {
    expect(heroSnippet({ encoding: "base64", content: "AAAA" })).toBeNull();
    expect(heroSnippet({ encoding: "utf8", content: "" })).toBeNull();
    expect(heroSnippet({ encoding: "utf8", content: "\n\n" })).toBeNull();
  });

  it("caps a pathological single line", () => {
    const out = heroSnippet({ encoding: "utf8", content: "x".repeat(50_000) });
    expect(out?.length).toBe(HERO_SNIPPET_MAX_CHARS);
  });
});
