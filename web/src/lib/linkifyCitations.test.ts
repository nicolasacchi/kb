import { describe, expect, it } from "vitest";
import { linkifyCitations, rehypeCitations, validCodeUrl } from "./linkifyCitations";

const CODE_URL = "https://kbc.example.com";

describe("validCodeUrl", () => {
  it("accepts an absolute URL", () => {
    expect(validCodeUrl(CODE_URL)).toBe(CODE_URL);
  });

  it("rejects null/undefined/empty/unparseable values", () => {
    expect(validCodeUrl(null)).toBeNull();
    expect(validCodeUrl(undefined)).toBeNull();
    expect(validCodeUrl("")).toBeNull();
    expect(validCodeUrl("/relative")).toBeNull();
  });
});

describe("linkifyCitations — sha shapes", () => {
  it("links a 7-hex sha", () => {
    const segs = linkifyCitations("fixed in a1b2c3d yesterday", CODE_URL);
    expect(segs[0]).toEqual({ kind: "text", text: "fixed in " });
    expect(segs[1]).toMatchObject({ kind: "sha", text: "a1b2c3d" });
    expect(segs[2]).toEqual({ kind: "text", text: " yesterday" });
    const href = (segs[1] as { href: string }).href;
    expect(new URL(href).searchParams.get("q")).toBe("a1b2c3d");
    expect(new URL(href).searchParams.has("repo")).toBe(false);
  });

  it("links a full 40-hex sha", () => {
    const sha40 = "a".repeat(40);
    const segs = linkifyCitations(`see ${sha40}`, CODE_URL);
    expect(segs[1]).toMatchObject({ kind: "sha", text: sha40 });
  });

  it("skips an exactly-12-hex token (ambiguous with a kb artifact id)", () => {
    const twelveHex = "a1b2c3d4e5f6"; // 12 lowercase-hex chars
    expect(twelveHex.length).toBe(12);
    const segs = linkifyCitations(`memory ${twelveHex} references this`, CODE_URL);
    expect(segs).toEqual([
      { kind: "text", text: `memory ${twelveHex} references this` },
    ]);
  });

  it("skips lengths outside {7..11, 40} (13-39, and <7)", () => {
    expect(linkifyCitations("a1b2c3", CODE_URL)).toEqual([
      { kind: "text", text: "a1b2c3" },
    ]);
    expect(linkifyCitations("a1b2c3d4e5f6a1b2c", CODE_URL)).toEqual([
      { kind: "text", text: "a1b2c3d4e5f6a1b2c" },
    ]);
  });

  it("never matches uppercase hex (grammar is lowercase-only)", () => {
    expect(linkifyCitations("A1B2C3D", CODE_URL)).toEqual([
      { kind: "text", text: "A1B2C3D" },
    ]);
  });
});

describe("linkifyCitations — path shapes", () => {
  it("links a path with a :line suffix", () => {
    const segs = linkifyCitations(
      "see crates/kb-core/src/memory.rs:401 for the fix",
      CODE_URL,
    );
    expect(segs[0]).toEqual({ kind: "text", text: "see " });
    expect(segs[1]).toMatchObject({
      kind: "path",
      text: "crates/kb-core/src/memory.rs:401",
      line: 401,
    });
    const href = (segs[1] as { href: string }).href;
    expect(new URL(href).searchParams.get("q")).toBe("#crates/kb-core/src/memory.rs");
    expect(new URL(href).searchParams.has("repo")).toBe(false);
    expect(segs[2]).toEqual({ kind: "text", text: " for the fix" });
  });

  it("links a path with no line number", () => {
    const segs = linkifyCitations("touched crates/kb-core/src/memory.rs today", CODE_URL);
    expect(segs[1]).toMatchObject({
      kind: "path",
      text: "crates/kb-core/src/memory.rs",
      line: null,
    });
  });

  it("prefers the longest whitelisted extension (js.erb over erb)", () => {
    const segs = linkifyCitations("app/views/widget.js.erb changed", CODE_URL);
    expect(segs[0]).toMatchObject({ kind: "path", text: "app/views/widget.js.erb" });
  });

  it("peels a trailing sentence period, keeping it as plain text", () => {
    const segs = linkifyCitations("see memory.rs.", CODE_URL);
    expect(segs[0]).toEqual({ kind: "text", text: "see " });
    expect(segs[1]).toMatchObject({ kind: "path", text: "memory.rs" });
    expect(segs[2]).toEqual({ kind: "text", text: "." });
  });

  it("does not link an extension the whitelist excludes (sql/env/config/html/md)", () => {
    expect(linkifyCitations("Arel.sql call", CODE_URL)[0]).toEqual({
      kind: "text",
      text: "Arel.sql call",
    });
    expect(linkifyCitations("process.env lookup", CODE_URL)[0]).toEqual({
      kind: "text",
      text: "process.env lookup",
    });
    expect(linkifyCitations("see plan-a2.html for details", CODE_URL)[0]).toEqual({
      kind: "text",
      text: "see plan-a2.html for details",
    });
  });

  it("does not link a bare compound-extension name (no real stem)", () => {
    // "html.erb" is itself a `CODE_EXTENSIONS` entry — author shorthand for
    // "an .html.erb file", never a citation to a file named exactly that
    // (mirrors the Rust W1.gate). The whole sentence has no OTHER
    // linkable token, so it passes through as one unchanged text segment.
    expect(linkifyCitations("an html.erb file", CODE_URL)).toEqual([
      { kind: "text", text: "an html.erb file" },
    ]);
  });
});

