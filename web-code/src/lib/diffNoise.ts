// V73-K2a — noise classification for the review diff (design §D9: "noise
// classification with stated reasons").
//
// THE RULE THIS FILE EXISTS TO ENFORCE: a noise class is a LABEL, never a
// filter. Nothing here removes a hunk, a file or a line from the page.
// `classifyFile`/`classifyHunk` return labels; the ONLY thing the route
// does with them is (a) render a chip whose `title` is the rule's own
// sentence, verbatim, and (b) — when the operator asks for it with
// `?noise=collapsed` — COLLAPSE a labelled hunk, visibly, with its counts
// still on screen and one click to expand. A collapsed hunk is still
// counted in every census. That distinction is the whole point: kb-code
// may say "this looks like generated output" and be wrong, and the
// operator must be able to see that it was wrong in one keystroke.
//
// Every rule is PURE, CLIENT-SIDE and stated in words. There is no score,
// no threshold that is not named in the rule text, no learned model and no
// server round trip. `NOISE_RULES` is the closed table; `ruleText()` is
// what the chip shows; both are golden-pinned by `diffNoise.test.ts` so a
// silent widening (adding `vendor/` to the generated set, say) reads as a
// golden diff rather than as a behaviour change nobody sees.
//
// The `moved` rule is the one that cannot be exhaustive, and it says so:
// it matches a hunk's REMOVED block against the ADDED blocks of the OTHER
// files whose diffs the page has actually loaded (the full-page diff
// fetches per file, lazily, on scroll). A `moved` label that appears is
// always backed by a named counterpart file; its ABSENCE never claims the
// block did not move. `buildMovedIndex`'s own `scanned` census is what the
// header strip reports, so the partiality is on screen rather than in a
// comment.

import type { DiffHunk, ParsedDiff } from "./diff";
import { hunkStats } from "./diffHunks";

export type NoiseClass = "generated" | "whitespace-only" | "moved" | "rename-only" | "large";

/// The closed rule table. `id` is the class; `rule` is the sentence a chip
/// shows in its `title` and the docs quote. Ordered as the chips render.
export const NOISE_RULES: ReadonlyArray<{ id: NoiseClass; label: string; rule: string }> = [
  {
    id: "generated",
    label: "generated",
    rule: "the path matches a generated-output pattern (lockfile, *.gen.*/*.generated.*, schema dump, minified bundle, test snapshot)",
  },
  {
    id: "rename-only",
    label: "rename-only",
    rule: "the file's status is a rename and the patch adds and removes zero lines",
  },
  {
    id: "whitespace-only",
    label: "whitespace-only",
    rule: "removing every space and tab makes the hunk's added text identical to its removed text",
  },
  {
    id: "moved",
    label: "moved",
    rule: "this hunk's removed block is byte-identical to an added block in another file already loaded on this page",
  },
  {
    id: "large",
    label: "large",
    rule: "the hunk changes more than 120 lines (a file is large past 800 changed lines)",
  },
];

/// Thresholds, named here so the rule text above and the code below can
/// never disagree by hand-typing a number twice.
export const LARGE_HUNK_LINES = 120;
export const LARGE_FILE_LINES = 800;
/// A moved block shorter than this is not evidence of a move — three
/// identical lines (`}`, blank, `end`) occur everywhere. Stated in the
/// label's own detail line.
export const MIN_MOVED_LINES = 6;
/// The moved index never walks more than this many hunks, so a 900-file
/// review cannot turn a label into a render stall. When the cap bites the
/// census says so (`MovedIndex.capped`).
export const MOVED_INDEX_HUNK_CAP = 600;

/// One label on one file or hunk. `detail` is the rule's own evidence for
/// THIS subject (the matched path pattern, the counterpart file, the line
/// count) — never a restatement of the rule.
export interface NoiseLabel {
  cls: NoiseClass;
  /// The rule that produced it, verbatim from `NOISE_RULES`.
  rule: string;
  /// Why this subject matched, in this subject's own terms.
  detail: string;
}

export function ruleText(cls: NoiseClass): string {
  return NOISE_RULES.find((r) => r.id === cls)?.rule ?? "";
}

export function noiseLabelText(cls: NoiseClass): string {
  return NOISE_RULES.find((r) => r.id === cls)?.label ?? cls;
}

// ── generated paths ────────────────────────────────────────────────────────
//
// Five NAMED sub-patterns under one class. Each carries its own `detail`
// so a chip never just says "generated" — it says which pattern matched.

const LOCKFILES: ReadonlySet<string> = new Set([
  "package-lock.json",
  "npm-shrinkwrap.json",
  "yarn.lock",
  "pnpm-lock.yaml",
  "bun.lockb",
  "Cargo.lock",
  "Gemfile.lock",
  "poetry.lock",
  "Pipfile.lock",
  "composer.lock",
  "go.sum",
  "flake.lock",
  "mix.lock",
]);

