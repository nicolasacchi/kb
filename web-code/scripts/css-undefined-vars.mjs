#!/usr/bin/env node
// V80-C1 — the undefined-CSS-custom-property sweep, made permanent. Three
// review passes during v8.0 (R0 on tours.css, R4 on --fg/--fg-dim, R5 on
// branches.css + a named list of five more files) each found the SAME bug
// independently: a `var(--name)` reference to a `--name` this app never
// declares. CSS's own failure mode hides it — an invalid var() reference
// makes the DECLARATION invalid at computed-value time, which for an
// INHERITED property (color, font-family, …) silently falls back to
// whatever the parent happened to be painting, and for a NON-inherited
// property (background, border, box-shadow, padding, …) falls back to that
// property's initial value (transparent / none / 0). Both are silent: no
// build error, no console warning, a plausible-looking render that is
// simply wrong — worse under a theme the developer's screen wasn't on.
//
// This script is the permanent gate: every `var(--name …)` under
// `web-code/src` must resolve to a real value with NO possible dead end.
//
//   npm run lint:css-vars            # human report, exit 1 on any hit
//   npm run lint:css-vars -- --json  # machine-readable
//
// ── What counts as "defined" ────────────────────────────────────────────
// A custom property name is DEFINED if `--name:` appears as a declaration
// in any `*.css` file under `src` (tokens.css, themes.gen.css, or any
// component sheet — a theme may legitimately be the ONLY place a role is
// declared, so all of `src/**/*.css` is the search set, not just
// tokens.css). This intentionally does NOT count an inline
// `style={{ "--name": … }}` in a `.tsx` component (the `--kbc-*`
// per-instance convention, e.g. `provHueStyle`/`RiskDial`/`ReportHero`) as
// a definition — those are scoped to one element, set conditionally, and
// exist precisely so a THEME can't reach them; a `var(--kbc-foo, …)` must
// still carry a fallback that resolves through the static side, and this
// script checks exactly that.
//
// ── The fallback ruling (brief asks us to decide and say so) ───────────
// `var(--name, <fallback>)` is NOT a hit when `<fallback>` resolves to a
// concrete value with no further dead end:
//   - a LITERAL fallback (`8px`, `#fff`, `0 6px 24px rgb(0 0 0 / 0.35)`,
//     `ui-monospace, monospace`, …) is always accepted — CSS guarantees a
//     real rendered value regardless of theme. This is what most of
//     `commands.css`/`nav.css`'s legacy `--space-N`/`--radius-N`/
//     `--shadow-N` names lean on; they are a DUPLICATE, DISJOINT naming
//     layer that predates `tokens.css`'s `--pad-N`/`--radius-*`/
//     `--shadow*` ramp (confirmed unmodified since the initial public
//     import) and normalizing them onto tokens.css is real but SEPARATE
//     cleanup — out of scope here because they already render correctly in
//     every theme (structural literals don't need to move with a palette).
//   - a fallback that is ITSELF exactly one `var(--other)` call is resolved
//     RECURSIVELY: `var(--kbc-prov-hue, var(--rule))` is safe because
//     `--rule` is defined; `var(--surface-2, var(--surface))` was a hit
//     before this unit's fix because NEITHER name is defined anywhere.
// A bare `var(--name)` with no fallback is always a hit when `--name` is
// undefined — this is the exact `--fg`/`--fg-dim` (R4) and
// `--line`/`--bg-raised` (R5) shape.
//
// Also scans `var(--…)` occurrences inside `.ts`/`.tsx` string literals
// (inline SVG fill/stroke, CodeMirror theme objects, style objects) against
// the SAME defined-name set — the reader's EgoGraph/RiskDial/ReportHero
// etc. reach for real tokens directly, so this is a cheap extra net for the
// same bug class outside `.css`.
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const SRC_DIR = join(__dirname, "..", "src");

const argv = process.argv.slice(2);
const JSON_OUT = argv.includes("--json");

function walk(dir, exts, out) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === "node_modules") continue;
    const p = join(dir, entry.name);
    if (entry.isDirectory()) walk(p, exts, out);
    else if (exts.some((e) => entry.name.endsWith(e))) out.push(p);
  }
  return out;
}

// Strip CSS comments (/* … */) so a comment MENTIONING `var(--x)` (e.g. the
// `--kbc-prov-hue` doc comment in branches.css, or this file's own header)
// never counts as a definition OR a usage.
function stripCssComments(text) {
  return text.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "));
}

// Strip JS/TS comments (// line, /* block */) for the same reason — several
// files document the `--kbc-*` convention in prose that names real vars.
function stripJsComments(text) {
  let out = text.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "));
  out = out.replace(/(^|[^:])\/\/[^\n]*/g, (_m, pre) => pre);
  return out;
}

const cssFiles = walk(SRC_DIR, [".css"], []);
const tsFiles = walk(SRC_DIR, [".ts", ".tsx"], []).filter(
  (f) => !f.endsWith(".gen.ts") && !f.endsWith(".gen.tsx") && !f.includes("/generated/"),
);

// ── Build the defined-name set ------------------------------------------
const defined = new Set();
const DEF_RE = /(--[a-zA-Z0-9-]+)\s*:/g;
const cssTextByFile = new Map();
for (const f of cssFiles) {
  const raw = readFileSync(f, "utf8");
  const text = stripCssComments(raw);
  cssTextByFile.set(f, text);
  for (const found of text.matchAll(DEF_RE)) defined.add(found[1]);
}

