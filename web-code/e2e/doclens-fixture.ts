// DCB W2.B — a lightweight `node:http` mock standing in for kb's own daemon
// (per `[kb_daemon] url`) for the lens e2e suite (R16 — supersedes the
// earlier "spawn a fixture kb-server" idea: this only needs to prove
// Lens.tsx's OWN rendering/nav/pin-round-trip logic against a fixed,
// deterministic `coderef/1` payload, not kb-server's real extraction or
// kb-code's real resolution engine — both already have their own golden
// suites). No framework, matching the harness's existing plain-Node style
// (`global-setup.ts`'s own `fetch`/`writeFileSync` use).
//
// **The rev_remap demo is the one part of this file that must be REAL,
// not canned**: `crates/kb-code-server/src/doclens/remap.rs` runs INSIDE
// the real `kb-code-server` binary under test, against its REAL configured
// fixture repo — a mock "kb" can hand it a `code_rev.sha` and a citation,
// but whether that citation actually remaps (and by how much) depends on
// genuine git history existing at that sha. `seedRemapDemo` therefore adds
// TWO real commits to the shared e2e fixture repo, additively (a brand-new
// file, `DOCLENS_REMAP_FILE` — never touching `fixture-repo.ts`'s own
// pinned files/lines, so no other spec's line-number assumptions move).

import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import * as http from "node:http";
import { join } from "node:path";

export const DOCLENS_FIXTURE_PORT = 4758; // distinct from 4737/4747/4748/4757
export const DOCLENS_KB = "fixture-kb";

export const DOCLENS_DOC_ID = "doc1";
export const DOCLENS_DOC_PATH = "docs/fixture-doc.html";
export const DOCLENS_DOC_TITLE = "Fixture Doc";
export const DOCLENS_GROUP_KEY = "kb-h-section-one"; // R17 — key === anchor
export const DOCLENS_GROUP_LABEL = "Section One";

export const DOCLENS_NEVER_SCANNED_ID = "neverscanned01";
export const DOCLENS_NEVER_SCANNED_PATH = "docs/never-scanned.html";

// DCB-W2.B.R fix 3 — a THIRD doc whose kb-side `truncated: true` (a real
// `coderef/1` extraction-cap flag, not something kb-code computes) exercises
// `truncationCaption`'s "showing N of M" arm end to end. Deliberately
// SEPARATE from `doc1`: `ref_count` here (the CLAIMED full total) is bigger
// than the actual `refs[]` shipped below — genuinely honest, the same shape
// a real capped kb-core extraction produces — where folding this into
// `doc1` would either falsify one of that doc's own already-pinned counts
// or render a self-contradictory "showing 9 of 9".
export const DOCLENS_TRUNCATED_DOC_ID = "doc3";
export const DOCLENS_TRUNCATED_DOC_PATH = "docs/truncated-fixture.html";

// DCB-W2.B.R fix 9 — real duplicate-basename files backing `doc1`'s two
// ambiguous refs. `resolve_path`'s suffix-candidate ambiguity is computed
// SERVER-SIDE against the REAL configured repo (`crates/kb-code-server/src/
// doclens/resolve.rs::resolve_path`) — a `candidate_count`/`candidates`
// value put directly on the canned `coderef/1` body would be silently
// ignored, since those are OUTPUT-only fields this daemon computes itself,
// never input `CodeRefRow` fields. `seedAmbiguityDemo` (below) adds these
// additively, same convention as `seedRemapDemo`.
export const DOCLENS_AMBIGUOUS_PAIR_BASENAME = "dup_pair.rs";
export const DOCLENS_AMBIGUOUS_MANY_BASENAME = "dup_many.rs";

// A SEPARATE doc for the rev_remap demo (never shares `doc1`'s `code_rev`):
// remap's current pre-W2.A.R behavior has no EOF bound-check (a fix landing
// after this phase adds one, values-only, per the mid-flight review note),
// so `doc1`'s own "hint past EOF" ref (`resolver.rs:9999`) deliberately
// carries NO `code_rev` at all — it must keep exercising the ORIGINAL
// token-pass predicate, not interact with remap's current known gap.
export const DOCLENS_REMAP_DOC_ID = "doc2";
export const DOCLENS_REMAP_DOC_PATH = "docs/remap-fixture.html";
export const DOCLENS_REMAP_FILE = "doclens_remap_fixture.rs";

