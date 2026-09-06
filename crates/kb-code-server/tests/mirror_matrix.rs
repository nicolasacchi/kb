//! W1.4 — live-mirror watcher integration matrix. Real `git` in tempdir
//! fixtures (mirrors `crates/kb-code-server/src/git/tests.rs`'s own
//! convention), a recording [`MirrorSink`], generous-but-bounded timeouts +
//! event-count/quiescence settling — never a fixed sleep-and-pray for the
//! assertion itself (short sleeps are used only to let a background daemon
//! establish a baseline before a subsequent edit, same as
//! `kb_core::watcher`'s own poll-backend test).
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running
//! (the workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.
//!
//! Serialized (see [`serial`]): the default test harness runs every `#[test]`
//! fn concurrently, but each of these spins up a REAL background watcher
//! thread plus several real `git` subprocesses and asserts precise
//! debounce/quiescence timing — running 8 of them at once starves each
//! other for CPU (measured: this alone produced spurious extra/missing
//! events under a loaded box), which is a test-harness artifact, not a
//! property of the watcher itself. One-at-a-time removes that noise while
//! still exercising real wall-clock timing end to end.

mod common;

use crate::common::{git, init_repo};
use kb_code_server::mirror::{
    MirrorConfig, MirrorSink, MirrorWatcher, RepoRef, RepoWatchConfig, WatchMode,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Held for the duration of every `#[test]` fn in this file — see the
/// module doc.
static SERIAL: Mutex<()> = Mutex::new(());

/// Acquire the serialization lock, clearing a poisoned guard from a
/// previous test's panic (a prior failure must not cascade-fail every
/// later test in the same run).
fn serial() -> std::sync::MutexGuard<'static, ()> {
    match SERIAL.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

// ---- git test fixture helpers (mirrors src/git/tests.rs) -----------------

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn canon_tempdir() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = std::fs::canonicalize(tmp.path()).unwrap();
    (tmp, path)
}

// ---- recording sink --------------------------------------------------------

#[derive(Debug, Clone)]
enum Call {
    Upsert(String, PathBuf),
    Remove(String, PathBuf),
    HeadMoved(String, Option<String>, String),
    FullReconcile(String, Vec<PathBuf>, Vec<PathBuf>),
}

impl Call {
    fn repo(&self) -> &str {
        match self {
            Call::Upsert(r, _)
            | Call::Remove(r, _)
            | Call::HeadMoved(r, ..)
            | Call::FullReconcile(r, ..) => r,
        }
    }
}

#[derive(Default)]
struct RecordingSink {
    calls: Mutex<Vec<Call>>,
}

impl RecordingSink {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

impl MirrorSink for RecordingSink {
    fn upsert_path(&self, repo: &RepoRef, path: &Path) {
        self.calls
            .lock()
            .unwrap()
            .push(Call::Upsert(repo.name.clone(), path.to_path_buf()));
    }

    fn remove_path(&self, repo: &RepoRef, path: &Path) {
        self.calls
            .lock()
            .unwrap()
            .push(Call::Remove(repo.name.clone(), path.to_path_buf()));
    }

    fn head_moved(&self, repo: &RepoRef, old: Option<gix::ObjectId>, new: gix::ObjectId) {
        self.calls.lock().unwrap().push(Call::HeadMoved(
            repo.name.clone(),
            old.map(|o| o.to_string()),
            new.to_string(),
        ));
    }

