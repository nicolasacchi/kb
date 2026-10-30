// LINT1 — fixtures for css-undefined-vars.mjs (run: node --test ui/scripts/css-undefined-vars.test.mjs).
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const script = join(here, "css-undefined-vars.mjs");
const app = join(here, "fixtures", "css-vars", "app");

function run() {
  // The script exits 1 when it has hits; --json still prints on stdout. ui/src
  // is scanned too (and the fixture app defines none of the real tokens), so
  // keep only the fixture app's own hits.
  let out;
  try {
    out = execFileSync("node", [script, app, "--json"], { encoding: "utf8" });
  } catch (e) {
    out = e.stdout;
  }
  return JSON.parse(out).hits.filter((h) => h.file.startsWith("src/"));
}

test("an undefined name is a hit with no fallback, a literal fallback, or a var() fallback", () => {
  const hits = run();
  const names = hits.map((h) => h.name).sort();
  assert.deepEqual(names, [
    "--fx-bare-missing",
    "--fx-literal-missing",
    "--fx-nested-fallback-missing",
    "--fx-nested-missing",
    "--fx-outer-missing",
    "--fx-ternary-not-declared",
    "--fx-ts-string-missing",
  ]);
});

test("a defined name (CSS or assigned from TS) may carry any fallback", () => {
  const names = new Set(run().map((h) => h.name));
  for (const ok of ["--fx-defined", "--fx-from-ts"]) assert.ok(!names.has(ok), ok);
});

test("a hit makes the script exit non-zero", () => {
  assert.throws(() => execFileSync("node", [script, app], { stdio: "pipe" }));
});

test("a file with huge whitespace runs scans in linear time (regression: quadratic TS matcher hung CI)", async () => {
  const { mkdtempSync, mkdirSync, writeFileSync, rmSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const dir = mkdtempSync(join(tmpdir(), "cssvars-"));
  try {
    mkdirSync(join(dir, "src"));
    // Blank indented lines plus blanked-out block comments make one huge
    // whitespace run containing ~100k line starts; a `^\s*…` matcher rescans
    // the rest of the run from every one of them.
    const ws = "\n  ".repeat(100000) + "/*" + "\n".repeat(50000) + "*/" + "\n  ".repeat(100000);
    writeFileSync(
      join(dir, "src", "big.tsx"),
      `const a = { "--fx-big-key": 1 };${ws}\nconst b = { ["--fx-big-bracket" as string]: 2 };\nconst c = z ? "--fx-big-ternary" : y;\n`,
    );
    const t0 = Date.now();
    let out;
    try {
      out = execFileSync("node", [script, dir, "--json"], { encoding: "utf8", timeout: 20000 });
    } catch (e) {
      out = e.stdout;
    }
    assert.ok(Date.now() - t0 < 5000, `scan took ${Date.now() - t0} ms`);
    assert.ok(JSON.parse(out), "script completed");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