const REMAP_BEFORE = [
  "fn doclens_remap_anchor() -> i32 {",
  "    1",
  "}",
  "",
  "fn doclens_remap_target() -> i32 {",
  "    2",
  "}",
  "",
];
const REMAP_INSERTED = [
  "// inserted for the DCB W2.B rev_remap e2e demo — a pure insertion above",
  "// doclens_remap_target(), which shifts DOWN by exactly",
  "// DOCLENS_REMAP_INSERTED_LINES; doclens_remap_anchor() above is unaffected.",
  "",
];

/// 1-based — `doclens_remap_anchor`'s line AT the older commit (`shaA`) —
/// ABOVE the later insertion, so it maps to ITSELF (delta 0 → a plain
/// "confirmed" badge, not "moved").
export const DOCLENS_REMAP_ANCHOR_LINE = REMAP_BEFORE.indexOf("fn doclens_remap_anchor() -> i32 {") + 1;
/// 1-based — `doclens_remap_target`'s line AT `shaA`, BELOW the insertion —
/// what the fixture's ref cites.
export const DOCLENS_REMAP_TARGET_OLD_LINE = REMAP_BEFORE.indexOf("fn doclens_remap_target() -> i32 {") + 1;
/// How many lines `seedRemapDemo`'s second commit inserts — the exact
/// `line_hint_delta` the "moved +N · git-verified" badge must render.
export const DOCLENS_REMAP_INSERTED_LINES = REMAP_INSERTED.length;
/// `doclens_remap_target`'s line in the CURRENT working tree — informational
/// only (the SPA never needs this; the daemon computes it fresh).
export const DOCLENS_REMAP_TARGET_NEW_LINE = DOCLENS_REMAP_TARGET_OLD_LINE + DOCLENS_REMAP_INSERTED_LINES;

// DCB W3.B — "cited by" demo: ONE dedicated file cited by TWO SEPARATE docs,
// so the reverse index's headline count is genuinely "2 docs", not one doc
// appearing twice (`doc1`'s own resolver.rs citations, ordinals 1 and 2
// below, are two ROWS from the SAME doc — not reused here on purpose;
// `web-code/src/lib/citedBy.test.ts`'s "same doc, two ordinals" case already
// covers that shape at the unit level). Additive-only, same convention as
// `seedRemapDemo`/`seedAmbiguityDemo` above — and, uniquely among this
// file's demo fixtures, the ONE file `doc-lens.spec.ts`'s own rotted-claim
// case deletes mid-suite (never `resolver.rs`/`lib.rs`/any file another spec
// depends on).
export const DOCLENS_CITEDBY_FILE = "doclens_citedby_fixture.rs";
export const DOCLENS_CITEDBY_DOC_A_ID = "doc4";
export const DOCLENS_CITEDBY_DOC_A_PATH = "docs/citedby-a.html";
export const DOCLENS_CITEDBY_DOC_A_TITLE = "Cited-by Demo A";
export const DOCLENS_CITEDBY_GROUP_KEY = "kb-h-citedby-findings";
export const DOCLENS_CITEDBY_GROUP_LABEL = "Findings";
// Mid-flight W3.A review note, corrected by W3.B.R (M2): `doc-refs/1`'s
// `doc_title` is non-null but the server never persists it EMPTY either —
// `sync.rs`'s `doc_title_or_fallback` (DCB-W3.A.R fix 6) already falls back
// to the doc path's basename, then the doc id, at WRITE time, so a
// title-less kb doc can't reach the wire as `""`. `doc5` deliberately
// carries `title: null` (below, in its `coderef/1` body — the INPUT to that
// server-side fallback) so this fixture exercises the SERVER-side ladder
// end to end; `CitedBy.tsx`'s own `citedByRowLabel` fallback is
// redundant-but-safe defense-in-depth on top of that (covered instead by
// `web-code/src/lib/citedBy.test.ts`'s unit cases). Either way the render
// falls back to the doc_path basename, `DOCLENS_CITEDBY_DOC_B_BASENAME`,
// not an invisible empty link.
export const DOCLENS_CITEDBY_DOC_B_ID = "doc5";
export const DOCLENS_CITEDBY_DOC_B_PATH = "docs/citedby-b.html";
export const DOCLENS_CITEDBY_DOC_B_BASENAME = "citedby-b.html";

