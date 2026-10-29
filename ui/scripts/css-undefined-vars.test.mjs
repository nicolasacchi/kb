// LINT1 — fixtures for css-undefined-vars.mjs (run: node --test ui/scripts).
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