describe("linkifyCitations — no-code_url passthrough", () => {
  it("returns the text unchanged when codeUrl is null", () => {
    const text = "fixed in a1b2c3d, see crates/kb-core/src/memory.rs:401";
    expect(linkifyCitations(text, null)).toEqual([{ kind: "text", text }]);
  });

  it("returns the text unchanged when codeUrl fails validation", () => {
    const text = "fixed in a1b2c3d";
    expect(linkifyCitations(text, "not-a-url")).toEqual([{ kind: "text", text }]);
  });
});

describe("rehypeCitations — never double-linkifies inside an existing <a>/<code>", () => {
  type HastNode = {
    type?: string;
    tagName?: string;
    value?: string;
    properties?: Record<string, unknown>;
    children?: HastNode[];
  };

  function textNodesOf(node: HastNode): string[] {
    if (node.type === "text") return [node.value ?? ""];
    return (node.children ?? []).flatMap(textNodesOf);
  }

  it("linkifies a bare-text sha but leaves one already inside <a> alone", () => {
    const tree: HastNode = {
      type: "root",
      children: [
        {
          type: "element",
          tagName: "p",
          children: [
            // No trailing text after the sha, so this splits into exactly
            // two hast nodes (text + link) — keeps sibling indices simple.
            { type: "text", value: "bare a1b2c3d" },
            {
              type: "element",
              tagName: "a",
              properties: { href: "https://example.com" },
              children: [{ type: "text", value: "a1b2c3d already linked" }],
            },
          ],
        },
      ],
    };
    rehypeCitations(CODE_URL)(tree);

    const p = tree.children![0];
    expect(p.children).toHaveLength(3);
    // The bare-text run split into text + a NEW <a> for the sha.
    const [textChild, newLink] = p.children!;
    expect(textChild).toEqual({ type: "text", value: "bare " });
    expect(newLink.tagName).toBe("a");
    expect((newLink.properties!.href as string)).toContain("q=a1b2c3d");

    // The existing <a> subtree is untouched — same single text child,
    // never further split/wrapped.
    const existingLink = p.children![2];
    expect(existingLink.tagName).toBe("a");
    expect(existingLink.children).toEqual([
      { type: "text", value: "a1b2c3d already linked" },
    ]);
  });

  it("leaves text inside <code>/<pre> untouched", () => {
    const tree: HastNode = {
      type: "root",
      children: [
        {
          type: "element",
          tagName: "code",
          children: [{ type: "text", value: "crates/kb-core/src/memory.rs:401" }],
        },
      ],
    };
    rehypeCitations(CODE_URL)(tree);
    expect(textNodesOf(tree)).toEqual(["crates/kb-core/src/memory.rs:401"]);
  });

  it("is a no-op when codeUrl is null", () => {
    const tree: HastNode = {
      type: "root",
      children: [
        { type: "element", tagName: "p", children: [{ type: "text", value: "a1b2c3d" }] },
      ],
    };
    rehypeCitations(null)(tree);
    expect(textNodesOf(tree)).toEqual(["a1b2c3d"]);
  });
});