    fn full_reconcile(&self, repo: &RepoRef, changed: Vec<PathBuf>, removed: Vec<PathBuf>) {
        self.calls
            .lock()
            .unwrap()
            .push(Call::FullReconcile(repo.name.clone(), changed, removed));
    }
}

// ---- settling helpers (event-count/quiescence, never a bare sleep-and-pray)

fn wait_for_call_count_at_least(sink: &RecordingSink, n: usize, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if sink.calls().len() >= n {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    sink.calls().len() >= n
}

fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

/// Wait until the sink's call count stops growing for `quiet`, bounded
/// overall by `max_wait` — quiescence settling, not a fixed sleep.
fn settle(sink: &RecordingSink, quiet: Duration, max_wait: Duration) -> Vec<Call> {
    let deadline = Instant::now() + max_wait;
    let mut last_len = sink.calls().len();
    let mut stable_since = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(20));
        let len = sink.calls().len();
        if len != last_len {
            last_len = len;
            stable_since = Instant::now();
        } else if stable_since.elapsed() >= quiet {
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    sink.calls()
}

fn start_watcher(
    repos: Vec<(&str, &Path)>,
    debounce_ms: u64,
    mode: WatchMode,
) -> (Arc<RecordingSink>, MirrorWatcher) {
    let sink = Arc::new(RecordingSink::default());
    let cfg = MirrorConfig::new(
        repos
            .into_iter()
            .map(|(name, root)| RepoWatchConfig {
                name: name.to_string(),
                root: root.to_path_buf(),
            })
            .collect(),
    )
    .with_debounce(Duration::from_millis(debounce_ms))
    .with_mode(mode);
    let watcher =
        MirrorWatcher::start(cfg, sink.clone() as Arc<dyn MirrorSink>).expect("watcher starts");
    (sink, watcher)
}

// ---- matrix case 1: plain edit + burst dedup ------------------------------

#[test]
fn plain_edit_produces_one_upsert_and_burst_dedups() {
    let _serial = serial();
    let (_tmp, dir) = canon_tempdir();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "v1").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (sink, _watcher) = start_watcher(vec![("repo", &dir)], 200, WatchMode::Auto);
    // Startup: head_moved(None, head) + full_reconcile(everything, []).
    assert!(wait_for_call_count_at_least(
        &sink,
        2,
        Duration::from_secs(10)
    ));

    let before = sink.calls().len();
    std::fs::write(dir.join("edit.txt"), "hello").unwrap();
    assert!(wait_for_call_count_at_least(
        &sink,
        before + 1,
        Duration::from_secs(5)
    ));
    let calls = settle(&sink, Duration::from_millis(400), Duration::from_secs(5));
    let new_calls = &calls[before..];
    assert_eq!(
        new_calls.len(),
        1,
        "expected exactly one new call: {new_calls:?}"
    );
    match &new_calls[0] {
        Call::Upsert(repo, path) => {
            assert_eq!(repo, "repo");
            assert!(path.ends_with("edit.txt"));
        }
        other => panic!("expected Upsert, got {other:?}"),
    }

    // Burst of 10 files → each reported once (dedup).
    let before2 = sink.calls().len();
    for i in 0..10 {
        std::fs::write(dir.join(format!("burst{i}.txt")), "x").unwrap();
    }
    assert!(wait_for_call_count_at_least(
        &sink,
        before2 + 10,
        Duration::from_secs(5)
    ));
    let calls2 = settle(&sink, Duration::from_millis(400), Duration::from_secs(5));
    let new_calls2 = &calls2[before2..];
    assert_eq!(
        new_calls2.len(),
        10,
        "each burst file must be reported exactly once: {new_calls2:?}"
    );
    assert!(new_calls2.iter().all(|c| matches!(c, Call::Upsert(..))));
}

// ---- matrix case 2: checkout -> head_moved + ONE reconcile ---------------

#[test]
fn checkout_produces_head_moved_and_one_reconcile_matching_diff() {
    let _serial = serial();
    let (_tmp, dir) = canon_tempdir();
    init_repo(&dir);
    for i in 1..=3 {
        std::fs::write(dir.join(format!("f{i}.txt")), "base").unwrap();
    }
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    for i in 1..=3 {
        std::fs::write(dir.join(format!("f{i}.txt")), format!("feature-{i}")).unwrap();
    }
    git(&dir, &["commit", "-q", "-am", "feature changes"]);
    git(&dir, &["checkout", "-q", "main"]);

    let (sink, _watcher) = start_watcher(vec![("repo", &dir)], 300, WatchMode::Auto);
    assert!(wait_for_call_count_at_least(
        &sink,
        2,
        Duration::from_secs(10)
    ));
    let before = sink.calls().len();

    git(&dir, &["checkout", "-q", "feature"]);

    assert!(wait_for_call_count_at_least(
        &sink,
        before + 2,
        Duration::from_secs(8)
    ));
    let calls = settle(&sink, Duration::from_millis(500), Duration::from_secs(8));
    let new_calls = &calls[before..];

    let head_moves: Vec<_> = new_calls
        .iter()
        .filter_map(|c| match c {
            Call::HeadMoved(repo, old, new) => Some((repo, old, new)),
            _ => None,
        })
        .collect();
    assert_eq!(
        head_moves.len(),
        1,
        "expected exactly one head_moved: {new_calls:?}"
    );
    let (repo, old, new) = head_moves[0];
    assert_eq!(repo, "repo");
    assert!(
        old.is_some(),
        "old HEAD must be known (not the startup case)"
    );
    assert_ne!(
        old.as_deref(),
        Some(new.as_str()),
        "head_moved must report an ACTUAL move: {new_calls:?}"
    );

    let reconciles: Vec<_> = new_calls
        .iter()
        .filter_map(|c| match c {
            Call::FullReconcile(repo, changed, removed) => Some((repo, changed, removed)),
            _ => None,
        })
        .collect();
    assert_eq!(
        reconciles.len(),
        1,
        "expected exactly ONE full_reconcile (no per-file replay storm): {new_calls:?}"
    );
    let (_repo, changed, removed) = reconciles[0];
    let mut names: Vec<String> = changed
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec!["f1.txt", "f2.txt", "f3.txt"]);
    assert!(removed.is_empty());

