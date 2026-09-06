// V70-A6 — the lint that keeps "one builder, one door" mechanical.
//
// Root CLAUDE.md #35's discipline is "every clickable surface builds its URL
// through the ONE builder, never an ad-hoc string". The recon found two live
// violations (`ReviewDiff.tsx:197,211`, since repaired by A3S) and a third
// this unit repaired (`ReviewFileItem.tsx`'s hand-assembled `/r/${repo}/…`);
// `app.tsx`'s `repoRoute` was a fourth. A prose rule found three of those AFTER
// they shipped, so this is the same rule as a test.
//
// TWO PATTERNS, both narrow on purpose:
//
//   * a template literal that STARTS a reader path (`` `/r/${ ``) anywhere
//     outside `src/lib/` (where the builders live) and `src/nav/` (which is
//     built on top of them);
//   * `navigate(` with a string/template literal argument that starts with a
//     slash — i.e. a route assembled at the call site rather than by a
//     builder.
//
// A false positive here is cheap to fix (call the builder); a false NEGATIVE
// is a URL grammar that drifts silently, which is the thing #35 exists to
// prevent. So the patterns are allowed to be blunt.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const SRC = fileURLToPath(new URL("..", import.meta.url));

/// `src/lib/` owns the builders themselves; `src/nav/` is the Location
/// Contract, which is built ON them and needs one literal fallback for a repo
/// root. Test files are exempt: a golden's whole job is to state the expected
/// STRING.
const EXEMPT_DIRS = ["lib", "nav"];

function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) {
      walk(p, out);
    } else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name)) {
      out.push(p);
    }
  }
  return out;
}

/// A COMMENT may name the pattern — this file's own doc does, and so do the
/// receipts left at each repaired call site. Skipping comment lines is what
/// lets the rule be explained where it is enforced. Deliberately line-shaped
/// (a `//`, `*` or `/*` opener) rather than a real parse: a lint that needs a
/// TypeScript AST to say "do not build URLs by hand" has outgrown its job.
function isComment(line: string): boolean {
  const t = line.trimStart();
  return t.startsWith("//") || t.startsWith("*") || t.startsWith("/*");
}

function offenders(pattern: RegExp): string[] {
  const hits: string[] = [];
  for (const file of walk(SRC)) {
    const rel = file.slice(SRC.length);
    if (EXEMPT_DIRS.some((d) => rel.startsWith(`${d}/`))) continue;
    const text = readFileSync(file, "utf8");
    text.split("\n").forEach((line, i) => {
      if (!isComment(line) && pattern.test(line)) hits.push(`${rel}:${i + 1}  ${line.trim()}`);
    });
  }
  return hits;
}

describe("URL construction stays inside the builders", () => {
  it("no component assembles a reader path by hand", () => {
    expect(
      offenders(/`\/r\/\$\{/),
      "build it with `lib/codeUrl.ts` (root CLAUDE.md #35) — a hand-assembled " +
        "path cannot grow `pane2`/`sym`/`trail` for free and drifts the moment " +
        "the grammar does",
    ).toEqual([]);
  });

  it("no component hands `navigate()` a hand-assembled route", () => {
    // An INTERPOLATED literal, or any literal reader path. A bare constant
    // route (`navigate("/")`, `navigate("/~inbox")`) is not drift — there is
    // no grammar to get wrong and no builder to call — so it stays legal.
    expect(
      offenders(/\bnavigate\(\s*(`[^`]*\$\{|["'`]\/r\/)/),
      "call a builder (`lib/codeUrl.ts`, `lib/setsUrl.ts`, …) and pass its " +
        "result, or go through `nav/navigate.ts`",
    ).toEqual([]);
  });
});
