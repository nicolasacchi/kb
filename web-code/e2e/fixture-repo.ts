// Builds a tiny, deterministic git repo for the e2e suite to search
// against — one known Rust symbol the specs can look for by name, so
// "type a known symbol → the symbols lane finds it" doesn't depend on any
// real project's contents. Mirrors `crates/kb-code-server/tests/
// e2e_daemon.rs`'s own fixture-repo convention (git init + a couple of
// `fs::write`s + `git add -A && git commit`), just in Node instead of Rust
// since this harness spawns the fast-profile BINARY rather than calling into
// the crate directly.

import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export const KNOWN_SYMBOL = "omniboxTargetFunction";
export const KNOWN_FILE = "lib.rs";
// A distinctive literal that appears in KNOWN_FILE's body (not just its
// declaration line) — the text lane searches file CONTENT, so a spec
// exercising `/` (text) needs a needle that isn't also a valid symbol
// query.
export const TEXT_NEEDLE = "kbcodeTextLaneNeedle";
// A second, non-current branch (see `createFixtureRepo`'s tail) — the ref
// picker's W4.7 "Switch" action needs a target that ISN'T the current HEAD.
export const OTHER_BRANCH = "other-branch";

// B1 (clickable code) fixtures — a SEPARATE file so `clickable.spec.ts`
// never has to touch KNOWN_FILE's own line numbers (several other specs
// pin those). `CALLER_FILE` calls `KNOWN_SYMBOL` twice (`gr` needs >= 2
// references across the repo — KNOWN_FILE alone already contributes its
// own declaration + `helper()`'s call, but a second file makes the
// cross-file, not just cross-line, nature of the grep-based `refs` lookup
// obvious) and carries a comment naming `KNOWN_FILE` plus a URL (the
// linkify layer's path/url tokens).
export const CALLER_FILE = "caller.rs";
export const CALLER_URL = "https://example.com/kb-code-notes";

// B3 (position-based resolve) fixtures — a THIRD separate file, additive
// only, so neither `KNOWN_FILE`'s nor `CALLER_FILE`'s own pinned line
// numbers move. `local_target()` is doc-commented and defined exactly once,
// with its own call site a few lines below — resolve's FILE-LOCAL tier
// answers with exactly one candidate (no repo-wide tags-tier ambiguity),
// giving `gd` a clean single-candidate direct-jump case distinct from
// `KNOWN_SYMBOL`'s cross-file one.
export const RESOLVER_FILE = "resolver.rs";
export const LOCAL_TARGET_FN = "local_target";
export const LOCAL_TARGET_DOC = "Computes the fixture's local target value.";
/// 1-based — `local_target`'s own declaration line in `RESOLVER_FILE`
/// (pinned so specs can assert `gd`'s landing line without re-deriving it).
export const LOCAL_TARGET_DEF_LINE = 2;

// Phase C-SPA (Wave C, "time-first-class") fixtures — a THIRD branch,
// additive only: `feature-x` carries exactly ONE commit beyond `main`'s own
// initial commit, adding a brand-new file. HEAD stays on `main` (the
// checkout below returns to it), and `global-setup.ts`'s own
// `file_count >= 4` gate is unaffected — `FEATURE_FILE` only exists on
// `feature-x`'s own working tree, never merged into `main`.
export const FEATURE_BRANCH = "feature-x";
export const FEATURE_FILE = "feature_x.rs";
export const FEATURE_COMMIT_SUBJECT = "add feature-x support";

// Phase C7 (story mode) fixture — a file with its OWN two-commit history,
// added directly on `main`, additively, at the very END of
// `createFixtureRepo` (see that function's tail). It has to land ON `main`
// (not a side branch, unlike `feature-x` above) so the reader's default
// HEAD-current view can open it with no `?ref=` gymnastics — but that means
// it necessarily moves `main`'s own HEAD past the "initial fixture commit,"
// which several OTHER specs assumed was main's tip (`time.spec.ts`'s own
// `mainHeadSha`, since renamed/reimplemented as `initialCommitSha`, resolves
// the initial commit via `KNOWN_FILE`'s file-history instead of `main`'s
// live ref for exactly this reason) and which shifts `feature-x`'s ahead/
// behind numbers (it's now behind `main` by these 2 commits, not 0 — see
// the "branches page" test). `global-setup.ts`'s `file_count` gate is bumped
// from 4 to 5 to match the extra root file.
export const STORY_FILE = "story.rs";
export const STORY_COMMIT_1_SUBJECT = "add story.rs";
export const STORY_COMMIT_2_SUBJECT = "expand story.rs";

