// v0.44 P2 (A2.f1) — a source-level gate for the private-note boundary.
//
// `fetchReview` returns the operator's `?visibility=all` file, notes
// included. Anything that hands review data across the operator boundary —
// the postMessage into the artifact iframe, the Claude prompt, the Markdown /
// portable exports — must project it through `publicView` first
// (privateNoteBoundary.test.tsx pins today's call sites by behaviour). This
// test pins the SHAPE so a NEW caller cannot forget: it fails when a source
// file calls one of those builders without `publicView(` in the same file,
// and when a file other than the two sanctioned ones starts asking the daemon
// for `visibility=all`.
import { describe, expect, it } from "vitest";

// Every non-test source file under src/ (generated bindings and the in-iframe
// annotator script excluded), as raw text. `import.meta.glob` rather than
// `node:fs` so the gate needs no Node typings and runs identically under
// `tsc -b` and vitest.
const sources = import.meta.glob(
  [
    "/src/**/*.ts",
    "/src/**/*.tsx",
    "!/src/**/*.test.ts",
    "!/src/**/*.test.tsx",
    "!/src/api/generated/**",
    "!/src/scripts/**",
  ],
  { query: "?raw", import: "default", eager: true },
) as Record<string, string>;

/// Code lines only: drop whole-line `//` / `///` / `*` comments so prose that
/// MENTIONS `visibility=all` or a builder does not count as a use.
function code(text: string): string {
  return text
    .split("\n")
    .filter((l) => !/^\s*\/\//.test(l) && !/^\s*\*/.test(l))
    .join("\n");
}

const files = Object.entries(sources).map(([path, text]) => ({
  path: path.replace(/^\/src\//, ""),
  code: code(text),
}));

describe("private-note boundary gate", () => {
  it("finds the source tree it is meant to scan", () => {
    expect(files.length).toBeGreaterThan(50);
  });

  it("only the review fetch and the attachment URL ask for visibility=all", () => {
    const allowed = new Set(["api/client.ts", "lib/attachmentUrl.ts"]);
    const offenders = files
      .filter((f) => /visibility=all/.test(f.code))
      .map((f) => f.path)
      .filter((r) => !allowed.has(r));
    expect(offenders).toEqual([]);
  });

  it("every caller of an export/iframe builder projects through publicView", () => {
    const callers = /(embedReviewIntoHtml\(|buildClaudePrompt\(|buildMarkdown\(|type:\s*"cm:refresh")/;
    const offenders = files
      .filter((f) => {
        const c = f.code
          .split("\n")
          .filter((l) => !/\bfunction\s/.test(l))
          .join("\n");
        return callers.test(c) && !/publicView\(/.test(c);
      })
      .map((f) => f.path);
    expect(offenders).toEqual([]);
  });
});