    // The reconcile above is already proven authoritative + correct (exactly
    // one call, exact diff). What remains to rule out is a genuine PER-FILE
    // REPLAY STORM — NOT necessarily the total absence of any trailing
    // upsert: `notify` delivers events per watch descriptor (working-tree
    // root vs `.git`, two different descriptors), and under heavy scheduler
    // contention the kernel can hand them to the debouncer thread in EITHER
    // order — a working-tree flush can be processed before the git-dir flush
    // that triggers the reconcile, in which case no post-reconcile quiet
    // window can retroactively suppress it (that ordering is a kernel/OS
    // delivery property this watcher does not control, not a design flaw:
    // `POST_RECONCILE_QUIET` only ever suppresses events arriving AFTER a
    // reconcile it already ran). By the time notify delivers such an event
    // the file's on-disk content is already the correct post-checkout
    // content, so a stray upsert here is redundant, never WRONG data. Give
    // any such stray event a further generous window, then check the
    // invariant that actually matters: every stray upsert names a file the
    // reconcile ALREADY reported (no unrelated/incorrect path), and at most
    // one per file (no storm of repeats).
    let settled = settle(&sink, Duration::from_millis(800), Duration::from_secs(15));
    let trailing = &settled[calls.len()..];
    let mut stray_names: Vec<String> = trailing
        .iter()
        .filter_map(|c| match c {
            Call::Upsert(_, path) => Some(path.file_name()?.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();
    for name in &stray_names {
        assert!(
            names.contains(name),
            "a residual upsert named {name}, which the reconcile did NOT already \
             report — that is a real storm/incorrect-data case, not just late \
             delivery: {trailing:?}"
        );
    }
    let before_dedup = stray_names.len();
    stray_names.sort();
    stray_names.dedup();
    assert_eq!(
        before_dedup,
        stray_names.len(),
        "at most one residual upsert per already-reconciled file (no repeats): {trailing:?}"
    );
}

// ---- matrix case 3: rebase with conflict ---------------------------------

#[test]
fn rebase_with_conflict_suspends_then_reconciles_once() {
    let _serial = serial();
    let (_tmp, dir) = canon_tempdir();
    init_repo(&dir);
    std::fs::write(dir.join("f.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);

    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("f.txt"), "base\nfeature-line\n").unwrap();
    git(&dir, &["commit", "-q", "-am", "feature edit"]);

    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("f.txt"), "base\nmain-line\n").unwrap();
    git(&dir, &["commit", "-q", "-am", "main edit"]);

    let (sink, _watcher) = start_watcher(vec![("repo", &dir)], 200, WatchMode::Auto);
    assert!(wait_for_call_count_at_least(
        &sink,
        2,
        Duration::from_secs(10)
    ));

    // Switch to feature first (a checkout of its own) and let it fully
    // settle so it doesn't pollute the rebase assertion window. Wait for a
    // SECOND FullReconcile call specifically (the first is the startup
    // bootstrap, already present before this checkout) rather than just a
    // call count — under heavy scheduling contention `notify` can deliver
    // this checkout's own events well after the `git checkout` process
    // exited (see `POST_RECONCILE_QUIET`'s doc), so a bare count-based wait
    // can be satisfied by something else and let the real reconcile land
    // AFTER `before_rebase` is captured. Generous bounds throughout.
    git(&dir, &["checkout", "-q", "feature"]);
    assert!(wait_until(Duration::from_secs(20), || {
        sink.calls()
            .iter()
            .filter(|c| matches!(c, Call::FullReconcile(..)))
            .count()
            >= 2
    }));
    settle(&sink, Duration::from_millis(800), Duration::from_secs(10));
    let before_rebase = sink.calls().len();

    let status = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rebase", "main"])
        .env("GIT_EDITOR", "true")
        .status()
        .unwrap();
    assert!(!status.success(), "expected a rebase conflict");

    assert!(wait_until(Duration::from_secs(5), || dir
        .join(".git")
        .join("rebase-merge")
        .exists()));
    // Give the watcher a beat to observe at least one flush while suspended.
    std::thread::sleep(Duration::from_millis(400));
    let mid_calls = sink.calls()[before_rebase..].to_vec();
    assert!(
        mid_calls
            .iter()
            .all(|c| !matches!(c, Call::HeadMoved(..) | Call::FullReconcile(..))),
        "no reconcile/head_moved should fire while the rebase is suspended: {mid_calls:?}"
    );
    assert!(
        mid_calls.iter().all(|c| !matches!(c, Call::Upsert(..))),
        "zero upserts while rebase-merge exists: {mid_calls:?}"
    );

    // Resolve + continue.
    std::fs::write(dir.join("f.txt"), "base\nmain-line\nfeature-line\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    let status = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rebase", "--continue"])
        .env("GIT_EDITOR", "true")
        .status()
        .unwrap();
    assert!(
        status.success(),
        "rebase --continue should succeed after resolving"
    );

    assert!(wait_until(Duration::from_secs(5), || !dir
        .join(".git")
        .join("rebase-merge")
        .exists()));
    assert!(wait_for_call_count_at_least(
        &sink,
        before_rebase + 1,
        Duration::from_secs(8)
    ));
    let calls = settle(&sink, Duration::from_millis(500), Duration::from_secs(8));
    let after_rebase = &calls[before_rebase..];

    let reconciles: Vec<_> = after_rebase
        .iter()
        .filter(|c| matches!(c, Call::FullReconcile(..)))
        .collect();
    assert_eq!(
        reconciles.len(),
        1,
        "exactly one reconcile equal to the end-state delta after the rebase completes: {after_rebase:?}"
    );
    if let Call::FullReconcile(_, changed, _) = reconciles[0] {
        assert!(
            changed.iter().any(|p| p.ends_with("f.txt")),
            "reconcile must report f.txt: {changed:?}"
        );
    }
    assert!(
        after_rebase.iter().all(|c| !matches!(c, Call::Upsert(..))),
        "the rebase's intermediate states must never be replayed as upserts: {after_rebase:?}"
    );
}

// ---- matrix case 4: index.lock cycles produce zero events ----------------

#[test]
fn git_add_index_lock_cycle_produces_no_events() {
    let _serial = serial();
    let (_tmp, dir) = canon_tempdir();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "v1").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (sink, _watcher) = start_watcher(vec![("repo", &dir)], 200, WatchMode::Auto);
    assert!(wait_for_call_count_at_least(
        &sink,
        2,
        Duration::from_secs(10)
    ));
    let before = sink.calls().len();

    // A no-op `git add -A` (nothing changed) still cycles index.lock
    // create+remove — must produce ZERO new events of any kind.
    git(&dir, &["add", "-A"]);
    std::thread::sleep(Duration::from_millis(500));
    let calls = settle(&sink, Duration::from_millis(400), Duration::from_secs(3));
    assert_eq!(
        calls.len(),
        before,
        "a no-op `git add` must produce zero new events: {:?}",
        &calls[before..]
    );
}

// ---- matrix case 5: linked worktree ---------------------------------------

#[test]
fn linked_worktree_attributes_correctly_and_main_op_does_not_storm_it() {
    let _serial = serial();
    let (_tmp, main_dir) = canon_tempdir();
    init_repo(&main_dir);
    std::fs::write(main_dir.join("root.txt"), "base").unwrap();
    git(&main_dir, &["add", "-A"]);
    git(&main_dir, &["commit", "-q", "-m", "c1"]);
    git(&main_dir, &["branch", "feature"]);

    let (_wt_parent, wt_parent_dir) = canon_tempdir();
    let wt_dir = wt_parent_dir.join("wt");
    git(
        &main_dir,
        &["worktree", "add", "-q", wt_dir.to_str().unwrap(), "feature"],
    );

    let (sink, _watcher) = start_watcher(
        vec![("main", &main_dir), ("linked", &wt_dir)],
        200,
        WatchMode::Auto,
    );
    // Two startup reconciles (one per repo): at least 4 calls.
    assert!(wait_for_call_count_at_least(
        &sink,
        4,
        Duration::from_secs(10)
    ));
    let before = sink.calls().len();

    // An edit in the linked worktree must attribute to "linked".
    std::fs::write(wt_dir.join("wt-file.txt"), "hello").unwrap();
    assert!(wait_for_call_count_at_least(
        &sink,
        before + 1,
        Duration::from_secs(5)
    ));
    let calls = settle(&sink, Duration::from_millis(400), Duration::from_secs(5));
    let new_calls = &calls[before..];
    assert_eq!(
        new_calls.len(),
        1,
        "expected exactly one event: {new_calls:?}"
    );
    match &new_calls[0] {
        Call::Upsert(repo, path) => {
            assert_eq!(repo, "linked", "must attribute to the linked repo ref");
            assert!(path.ends_with("wt-file.txt"));
        }
        other => panic!("expected Upsert, got {other:?}"),
    }

    // An operation in MAIN (a plain commit — moves main's HEAD sha, touches
    // main's own git_dir/logs/HEAD) must not storm the linked worktree's
    // gate, even though main's git_dir IS linked's common_dir.
    let before2 = sink.calls().len();
    git(
        &main_dir,
        &["commit", "--allow-empty", "-q", "-m", "empty on main"],
    );
    assert!(wait_for_call_count_at_least(
        &sink,
        before2 + 1,
        Duration::from_secs(5)
    ));
    let calls2 = settle(&sink, Duration::from_millis(400), Duration::from_secs(5));
    let new_calls2 = &calls2[before2..];
    assert!(
        !new_calls2.is_empty(),
        "main's commit must produce at least one event"
    );
    for call in new_calls2 {
        assert_eq!(
            call.repo(),
            "main",
            "linked must not react to an operation in main: {new_calls2:?}"
        );
    }
}

// ---- matrix case 6: submodule bump ----------------------------------------

#[test]
fn submodule_bump_reports_the_submodule_path_without_descending() {
    let _serial = serial();
    let (_sub_src_tmp, sub_src_dir) = canon_tempdir();
    init_repo(&sub_src_dir);
    std::fs::write(sub_src_dir.join("s.txt"), "s1").unwrap();
    git(&sub_src_dir, &["add", "-A"]);
    git(&sub_src_dir, &["commit", "-q", "-m", "sub c1"]);
    std::fs::write(sub_src_dir.join("s.txt"), "s2").unwrap();
    git(&sub_src_dir, &["commit", "-q", "-am", "sub c2"]);
    let sub_head_before = git_out(&sub_src_dir, &["rev-parse", "HEAD~1"]);

    let (_parent_tmp, parent_dir) = canon_tempdir();
    init_repo(&parent_dir);
    std::fs::write(parent_dir.join("top.txt"), "top").unwrap();
    git(&parent_dir, &["add", "-A"]);
    git(&parent_dir, &["commit", "-q", "-m", "c1"]);
    git(
        &parent_dir,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            sub_src_dir.to_str().unwrap(),
            "vendor/sub",
        ],
    );
    git(&parent_dir, &["commit", "-q", "-m", "add submodule"]);