function git(dir: string, args: string[]): string {
  return execFileSync("git", ["-C", dir, ...args], { encoding: "utf-8" }).trim();
}

/// Additive-only (see this module's header doc). Returns the OLDER commit's
/// sha — the coderef/1 fixture's `doc2` `code_rev.sha`.
export function seedRemapDemo(repoDir: string): string {
  writeFileSync(join(repoDir, DOCLENS_REMAP_FILE), REMAP_BEFORE.join("\n"));
  git(repoDir, ["add", "-A"]);
  git(repoDir, ["commit", "-q", "-m", "doclens: add rev_remap demo fixture"]);
  const shaA = git(repoDir, ["rev-parse", "HEAD"]);

  const after = [...REMAP_BEFORE.slice(0, 4), ...REMAP_INSERTED, ...REMAP_BEFORE.slice(4)];
  writeFileSync(join(repoDir, DOCLENS_REMAP_FILE), after.join("\n"));
  git(repoDir, ["add", "-A"]);
  git(repoDir, ["commit", "-q", "-m", "doclens: shift rev_remap demo target"]);

  return shaA;
}

/// DCB-W2.B.R fix 9 — additive-only (see the module header doc). Seeds:
/// - 2 real files sharing [`DOCLENS_AMBIGUOUS_PAIR_BASENAME`] under
///   different directories ⇒ `resolve_path`'s suffix-candidate match finds
///   exactly 2 candidates, the `ambiguous-inline` tier (≤3, `AMBIGUITY_
///   INLINE_MAX`).
/// - 5 real files sharing [`DOCLENS_AMBIGUOUS_MANY_BASENAME`] ⇒ 5
///   candidates, the `ambiguous-search` tier — `candidate_count: 5`,
///   `candidates: []` (over the inline cap), the exact B2-named regression
///   shape `refTier.test.ts` already pins at the unit level and this
///   fixture now exercises end to end.
///
/// Nested under ONE new top-level directory (`ambig/`), not seven —
/// `lib/tree.ts`'s `sortTreeEntries` always ranks directories BEFORE files
/// regardless of name (the conventional file-tree sort), so any new
/// top-level directory shifts every root FILE row down by one; consolidating
/// to a single one keeps that shift to exactly one row instead of seven
/// (`split.spec.ts`'s tree-cursor test accounts for the one-row shift — see
/// its own updated comment).
function seedAmbiguityDemo(repoDir: string): void {
  const base = "ambig";
  for (const dir of ["a", "b"]) {
    mkdirSync(join(repoDir, base, dir), { recursive: true });
    writeFileSync(join(repoDir, base, dir, DOCLENS_AMBIGUOUS_PAIR_BASENAME), `// ${dir}\n`);
  }
  for (const dir of ["c1", "c2", "c3", "c4", "c5"]) {
    mkdirSync(join(repoDir, base, dir), { recursive: true });
    writeFileSync(join(repoDir, base, dir, DOCLENS_AMBIGUOUS_MANY_BASENAME), `// ${dir}\n`);
  }
  git(repoDir, ["add", "-A"]);
  git(repoDir, ["commit", "-q", "-m", "doclens: add ambiguity demo fixtures"]);
}

/// Exported so `doc-lens.spec.ts`'s rotted-claim case can restore the file
/// byte-for-byte in its own `afterAll` — that spec deletes this file from
/// disk to exercise `DocRefsOut.live: false`, and MUST leave the fixture
/// repo clean afterward (git-tracked, unlike `checkout-dirty.spec.ts`'s own
/// untracked scratch file): every OTHER spec that runs after `doc-lens.spec.ts`
/// alphabetically shares this SAME daemon + repo, and several drive real
/// checkouts (`POST /api/checkout` 409s on a dirty tree).
export const DOCLENS_CITEDBY_FILE_CONTENT = ["fn doclens_citedby_fixture() -> i32 {", "    7", "}", ""].join("\n");