// V3.N2 — a dedicated file with TODO/FIXME/HACK markers for `todos.spec.ts`.
// Kept SEPARATE from KNOWN_FILE so existing line-number pins (clickable/
// resolve/reader-vim) stay byte-stable. Written into the initial commit
// (not a later main tip commit) so it doesn't shift story-mode HEAD math.
export const TODOS_FILE = "todos_fixture.rs";

// V3.1-H3a — type hierarchy fixture (trait + implementor). Added as a NEW
// commit on main at the END of createFixtureRepo (never touching the
// pinned initial commit or feature-x). hierarchy.spec.ts drives `gt` here.
export const HIER_TRAIT_FILE = "hier_trait.rs";
export const HIER_TRAIT_NAME = "Drawable";
export const HIER_IMPL_NAME = "Circle";
export const HIER_TRAIT_COMMIT_SUBJECT = "add hierarchy trait fixture";

// V3.1-H3b — impact / lenses / ego-graph additive fixtures. NEW commit only
// (never the pinned initial commit, never feature-x). Extra call site + a
// small test-ish path so the tests bucket and transitive edges have signal.
export const IMPACT_EXTRA_FILE = "impact_extra.rs";
export const IMPACT_HELPER_FN = "impact_helper";
export const IMPACT_TEST_FN = "test_impact_helper";
export const IMPACT_COMMIT_SUBJECT = "add impact/lenses ego fixture";

// V72-G1.2 — the `entity/1` dossier fixture. Ruby, because the entity index
// is Ruby-only (`entities/mod.rs`), and this suite had no `.rb` file at all.
//
// TWO DELIBERATE SHAPE CHOICES, both about NOT disturbing a shared fixture:
//
//  1. **Top-level files, not an `app/models/…` tree.** A second top-level
//     DIRECTORY would sort above every root file and shift the tree's row
//     cursor — the exact hazard `doclens-fixture.ts` records from when
//     `ambig/` was added, and the thing `nav-ramp.spec.ts`'s `openTreeScope`
//     (one `j`, then "the focused row is a file") depends on. Flat `.rb`
//     files sort among the existing root FILES, so that helper still lands on
//     a file and every row stays a row. The cost is that Zeitwerk has no
//     `app/` to read, so the dossier reports `zeitwerk: degraded` and
//     `honesty.state: "partial"` — which is not a loss but a GIFT: the spec
//     gets to assert the honest degraded rendering on real daemon output,
//     and `exact` is still reachable because it comes from lexical NESTING,
//     not from the autoload config (`entities::class_for`).
//  2. **Folded into the EXISTING impact commit, never a new one.** A new
//     commit on `main` bumps `feature-x`'s behind-count, which
//     `time.spec.ts` pins verbatim; the initial commit is pinned too (its
//     "5 root files" assertion). The impact commit is asserted by neither.
export const RUBY_ORDER_FILE = "shop_order.rb";
export const RUBY_RECORD_FILE = "application_record.rb";
export const RUBY_PAYABLE_FILE = "shop_payable.rb";
export const RUBY_INVOICE_FILE = "shop_invoice.rb";
/// The address `?ent=` is opened with. `Shop::Order` is defined by lexical
/// nesting (`module Shop; class Order`), so its definition block reaches
/// `exact` with no Rails tree in sight.
export const RUBY_ENTITY = "Shop::Order";
/// A method the member table must list — `tree`-derived, so `exact`.
export const RUBY_MEMBER = "to_s";
/// The nested class the namespace tree must show as a child of the entity.
export const RUBY_NESTED_CHILD = "Line";
/// An inherited member that appears ONLY when `?inherited=1` re-fetches:
/// `save` comes from `ApplicationRecord`, not from `Shop::Order`'s own body.
export const RUBY_INHERITED_MEMBER = "save";

// V3.3-U1 / stacks — a SECOND layer stacked ON feature-x (never moves
// feature-x's tip, never touches main or the pinned initial commit).
// Creates a 2-layer stack: main ← feature-x ← feature-x-2 for stacks.spec.
export const FEATURE_X2_BRANCH = "feature-x-2";
export const FEATURE_X2_FILE = "feature_x2.rs";
export const FEATURE_X2_COMMIT_SUBJECT = "add feature-x-2 stack layer";

function git(dir: string, args: string[]) {
  execFileSync("git", ["-C", dir, ...args], { stdio: "inherit" });
}