/// A schema DUMP — the file a migration tool rewrites wholesale. Hand-
/// authored migrations under `migrations/` are deliberately NOT here: they
/// are the change, not a rendering of it, and calling them generated would
/// be the kind of confident wrong label this module's header warns about.
const SCHEMA_DUMPS: ReadonlySet<string> = new Set([
  "db/schema.rb",
  "db/structure.sql",
  "priv/repo/structure.sql",
]);

function basename(path: string): string {
  const i = path.lastIndexOf("/");
  return i === -1 ? path : path.slice(i + 1);
}

/// `null` = not a generated path. Otherwise the `detail` naming the
/// sub-pattern that matched.
export function generatedReason(path: string): string | null {
  const base = basename(path);
  if (LOCKFILES.has(base)) return `lockfile: ${base}`;
  if (SCHEMA_DUMPS.has(path)) return `schema dump: ${path}`;
  if (/\.(gen|generated)\.[^./]+$/.test(base)) return `generated-marker filename: ${base}`;
  if (/_pb2?\.pyi?$/.test(base) || /\.pb\.go$/.test(base) || /_generated\.go$/.test(base)) {
    return `protobuf/codegen filename: ${base}`;
  }
  if (/\.min\.(js|css|mjs|cjs)$/.test(base) || /\.js\.map$/.test(base) || /\.css\.map$/.test(base)) {
    return `minified or source-map bundle: ${base}`;
  }
  if (path.includes("/__snapshots__/") || path.startsWith("__snapshots__/") || /\.snap$/.test(base)) {
    return `test snapshot: ${path}`;
  }
  return null;
}

// ── whitespace-only ────────────────────────────────────────────────────────

function stripWs(s: string): string {
  return s.replace(/\s+/g, "");
}

/// True when deleting every space/tab/newline from the hunk's added text
/// yields exactly its removed text — a re-indent, a reflow, a trailing-
/// whitespace sweep. A hunk with no `+`/`-` lines at all is NOT
/// whitespace-only (there is no change to describe), and neither is a
/// pure addition or a pure deletion.
export function isWhitespaceOnlyHunk(hunk: DiffHunk): boolean {
  const adds: string[] = [];
  const removes: string[] = [];
  for (const line of hunk.lines) {
    if (line.kind === "add") adds.push(line.text);
    else if (line.kind === "remove") removes.push(line.text);
  }
  if (adds.length === 0 || removes.length === 0) return false;
  const a = stripWs(adds.join(""));
  const r = stripWs(removes.join(""));
  if (a === "" && r === "") return false;
  return a === r;
}

// ── moved ──────────────────────────────────────────────────────────────────

/// The normalized signature of a hunk's added (or removed) block: each
/// line trimmed of leading/trailing whitespace, joined by `\n`. Trimming
/// is what lets a block that moved into a different nesting depth still
/// match; interior whitespace is preserved, so two genuinely different
/// lines never collapse together.
function blockSignature(hunk: DiffHunk, kind: "add" | "remove"): string | null {
  const lines: string[] = [];
  for (const line of hunk.lines) {
    if (line.kind === kind) lines.push(line.text.trim());
  }
  if (lines.length < MIN_MOVED_LINES) return null;
  if (lines.every((l) => l === "")) return null;
  return lines.join("\n");
}

export interface MovedIndex {
  /// added-block signature → the path it was added in (first wins; a
  /// signature added in two files is ambiguous and this keeps the first,
  /// which is only ever used to NAME a counterpart, never to rank).
  bySignature: ReadonlyMap<string, string>;
  /// How many hunks were walked, and whether `MOVED_INDEX_HUNK_CAP` bit —
  /// the census the header strip prints so partiality is visible.
  scanned: number;
  capped: boolean;
}

/// Build the added-block index over every file diff the page currently
/// holds. `parsedByPath` is deliberately whatever has LOADED — see this
/// module's header for why that partiality is stated rather than fixed.
export function buildMovedIndex(parsedByPath: ReadonlyMap<string, ParsedDiff>): MovedIndex {
  const bySignature = new Map<string, string>();
  let scanned = 0;
  let capped = false;
  for (const [path, parsed] of parsedByPath) {
    for (const hunk of parsed.hunks) {
      if (scanned >= MOVED_INDEX_HUNK_CAP) {
        capped = true;
        return { bySignature, scanned, capped };
      }
      scanned += 1;
      const sig = blockSignature(hunk, "add");
      if (sig !== null && !bySignature.has(sig)) bySignature.set(sig, path);
    }
  }
  return { bySignature, scanned, capped };
}

/// The path this hunk's removed block reappears in, or `null`. A match
/// inside the SAME file is not a move between files and is not reported
/// (that is an intra-file reorder, which git's own diff already shows).
export function movedCounterpart(
  path: string,
  hunk: DiffHunk,
  index: MovedIndex,
): string | null {
  const sig = blockSignature(hunk, "remove");
  if (sig === null) return null;
  const to = index.bySignature.get(sig);
  return to !== undefined && to !== path ? to : null;
}

// ── the two public classifiers ─────────────────────────────────────────────