/// DCB W3.B — additive-only (see this module's header doc + the constants'
/// own comment above). A single small file, unique basename in the repo, so
/// `resolve_path` gives both `doc4`'s and `doc5`'s citation a `path_state:
/// "present"` unique match — the ONLY thing W3.A's `doc_refs` sync persists
/// a row for (§1.1's filter). Deliberately its own commit, separate from
/// `seedAmbiguityDemo`'s, so deleting this ONE file (the rotted-claim e2e
/// case) never touches the ambiguity demo's own files.
function seedCitedByDemo(repoDir: string): void {
  writeFileSync(join(repoDir, DOCLENS_CITEDBY_FILE), DOCLENS_CITEDBY_FILE_CONTENT);
  git(repoDir, ["add", "-A"]);
  git(repoDir, ["commit", "-q", "-m", "doclens: add cited-by demo fixture"]);
}

// --- canned coderef/1 bodies (R1/R3/R17 field shapes) -----------------------

interface CodeRefsBody {
  schema: string;
  kb: string;
  doc_id: string;
  doc_path: string;
  doc_hash: string | null;
  // `null` is a real, on-the-wire kb state (a doc with no title at all) —
  // widened from `string` so `doc5`'s deliberate empty-title fixture (the
  // mid-flight W3.A review note) type-checks. Every OTHER doc here still
  // carries a real string.
  title: string | null;
  extracted_at: number | null;
  never_scanned: boolean;
  code_rev: { label: string; sha: string; dirty: boolean } | null;
  ref_count: number;
  ungrouped_count: number;
  truncated: boolean;
  groups: Array<{ key: string; label: string; anchor: string; ordinal: number }>;
  refs: unknown[];
}