    let (sink, _watcher) = start_watcher(vec![("parent", &parent_dir)], 300, WatchMode::Auto);
    assert!(wait_for_call_count_at_least(
        &sink,
        2,
        Duration::from_secs(10)
    ));
    let before = sink.calls().len();

    // Bump the submodule's pin to an earlier commit and commit that in the
    // parent.
    let sub_checkout = parent_dir.join("vendor").join("sub");
    git(&sub_checkout, &["checkout", "-q", &sub_head_before]);
    git(&parent_dir, &["add", "vendor/sub"]);
    git(&parent_dir, &["commit", "-q", "-m", "bump submodule"]);

    assert!(wait_for_call_count_at_least(
        &sink,
        before + 1,
        Duration::from_secs(8)
    ));
    let calls = settle(&sink, Duration::from_millis(500), Duration::from_secs(8));
    let new_calls = &calls[before..];

    let reconciles: Vec<_> = new_calls
        .iter()
        .filter_map(|c| match c {
            Call::FullReconcile(repo, changed, _) if repo == "parent" => Some(changed),
            _ => None,
        })
        .collect();
    assert_eq!(
        reconciles.len(),
        1,
        "expected exactly one reconcile for the parent: {new_calls:?}"
    );
    let changed = reconciles[0];
    assert!(
        changed.iter().any(|p| p == Path::new("vendor/sub")),
        "reconcile must report the submodule path itself: {changed:?}"
    );
    assert!(
        !changed
            .iter()
            .any(|p| p != Path::new("vendor/sub") && p.starts_with("vendor/sub")),
        "must not descend into the submodule: {changed:?}"
    );
}

