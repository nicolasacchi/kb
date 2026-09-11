//! SEC-17 — the source-scan lint that keeps the `Revspec`/`RefRange`
//! newtypes from decaying back into a convention.
//!
//! # This test's contract, stated plainly
//!
//! It is a SOURCE SCAN with an allowlist, not a dataflow analysis. It
//! cannot prove that no unvalidated `String` reaches `git`; nothing short
//! of a real taint analysis could, and this crate is not going to grow
//! one. What it CAN do — and what makes it worth its weight, in the spirit
//! of `schema::drift` — is make the surface it guards **impossible to grow
//! silently**:
//!
//! 1. The set of files that spawn a `git` subprocess is PINNED. Adding a
//!    new one fails this test with a message telling the author to make
//!    the new site take `Revspec`/`RefRange` and then add the file here —
//!    so a new git-spawning module is a reviewed decision, not a diff
//!    nobody read.
//! 2. The VALIDATION PREDICATE exists in exactly one place. Any file other
//!    than `git/revspec.rs` that re-implements the `starts_with('-')` +
//!    `contains("..")` shape is a second validator, and a second validator
//!    is how the two drift. (`history::reject_dash_prefixed` is the ONE
//!    named exception, and its own doc says why: git permits a ref whose
//!    NAME starts with `-`, so a name read out of `for-each-ref` still
//!    needs a check a type cannot give it.)
//! 3. Every git invocation that passes a caller-supplied PATHSPEC puts
//!    `--` before it.
//!
//! Failure of any of the three is a real signal; passing all three is not
//! a proof. That asymmetry is the honest description of what a source-scan
//! lint buys, and it is written here rather than in a commit message so
//! the next person to extend it knows what they are extending.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let p = entry.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// `needle` appears on a line that is not a comment.
///
/// Line-oriented, and that is the whole of its cleverness: it strips lines
/// whose first non-space characters are `//` (ordinary comments AND `///`
/// doc comments) and then looks for the needle. It does not parse Rust, so
/// it would still see a needle inside a string literal or a `/* */` block
/// — neither of which this crate's guards use.
///
/// Needed because the two sites this lint CAUGHT (`checkout.rs`,
/// `behavioral/ingest.rs`) were migrated onto `Revspec` and now carry
/// comments *explaining* the removed `starts_with('-')` guard. A scan that
/// cannot tell code from prose about code punishes the fix it asked for.
fn contains_outside_comments(src: &str, needle: &str) -> bool {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .any(|l| l.contains(needle))
}