function codeRefsBodies(shaA: string): Record<string, CodeRefsBody> {
  return {
    [DOCLENS_DOC_ID]: {
      schema: "coderef/1",
      kb: DOCLENS_KB,
      doc_id: DOCLENS_DOC_ID,
      doc_path: DOCLENS_DOC_PATH,
      doc_hash: "fixturehash1",
      title: DOCLENS_DOC_TITLE,
      extracted_at: 1_754_500_000,
      never_scanned: false,
      code_rev: null,
      // DCB-W2.B.R fix 9 bumped these from 5/3 to 9/7 — 4 new refs
      // (ordinals 5-8 below), all ungrouped.
      ref_count: 9,
      ungrouped_count: 7,
      truncated: false,
      groups: [
        { key: DOCLENS_GROUP_KEY, label: DOCLENS_GROUP_LABEL, anchor: DOCLENS_GROUP_KEY, ordinal: 0 },
      ],
      refs: [
        // ordinal 0 — bare `path` kind, unique basename in the fixture repo:
        // `path_state: present`, and — since it carries no line hint at
        // all — a non-null `reader` whose `line` is still `null` (the
        // null-reader.line ref).
        {
          ordinal: 0,
          group: DOCLENS_GROUP_KEY,
          kind: "path",
          raw: "lib.rs",
          path_hint: "lib.rs",
          line_start: null,
          line_end: null,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: "see lib.rs for the main entry point",
          context_tokens: [],
          declared: false,
        },
        // ordinal 1 — `resolver.rs:2` (LOCAL_TARGET_DEF_LINE), containing
        // the token `local_target` at the cited line ⇒ `line_state:
        // confirmed`.
        {
          ordinal: 1,
          group: DOCLENS_GROUP_KEY,
          kind: "path_line",
          raw: "resolver.rs:2",
          path_hint: "resolver.rs",
          line_start: 2,
          line_end: 2,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: "the local target function",
          context_tokens: ["local_target"],
          declared: false,
        },
        // ordinal 2 — same path, a wildly-off line (past EOF) ⇒
        // `line_state: unverifiable` (zero extractable tokens in the ±64
        // window around 9999 — the fixture repo's `resolver.rs` is a
        // dozen lines long).
        {
          ordinal: 2,
          group: null,
          kind: "path_line",
          raw: "resolver.rs:9999",
          path_hint: "resolver.rs",
          line_start: 9999,
          line_end: 9999,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: "a citation with a wildly off line number",
          context_tokens: ["local_target"],
          declared: false,
        },
        // ordinal 3 — `path_state: absent`; `declared: true` exercises
        // Decision 2's doc-rot signal (`note: "declared but absent"`).
        {
          ordinal: 3,
          group: null,
          kind: "path_line",
          raw: "nonexistent-file.rb:1",
          path_hint: "nonexistent-file.rb",
          line_start: 1,
          line_end: 1,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: "a file that was never real",
          context_tokens: [],
          declared: true,
        },
        // ordinal 4 — pathless symbol ref hitting nothing in the fixture
        // repo ⇒ `symbol_state: no_symbol`.
        {
          ordinal: 4,
          group: null,
          kind: "symbol_method",
          raw: "Namespace::Class#method",
          path_hint: null,
          line_start: null,
          line_end: null,
          line_spans: null,
          symbol_container: "Namespace::Class",
          symbol_member: "method",
          context: "a symbol this fixture repo never defines",
          context_tokens: [],
          declared: false,
        },
        // ordinal 5 — DCB-W2.B.R fix 9: ambiguous, 2 real candidates
        // (`seedAmbiguityDemo`) ⇒ `path_state: ambiguous`, `candidate_count:
        // 2` (≤ AMBIGUITY_INLINE_MAX) ⇒ tier `ambiguous-inline`.
        {
          ordinal: 5,
          group: null,
          kind: "path",
          raw: DOCLENS_AMBIGUOUS_PAIR_BASENAME,
          path_hint: DOCLENS_AMBIGUOUS_PAIR_BASENAME,
          line_start: null,
          line_end: null,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: "an ambiguous path with two real candidates",
          context_tokens: [],
          declared: false,
        },
        // ordinal 6 — DCB-W2.B.R fix 9: ambiguous, 5 real candidates ⇒
        // `candidate_count: 5`, `candidates: []` (over the inline cap) ⇒
        // tier `ambiguous-search` — the exact B2-named regression shape.
        {
          ordinal: 6,
          group: null,
          kind: "path",
          raw: DOCLENS_AMBIGUOUS_MANY_BASENAME,
          path_hint: DOCLENS_AMBIGUOUS_MANY_BASENAME,
          line_start: null,
          line_end: null,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: "an ambiguous path with five real candidates",
          context_tokens: [],
          declared: false,
        },
        // ordinal 7 — DCB-W2.B.R fix 9: an issue ref. R11's overload: for
        // `kind === "issue"`, `line_start` is the issue NUMBER, not a line,
        // and `path_hint` is `owner/repo`.
        {
          ordinal: 7,
          group: null,
          kind: "issue",
          raw: "acme/shopfront#15357",
          path_hint: "acme/shopfront",
          line_start: 15357,
          line_end: null,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: "a citation to a tracked issue, not a file",
          context_tokens: [],
          declared: false,
        },
        // ordinal 8 — DCB-W2.B.R fix 9: an external (gem/vendor) ref —
        // fully inert, never resolved against the checkout ⇒
        // `path_state: external`, tier `external` (fix 6).
        {
          ordinal: 8,
          group: null,
          kind: "external",
          raw: "lodash (vendor)",
          path_hint: null,
          line_start: null,
          line_end: null,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: "a vendor dependency, not part of this repo",
          context_tokens: [],
          declared: false,
        },
      ],
    },
    [DOCLENS_REMAP_DOC_ID]: {
      schema: "coderef/1",
      kb: DOCLENS_KB,
      doc_id: DOCLENS_REMAP_DOC_ID,
      doc_path: DOCLENS_REMAP_DOC_PATH,
      doc_hash: "fixturehash2",
      title: "Rev-remap demo",
      extracted_at: 1_754_500_100,
      never_scanned: false,
      // `label` must equal the configured repo's `name` (`REPO_NAME`,
      // case-insensitively) or the remap engine refuses with
      // `rev_label_mismatch` instead of applying (`remap.rs::prepare`).
      code_rev: { label: "fixture", sha: shaA, dirty: false },
      ref_count: 2,
      ungrouped_count: 2,
      truncated: false,
      groups: [],
      refs: [
        {
          ordinal: 0,
          group: null,
          kind: "path_line",
          raw: `${DOCLENS_REMAP_FILE}:${DOCLENS_REMAP_ANCHOR_LINE}`,
          path_hint: DOCLENS_REMAP_FILE,
          line_start: DOCLENS_REMAP_ANCHOR_LINE,
          line_end: DOCLENS_REMAP_ANCHOR_LINE,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: "the anchor, unaffected by the later insertion",
          context_tokens: [],
          declared: false,
        },
        {
          ordinal: 1,
          group: null,
          kind: "path_line",
          raw: `${DOCLENS_REMAP_FILE}:${DOCLENS_REMAP_TARGET_OLD_LINE}`,
          path_hint: DOCLENS_REMAP_FILE,
          line_start: DOCLENS_REMAP_TARGET_OLD_LINE,
          line_end: DOCLENS_REMAP_TARGET_OLD_LINE,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: "the target, shifted by the later insertion",
          context_tokens: [],
          declared: false,
        },
      ],
    },
    [DOCLENS_NEVER_SCANNED_ID]: {
      schema: "coderef/1",
      kb: DOCLENS_KB,
      doc_id: DOCLENS_NEVER_SCANNED_ID,
      doc_path: DOCLENS_NEVER_SCANNED_PATH,
      doc_hash: null,
      title: "Never scanned",
      extracted_at: null,
      never_scanned: true,
      code_rev: null,
      ref_count: 0,
      ungrouped_count: 0,
      truncated: false,
      groups: [],
      refs: [],
    },
    // DCB-W2.B.R fix 3 — `truncated: true` with `ref_count` (kb's CLAIMED
    // full total) bigger than the 2 refs actually shipped below — the same
    // honest shape a real kb-core extraction cap produces, exercising
    // `truncationCaption`'s "showing N of M" arm end to end.
    [DOCLENS_TRUNCATED_DOC_ID]: {
      schema: "coderef/1",
      kb: DOCLENS_KB,
      doc_id: DOCLENS_TRUNCATED_DOC_ID,
      doc_path: DOCLENS_TRUNCATED_DOC_PATH,
      doc_hash: "fixturehash3",
      title: "Truncated demo",
      extracted_at: 1_754_500_200,
      never_scanned: false,
      code_rev: null,
      ref_count: 20,
      ungrouped_count: 2,
      truncated: true,
      groups: [],
      refs: [
        {
          ordinal: 0,
          group: null,
          kind: "path",
          raw: "lib.rs",
          path_hint: "lib.rs",
          line_start: null,
          line_end: null,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: null,
          context_tokens: [],
          declared: false,
        },
        {
          ordinal: 1,
          group: null,
          kind: "path",
          raw: "resolver.rs",
          path_hint: "resolver.rs",
          line_start: null,
          line_end: null,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: null,
          context_tokens: [],
          declared: false,
        },
      ],
    },
    // DCB W3.B — the cited-by demo: two SEPARATE docs, each with ONE ref
    // citing `DOCLENS_CITEDBY_FILE`. `doc4`'s ref carries a group (exercises
    // the expanded list's group-label row); `doc5`'s is ungrouped.
    [DOCLENS_CITEDBY_DOC_A_ID]: {
      schema: "coderef/1",
      kb: DOCLENS_KB,
      doc_id: DOCLENS_CITEDBY_DOC_A_ID,
      doc_path: DOCLENS_CITEDBY_DOC_A_PATH,
      doc_hash: "fixturehash4",
      title: DOCLENS_CITEDBY_DOC_A_TITLE,
      extracted_at: 1_754_500_300,
      never_scanned: false,
      code_rev: null,
      ref_count: 1,
      ungrouped_count: 0,
      truncated: false,
      groups: [
        {
          key: DOCLENS_CITEDBY_GROUP_KEY,
          label: DOCLENS_CITEDBY_GROUP_LABEL,
          anchor: DOCLENS_CITEDBY_GROUP_KEY,
          ordinal: 0,
        },
      ],
      refs: [
        {
          ordinal: 0,
          group: DOCLENS_CITEDBY_GROUP_KEY,
          kind: "path",
          raw: DOCLENS_CITEDBY_FILE,
          path_hint: DOCLENS_CITEDBY_FILE,
          line_start: null,
          line_end: null,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: "demo A's grouped citation",
          context_tokens: [],
          declared: false,
        },
      ],
    },
    [DOCLENS_CITEDBY_DOC_B_ID]: {
      schema: "coderef/1",
      kb: DOCLENS_KB,
      doc_id: DOCLENS_CITEDBY_DOC_B_ID,
      doc_path: DOCLENS_CITEDBY_DOC_B_PATH,
      doc_hash: "fixturehash5",
      // Deliberately titleless (mid-flight W3.A review note, corrected by
      // W3.B.R — see the constant's own comment above): `doc_title_or_
      // fallback` (`sync.rs`, DCB-W3.A.R fix 6) resolves this `null` INPUT
      // to the doc_path basename before it's ever persisted — `doc_refs.
      // doc_title` never lands on the wire as `""`. `CitedBy.tsx`'s row
      // renders whatever the server sends, which is already the basename
      // here, not an empty string it has to rescue client-side.
      title: null,
      extracted_at: 1_754_500_400,
      never_scanned: false,
      code_rev: null,
      ref_count: 1,
      ungrouped_count: 1,
      truncated: false,
      groups: [],
      refs: [
        {
          ordinal: 0,
          group: null,
          kind: "path",
          raw: DOCLENS_CITEDBY_FILE,
          path_hint: DOCLENS_CITEDBY_FILE,
          line_start: null,
          line_end: null,
          line_spans: null,
          symbol_container: null,
          symbol_member: null,
          context: "demo B's ungrouped citation",
          context_tokens: [],
          declared: false,
        },
      ],
    },
  };
}

