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
// `<app>/src` and `ui/src` must resolve to a real value with NO possible dead end.
//
//   node ../ui/scripts/css-undefined-vars.mjs .          # from an app dir
//   node ui/scripts/css-undefined-vars.mjs web-code      # from the repo root
//   ... --json                                           # machine-readable
//
// OL F10 — moved here from web-code/scripts and parameterised by app dir so
// kb (web/) is gated too. Both `<app>/src` and `ui/src` are scanned for
// DEFINITIONS and for REFERENCES. `<app>/scripts/ci/css-vars-allowlist.txt`
// (optional; one `--name` per line, `#` comments) grandfathers names that are
// referenced but undeclared; it is TWO-WAY: an entry that no longer matches a
// reference is itself a failure, so the list can only shrink.
//
// ── What counts as "defined" ────────────────────────────────────────────
// A custom property name is DEFINED if `--name:` appears as a declaration
// in any `*.css` file under `src` (tokens.css, themes.gen.css, or any
// component sheet — a theme may legitimately be the ONLY place a role is
// declared, so all of `src/**/*.css` is the search set, not just
// tokens.css). A name ASSIGNED from TS/TSX also counts (an inline
// `style={{ "--card-accent": … }}`, `["--x" as string]: …` or
// `el.style.setProperty("--x", …)`, the `--kbc-*` per-instance convention):
// it is a real value on a real element; a reference to it keeps its fallback
// for the elements that do not set it. See the TS pass below.
//
// ── The fallback ruling (LINT1 reversed the F10 ruling) ────────────────
// A fallback does NOT rescue an undeclared name. `var(--name, <anything>)` is
// a hit when `--name` is declared nowhere the app can see: a literal fallback
// (`var(--space-1, 4px)`, `var(--ink-mute, #999)`) only HIDES a made-up token
// behind a plausible render, which is exactly how `--space-N`/`--radius-N`
// reached review. Map the reference to the real token instead. A name that IS
// declared (in any src/**/*.css, or assigned from TS/TSX as a per-instance
// custom property, see below) but may be unset on some element or theme keeps
// its fallback — that is what a fallback is for. Fallback chains are checked
// at every level: `var(--a, var(--b))` flags `--a` when undeclared and, on its
// own, `--b` when undeclared. A bare `var(--undefined)` is a hit as before.
//
// Also scans `var(--…)` occurrences inside `.ts`/`.tsx` string literals
// (inline SVG fill/stroke, CodeMirror theme objects, style objects) against
// the SAME defined-name set — the reader's EgoGraph/RiskDial/ReportHero
// etc. reach for real tokens directly, so this is a cheap extra net for the
// same bug class outside `.css`.
import { readFileSync, readdirSync } from "node:fs";
import { existsSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const UI_SRC = join(__dirname, "..", "src");

const argv = process.argv.slice(2);
const JSON_OUT = argv.includes("--json");
const appArg = argv.find((a) => !a.startsWith("--"));
if (!appArg) {
  console.error("usage: css-undefined-vars.mjs <appdir> [--json]");
  process.exit(2);
}
const APP_DIR = resolve(appArg);
const SRC_DIR = join(APP_DIR, "src");
const ALLOWLIST = join(APP_DIR, "scripts", "ci", "css-vars-allowlist.txt");

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

const cssFiles = [...walk(SRC_DIR, [".css"], []), ...walk(UI_SRC, [".css"], [])];
const tsFiles = [...walk(SRC_DIR, [".ts", ".tsx"], []), ...walk(UI_SRC, [".ts", ".tsx"], [])].filter(
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
// Names ASSIGNED from TS/TSX count as defined too (LINT1): the per-instance
// convention is `style={{ "--card-accent": accent }}`,
// `style={{ ["--x" as string]: v }}` or `el.style.setProperty("--x", v)`. They
// are real values on a real element, not made-up tokens. A `var(--x)` of such
// a name is still allowed to carry a fallback for the elements that do not set it.
// Linear-time by construction: the regexes start at a quote (never at
// whitespace), so no `\s*` is re-entered from every position of a long
// whitespace run (comment stripping blanks comments into such runs, and the
// earlier `(?:^|[{,])\s*\[?\s*"…` form went quadratic on them). The
// object-key context is then checked by one bounded backward walk per match.
const TS_KEY_RE = /["'`](--[a-zA-Z0-9-]+)["'`](?:\s+as\s+string)?\s*\]?\s*:/g;
const TS_SET_PROP_RE = /setProperty\(\s*["'`](--[a-zA-Z0-9-]+)["'`]/g;

// Object-key position only (after `{`, `,` or at line start, optionally inside
// `[ … ]`) so a ternary branch like `c ? "--x" : y` does not count.
function isObjectKeyPosition(text, quoteIdx) {
  let i = quoteIdx - 1;
  let sawNewline = false;
  const skipWs = () => {
    while (i >= 0 && /\s/.test(text[i])) {
      if (text[i] === "\n") sawNewline = true;
      i--;
    }
  };
  skipWs();
  if (i >= 0 && text[i] === "[") {
    i--;
    skipWs();
  }
  return i < 0 || sawNewline || text[i] === "{" || text[i] === ",";
}

function tsDeclaredNames(text) {
  const names = [];
  for (const found of text.matchAll(TS_KEY_RE)) {
    if (isObjectKeyPosition(text, found.index)) names.push(found[1]);
  }
  for (const found of text.matchAll(TS_SET_PROP_RE)) names.push(found[1]);
  return names;
}
for (const f of tsFiles) {
  const raw = readFileSync(f, "utf8");
  if (!raw.includes("--")) continue;
  for (const name of tsDeclaredNames(stripJsComments(raw))) defined.add(name);
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

// A reference resolves iff the NAME it reads is declared somewhere the app can
// see (LINT1: a fallback no longer rescues an undeclared name). A declared
// name that is merely unset in some theme may still carry a fallback; that is
// what a fallback is for. The fallback's own nested var() calls are visited
// separately by the scan loop (every `var(` occurrence is checked), so
// `var(--a, var(--b))` flags `--a` and, independently, `--b` when undeclared.
function resolves(inner, definedNames = defined) {
  const { name } = splitVarArgs(inner);
  if (!/^--[a-zA-Z0-9-]+$/.test(name)) return true; // malformed — don't flag
  return definedNames.has(name);
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

// Two-way allowlist: grandfathered names are dropped from the hit list, and an
// entry that matches no remaining reference is stale and fails.
const allow = new Set(
  existsSync(ALLOWLIST)
    ? readFileSync(ALLOWLIST, "utf8")
        .split("\n")
        .map((l) => l.replace(/#.*/, "").trim())
        .filter(Boolean)
    : [],
);
const referenced = new Set(hits.map((h) => h.name));
const stale = [...allow].filter((n) => !referenced.has(n));
for (let i = hits.length - 1; i >= 0; i--) if (allow.has(hits[i].name)) hits.splice(i, 1);

hits.sort((a, b) => (a.file === b.file ? a.line - b.line : a.file.localeCompare(b.file)));

if (JSON_OUT) {
  console.log(
    JSON.stringify(
      {
        definedCount: defined.size,
        hitCount: hits.length,
        hits: hits.map((h) => ({ ...h, file: relative(APP_DIR, h.file) })),
      },
      null,
      2,
    ),
  );
} else {
  console.log(`css-undefined-vars (${relative(process.cwd(), APP_DIR) || "."}): ${defined.size} defined custom properties across ${cssFiles.length} CSS files.`);
  if (hits.length === 0) {
    console.log("No unresolved var(--…) references. ✓");
  } else {
    console.log(`${hits.length} unresolved var(--…) reference(s):\n`);
    for (const h of hits) {
      console.log(`  ${relative(APP_DIR, h.file)}:${h.line}  ${h.name}`);
      console.log(`    ${h.text}`);
    }
    console.log(
      "\nEach name above is never declared as `--name:` in any src/**/*.css file, and a fallback " +
        "does not rescue it — the declaration is invalid at computed-value time and silently " +
        "falls back to inherited/initial. Map it to " +
        "a real tokens.css name (see ui/src/tokens.css).",
    );
  }
}

if (stale.length) {
  console.log(
    `\nStale allowlist entries (no longer referenced; delete them from ${relative(APP_DIR, ALLOWLIST)}):\n  ` +
      stale.join("\n  "),
  );
}

process.exit(hits.length || stale.length ? 1 : 0);