export function createFixtureRepo(dir: string): void {
  mkdirSync(dir, { recursive: true });
  git(dir, ["init", "-q", "-b", "main"]);
  git(dir, ["config", "user.email", "e2e@example.com"]);
  git(dir, ["config", "user.name", "kb-code e2e"]);

  writeFileSync(
    join(dir, KNOWN_FILE),
    [
      `fn ${KNOWN_SYMBOL}(a: i32, b: i32) -> i32 {`,
      `    // ${TEXT_NEEDLE}`,
      "    a + b",
      "}",
      "",
      "fn helper() -> i32 {",
      `    ${KNOWN_SYMBOL}(1, 2)`,
      "}",
      "",
    ].join("\n"),
  );
  writeFileSync(join(dir, "README.md"), "# fixture\n\nA fixture repo for kb-code's e2e suite.\n");

  // V3.N2 — separate file so KNOWN_FILE line numbers stay pinned for other
  // specs. tree-sitter extracts these comment markers into the TODO index.
  writeFileSync(
    join(dir, TODOS_FILE),
    [
      "// " + "TODO: e2e fixture todo marker",
      "fn todos_fixture() -> i32 {",
      "    // FIXME: e2e fixture fixme marker",
      "    1",
      "}",
      "",
      "// HACK: e2e fixture hack marker",
      "",
    ].join("\n"),
  );

  writeFileSync(
    join(dir, CALLER_FILE),
    [
      `// see ${KNOWN_FILE} and ${CALLER_URL} for context`,
      `fn caller_one() -> i32 {`,
      `    ${KNOWN_SYMBOL}(3, 4)`,
      `}`,
      "",
      `fn caller_two() -> i32 {`,
      `    ${KNOWN_SYMBOL}(5, 6)`,
      `}`,
      "",
    ].join("\n"),
  );

  writeFileSync(
    join(dir, RESOLVER_FILE),
    [
      `/// ${LOCAL_TARGET_DOC}`,
      `fn ${LOCAL_TARGET_FN}() -> i32 {`,
      "    42",
      "}",
      "",
      "fn other_helper() -> i32 {",
      "    1",
      "}",
      "",
      "fn calls_local_target() -> i32 {",
      `    ${LOCAL_TARGET_FN}()`,
      "}",
      "",
    ].join("\n"),
  );

  git(dir, ["add", "-A"]);
  git(dir, ["commit", "-q", "-m", "initial fixture commit"]);

  // A second, non-current branch pointing at the same commit — HEAD stays
  // on `main` (this does NOT check it out), giving the ref picker's W4.7
  // "Switch" action something to target (`checkout-dirty.spec.ts`). Named
  // distinctly from `main` so `RefPicker`'s `!r.is_head` guard renders its
  // own "Switch" button for it.
  git(dir, ["branch", OTHER_BRANCH]);

  // A third branch, `feature-x` — one commit ahead of `main`, adding a new
  // file. The checkout is temporary: the final `checkout -q main` below
  // restores HEAD to `main` before this function returns, so every OTHER
  // fixture assumption (current branch, `main`'s own file set) is
  // untouched — this is purely additive.
  git(dir, ["checkout", "-q", "-b", FEATURE_BRANCH]);
  writeFileSync(
    join(dir, FEATURE_FILE),
    ["// added on feature-x", "fn feature_x() -> i32 {", "    1", "}", ""].join("\n"),
  );
  git(dir, ["add", "-A"]);
  git(dir, ["commit", "-q", "-m", FEATURE_COMMIT_SUBJECT]);
  git(dir, ["checkout", "-q", "main"]);

  // Phase C7 — `story.rs`, two commits directly on `main` (see this file's
  // own doc above for why it can't live on a side branch, and the blast
  // radius of moving `main`'s HEAD past the initial commit). The second
  // commit ADDS a function rather than editing the first one, so its own
  // diff is a clean addition (a real "+" hunk for `story.spec.ts`'s
  // changed-line-tint assertion) while the FIRST commit's diff — against
  // its parent, the initial fixture commit, where `story.rs` doesn't exist
  // at all — is a from-scratch add (every line counts as new, the story
  // player's oldest-step case).
  writeFileSync(join(dir, STORY_FILE), ["fn tell_story() -> i32 {", "    1", "}", ""].join("\n"));
  git(dir, ["add", "-A"]);
  git(dir, ["commit", "-q", "-m", STORY_COMMIT_1_SUBJECT]);

  writeFileSync(
    join(dir, STORY_FILE),
    [
      "fn tell_story() -> i32 {",
      "    1",
      "}",
      "",
      "fn continue_story() -> i32 {",
      "    2",
      "}",
      "",
    ].join("\n"),
  );
  git(dir, ["add", "-A"]);
  git(dir, ["commit", "-q", "-m", STORY_COMMIT_2_SUBJECT]);

  // V3.1-H3a — additive NEW commit: small trait/impl pair for `gt` e2e.
  // Does not touch the initial fixture commit or feature-x.
  writeFileSync(
    join(dir, HIER_TRAIT_FILE),
    [
      `pub trait ${HIER_TRAIT_NAME} {`,
      "    fn draw(&self);",
      "}",
      "",
      `pub struct ${HIER_IMPL_NAME};`,
      "",
      `impl ${HIER_TRAIT_NAME} for ${HIER_IMPL_NAME} {`,
      "    fn draw(&self) {}",
      "}",
      "",
    ].join("\n"),
  );
  git(dir, ["add", "-A"]);
  git(dir, ["commit", "-q", "-m", HIER_TRAIT_COMMIT_SUBJECT]);

  // V3.1-H3b — additive NEW commit only (never the pinned initial commit,
  // never feature-x's tip). Extra call sites + a small test-ish file so
  // impact/lenses/ego-graph e2e have richer neighborhoods without moving
  // existing line pins.
  writeFileSync(
    join(dir, IMPACT_EXTRA_FILE),
    [
      `// impact/ego neighborhood helper for ${KNOWN_SYMBOL}`,
      `fn ${IMPACT_HELPER_FN}() -> i32 {`,
      `    ${KNOWN_SYMBOL}(7, 8)`,
      "}",
      "",
      `#[cfg(test)]`,
      `mod tests {`,
      `    use super::*;`,
      `    #[test]`,
      `    fn ${IMPACT_TEST_FN}() {`,
      `        let _ = ${KNOWN_SYMBOL}(0, 0);`,
      `    }`,
      `}`,
      "",
    ].join("\n"),
  );
  // V72-G1.2 — the Ruby entity fixture rides THIS commit (see the constants'
  // own doc for why it is neither a new commit nor a new directory).
  writeFileSync(
    join(dir, RUBY_RECORD_FILE),
    ["class ApplicationRecord", "  def save", "    true", "  end", "end", ""].join("\n"),
  );
  writeFileSync(
    join(dir, RUBY_PAYABLE_FILE),
    [
      "module Shop",
      "  module Payable",
      "    def pay",
      "      :paid",
      "    end",
      "  end",
      "end",
      "",
    ].join("\n"),
  );
  writeFileSync(
    join(dir, RUBY_ORDER_FILE),
    [
      "module Shop",
      "  class Order < ApplicationRecord",
      "    include Payable",
      "",
      "    TAX_RATE = 0.2",
      "",
      "    attr_accessor :total",
      "",
      `    def ${RUBY_MEMBER}`,
      '      "Order(#{@total})"',
      "    end",
      "",
      "    private",
      "",
      "    def secret_rate",
      "      TAX_RATE",
      "    end",
      "",
      "    # A metaprogramming hole the index reports but cannot see through.",
      "    define_method(:dynamic_total) { @total }",
      "",
      `    class ${RUBY_NESTED_CHILD}`,
      "      def amount",
      "        1",
      "      end",
      "    end",
      "  end",
      "end",
      "",
    ].join("\n"),
  );
  writeFileSync(
    join(dir, RUBY_INVOICE_FILE),
    ["module Shop", "  class Invoice < Order", "    include Payable", "  end", "end", ""].join("\n"),
  );
  git(dir, ["add", "-A"]);
  git(dir, ["commit", "-q", "-m", IMPACT_COMMIT_SUBJECT]);

  // V3.3-U1 — feature-x-2 stacked on feature-x's tip (one commit). HEAD
  // returns to main; feature-x tip is never moved (branch -f / checkout
  // feature-x only for the base of the new branch).
  git(dir, ["checkout", "-q", FEATURE_BRANCH]);
  git(dir, ["checkout", "-q", "-b", FEATURE_X2_BRANCH]);
  writeFileSync(
    join(dir, FEATURE_X2_FILE),
    [
      "// stacked on feature-x for stacks e2e",
      "fn feature_x2() -> i32 {",
      "    2",
      "}",
      "",
    ].join("\n"),
  );
  git(dir, ["add", "-A"]);
  git(dir, ["commit", "-q", "-m", FEATURE_X2_COMMIT_SUBJECT]);
  git(dir, ["checkout", "-q", "main"]);
}
