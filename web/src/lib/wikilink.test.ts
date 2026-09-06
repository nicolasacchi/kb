import { describe, it, expect } from "vitest";
import {
  parseWikilink,
  normalizeTarget,
  splitWikilinks,
  rehypeWikilinks,
  type WikiResolved,
} from "./wikilink";

// A resolver stub: "Known" resolves, "Both" is ambiguous, everything else
// dangles. Mirrors the server's ResolvedLink.state contract.
const resolve = (t: string): WikiResolved | null => {
  if (t === "Known") return { state: "resolved", href: "/a/kb/known.md", title: "Known" };
  if (t === "Both") return { state: "ambiguous", title: undefined };
  return null;
};

describe("parseWikilink", () => {
  it("plain target", () => {
    expect(parseWikilink("Deploy checklist")).toEqual({ target: "Deploy checklist" });
  });
  it("target | alias (single pipe, trimmed)", () => {
    expect(parseWikilink("Deploy | the list ")).toEqual({
      target: "Deploy",
      alias: "the list",
    });
  });
  // invariant:29
  it("a SECOND pipe → null (matches comrak; stays literal, lock-step #29)", () => {
    expect(parseWikilink("a|b|c")).toBeNull();
  });
  it("empty target → null", () => {
    expect(parseWikilink("   ")).toBeNull();
    expect(parseWikilink("|only alias")).toBeNull();
  });
});

describe("normalizeTarget (mirrors kb_core::links::normalize_target)", () => {
  it("strips fragment, leading slash, backslashes", () => {
    expect(normalizeTarget("/ops/x.md#section")).toBe("ops/x.md");
    expect(normalizeTarget("  Title #frag ")).toBe("Title");
    expect(normalizeTarget("a\\b")).toBe("a/b");
  });
});

describe("splitWikilinks", () => {
  it("resolved link becomes an <a> with href + alias display", () => {
    const nodes = splitWikilinks("see [[Known|the doc]] now", resolve);
    expect(nodes.map((n) => n.type)).toEqual(["text", "element", "text"]);
    const a = nodes[1];
    expect(a.tagName).toBe("a");
    expect(a.properties?.href).toBe("/a/kb/known.md");
    expect(a.properties?.dataWikilink).toBe("true");
    expect(a.children?.[0]?.value).toBe("the doc");
  });

  it("dangling + ambiguous become unresolved spans", () => {
    const dangling = splitWikilinks("[[Missing]]", resolve)[0];
    expect(dangling.tagName).toBe("span");
    expect(dangling.properties?.className).toContain("kb-wikilink--unresolved");
    expect(dangling.children?.[0]?.value).toBe("Missing");

    const ambiguous = splitWikilinks("[[Both]]", resolve)[0];
    expect(ambiguous.tagName).toBe("span");
    expect(ambiguous.properties?.className).toContain("kb-wikilink--unresolved");
  });

  it("empty target stays literal text", () => {
    const nodes = splitWikilinks("a [[ ]] b", resolve);
    expect(nodes).toHaveLength(1);
    expect(nodes[0]).toEqual({ type: "text", value: "a [[ ]] b" });
  });

  it("multiple links in one string keep surrounding text", () => {
    const nodes = splitWikilinks("x [[Known]] y [[Missing]] z", resolve);
    expect(nodes.map((n) => n.tagName ?? n.type)).toEqual([
      "text",
      "a",
      "text",
      "span",
      "text",
    ]);
  });
});

describe("rehypeWikilinks", () => {
  const textNode = (value: string) => ({ type: "text", value });
  const el = (tagName: string, children: unknown[]) => ({
    type: "element",
    tagName,
    children,
  });

  it("rewrites text but skips code / pre / a subtrees", () => {
    const tree = {
      type: "root",
      children: [
        el("p", [textNode("see [[Known]]")]),
        el("code", [textNode("[[Known]]")]),
        el("a", [textNode("[[Known]]")]),
      ],
    };
    rehypeWikilinks(resolve)(tree as never);
    // <p> got a wikilink anchor injected.
    const p = (tree.children[0] as ReturnType<typeof el>).children as {
      tagName?: string;
    }[];
    expect(p.some((n) => n.tagName === "a")).toBe(true);
    // <code> + <a> untouched (single literal text child each).
    const code = (tree.children[1] as ReturnType<typeof el>).children as {
      type?: string;
      value?: string;
    }[];
    expect(code).toEqual([{ type: "text", value: "[[Known]]" }]);
    const anchor = (tree.children[2] as ReturnType<typeof el>).children as {
      type?: string;
      value?: string;
    }[];
    expect(anchor).toEqual([{ type: "text", value: "[[Known]]" }]);
  });
});