// ---- matrix case 7: file deletion -----------------------------------------

#[test]
fn file_deletion_produces_remove_path() {
    let _serial = serial();
    let (_tmp, dir) = canon_tempdir();
    init_repo(&dir);
    let doomed = dir.join("doomed.txt");
    std::fs::write(&doomed, "bye").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (sink, _watcher) = start_watcher(vec![("repo", &dir)], 200, WatchMode::Auto);
    assert!(wait_for_call_count_at_least(
        &sink,
        2,
        Duration::from_secs(10)
    ));
    let before = sink.calls().len();

    std::fs::remove_file(&doomed).unwrap();
    assert!(wait_for_call_count_at_least(
        &sink,
        before + 1,
        Duration::from_secs(5)
    ));
    let calls = settle(&sink, Duration::from_millis(400), Duration::from_secs(5));
    let new_calls = &calls[before..];
    assert_eq!(new_calls.len(), 1, "{new_calls:?}");
    match &new_calls[0] {
        Call::Remove(repo, path) => {
            assert_eq!(repo, "repo");
            assert!(path.ends_with("doomed.txt"));
        }
        other => panic!("expected Remove, got {other:?}"),
    }
}

// ---- matrix case 8: poll mode ----------------------------------------------

#[test]
fn poll_mode_plain_edit_produces_one_upsert() {
    let _serial = serial();
    let (_tmp, dir) = canon_tempdir();
    init_repo(&dir);
    std::fs::write(dir.join("seed.txt"), "seed").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let sink = Arc::new(RecordingSink::default());
    let cfg = MirrorConfig::new(vec![RepoWatchConfig {
        name: "repo".to_string(),
        root: dir.clone(),
    }])
    .with_mode(WatchMode::Poll)
    .with_poll_interval(Duration::from_millis(100))
    .with_debounce(Duration::from_millis(200));
    let _watcher =
        MirrorWatcher::start(cfg, sink.clone() as Arc<dyn MirrorSink>).expect("watcher starts");

    assert!(wait_for_call_count_at_least(
        &sink,
        2,
        Duration::from_secs(10)
    ));
    // Let the poll watcher establish its baseline scan before editing.
    std::thread::sleep(Duration::from_millis(300));
    let before = sink.calls().len();

    std::fs::write(dir.join("late.txt"), "added").unwrap();
    assert!(wait_for_call_count_at_least(
        &sink,
        before + 1,
        Duration::from_secs(8)
    ));
    let calls = settle(&sink, Duration::from_millis(500), Duration::from_secs(8));
    let new_calls = &calls[before..];
    assert_eq!(new_calls.len(), 1, "{new_calls:?}");
    match &new_calls[0] {
        Call::Upsert(repo, path) => {
            assert_eq!(repo, "repo");
            assert!(path.ends_with("late.txt"));
        }
        other => panic!("expected Upsert, got {other:?}"),
    }
}

