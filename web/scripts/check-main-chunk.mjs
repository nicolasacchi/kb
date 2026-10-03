// v0.44 X2 (A11.f1) - the build-time guard that mermaid stays out of the SPA
// shell's STATIC graph.
//
// Why: chunking is configured by hand (`output.codeSplitting` groups in
// vite.config.ts). SL4 found that naming a mermaid bucket hoisted Vite's
// shared preload helper into the 3 MB mermaid chunk, so every route chunk
// carried a hard `import "./mermaid-*.js"` and the shell paid for a diagram
// renderer it never calls. That property used to be a comment; this makes it
// a failing build.
//
// How: walk the STATIC import closure (plus modulepreload links) of
// dist/index.html and of dist/sketch.html (the only entry that may load
// mermaid). Any large chunk (>= 500 KB) in the sketch closure that is ALSO in
// the shell closure is mermaid (or something that drags it) in the main graph.
// Dynamic `import(...)` edges are ignored on purpose: they are lazy by
// definition. Name-independent, so a renamed chunk cannot slip through.
import { readFileSync, statSync, existsSync } from "node:fs";
import { dirname, join, normalize } from "node:path";

const DIST = process.argv[2] ?? "dist";
const BIG = 500 * 1024;

function htmlRoots(file) {
  const html = readFileSync(join(DIST, file), "utf8");
  const roots = new Set();
  for (const m of html.matchAll(/<(?:script|link)\b[^>]*\b(?:src|href)="([^"]+\.js)"[^>]*>/g)) {
    if (/rel="(?!modulepreload)/.test(m[0]) && /<link/.test(m[0])) continue;
    roots.add(normalize(m[1].replace(/^\//, "")));
  }
  return roots;
}

// Static import specifiers only: `import"./x.js"`, `import ... from"./x.js"`,
// `export ... from"./x.js"`. A dynamic `import("./x.js")` has a `(` and is skipped.
const STATIC_RE = /(?:^|[;{}()\s])(?:import|export)\s*(?:[\w$*{][^"'`;]*?\bfrom\s*)?["']([^"']+\.js)["']/g;

function closure(roots) {
  const seen = new Set();
  const queue = [...roots];
  while (queue.length) {
    const rel = queue.pop();
    if (seen.has(rel)) continue;
    const abs = join(DIST, rel);
    if (!existsSync(abs)) continue;
    seen.add(rel);
    const src = readFileSync(abs, "utf8");
    for (const m of src.matchAll(STATIC_RE)) {
      const spec = m[1];
      if (!spec.startsWith(".")) continue;
      queue.push(normalize(join(dirname(rel), spec)));
    }
  }
  return seen;
}

const shell = closure(htmlRoots("index.html"));
const sketch = closure(htmlRoots("sketch.html"));
const size = (rel) => statSync(join(DIST, rel)).size;

const bigSketch = [...sketch].filter((f) => size(f) >= BIG);
if (bigSketch.length === 0) {
  console.error("::error::check-main-chunk: found no large chunk in sketch.html's graph; the guard cannot tell where mermaid is (did the sketch entry move?)");
  process.exit(1);
}
const leaked = bigSketch.filter((f) => shell.has(f));
if (leaked.length > 0) {
  console.error(
    "::error::mermaid-sized chunk(s) reachable from the SPA shell's static graph: " +
      leaked.map((f) => `${f} (${(size(f) / 1024).toFixed(0)} KB)`).join(", "),
  );
  process.exit(1);
}
console.log(
  `check-main-chunk: shell graph ${shell.size} chunks, none of the ${bigSketch.length} large sketch chunk(s) ` +
    `(${bigSketch.map((f) => `${f} ${(size(f) / 1024).toFixed(0)} KB`).join(", ")}) is in it`,
);
