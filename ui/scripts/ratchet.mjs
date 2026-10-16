#!/usr/bin/env node
// OL F10 — the literal ratchet. Counts the style literals that the shared
// tokens (ui/src/tokens.css) exist to replace and compares them with the
// committed baseline `<appdir>/ratchet.json`.
//
//   node ui/scripts/ratchet.mjs <appdir>           # check (CI)
//   node ui/scripts/ratchet.mjs <appdir> --write   # rewrite the baseline
//
// TWO-WAY, the same discipline as scripts/check-wire-ratchet.sh: a count ABOVE
// its baseline is a regression (use a token), and a count BELOW it fails too
// ("baseline stale") so a reduction is locked in by the same PR that makes it
// and can never be spent later. Both failures print a per-file table for the
// offending key.
//
// Scanned: <appdir>/src/**/*.{css,ts,tsx}, minus
//   - src/themes/*.gen.*  (generated palette),
//   - any tokens.css      (the one place literals are the definition),
//   - src/api/generated/** (ts-rs output),
//   - *.test.* and *.spec.* files (fixtures legitimately carry literals).
// fontSizeLiterals: px/rem `font-size:` in CSS; a numeric or digit-leading
// string `fontSize:` in TS/TSX (a `"var(--fs-xs)"` token is not a literal).
// zIndexLiterals: `z-index: N` in CSS, `zIndex: N` in TS/TSX. CSS embedded in
// a TS string (the iframe-injected web/src/scripts/, where no tokens exist) is
// deliberately not scanned.
// A line carrying `kb-literal:` is an explicit, reasoned opt-out: it is not
// counted under the literal keys, and is itself counted under kbLiteralOptOuts
// so opt-outs cannot multiply unseen.
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join, relative, resolve } from "node:path";

const argv = process.argv.slice(2);
const WRITE = argv.includes("--write");
const appArg = argv.find((a) => !a.startsWith("--"));
if (!appArg) {
  console.error("usage: ratchet.mjs <appdir> [--write]");
  process.exit(2);
}
const APP = resolve(appArg);
const SRC = join(APP, "src");
const BASELINE = join(APP, "ratchet.json");

// The twelve colour aliases F3 retired in favour of role tokens.
const LEGACY = [
  "text", "text-strong", "surface", "elev", "elev-hi", "border", "border-hi",
  "muted", "muted-2", "bg-alt", "fg", "fg-muted",
];
const LEGACY_RE = new RegExp(`var\\(--(?:${LEGACY.join("|")})(?![\\w-])`, "g");
const HEX_RE = /#[0-9a-fA-F]{3,8}\b/g;
const KEYS = ["fontSizeLiterals", "hexLiterals", "zIndexLiterals", "legacyAliases", "kbLiteralOptOuts"];

function walk(dir, out = []) {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    if (e.name === "node_modules") continue;
    const p = join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else if (/\.(css|ts|tsx)$/.test(e.name)) out.push(p);
  }
  return out;
}

function skipped(rel) {
  return (
    /(^|\/)tokens\.css$/.test(rel) ||
    /^src\/themes\/[^/]*\.gen\./.test(rel) ||
    rel.startsWith("src/api/generated/") ||
    /\.(test|spec)\.(ts|tsx)$/.test(rel)
  );
}

const counts = Object.fromEntries(KEYS.map((k) => [k, 0]));
const perFile = Object.fromEntries(KEYS.map((k) => [k, new Map()]));
const bump = (key, rel, n) => {
  if (!n) return;
  counts[key] += n;
  perFile[key].set(rel, (perFile[key].get(rel) ?? 0) + n);
};

for (const file of walk(SRC).sort()) {
  const rel = relative(APP, file);
  if (skipped(rel)) continue;
  const isCss = file.endsWith(".css");
  for (const line of readFileSync(file, "utf8").split("\n")) {
    if (line.includes("kb-literal:")) {
      bump("kbLiteralOptOuts", rel, 1);
      continue;
    }
    if (isCss ? /font-size:\s*[\d.]+(px|rem)\b/.test(line) : /fontSize:\s*("?[\d.])/.test(line)) bump("fontSizeLiterals", rel, 1);
    if (isCss ? /z-index:\s*\d/.test(line) : /zIndex:\s*\d/.test(line)) bump("zIndexLiterals", rel, 1);
    bump("hexLiterals", rel, (line.match(HEX_RE) ?? []).length);
    bump("legacyAliases", rel, (line.match(LEGACY_RE) ?? []).length);
  }
}

const rel0 = relative(process.cwd(), APP) || ".";
if (WRITE) {
  writeFileSync(BASELINE, JSON.stringify(counts, null, 2) + "\n");
  console.log(`ratchet (${rel0}): wrote ${relative(process.cwd(), BASELINE)}`);
  console.log(JSON.stringify(counts));
  process.exit(0);
}

if (!existsSync(BASELINE)) {
  console.error(`ratchet (${rel0}): ${BASELINE} is missing; run with --write`);
  process.exit(1);
}
const base = JSON.parse(readFileSync(BASELINE, "utf8"));
let bad = false;
for (const key of KEYS) {
  const have = counts[key];
  const want = base[key];
  if (typeof want !== "number") {
    console.error(`RATCHET: ${key} has no baseline in ${rel0}/ratchet.json; run with --write`);
    bad = true;
    continue;
  }
  if (have === want) continue;
  bad = true;
  if (have > want) {
    console.error(`RATCHET: ${key} is ${have}, baseline ${want} (regression) - use a token from ui/src/tokens.css or add a reasoned kb-literal: opt-out`);
  } else {
    console.error(`RATCHET: ${key} dropped to ${have}, baseline ${want} (baseline stale) - run \`node ui/scripts/ratchet.mjs ${rel0} --write\` and commit ratchet.json in this PR`);
  }
  const rows = [...perFile[key].entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
  for (const [f, n] of rows.slice(0, 40)) console.error(`    ${String(n).padStart(5)}  ${f}`);
  if (rows.length > 40) console.error(`    ... ${rows.length - 40} more files`);
}
for (const key of Object.keys(base)) if (!KEYS.includes(key)) {
  console.error(`RATCHET: unknown key ${key} in ratchet.json`);
  bad = true;
}
if (!bad) console.log(`ratchet (${rel0}): ${KEYS.map((k) => `${k}=${counts[k]}`).join(" ")} (at baseline)`);
process.exit(bad ? 1 : 0);
