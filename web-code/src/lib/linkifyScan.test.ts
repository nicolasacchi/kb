import { describe, expect, it } from "vitest";
import type { HighlightClass, Span } from "../api/types";
import { scanLinkTokens } from "./linkifyScan";

const encoder = new TextEncoder();

/// Build a `Span` covering `needle`'s FIRST occurrence in `content`,
/// computed via UTF-8 byte lengths (not UTF-16 indices) — same convention
/// `decorations.test.ts` uses for its own multi-byte cases, since the real
/// `Span[]` the server sends is always byte-offset.
function byteSpan(content: string, needle: string, cls: HighlightClass): Span {
  const utf16At = content.indexOf(needle);
  if (utf16At === -1) throw new Error(`fixture bug: ${JSON.stringify(needle)} not found in content`);
  const byte_start = encoder.encode(content.slice(0, utf16At)).length;
  const byte_len = encoder.encode(needle).length;
  return { byte_start, byte_len, class: cls };
}

describe("scanLinkTokens", () => {
  it("finds a URL inside a comment span", () => {
    const content = "// see https://example.com/docs for details\nfn f() {}\n";
    const comment = "// see https://example.com/docs for details";
    const spans = [byteSpan(content, comment, "comment")];
    const tokens = scanLinkTokens(content, spans);
    expect(tokens).toHaveLength(1);
    expect(tokens[0]).toMatchObject({ kind: "url", value: "https://example.com/docs" });
    expect(content.slice(tokens[0].from, tokens[0].to)).toBe("https://example.com/docs");
  });

  it("finds a repo-relative-looking path inside a string span", () => {
    const content = 'let p = "src/lib.rs";\n';
    const spans = [byteSpan(content, '"src/lib.rs"', "string")];
    const tokens = scanLinkTokens(content, spans);
    expect(tokens).toHaveLength(1);
    expect(tokens[0]).toMatchObject({ kind: "path", value: "src/lib.rs" });
  });

  it("finds a bare filename with a recognized source extension, even with no slash", () => {
    // A file at a repo ROOT has no directory prefix to offer — "see lib.rs"
    // is an extremely common real-world comment shape, and must still
    // linkify (see `looksLikePath`'s doc for the slash-less allowlist).
    const content = "// see lib.rs for the implementation\n";
    const spans = [byteSpan(content, content.trim(), "comment")];
    const tokens = scanLinkTokens(content, spans);
    expect(tokens).toHaveLength(1);
    expect(tokens[0]).toMatchObject({ kind: "path", value: "lib.rs" });
  });

  it("rejects a slash-less dotted token whose extension isn't recognized", () => {
    const content = "// see e.g. v1.2.3 for context\n";
    const spans = [byteSpan(content, "// see e.g. v1.2.3 for context", "comment")];
    expect(scanLinkTokens(content, spans)).toEqual([]);
  });

  it("finds a Kb-Session trailer value", () => {
    const content = "// Kb-Session: 12345678-1234-1234-1234-123456789abc\n";
    const spans = [byteSpan(content, content.trim(), "comment")];
    const tokens = scanLinkTokens(content, spans);
    expect(tokens).toHaveLength(1);
    expect(tokens[0]).toMatchObject({
      kind: "session",
      value: "12345678-1234-1234-1234-123456789abc",
    });
  });

  it("never scans a code-class span (e.g. a keyword/function token)", () => {
    // Same text a path/URL scan WOULD match, but tagged as code, not a
    // comment/string — must be ignored entirely.
    const content = 'const x = "https://example.com/lib.rs";\n';
    const spans: Span[] = [
      byteSpan(content, "const", "keyword"),
      byteSpan(content, "x", "variable"),
    ];
    expect(scanLinkTokens(content, spans)).toEqual([]);
  });

  it("does not double-linkify a path-shaped tail of an already-matched URL", () => {
    const content = "// https://example.com/path/to/file.rs is the source\n";
    const spans = [byteSpan(content, content.trim(), "comment")];
    const tokens = scanLinkTokens(content, spans);
    expect(tokens).toHaveLength(1);
    expect(tokens[0].kind).toBe("url");
  });

  it("returns tokens in ascending order across multiple spans", () => {
    const content = "// first path/one.rs\nfn f() {}\n// second path/two.rs\n";
    const spans = [
      byteSpan(content, "// first path/one.rs", "comment"),
      byteSpan(content, "// second path/two.rs", "comment"),
    ];
    const tokens = scanLinkTokens(content, spans);
    expect(tokens.map((t) => t.value)).toEqual(["path/one.rs", "path/two.rs"]);
    expect(tokens[0].from).toBeLessThan(tokens[1].from);
  });

  it("maps UTF-16 offsets correctly when the comment contains multi-byte text", () => {
    // "café" precedes the path — byte offsets differ from utf16 offsets
    // from that point on; the returned token's [from, to) must still slice
    // out the right substring of `content` (a UTF-16 JS string).
    const content = "// café notes: src/lib.rs\n";
    const spans = [byteSpan(content, content.trim(), "comment")];
    const tokens = scanLinkTokens(content, spans);
    expect(tokens).toHaveLength(1);
    expect(content.slice(tokens[0].from, tokens[0].to)).toBe("src/lib.rs");
  });

  it("finds a bare commit sha (7-40 lowercase hex) inside a comment", () => {
    const content = "// see abc1234 for the fix\n";
    const spans = [byteSpan(content, content.trim(), "comment")];
    const tokens = scanLinkTokens(content, spans);
    expect(tokens).toHaveLength(1);
    expect(tokens[0]).toMatchObject({ kind: "sha", value: "abc1234" });
  });

  it("finds a full 40-char sha", () => {
    const sha = "a".repeat(40);
    const content = `// fixed in ${sha}\n`;
    const spans = [byteSpan(content, content.trim(), "comment")];
    const tokens = scanLinkTokens(content, spans);
    expect(tokens).toHaveLength(1);
    expect(tokens[0]).toMatchObject({ kind: "sha", value: sha });
  });

  it("does not match a hex run shorter than 7 chars", () => {
    const content = "// see abc123 for the fix\n";
    const spans = [byteSpan(content, content.trim(), "comment")];
    expect(scanLinkTokens(content, spans)).toEqual([]);
  });

  it("does not partially match a hex-prefixed identifier longer than 40 chars", () => {
    const content = `// ${"a".repeat(45)} is not a sha\n`;
    const spans = [byteSpan(content, content.trim(), "comment")];
    expect(scanLinkTokens(content, spans)).toEqual([]);
  });

  it("does not match a hex-looking prefix immediately followed by a non-hex word char", () => {
    // Word-bounded: "abc1234xyz" has no boundary between "4" and "x" (both
    // \w), so the whole token must be rejected, not truncated to "abc1234".
    const content = "// see abc1234xyz for context\n";
    const spans = [byteSpan(content, content.trim(), "comment")];
    expect(scanLinkTokens(content, spans)).toEqual([]);
  });

  it("is case-sensitive — uppercase hex never linkifies as a sha", () => {
    const content = "// see ABC1234 for the fix\n";
    const spans = [byteSpan(content, content.trim(), "comment")];
    expect(scanLinkTokens(content, spans)).toEqual([]);
  });

  it("does not double-linkify a sha-shaped substring of an already-matched URL", () => {
    const content = "// https://example.com/commit/abc1234def is the source\n";
    const spans = [byteSpan(content, content.trim(), "comment")];
    const tokens = scanLinkTokens(content, spans);
    expect(tokens).toHaveLength(1);
    expect(tokens[0].kind).toBe("url");
  });

  it("returns an empty array when there are no spans", () => {
    expect(scanLinkTokens("// src/lib.rs\n", [])).toEqual([]);
  });

  it("returns an empty array when no span is comment/string class", () => {
    const content = "fn run() {}\n";
    const spans = [byteSpan(content, "run", "function")];
    expect(scanLinkTokens(content, spans)).toEqual([]);
  });
});
