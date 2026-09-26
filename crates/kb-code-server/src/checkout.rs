//! W4.7 — confirmed checkout (wave-4 operator ruling: the first
//! sanctioned working-tree mutation this daemon performs; V4.S1's
//! suggestion apply is the second). `switch_repo` refuses
//! (structured, listing every dirty path) on a dirty working tree; a clean
//! tree gets `git switch <ref>` for a known local branch, or `git checkout
//! <ref>` for anything else (tag, remote branch, or a raw sha — `git
//! switch` refuses those without an explicit `--detach`, whereas plain
//! `git checkout` has always detached HEAD for them, exactly matching what
//! an operator typing the equivalent command at a terminal would get).
//!
//! Deliberately owns NO store/mirror wiring: the existing live-mirror
//! watcher (`crate::mirror`) already treats a plain (non-marker) `git
//! checkout`'s HEAD move as an ordinary idle `GitDirSignal::HeadCandidate`
//! (see that module's doc, "A plain (non-marker) operation like `git
//! checkout`" — and `tests/mirror_matrix.rs`'s own
//! `checkout_produces_head_moved_and_one_reconcile_matching_diff` case
//! already pins exactly this for a raw `git checkout` subprocess), so
//! `switch_repo` below is nothing more than the subprocess call itself —
//! the daemon's already-armed watcher picks up the resulting HEAD move and
//! working-tree delta on its own, with no special-casing required.
//!
//! `CheckoutError` is its own enum, deliberately NOT shared with this
//! crate's other five git-subprocess wrappers (`diff::DiffError`,
//! `blame::BlameError`, `sessiondiff::git_diff::DiffError`,
//! `history::HistoryError`, `mirror::reconcile`'s error-less subprocess
//! call) — see `diff.rs`'s
//! module doc ("Why `DiffError` isn't shared with its siblings") for the
//! full rationale. This module's own variant that only ITS callers need:
//! `Dirty`, the refuse-on-a-dirty-tree guard `POST /api/checkout` maps to a
//! structured 409 body (`routes.rs`) — a shape no sibling wrapper has any
//! use for.
//!
//! **RS-U8** adds the review-store bridge ([`resolve_target_via_store`]):
//! once a repo's review store is `ready`, a checkout/worktree-add target
//! that names a review/PR tip (a full sha, or a `refs/kbc/*` name — see
//! `crate::git::roots::is_store_addressable`) the local clone does not yet
//! have is fetched BY SHA from the store, with NO ref written into the
//! clone (design-internal-store.md §7 step 2: no destination refspec, so
//! git lands the object and nothing else — not even `FETCH_HEAD`,
//! suppressed by `--no-write-fetch-head`). This is why `switch_repo` takes
//! an `Option<&StoreRoot>` now: with `None` (no ready store — every repo
//! before its store finishes seeding) the bridge is a hard no-op and
//! behaviour is byte-for-byte what it was before this unit — README
//! §10.1's "exactly today's behaviour". `worktrees::create_worktree`'s
//! `branch` arm reuses [`resolve_target_via_store`] for the exact same
//! reason (a linked worktree materializing a review tip is the other half
//! of README §9's "3 checkout/worktree changes that fetch by sha from the
//! store").

use crate::git::roots::StoreRoot;
use crate::git::Revspec;
use std::path::Path;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum CheckoutError {
    /// The working tree has uncommitted changes — every dirty path, exactly
    /// as `git status --porcelain` reports it (the 2-character status
    /// prefix stripped).
    #[error("working tree is dirty ({} path(s))", .0.len())]
    Dirty(Vec<String>),
    #[error("failed to spawn git: {0}")]
    Spawn(std::io::Error),
    #[error("git {} failed: {stderr}", .args.join(" "))]
    GitFailed { args: Vec<String>, stderr: String },
    /// `target` reached here starting with `-` — passed as a raw argv entry
    /// to `git switch`/`git checkout`, where a leading dash makes git
    /// option-parse it rather than treat it as a ref, even though this
    /// spawn never goes through a shell (same injection class as
    /// `diff::DiffError::BadRevspec`). `POST /api/checkout` is already
    /// mounted on the LOOPBACK-ONLY sub-router (`router.rs`), so this is
    /// belt-and-suspenders rather than the primary defense — but rejected
    /// before `is_local_branch`/`run_git` ever see it regardless.
    #[error("ref must not start with '-': {0:?}")]
    BadTarget(String),
}