export interface NoiseFileSubject {
  path: string;
  /// git's own status letter(s) — `"R100"`, `"M"`, `"A"`, `"D"`.
  status: string;
  additions: number;
  deletions: number;
}

/// File-level labels. `generated` and `rename-only` are facts about the
/// FILE; `large` is reported at file level too (with its own, higher
/// threshold) because a 3000-line file diff is a different reading
/// decision from a 200-line hunk.
export function classifyFile(file: NoiseFileSubject): NoiseLabel[] {
  const out: NoiseLabel[] = [];
  const gen = generatedReason(file.path);
  if (gen !== null) out.push({ cls: "generated", rule: ruleText("generated"), detail: gen });
  if (file.status.startsWith("R") && file.additions === 0 && file.deletions === 0) {
    out.push({
      cls: "rename-only",
      rule: ruleText("rename-only"),
      detail: `status ${file.status}, +0 −0`,
    });
  }
  const changed = file.additions + file.deletions;
  if (changed > LARGE_FILE_LINES) {
    out.push({
      cls: "large",
      rule: ruleText("large"),
      detail: `${changed} changed lines (file threshold ${LARGE_FILE_LINES})`,
    });
  }
  return out;
}

/// Hunk-level labels. The FILE's own labels are passed in and INHERITED
/// (a hunk of a lockfile is generated), so the collapse rule can key on
/// one list per hunk; `large` is re-evaluated at the hunk threshold rather
/// than inherited, since that is a different question.
export function classifyHunk(
  path: string,
  hunk: DiffHunk,
  fileLabels: readonly NoiseLabel[],
  movedIndex: MovedIndex | null,
): NoiseLabel[] {
  const out: NoiseLabel[] = fileLabels.filter((l) => l.cls !== "large");
  if (isWhitespaceOnlyHunk(hunk)) {
    out.push({
      cls: "whitespace-only",
      rule: ruleText("whitespace-only"),
      detail: "added and removed text match once whitespace is removed",
    });
  }
  if (movedIndex) {
    const to = movedCounterpart(path, hunk, movedIndex);
    if (to !== null) {
      out.push({
        cls: "moved",
        rule: ruleText("moved"),
        detail: `the same ${MIN_MOVED_LINES}+ line block is added in ${to}`,
      });
    }
  }
  const { additions, deletions } = hunkStats(hunk);
  const changed = additions + deletions;
  if (changed > LARGE_HUNK_LINES) {
    out.push({
      cls: "large",
      rule: ruleText("large"),
      detail: `${changed} changed lines (hunk threshold ${LARGE_HUNK_LINES})`,
    });
  }
  return out;
}

/// The one-line summary a chip group renders when several labels apply —
/// classes in `NOISE_RULES` order, comma-joined. Purely presentational;
/// no label is ever dropped.
export function noiseSummary(labels: readonly NoiseLabel[]): string {
  const seen = new Set(labels.map((l) => l.cls));
  return NOISE_RULES.filter((r) => seen.has(r.id))
    .map((r) => r.label)
    .join(", ");
}

/// `?noise=` — `"shown"` (the default: every labelled hunk renders open)
/// or `"collapsed"` (labelled hunks render collapsed, counted, one click
/// from open). There is no third value and there is no "hidden".
export type NoiseMode = "shown" | "collapsed";

export function parseNoiseMode(raw: string | null): NoiseMode {
  return raw === "collapsed" ? "collapsed" : "shown";
}

/// Would `mode` collapse a hunk carrying `labels`? The ONE place the
/// collapse decision is made, so "a label is not a filter" is testable in
/// one assertion rather than trusted across three components.
export function noiseCollapses(mode: NoiseMode, labels: readonly NoiseLabel[]): boolean {
  return mode === "collapsed" && labels.length > 0;
}

/// The census the diff header prints: how many hunks carry a label, per
/// class, plus the total. Every number here is over hunks the page has
/// LOADED — the same partiality `buildMovedIndex` states, reported rather
/// than smoothed over.
export interface NoiseCensus {
  total: number;
  labelled: number;
  byClass: ReadonlyMap<NoiseClass, number>;
}

export function noiseCensus(perHunk: ReadonlyArray<readonly NoiseLabel[]>): NoiseCensus {
  const byClass = new Map<NoiseClass, number>();
  let labelled = 0;
  for (const labels of perHunk) {
    if (labels.length > 0) labelled += 1;
    for (const cls of new Set(labels.map((l) => l.cls))) {
      byClass.set(cls, (byClass.get(cls) ?? 0) + 1);
    }
  }
  return { total: perHunk.length, labelled, byClass };
}

export function noiseCensusText(census: NoiseCensus): string {
  if (census.total === 0) return "no hunks loaded";
  if (census.labelled === 0) return `${census.total} hunks · none labelled noise`;
  const parts = NOISE_RULES.filter((r) => (census.byClass.get(r.id) ?? 0) > 0).map(
    (r) => `${r.label} ${census.byClass.get(r.id)}`,
  );
  return `${census.labelled}/${census.total} hunks labelled · ${parts.join(" · ")}`;
}