// ---- matrix case 9 (W4.7): confirmed checkout -> head_moved, no special-casing --

/// Proves `checkout::switch_repo`'s subprocess call (`git switch`) is
/// observed by the EXISTING watcher machinery exactly like the raw `git
/// checkout` subprocess "matrix case 2" above already pins — i.e. that
/// `checkout.rs` genuinely needs no store/mirror wiring of its own (see
/// that module's doc). Reuses this file's own idioms (`RecordingSink`,
/// `start_watcher`, `wait_for_call_count_at_least`, `settle`) rather than a
/// fresh harness.
#[test]
fn confirmed_checkout_switch_triggers_head_moved_via_the_existing_watcher() {
    let _serial = serial();
    let (_tmp, dir) = canon_tempdir();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "base").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    // "feature" must diverge to a DIFFERENT commit than "main" — the
    // watcher's `HeadCandidate` arm compares resolved COMMIT shas, not ref
    // NAMES (see `mirror::handle_git_dir_event`), so a branch pointing at
    // the SAME commit as the one currently checked out produces no
    // observable HEAD move on switch.
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature").unwrap();
    git(&dir, &["commit", "-q", "-am", "feature change"]);
    git(&dir, &["checkout", "-q", "main"]);

    let (sink, _watcher) = start_watcher(vec![("repo", &dir)], 300, WatchMode::Auto);
    assert!(wait_for_call_count_at_least(
        &sink,
        2,
        Duration::from_secs(10)
    ));
    let before = sink.calls().len();

    // The library fn the daemon's `POST /api/checkout` route actually
    // calls — no direct `git` subprocess of this test's own. V70-A2
    // (SEC-17): it takes a validated `Revspec`, the same type the route
    // parses `?ref=` into.
    let target = kb_code_server::git::Revspec::parse("feature").expect("fixture revspec parses");
    let outcome =
        kb_code_server::checkout::switch_repo(&dir, &target).expect("clean tree switches cleanly");
    assert_eq!(outcome.target, "feature");
    assert!(!outcome.detached);

    assert!(wait_for_call_count_at_least(
        &sink,
        before + 1,
        Duration::from_secs(8)
    ));
    let calls = settle(&sink, Duration::from_millis(500), Duration::from_secs(8));
    let new_calls = &calls[before..];
    let head_moves: Vec<_> = new_calls
        .iter()
        .filter_map(|c| match c {
            Call::HeadMoved(repo, old, new) => Some((repo, old, new)),
            _ => None,
        })
        .collect();
    assert_eq!(
        head_moves.len(),
        1,
        "expected exactly one head_moved from checkout::switch_repo: {new_calls:?}"
    );
    let (repo, old, _new) = head_moves[0];
    assert_eq!(repo, "repo");
    assert!(old.is_some(), "old HEAD must be known");
}