const BY_PATH: Record<string, string> = {
  [DOCLENS_DOC_PATH]: DOCLENS_DOC_ID,
  [DOCLENS_REMAP_DOC_PATH]: DOCLENS_REMAP_DOC_ID,
  [DOCLENS_NEVER_SCANNED_PATH]: DOCLENS_NEVER_SCANNED_ID,
  [DOCLENS_TRUNCATED_DOC_PATH]: DOCLENS_TRUNCATED_DOC_ID,
  [DOCLENS_CITEDBY_DOC_A_PATH]: DOCLENS_CITEDBY_DOC_A_ID,
  [DOCLENS_CITEDBY_DOC_B_PATH]: DOCLENS_CITEDBY_DOC_B_ID,
};

function json(res: http.ServerResponse, status: number, body: unknown) {
  const bytes = JSON.stringify(body);
  res.writeHead(status, { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(bytes) });
  res.end(bytes);
}

// DCB W3.C — mutable per-doc `doc_hash` override, flipped ONLY via the
// dev-only `POST /__test__/bump-hash` route below. `doc-lens.spec.ts`'s
// "doc changed since" banner + re-materialize case needs a REAL hash
// change to observe (`SetDetail.tsx`'s `useDocLens` re-fetch reads whatever
// this mock returns at request time, not a canned constant) — never
// present outside this fixture. `Map`, not a single mutable slot: keyed
// per-doc so a future spec could bump a different doc without this one's
// override leaking onto it.
const hashOverrides = new Map<string, string>();