fn rel(p: &Path) -> String {
    p.strip_prefix(src_root())
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every file allowed to spawn `git`. Growing this list is the reviewed
/// decision this test exists to force — see the module doc.
const GIT_SPAWNING_FILES: &[&str] = &[
    "blame/incremental.rs",
    "blame/mod.rs",
    "blame/timeline.rs",
    "behavioral/ingest.rs",
    "checkout.rs",
    "config.rs",
    "diff.rs",
    "doclens/remap.rs",
    "doclens/resolve_tests.rs",
    "doclens/sync_tests.rs",
    "git/commit.rs",
    "git/tests.rs",
    // V70-A3X's working-tree status/tree reader. Audited when the lint first
    // caught it: it spawns `git` twice and takes NO caller-supplied REF, so
    // there is nothing for `Revspec`/`RefRange` to wrap. Its one
    // caller-supplied value is a PATHSPEC (`list_worktree_dir`'s `dir`), and
    // it is already passed after an explicit `--`; the other call is a fixed
    // `status --porcelain=v2 -z` with no interpolation. The third
    // `Command::new("git")` in the file is a `#[cfg(test)]` fixture helper.
    "git_status.rs",
    "github.rs",
    "history/branches.rs",
    "history/commit.rs",
    "history/compare.rs",
    "history/file_history.rs",
    // V76-R3d — production code spawns nothing (`run_git_raw`); the only
    // `Command::new("git")` is the `#[cfg(test)]` fixture helper aliased
    // as `StdCommand` (the `file_history.rs` case). Pathspecs go after `--`.
    "history/scrub.rs",
    "history/merge_check.rs",
    "history/mod.rs",
    // V75-M3's conflict radar. It spawns `git` directly for the same
    // reason `history/merge_check.rs` does and one more: `merge-tree
    // --write-tree` exits 1 for an ordinary conflict (so `run_git_raw`'s
    // any-non-zero-is-a-failure contract would misclassify it), AND every
    // child must carry the SEC-15 scratch-ODB environment, which
    // `run_git_raw` does not set. Both caller-supplied values are
    // `Revspec`s (`?against=` and each enumerated candidate ref); the only
    // other interpolated argv entry is `<tree oid>:<path>`, where the oid
    // is 40 hex characters git itself printed and the path can therefore
    // never be read as a flag.
    "history/radar.rs",
    "history/range_diff.rs",
    "history/scratch.rs",
    "history/stacks.rs",
    "ingest.rs",
    "join/backfill.rs",
    "join/backfill_tests.rs",
    "join/ladder_tests.rs",
    "join/local.rs",
    // V72-H4a's `git.behavior` lane. Audited when the lint caught it: the
    // PRODUCTION code in this file spawns nothing — every one of its four
    // git invocations goes through `history::run_git_raw` — and the only
    // `Command::new("git")` is the `#[cfg(test)]` fixture builder that
    // constructs a synthetic repo, exactly the case `git_status.rs`'s own
    // note records. Its one caller-supplied value is a PATHSPEC (the facts
    // route's `?path=`), passed after an explicit `--` and asserted by
    // `caller_supplied_pathspecs_are_preceded_by_a_double_dash` below; the
    // only other interpolated argv entries are shas the daemon read out of
    // its OWN `git log` output and re-validated as 40 hex characters.
    "lanes/git_behavior.rs",
    "mirror/reconcile.rs",
    "provenance/report.rs",
    "recipes.rs",
    "repo_state.rs",
    "reviews.rs",
    "scip.rs",
    "sessiondiff/git_diff.rs",
    "sessiondiff/session_diff_tests.rs",
    "sink.rs",
    "agentview/impact.rs",
    // V76-R3b — worktree lifecycle (create/lock/unlock/repair/prune/
    // remove). Audited when added: every caller-supplied PATH is passed
    // after an explicit `--` via `run_git_path`; every caller-supplied
    // branch is a `Revspec`; lock `--reason=` is a single argv entry so a
    // reason cannot be option-parsed. Fixture helpers under `#[cfg(test)]`
    // also spawn `git`. Classify itself shells only through
    // `workspace::common_dir`/`root_commit` (`history::run_git_raw`).
    "worktrees.rs",
];

#[test]
fn the_set_of_git_spawning_files_is_pinned() {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    let found: BTreeSet<String> = files
        .iter()
        .filter(|p| {
            std::fs::read_to_string(p)
                .map(|s| contains_outside_comments(&s, "Command::new(\"git\")"))
                .unwrap_or(false)
        })
        .map(|p| rel(p))
        .collect();
    let allowed: BTreeSet<String> = GIT_SPAWNING_FILES.iter().map(|s| s.to_string()).collect();

    let added: Vec<&String> = found.difference(&allowed).collect();
    assert!(
        added.is_empty(),
        "SEC-17: these files newly spawn `git`: {added:?}\n\
         Make every CALLER-SUPPLIED ref/range argument a `crate::git::Revspec` \
         or `RefRange` (their only constructors are the validator), put `--` \
         before any caller-supplied pathspec, then add the file to \
         GIT_SPAWNING_FILES in this test."
    );
    let removed: Vec<&String> = allowed.difference(&found).collect();
    assert!(
        removed.is_empty(),
        "SEC-17: these files no longer spawn `git` — drop them from \
         GIT_SPAWNING_FILES so the list keeps meaning something: {removed:?}"
    );
}

#[test]
fn the_revspec_validation_predicate_lives_in_exactly_one_place() {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    // The shape of a hand-rolled revspec guard: a leading-dash rejection.
    // `git/revspec.rs` owns it; `history/mod.rs` keeps ONE documented
    // exception for repo-ENUMERATED names (see its doc).
    //
    // This test found two more on its first run — `checkout::switch_repo`
    // (the daemon's first sanctioned working-tree mutation, whose target
    // is a request-body field) and `behavioral::ingest::commit_reachable`
    // — and both were migrated onto `Revspec` rather than added here. A
    // lint whose failure mode is "extend the allowlist" is a lint that
    // stops meaning anything.
    const OWNER: &str = "git/revspec.rs";
    const EXCEPTION: &str = "history/mod.rs";
    let offenders: Vec<String> = files
        .iter()
        .filter(|p| {
            let r = rel(p);
            r != OWNER && r != EXCEPTION
        })
        .filter(|p| {
            std::fs::read_to_string(p)
                .map(|s| contains_outside_comments(&s, "starts_with('-')"))
                .unwrap_or(false)
        })
        .map(|p| rel(p))
        .collect();
    assert!(
        offenders.is_empty(),
        "SEC-17: a second revspec validator in {offenders:?}. The predicate \
         belongs to `git::revspec::Revspec::parse` — call it (or take the \
         type) rather than re-deriving the rule."
    );
}

#[test]
fn caller_supplied_pathspecs_are_preceded_by_a_double_dash() {
    // The two helpers that take BOTH a revspec and a caller-supplied path
    // in one argv. Asserted on the source rather than by execution because
    // the failure mode is silent (git re-reads the path as a revspec, or
    // vice versa) and only shows up on a pathological filename.
    let diff = std::fs::read_to_string(src_root().join("diff.rs")).unwrap();
    let sep = diff
        .find("args.push(\"--\");")
        .expect("diff_file separates pathspecs with `--`");
    let path_push = diff
        .find("args.push(path);")
        .expect("diff_file pushes the pathspec");
    assert!(
        sep < path_push,
        "diff_file must push `--` BEFORE the pathspec"
    );

    let fh = std::fs::read_to_string(src_root().join("history/file_history.rs")).unwrap();
    assert!(
        fh.contains("\"--\""),
        "file_history must separate its pathspec with `--`"
    );

    // V72-H4a — `lanes/git_behavior.rs` takes a caller-supplied PATH
    // (`?path=` off the facts route) into two `git log` invocations. It
    // spawns nothing itself (every call goes through
    // `history::run_git_raw`, which is why it is absent from
    // GIT_SPAWNING_FILES), but the pathspec rule is about ARGV, not about
    // who spawns — so it is asserted here beside the other two.
    let gb = std::fs::read_to_string(src_root().join("lanes/git_behavior.rs")).unwrap();
    let sep = gb
        .find("\"--\",")
        .expect("git_behavior separates its pathspec with `--`");
    let path_arg = gb
        .find("            path,")
        .expect("git_behavior passes the caller-supplied path as an argv entry");
    assert!(
        sep < path_arg,
        "git_behavior must push `--` BEFORE the pathspec"
    );

    // V75-M3 — `history/facts.rs` takes a caller-supplied PATH
    // (`touches:<path>`, off the branch-facts route's kbcq/1 query) into a
    // `git diff --name-only <range> -- <path>`. It spawns nothing itself
    // (every call goes through `history::run_git_raw`, which is why it is
    // absent from GIT_SPAWNING_FILES), but the pathspec rule is about
    // ARGV, not about who spawns.
    //
    // Asserted on the WHITESPACE-STRIPPED source rather than on two
    // `find` offsets: this argv is one short slice literal that rustfmt is
    // free to keep on one line or break across five, and an assertion that
    // fails on FORMATTING is the failure mode `contains_outside_comments`
    // above already exists to avoid.
    let facts: String = std::fs::read_to_string(src_root().join("history/facts.rs"))
        .unwrap()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(
        facts.contains("\"--\",path]"),
        "facts::touches_path must place `--` immediately before its caller-supplied pathspec"
    );

    // `blame/timeline.rs` is the ONE documented non-case: it embeds the
    // path inside `-L <line>,<line>:<path>`, an option VALUE rather than a
    // pathspec position, so `--` neither applies nor helps. Recorded here
    // so the next reader does not "fix" it.
    let tl = std::fs::read_to_string(src_root().join("blame/timeline.rs")).unwrap();
    assert!(
        tl.contains("{line},{line}:{path}"),
        "blame/timeline still embeds its path in `-L`; revisit the `--` \
         exception recorded in this test if that changed"
    );

    // V76-R3b — every caller-supplied worktree PATH is preceded by `--`
    // inside `run_git_path` (create/lock/unlock/repair/remove).
    let wt: String = std::fs::read_to_string(src_root().join("worktrees.rs"))
        .unwrap()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(
        wt.contains("args.push(\"--\");args.push(path_s.as_ref());"),
        "worktrees::run_git_path must push `--` immediately before its caller-supplied path"
    );

    // V76-R3d — `history/scrub.rs` takes a caller-supplied PATH into
    // `git log --follow … -- <path>` (one walk, `collect_stops`). The
    // floor is derived from that walk (`file_floor(stops: &[Stop])`), so
    // there is no second git invocation to lint — pin that it stays pure.
    let scrub = std::fs::read_to_string(src_root().join("history/scrub.rs")).unwrap();
    let sep = scrub
        .find("args.push(\"--\");")
        .expect("collect_stops separates its pathspec with `--`");
    let path_push = scrub
        .find("args.push(path);")
        .expect("collect_stops pushes the pathspec");
    assert!(
        sep < path_push,
        "collect_stops must push `--` BEFORE the pathspec"
    );
    assert!(
        scrub.contains("fn file_floor(stops: &[Stop])"),
        "file_floor must derive the floor from the followed walk, never a second git call"
    );
}
