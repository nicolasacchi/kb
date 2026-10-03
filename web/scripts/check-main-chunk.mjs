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
// dist/index.html. Mermaid itself is identified by CONTENT, not by chunk name
// (names are bundler-chosen): the chunk(s) under dist/assets that carry the
// `mermaidAPI` export. None of them may be in the shell's static closure.
// Dynamic `import(...)` edges are ignored on purpose: they are lazy by
// definition. The guard also fails when it finds no mermaid chunk at all, so
// a rename of that export cannot silently turn it into a no-op.
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { dirname, join, normalize } from "node:path";

const DIST = process.argv[2] ?? "dist";
const MARKER = "mermaidAPI";

function htmlRoots(file) {
  const html = readFileSync(join(DIST, file), "utf8");
  const roots = new Set();
  for (const m of html.matchAll(/<(?:script|link)\b[^>]*\b(?:src|href)="([^"]+\.js)"[^>]*>/g)) {
    if (/<link/.test(m[0]) && !/rel="modulepreload"/.test(m[0])) continue;
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

const assets = join(DIST, "assets");
const mermaidChunks = readdirSync(assets)
  .filter((f) => f.endsWith(".js"))
  .map((f) => normalize(join("assets", f)))
  .filter((rel) => readFileSync(join(DIST, rel), "utf8").includes(MARKER));
if (mermaidChunks.length === 0) {
  console.error(`::error::check-main-chunk: no chunk under dist/assets contains "${MARKER}"; the guard cannot find mermaid (did its export name change?)`);
  process.exit(1);
}
const leaked = mermaidChunks.filter((f) => shell.has(f));
if (leaked.length > 0) {
  console.error("::error::mermaid is reachable from the SPA shell's static import graph via: " + leaked.join(", "));
  process.exit(1);
}
console.log(
  `check-main-chunk: shell static graph ${shell.size} chunks; mermaid lives in ${mermaidChunks.join(", ")} and is not among them`,
);