/// Applies [`hashOverrides`] to one canned body, if present — used by BOTH
/// the single-doc route and the feed walk below so a sync pass run AFTER a
/// bump also observes it.
function withHashOverride(body: CodeRefsBody): CodeRefsBody {
  const override = hashOverrides.get(body.doc_id);
  return override ? { ...body, doc_hash: override } : body;
}

/// Seeds the rev_remap demo commits + the ambiguity demo fixtures (fix 9),
/// then starts the mock. Both routes ignore the `Authorization` header
/// entirely — a fixture, not a security test.
export function startDoclensFixture(repoDir: string): Promise<http.Server> {
  const shaA = seedRemapDemo(repoDir);
  seedAmbiguityDemo(repoDir);
  seedCitedByDemo(repoDir);
  const bodies = codeRefsBodies(shaA);

  const server = http.createServer((req, res) => {
    const url = new URL(req.url ?? "/", `http://127.0.0.1:${DOCLENS_FIXTURE_PORT}`);
    const path = url.pathname;

    // DCB W3.C — dev-only test hook: `{"doc": "<doc_id>"}` flips that doc's
    // `doc_hash` to a fresh, timestamped value. Never present outside this
    // fixture; `doc-lens.spec.ts`'s own "sets-from-doc" describe block is
    // the sole caller.
    if (path === "/__test__/bump-hash" && req.method === "POST") {
      let raw = "";
      req.on("data", (chunk: Buffer) => (raw += chunk.toString("utf-8")));
      req.on("end", () => {
        let doc: unknown;
        try {
          ({ doc } = JSON.parse(raw || "{}") as { doc?: unknown });
        } catch {
          json(res, 400, { error: "bad json" });
          return;
        }
        if (typeof doc !== "string" || !doc) {
          json(res, 400, { error: "doc must be a non-empty string" });
          return;
        }
        hashOverrides.set(doc, `bumped-${Date.now()}`);
        json(res, 200, { ok: true });
      });
      return;
    }

    // `GET /api/kb/{kb}/docs/{id}/code-refs`
    const oneDoc = path.match(/^\/api\/kb\/([^/]+)\/docs\/([^/]+)\/code-refs$/);
    if (oneDoc) {
      const [, kb, id] = oneDoc;
      if (kb === DOCLENS_KB && Object.prototype.hasOwnProperty.call(bodies, id)) {
        json(res, 200, withHashOverride(bodies[id]));
      } else {
        json(res, 404, { error: `no such doc ${id}` });
      }
      return;
    }

    // `GET /api/kb/{kb}/code-refs?cursor=&limit=&refs=` — R3/B5: NO `/docs/`
    // segment. DCB W3.B: this used to always return an empty page ("W3's
    // sync feed isn't exercised by this spec") — now that `doc-lens.spec.ts`
    // drives a real `POST /api/doc-lens/sync` pass, it must hand back real
    // doc headers to walk. One page, every doc in `bodies` (sync itself
    // filters to pinned docs — see `sync.rs`'s own `docs_skipped_unpinned`),
    // `next_cursor` omitted so `run_doclens_sync` treats it as the last page
    // and never loops. `?refs=0` (the header-only walk `KbClient::
    // code_refs_feed`'s own doc names) strips `refs`/`groups` per doc, same
    // shape kb's real feed uses — pagination mechanics themselves aren't
    // this fixture's job (kb-core's own feed-cursor unit tests cover that).
    const feed = path.match(/^\/api\/kb\/([^/]+)\/code-refs$/);
    if (feed) {
      const [, kb] = feed;
      if (kb !== DOCLENS_KB) {
        json(res, 200, { schema: "coderef-feed/1", kb, docs: [] });
        return;
      }
      const withRefs = url.searchParams.get("refs") !== "0";
      const docs = Object.values(bodies)
        .map(withHashOverride)
        .map((b) => (withRefs ? b : { ...b, refs: [], groups: [] }));
      json(res, 200, { schema: "coderef-feed/1", kb, docs });
      return;
    }

    // `GET /api/kb/{kb}/docs/by-path/{*path}` — kb's own by-path lookup
    // (R2/R20's resolve-path ramp calls THIS, server-side, through
    // `KbClient::resolve_doc_by_path`).
    const byPath = path.match(/^\/api\/kb\/([^/]+)\/docs\/by-path\/(.+)$/);
    if (byPath) {
      const [, kb, rawPath] = byPath;
      const decoded = decodeURIComponent(rawPath);
      const id = kb === DOCLENS_KB ? BY_PATH[decoded] : undefined;
      if (id) {
        json(res, 200, { id, path: decoded, title: bodies[id]?.title ?? decoded });
      } else {
        json(res, 404, { error: `no doc at path ${decoded}` });
      }
      return;
    }

    json(res, 404, { error: "not found" });
  });

  return new Promise((resolvePromise, reject) => {
    server.once("error", reject);
    server.listen(DOCLENS_FIXTURE_PORT, "127.0.0.1", () => resolvePromise(server));
  });
}