// ── Fallback-chain resolution --------------------------------------------
// Given the text strictly INSIDE a var(...) call's parens (i.e. everything
// after `var(`, up to its matching close paren, exclusive), split into the
// referenced name and the fallback (if any), then decide if the whole
// reference is guaranteed to resolve.
function splitVarArgs(inner) {
  // inner starts with "--name" (already validated by caller's regex site);
  // find the first top-level comma.
  let depth = 0;
  for (let i = 0; i < inner.length; i++) {
    const c = inner[i];
    if (c === "(") depth++;
    else if (c === ")") depth--;
    else if (c === "," && depth === 0) {
      return { name: inner.slice(0, i).trim(), fallback: inner.slice(i + 1).trim() };
    }
  }
  return { name: inner.trim(), fallback: null };
}

// Find the text inside the matching parens for a `var(` whose "(" sits at
// `openIdx` in `text`.
function extractParen(text, openIdx) {
  let depth = 0;
  for (let i = openIdx; i < text.length; i++) {
    if (text[i] === "(") depth++;
    else if (text[i] === ")") {
      depth--;
      if (depth === 0) return { inner: text.slice(openIdx + 1, i), endIdx: i };
    }
  }
  return null; // unbalanced — caller treats as not-a-hit (can't parse)
}

const VAR_CALL_RE = /^var\(\s*(--[a-zA-Z0-9-]+)([\s\S]*)\)\s*$/;

// Returns true iff `inner` (the text between a `var(` and its matching `)`)
// resolves to a concrete value with no dead end. `seen` guards runaway
// recursion on a cyclic fallback (shouldn't happen, but never hang).
function resolves(inner, seen = new Set()) {
  const { name, fallback } = splitVarArgs(inner);
  if (!/^--[a-zA-Z0-9-]+$/.test(name)) return true; // malformed — don't flag
  if (defined.has(name)) return true;
  if (fallback === null) return false; // bare var(--undefined) — always a hit
  if (seen.has(name)) return false; // cycle guard
  seen.add(name);
  const trimmed = fallback.trim();
  const asVarCall = trimmed.match(VAR_CALL_RE);
  if (asVarCall) {
    // fallback is EXACTLY one var(...) call — resolve it recursively.
    const nestedOpen = trimmed.indexOf("(");
    const nested = extractParen(trimmed, nestedOpen);
    if (!nested) return true; // unparseable, don't flag
    return resolves(nested.inner, seen);
  }
  // Fallback contains literal content (px value, color, font stack, a
  // color-mix()/calc() expression, or a var() call mixed with other
  // tokens) — CSS always produces a value here, so treat as resolved.
  return true;
}

// ── Scan for var(--…) usages, report every unresolved one ---------------
const hits = [];

function scanText(file, text, isCss) {
  const lines = text.split("\n");
  const lineStarts = [0];
  let offset = 0;
  for (const line of lines) lineStarts.push((offset += line.length + 1));

  const OPEN_RE = /var\(/g;
  for (const found of text.matchAll(OPEN_RE)) {
    const openIdx = found.index + 3; // index of the "(" itself
    const paren = extractParen(text, openIdx);
    if (!paren) continue;
    const { name } = splitVarArgs(paren.inner);
    if (!/^--[a-zA-Z0-9-]+$/.test(name)) continue;
    if (resolves(paren.inner)) continue;
    // Locate line/col of the match for reporting.
    let lo = 0;
    while (lo + 1 < lineStarts.length && lineStarts[lo + 1] <= found.index) lo++;
    const lineNo = lo + 1;
    const lineText = lines[lo]?.trim() ?? "";
    hits.push({ file, line: lineNo, name, text: lineText, kind: isCss ? "css" : "ts" });
  }
}

for (const f of cssFiles) scanText(f, cssTextByFile.get(f), true);
for (const f of tsFiles) {
  const raw = readFileSync(f, "utf8");
  // Only bother scanning files that even mention `var(--` — cheap filter.
  if (!raw.includes("var(--")) continue;
  scanText(f, stripJsComments(raw), false);
}

hits.sort((a, b) => (a.file === b.file ? a.line - b.line : a.file.localeCompare(b.file)));

if (JSON_OUT) {
  console.log(
    JSON.stringify(
      {
        definedCount: defined.size,
        hitCount: hits.length,
        hits: hits.map((h) => ({ ...h, file: relative(join(__dirname, ".."), h.file) })),
      },
      null,
      2,
    ),
  );
} else {
  console.log(`css-undefined-vars: ${defined.size} defined custom properties across ${cssFiles.length} CSS files.`);
  if (hits.length === 0) {
    console.log("No unresolved var(--…) references. ✓");
  } else {
    console.log(`${hits.length} unresolved var(--…) reference(s):\n`);
    for (const h of hits) {
      console.log(`  ${relative(join(__dirname, ".."), h.file)}:${h.line}  ${h.name}`);
      console.log(`    ${h.text}`);
    }
    console.log(
      "\nEach name above is never declared as `--name:` in any src/**/*.css file, and has no " +
        "fallback (or only a fallback chain to another undefined name) — the declaration is " +
        "invalid at computed-value time and silently falls back to inherited/initial. Map it to " +
        "a real tokens.css name (see web-code/CLAUDE.md § Themes).",
    );
  }
}

process.exit(hits.length ? 1 : 0);