pub type Result<T> = std::result::Result<T, CheckoutError>;

/// What actually happened — the route's 200 body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutOutcome {
    /// The ref as given by the caller (branch name, tag, or sha).
    pub target: String,
    /// `true` when this landed on a detached HEAD (anything that isn't a
    /// known local branch).
    pub detached: bool,
}

/// `git status --porcelain` — every dirty path (staged, unstaged, or
/// untracked), repo-relative, with the 2-character porcelain status prefix
/// stripped. Empty means a clean working tree. Deliberately no further
/// parsing of the status codes themselves (mirrors `blame`/`sessiondiff`'s
/// own git subprocess wrappers: pass the caller everything, don't
/// over-interpret) — a rename's `old -> new` form is kept verbatim.
pub fn dirty_paths(repo_root: &Path) -> Result<Vec<String>> {
    let out = run_git(repo_root, &["status", "--porcelain"])?;
    Ok(out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.get(3..).unwrap_or(l).trim().to_string())
        .collect())
}

/// `true` if `target` names a local branch (`refs/heads/<target>`) in this
/// repo — decides `switch` vs `checkout` below.
fn is_local_branch(repo_root: &Path, target: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{target}"),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Refuse (`CheckoutError::Dirty`) on a dirty working tree; otherwise run
/// `git switch <target>` (known local branch) or `git checkout <target>`
/// (anything else) — see the module doc for the full contract.
///
/// V70-A2 (SEC-17) — `target` is a [`Revspec`], the type whose only
/// constructor is the validator. This is the daemon's FIRST sanctioned
/// working-tree mutation, so it is exactly the call the critique means by
/// "make it a type, not a habit": the old inline `starts_with('-')` guard
/// is gone, and an unvalidated `String` can no longer reach this fn at
/// all. `CheckoutError::BadTarget` survives as the wire shape the route
/// maps a `RevspecError` to, so the refusal a caller sees is unchanged.
///
/// RS-U8 — `store` is the caller's repo's review store, ONLY when it is
/// `ready` (`crate::git::roots::GitCtx::store_root`); see
/// [`resolve_target_via_store`] for the fetch-by-sha bridge this runs
/// AFTER the dirty check (never before — a dirty tree is refused before
/// this daemon does any store I/O on its behalf) and BEFORE deciding
/// `switch` vs `checkout` (the resolved sha is never a local branch, so a
/// bridged checkout is always reported `detached`, matching the design's
/// "`git switch --detach <sha>`").
pub fn switch_repo(
    repo_root: &Path,
    target: &Revspec,
    store: Option<&StoreRoot>,
) -> Result<CheckoutOutcome> {
    let requested = target.as_str();
    let dirty = dirty_paths(repo_root)?;
    if !dirty.is_empty() {
        return Err(CheckoutError::Dirty(dirty));
    }
    let effective = resolve_target_via_store(repo_root, store, target)?;
    let detached = !is_local_branch(repo_root, &effective);
    let verb = if detached { "checkout" } else { "switch" };
    run_git(repo_root, &[verb, &effective])?;
    Ok(CheckoutOutcome {
        target: requested.to_string(),
        detached,
    })
}

/// RS-U8 — resolve `target` against `repo_root`, fetching the object BY
/// SHA from a ready review store when the local repo does not have it yet.
/// Returns the revision the caller should actually hand to `git switch`/
/// `git checkout`/`git worktree add` in place of `target`.
///
/// * `store = None` is ALWAYS a no-op: `target` comes back unchanged and
///   nothing beyond the caller's own eventual git invocation runs — "no
///   store" covers every repo before its review store finishes seeding
///   (README §10.1).
/// * A `target` that is not store-addressable — an ordinary local branch
///   name, `HEAD`, anything that is not a full sha or a `refs/kbc/*` name
///   (`crate::git::roots::is_store_addressable`) — is also a no-op: such a
///   name means something different in a bare store than in the user
///   clone (design-internal-store.md §6), so the store is never even
///   asked.
/// * Otherwise: a `refs/kbc/*` NAME is resolved to a sha INSIDE the store
///   first (`git rev-parse`) — never against the clone, which may hold a
///   stale legacy ref of the same name; a miss in the store is not an
///   error — `target` comes back unchanged and the caller's own
///   switch/checkout behaves exactly as it always would have. A full sha is
///   used as is. The sha is fetched only if `repo_root` does not already
///   have the object (idempotent: a repeat checkout of an already-fetched
///   review tip does not fetch again), and the SHA is returned.
///
/// The fetch itself is `git -C <repo_root> fetch --no-tags
/// --no-write-fetch-head --no-auto-gc --no-auto-maintenance <store> <sha>`
/// (design-internal-store.md §7 step 2) — a LOCAL, uncredentialed,
/// ref-less fetch: no destination refspec means git lands the object in
/// the ODB and writes NO ref, not even `FETCH_HEAD`. `crate::review_store`'s
/// `StoreGit` (the ONLY spawner in this crate allowed to carry a
/// credential) is deliberately not used here — both sides of this fetch
/// are local filesystem paths, and the store's own config already sets
/// `uploadpack.allowAnySHA1InWant=true` (`review_store::seed`); the
/// `--upload-pack` override below is belt-and-suspenders, matching the
/// design's explicit "the override is also passed per invocation".
///
/// Because the returned revision is a SHA whenever the store had to be
/// consulted at all, a caller that substitutes a `refs/kbc/*` name must
/// use the RETURNED string for the actual mutation — that name still does
/// not exist in `repo_root` afterwards, only the object it pointed to
/// does.
pub(crate) fn resolve_target_via_store(
    repo_root: &Path,
    store: Option<&StoreRoot>,
    target: &Revspec,
) -> Result<String> {
    let target = target.as_str();
    let Some(store) = store else {
        return Ok(target.to_string());
    };
    if !crate::git::roots::is_store_addressable(target) {
        return Ok(target.to_string());
    }
    // A `refs/kbc/*` NAME is resolved in the store FIRST: the store is
    // authoritative, and a user clone may still carry a stale legacy ref of
    // the same name (pre-store installs wrote `refs/kbc/*` into clones) —
    // checking the name locally first would silently check out that stale
    // commit. Only the resulting sha is ever looked up in the clone.
    let sha = if crate::git::roots::is_object_id(target) {
        target.to_string()
    } else {
        match rev_parse_commit_in(store.git_dir(), target) {
            Some(sha) => sha,
            None => return Ok(target.to_string()),
        }
    };
    if !object_exists_locally(repo_root, &sha) {
        fetch_sha_no_ref(repo_root, store.git_dir(), &sha)?;
    }
    Ok(sha)
}

/// `true` if `repo_root`'s own ODB already has `rev` — checked BEFORE any
/// store lookup so a repeat checkout/worktree-add of an already-fetched
/// review tip touches the store exactly zero times.
fn object_exists_locally(repo_root: &Path, rev: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["cat-file", "-e", rev])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve `rev` to a full commit sha INSIDE `store_git_dir` — never run
/// against the user repo. `None` means the store doesn't have this name
/// either (not an error: see [`resolve_target_via_store`]'s doc).
fn rev_parse_commit_in(store_git_dir: &Path, rev: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(store_git_dir)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{rev}^{{commit}}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// `git -C repo_root fetch --no-write-fetch-head <store_git_dir> <sha>` —
/// see [`resolve_target_via_store`]'s doc for why this writes no ref.
fn fetch_sha_no_ref(repo_root: &Path, store_git_dir: &Path, sha: &str) -> Result<()> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-auto-gc",
            "--no-auto-maintenance",
            "--quiet",
            "--upload-pack",
            "git -c uploadpack.allowAnySHA1InWant=true upload-pack",
        ])
        .arg(store_git_dir)
        .arg(sha)
        .output()
        .map_err(CheckoutError::Spawn)?;
    if !out.status.success() {
        return Err(CheckoutError::GitFailed {
            args: vec![
                "fetch".to_string(),
                store_git_dir.display().to_string(),
                sha.to_string(),
            ],
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(())
}

/// V70-A2 (SEC-17) — a `RevspecError` from the route's own
/// `Revspec::parse` folds into this module's `BadTarget`, so a rejected
/// `POST /api/checkout` body renders byte-identically to the pre-V70-A2
/// inline guard's refusal.
impl From<crate::git::RevspecError> for CheckoutError {
    fn from(e: crate::git::RevspecError) -> Self {
        CheckoutError::BadTarget(e.0)
    }
}

fn run_git(repo_root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(CheckoutError::Spawn)?;
    if !out.status.success() {
        return Err(CheckoutError::GitFailed {
            args: args.iter().map(|s| s.to_string()).collect(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Test-local shorthand: every fixture target here is a branch name or
    /// sha this test just minted, so `parse` cannot fail.
    fn rs(s: &str) -> Revspec {
        Revspec::parse(s).expect("fixture revspec parses")
    }
    use std::process::Command as StdCommand;

    fn git(dir: &Path, args: &[&str]) {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git -C {} {:?} failed: {}",
            dir.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
    }

    fn fixture_two_branches() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init_repo(dir);
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        git(dir, &["branch", "feature"]);
        tmp
    }

    #[test]
    fn dirty_paths_is_empty_on_a_clean_tree() {
        let tmp = fixture_two_branches();
        assert!(dirty_paths(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn dirty_paths_lists_untracked_and_modified_files() {
        let tmp = fixture_two_branches();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "modified\n").unwrap();
        std::fs::write(dir.join("untracked.txt"), "new\n").unwrap();
        let mut dirty = dirty_paths(dir).unwrap();
        dirty.sort();
        assert_eq!(
            dirty,
            vec!["a.txt".to_string(), "untracked.txt".to_string()]
        );
    }

    #[test]
    fn switch_repo_refuses_on_a_dirty_tree_listing_every_path() {
        let tmp = fixture_two_branches();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "modified\n").unwrap();
        let err = switch_repo(dir, &rs("feature"), None).unwrap_err();
        match err {
            CheckoutError::Dirty(paths) => assert_eq!(paths, vec!["a.txt".to_string()]),
            other => panic!("expected Dirty, got {other:?}"),
        }
        // HEAD must not have moved.
        let head = git_head(dir);
        assert_eq!(head, "main");
    }

    #[test]
    fn switch_repo_rejects_a_dash_prefixed_target_without_moving_head() {
        let tmp = fixture_two_branches();
        let dir = tmp.path();
        // V70-A2: the refusal moved INTO the type this fn takes, so a
        // dash-prefixed target is unreachable by construction; the
        // route-visible error is the same `BadTarget` as before.
        let err: CheckoutError = Revspec::parse("--help").unwrap_err().into();
        assert!(matches!(err, CheckoutError::BadTarget(_)), "got: {err:?}");
        // ...and HEAD is where it was: nothing ran.
        assert_eq!(git_head(dir), "main");
    }

    #[test]
    fn switch_repo_switches_a_clean_tree_to_a_local_branch() {
        let tmp = fixture_two_branches();
        let dir = tmp.path();
        let outcome = switch_repo(dir, &rs("feature"), None).unwrap();
        assert_eq!(outcome.target, "feature");
        assert!(!outcome.detached);
        assert_eq!(git_head(dir), "feature");
    }

    #[test]
    fn switch_repo_detaches_for_a_raw_sha() {
        let tmp = fixture_two_branches();
        let dir = tmp.path();
        let sha = String::from_utf8(
            StdCommand::new("git")
                .arg("-C")
                .arg(dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let outcome = switch_repo(dir, &rs(&sha), None).unwrap();
        assert!(outcome.detached);
        // Detached HEAD: `git symbolic-ref` fails (no branch name).
        let sym = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["symbolic-ref", "-q", "--short", "HEAD"])
            .output()
            .unwrap();
        assert!(!sym.status.success(), "HEAD must be detached");
    }

    #[test]
    fn switch_repo_reports_a_clean_error_for_an_unknown_ref() {
        let tmp = fixture_two_branches();
        let dir = tmp.path();
        let err = switch_repo(dir, &rs("does-not-exist"), None).unwrap_err();
        match err {
            CheckoutError::GitFailed { .. } => {}
            other => panic!("expected GitFailed, got {other:?}"),
        }
        // HEAD must still be on main — a failed checkout leaves it alone.
        assert_eq!(git_head(dir), "main");
    }

    fn git_head(dir: &Path) -> String {
        String::from_utf8(
            StdCommand::new("git")
                .arg("-C")
                .arg(dir)
                .args(["symbolic-ref", "--short", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }

    fn git_head_sha(dir: &Path) -> String {
        String::from_utf8(
            StdCommand::new("git")
                .arg("-C")
                .arg(dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }

    /// `for-each-ref` of `dir` — the invariance snapshot BUILD-BRIEF U8 asks
    /// for ("record for-each-ref + packed-refs + the refs/ tree ... before
    /// and after"). `for-each-ref` never lists `HEAD` itself, so a
    /// byte-identical result before/after a checkout is exactly "only HEAD
    /// moved, no ref was written".
    fn ref_tree(dir: &Path) -> Vec<String> {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(["for-each-ref", "--format=%(refname) %(objectname)"])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A "user repo" with one commit, and a separate bare "store" (the
    /// same `uploadpack.allowAnySHA1InWant=true` config `review_store::seed`
    /// writes into a real store) holding a SECOND commit the user repo has
    /// never seen, reachable inside the store only by the review-ref shape
    /// `refs/kbc/review/<id>/ps<n>` — never a branch, so `is_local_branch`
    /// can never see it either.
    fn store_bridge_fixture() -> (tempfile::TempDir, PathBuf, StoreRoot, String) {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        std::fs::create_dir_all(&user).unwrap();
        init_repo(&user);
        std::fs::write(user.join("a.txt"), "one\n").unwrap();
        git(&user, &["add", "-A"]);
        git(&user, &["commit", "-q", "-m", "one"]);

        // The commit that will live ONLY in the store — an unrelated repo,
        // minted separately so the user repo's own ODB never sees it by
        // accident (ancestry doesn't matter: allowAnySHA1InWant fetches by
        // sha regardless).
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        init_repo(&src);
        std::fs::write(src.join("b.txt"), "two\n").unwrap();
        git(&src, &["add", "-A"]);
        git(&src, &["commit", "-q", "-m", "review tip"]);
        let tip = git_head_sha(&src);

        let store_dir = tmp.path().join("store.git");
        StdCommand::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&store_dir)
            .status()
            .unwrap();
        std::fs::write(
            store_dir.join("config"),
            "[core]\n\trepositoryformatversion = 0\n\tbare = true\n\
             [uploadpack]\n\tallowAnySHA1InWant = true\n",
        )
        .unwrap();
        git(
            &store_dir,
            &[
                "fetch",
                "-q",
                src.to_str().unwrap(),
                "HEAD:refs/kbc/review/1/ps1",
            ],
        );

        (tmp, user, StoreRoot::for_test(store_dir), tip)
    }

    #[test]
    fn a_store_only_review_ref_is_fetched_by_sha_and_checked_out_detached_with_no_new_ref() {
        let (_tmp, user, store, tip) = store_bridge_fixture();
        let before = ref_tree(&user);
        let target = rs("refs/kbc/review/1/ps1");
        let outcome = switch_repo(&user, &target, Some(&store)).unwrap();
        assert!(outcome.detached);
        assert_eq!(git_head_sha(&user), tip);
        let after = ref_tree(&user);
        assert_eq!(
            before, after,
            "checkout via the store bridge must write no ref besides HEAD"
        );
        // The store-only NAME itself still does not exist locally — only
        // the object it pointed to does.
        assert!(!object_exists_locally(&user, "refs/kbc/review/1/ps1"));
    }

    #[test]
    fn a_stale_legacy_ref_in_the_clone_never_shadows_the_store() {
        let (_tmp, user, store, tip) = store_bridge_fixture();
        // A pre-store install left `refs/kbc/review/1/ps1` in the clone,
        // pointing at an older commit than the store's.
        let stale = git_head_sha(&user);
        assert_ne!(stale, tip);
        git(&user, &["update-ref", "refs/kbc/review/1/ps1", &stale]);
        let outcome = switch_repo(&user, &rs("refs/kbc/review/1/ps1"), Some(&store)).unwrap();
        assert!(outcome.detached);
        assert_eq!(git_head_sha(&user), tip, "the store is authoritative");
    }

    #[test]
    fn a_full_sha_already_fetched_is_never_re_fetched() {
        let (_tmp, user, store, tip) = store_bridge_fixture();
        switch_repo(&user, &rs("refs/kbc/review/1/ps1"), Some(&store)).unwrap();
        // The bogus store below would error loudly if it were ever
        // consulted — proving the second checkout of the SAME sha touches
        // the store zero times (idempotent, README's "0 fallback" spirit).
        let bogus = StoreRoot::for_test(PathBuf::from("/nonexistent-kb-code-store"));
        let outcome = switch_repo(&user, &rs(&tip), Some(&bogus)).unwrap();
        assert!(outcome.detached);
        assert_eq!(git_head_sha(&user), tip);
    }

    #[test]
    fn a_ready_store_lacking_the_name_falls_back_to_the_ordinary_refusal() {
        let (_tmp, user, store, _tip) = store_bridge_fixture();
        let err = switch_repo(&user, &rs("refs/kbc/review/999/ps1"), Some(&store)).unwrap_err();
        assert!(
            matches!(err, CheckoutError::GitFailed { .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn an_ordinary_branch_target_never_consults_the_store() {
        let (_tmp, user, _store, _tip) = store_bridge_fixture();
        git(&user, &["branch", "feature"]);
        // A bogus store: if `switch_repo` ever asked it about "feature"
        // (not store-addressable), this would error loudly.
        let bogus = StoreRoot::for_test(PathBuf::from("/nonexistent-kb-code-store"));
        let outcome = switch_repo(&user, &rs("feature"), Some(&bogus)).unwrap();
        assert!(!outcome.detached);
    }

    #[test]
    fn no_ready_store_is_byte_identical_to_before_this_bridge_existed() {
        let (_tmp, user, _store, _tip) = store_bridge_fixture();
        let before = ref_tree(&user);
        let err = switch_repo(&user, &rs("refs/kbc/review/1/ps1"), None).unwrap_err();
        assert!(
            matches!(err, CheckoutError::GitFailed { .. }),
            "got: {err:?}"
        );
        assert_eq!(ref_tree(&user), before);
    }
}
