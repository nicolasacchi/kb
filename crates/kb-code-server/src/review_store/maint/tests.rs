//! RS-U9 — maintenance/GC/backup/restore-guard tests. Hermetic: real git
//! over local fixture clones (synthetic acme/widgets), a temp DB, no
//! network. Test-only direct `git` spawns build the fixtures — this file
//! is in SEC-17's `GIT_SPAWNING_FILES` (`crates/kb-code-server/tests/
//! security/git_argv_lint.rs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::*;
use crate::config::{RepoEntry, ReviewSection};
use crate::review_store::registry::{Registration, ReviewStores};
use crate::review_store::seed;
use crate::store::{ReviewStoreRow, Store};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit(dir: &Path, file: &str, body: &str) -> String {
    std::fs::write(dir.join(file), body).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
    git(dir, &["rev-parse", "HEAD"])
}

/// One member clone, `widgets-01` (`acme/widgets`), with a `main` and a
/// `feature/x` branch.
struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    one: PathBuf,
    main_tip: String,
    feat_tip: String,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("state");
    let one = tmp.path().join("work/widgets-01");
    std::fs::create_dir_all(&one).unwrap();
    git(&one, &["init", "-q", "-b", "main"]);
    commit(&one, "a.txt", "a");
    let main_tip = commit(&one, "b.txt", "b");
    git(&one, &["checkout", "-q", "-b", "feature/x"]);
    let feat_tip = commit(&one, "c.txt", "c");
    git(&one, &["checkout", "-q", "main"]);
    git(
        &one,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    );
    Fixture {
        _tmp: tmp,
        home,
        one,
        main_tip,
        feat_tip,
    }
}

struct Env {
    fx: Fixture,
    store: Store,
    rs: ReviewStores,
}

fn env() -> Env {
    let fx = fixture();
    std::fs::create_dir_all(&fx.home).unwrap();
    let store = Store::open(&fx.home.join("index.db")).unwrap();
    let repos = vec![RepoEntry {
        name: "widgets-01".into(),
        path: fx.one.clone(),
    }];
    let mut ids = HashMap::new();
    ids.insert(
        "widgets-01".to_string(),
        store
            .upsert_repo("widgets-01", &fx.one.to_string_lossy())
            .unwrap(),
    );
    let rs = ReviewStores::new(&ReviewSection::default(), &fx.home, &repos, &ids);
    Env { fx, store, rs }
}

fn member_id(r: &Registration) -> i64 {
    match r {
        Registration::Member { store_id, .. } => *store_id,
        other => panic!("expected Member, got {other:?}"),
    }
}

/// A review with one patchset whose ref is pinned in the clone (README
/// §5.2 step 2's precondition — the member import fetches
/// `refs/kbc/review/*` FROM the clone).
fn review_with_patchset(e: &Env, tip: &str, base: &str) -> i64 {
    let id = e
        .store
        .create_review(
            "widgets-01",
            Some("t"),
            "refs/remotes/origin/main",
            "feature/x",
            None,
            1,
        )
        .unwrap();
    e.store.insert_patchset(id, 1, tip, base, 1).unwrap();
    git(&e.fx.one, &["update-ref", &seed::patchset_ref(id, 1), tip]);
    id
}

/// Register + seed `widgets-01` offline (no network) and return the
/// resulting `ready` store row.
fn ready_row(e: &Env) -> ReviewStoreRow {
    let id = member_id(&e.rs.register_repo(
        &e.store,
        "widgets-01",
        Some("https://github.com/acme/widgets.git"),
    ));
    e.rs.seed(&e.store, id, false).unwrap();
    e.store
        .store_for_repo_name("widgets-01")
        .unwrap()
        .expect("a store")
}

fn store_refs(dir: &Path) -> Vec<String> {
    git(dir, &["for-each-ref", "--format=%(refname)"])
        .lines()
        .map(str::to_string)
        .collect()
}

/// `git fsck --no-dangling`, tolerant of the store's OWN deliberate HEAD
/// placeholder (`write_store_config` in `seed.rs` points HEAD at
/// `refs/kbc/none`, which resolves nowhere by design — a store has no
/// natural "current branch"). Without `-c fsck.badHeadTarget=ignore`,
/// fsck reports that as `HEAD: badHeadTarget: …` and exits non-zero even
/// though the object graph itself is perfectly sound — this asserts the
/// object graph, not the placeholder.
fn fsck(dir: &Path) {
    git(
        dir,
        &["-c", "fsck.badHeadTarget=ignore", "fsck", "--no-dangling"],
    );
}

// ── due_tasks (pure) ────────────────────────────────────────────────────

#[test]
fn every_cadence_is_due_when_nothing_has_ever_run() {
    let due = due_tasks(1_000_000, LastMaint::default());
    assert_eq!(
        due,
        vec![MaintTask::Daily, MaintTask::Weekly, MaintTask::Monthly]
    );
}

#[test]
fn a_cadence_is_due_only_once_its_own_period_has_elapsed() {
    let last = LastMaint {
        daily: Some(1000),
        weekly: Some(1000),
        monthly: Some(1000),
    };
    // Just under a day: nothing due.
    assert!(due_tasks(1000 + DAY_SECS - 1, last).is_empty());
    // Exactly a day: daily only.
    assert_eq!(due_tasks(1000 + DAY_SECS, last), vec![MaintTask::Daily]);
    // Exactly a week: daily + weekly (daily's own period has ALSO elapsed
    // many times over — `due_tasks` reports "at least due", not "how many
    // times").
    assert_eq!(
        due_tasks(1000 + WEEK_SECS, last),
        vec![MaintTask::Daily, MaintTask::Weekly]
    );
    assert_eq!(
        due_tasks(1000 + MONTH_SECS, last),
        vec![MaintTask::Daily, MaintTask::Weekly, MaintTask::Monthly]
    );
}

#[test]
fn a_clock_that_moved_backward_never_makes_a_cadence_due() {
    let last = LastMaint {
        daily: Some(10_000),
        weekly: None,
        monthly: None,
    };
    let due = due_tasks(1, last);
    assert!(!due.contains(&MaintTask::Daily), "{due:?}");
    // weekly/monthly never ran: still due regardless of the clock.
    assert!(due.contains(&MaintTask::Weekly));
    assert!(due.contains(&MaintTask::Monthly));
}

#[test]
fn last_maint_round_trips_through_state_json() {
    let mut last = LastMaint::default();
    last.set(MaintTask::Daily, 42);
    last.set(MaintTask::Monthly, 99);
    let sj = serde_json::json!({ "last_maint": last, "code": "ready" });
    let read_back = LastMaint::from_state_json(Some(&sj));
    assert_eq!(read_back.daily, Some(42));
    assert_eq!(read_back.weekly, None);
    assert_eq!(read_back.monthly, Some(99));
}

#[test]
fn a_missing_or_malformed_last_maint_reads_as_never_run() {
    assert_eq!(LastMaint::from_state_json(None), LastMaint::default());
    let sj = serde_json::json!({ "last_maint": "not an object" });
    assert_eq!(LastMaint::from_state_json(Some(&sj)), LastMaint::default());
}

/// Should-fix review finding: a failed task must not be retried every
/// single scheduler tick.
#[test]
fn a_failed_task_backs_off_and_a_success_clears_the_backoff() {
    let mut last = LastMaint::default();
    last.set(MaintTask::Daily, 1000); // a prior success
                                      // Now due again (a full day later) — but it FAILS.
    let t = 1000 + DAY_SECS;
    assert!(due_tasks(t, last).contains(&MaintTask::Daily));
    last.record_failure(MaintTask::Daily, t);
    // Immediately after the failure, NOT due — even though the period
    // condition is still satisfied.
    assert!(!due_tasks(t + 1, last).contains(&MaintTask::Daily));
    assert!(!due_tasks(t + TASK_RETRY_BACKOFF_SECS - 1, last).contains(&MaintTask::Daily));
    // Due again once the backoff elapses.
    assert!(due_tasks(t + TASK_RETRY_BACKOFF_SECS, last).contains(&MaintTask::Daily));
    // A SUCCESS clears the backoff for that cadence.
    last.set(MaintTask::Daily, t + TASK_RETRY_BACKOFF_SECS);
    assert_eq!(last.daily_retry_after, None);
}

// ── B3: monthly cruft expiration gating (pure) ──────────────────────────

#[test]
fn cruft_never_expires_with_no_recorded_gc_apply() {
    let sj = serde_json::json!({});
    assert!(!cruft_allow_expire(&sj, 10_000_000));
}

#[test]
fn cruft_never_expires_within_the_cooldown_of_the_last_apply() {
    let sj = serde_json::json!({ "last_gc_apply": { "at": 1000 } });
    assert!(!cruft_allow_expire(
        &sj,
        1000 + CRUFT_EXPIRE_COOLDOWN_SECS - 1
    ));
    assert!(cruft_allow_expire(&sj, 1000 + CRUFT_EXPIRE_COOLDOWN_SECS));
}

// ── restore guard (filesystem only) ─────────────────────────────────────

#[test]
fn an_epoch_that_only_ever_increases_never_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let p = restore_guard::path_for(tmp.path());
    let s1 = restore_guard::observe_boot_epoch(&p, Some(40), 100);
    assert!(!s1.flagged);
    assert_eq!(s1.high_water_epoch, Some(40));
    let s2 = restore_guard::observe_boot_epoch(&p, Some(45), 200);
    assert!(!s2.flagged);
    assert_eq!(s2.high_water_epoch, Some(45));
    // Re-observing the SAME epoch is also not a regression.
    let s3 = restore_guard::observe_boot_epoch(&p, Some(45), 300);
    assert!(!s3.flagged);
}

#[test]
fn an_epoch_regression_flags_exactly_once_and_acknowledgement_is_per_store() {
    let tmp = tempfile::tempdir().unwrap();
    let p = restore_guard::path_for(tmp.path());
    restore_guard::observe_boot_epoch(&p, Some(45), 100);
    let s2 = restore_guard::observe_boot_epoch(&p, Some(40), 200);
    assert!(s2.flagged, "{s2:?}");
    assert!(
        s2.just_flagged,
        "the FIRST regressed boot must report just_flagged"
    );
    assert_eq!(s2.high_water_epoch, Some(45), "the mark never lowers");
    // A second regressed boot still finds it flagged, but does NOT
    // re-report just_flagged (a caller uses that to avoid re-bundling on
    // every single boot while the guard is already flagged).
    let s3 = restore_guard::observe_boot_epoch(&p, Some(40), 300);
    assert!(s3.flagged);
    assert!(!s3.just_flagged);

    // Acknowledging store "r" (review finding B2, Should-fix: PER STORE)
    // never clears the GLOBAL flag and never unblocks a different store.
    let s4 = restore_guard::acknowledge(&p, "r", 400).unwrap();
    assert!(s4.flagged, "the flag itself is never cleared by an ack");
    assert_eq!(s4.cleared_at, Some(400));
    assert!(!s4.blocks("r"));
    assert!(s4.blocks("s"), "store s was never acknowledged");

    // Acknowledging a not-flagged guard is a harmless no-op.
    let other_tmp = tempfile::tempdir().unwrap();
    let other_p = restore_guard::path_for(other_tmp.path());
    let never_flagged = restore_guard::acknowledge(&other_p, "r", 1).unwrap();
    assert!(!never_flagged.flagged);
    assert_eq!(never_flagged.cleared_at, None);
}

/// RS-U9 review finding B2: a NEW flagging incident (a second, different
/// regression) must not let a PREVIOUS incident's acknowledgements carry
/// over — that would silently unblock a store nobody has looked at for
/// THIS incident.
#[test]
fn a_new_flagging_incident_clears_previous_acknowledgements() {
    let tmp = tempfile::tempdir().unwrap();
    let p = restore_guard::path_for(tmp.path());
    restore_guard::observe_boot_epoch(&p, Some(45), 100);
    restore_guard::observe_boot_epoch(&p, Some(40), 200); // incident 1
    restore_guard::acknowledge(&p, "r", 300).unwrap();
    assert!(!restore_guard::read(&p).blocks("r"));

    // A SECOND, later regression (e.g. epoch 40 -> a still-lower 35, or in
    // practice a fresh `flag_manual` incident) must not inherit "r"'s old
    // acknowledgement.
    let s = restore_guard::flag_manual(&p, "a second, unrelated incident", 400).unwrap();
    assert!(s.just_flagged);
    assert!(
        restore_guard::read(&p).blocks("r"),
        "the previous incident's acknowledgement must not carry over"
    );
}

/// RS-U9 review finding B2: a corrupt (present but unparseable) sentinel
/// must read as FLAGGED, never silently unflagged.
#[test]
fn a_corrupt_sentinel_reads_as_flagged() {
    let tmp = tempfile::tempdir().unwrap();
    let p = restore_guard::path_for(tmp.path());
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, b"{ not valid json").unwrap();
    let s = restore_guard::read(&p);
    assert!(s.flagged, "a corrupt sentinel must fail CLOSED: {s:?}");
}

#[test]
fn flag_manual_sets_just_flagged_and_a_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let p = restore_guard::path_for(tmp.path());
    let s = restore_guard::flag_manual(&p, "operator restored a bundle by hand", 100).unwrap();
    assert!(s.flagged);
    assert!(s.just_flagged);
    assert_eq!(
        s.flagged_reason.as_deref(),
        Some("operator restored a bundle by hand")
    );
    // Persisted: a fresh read sees it too (though just_flagged resets).
    let read_back = restore_guard::read(&p);
    assert!(read_back.flagged);
    assert!(!read_back.just_flagged);
}

#[test]
fn a_missing_guard_file_reads_as_unflagged() {
    let tmp = tempfile::tempdir().unwrap();
    let p = restore_guard::path_for(tmp.path());
    let s = restore_guard::read(&p);
    assert!(!s.flagged);
    assert_eq!(s.high_water_epoch, None);
}

// ── stale tmp_pack sweep ────────────────────────────────────────────────

#[test]
fn stale_tmp_pack_sweep_removes_only_tmp_pack_names() {
    let tmp = tempfile::tempdir().unwrap();
    let pack_dir = tmp.path().join("objects/pack");
    std::fs::create_dir_all(&pack_dir).unwrap();
    std::fs::write(pack_dir.join("tmp_pack_abc123"), b"x").unwrap();
    std::fs::write(pack_dir.join("tmp_pack_def456.idx"), b"x").unwrap();
    std::fs::write(pack_dir.join("pack-realpack.pack"), b"x").unwrap();
    // Duration::ZERO: everything already on disk qualifies as "at least
    // zero seconds old" (see sweep_pack_tmp_older_than's doc).
    let n = sweep_pack_tmp_older_than(tmp.path(), Duration::ZERO);
    assert_eq!(n, 2, "both tmp_pack_* files, and only those");
    let remaining: Vec<_> = std::fs::read_dir(&pack_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(remaining, vec!["pack-realpack.pack".to_string()]);
}

#[test]
fn stale_tmp_pack_sweep_on_a_missing_dir_is_a_harmless_zero() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(sweep_pack_tmp_older_than(tmp.path(), Duration::ZERO), 0);
}

// ── the daily/weekly/monthly tasks against a real store ────────────────

#[test]
fn maintenance_tasks_run_and_leave_the_store_valid() {
    let e = env();
    let rid = review_with_patchset(&e, &e.fx.feat_tip, &e.fx.main_tip);
    let row = ready_row(&e);
    let git_spawner = e.rs.git().unwrap();
    let dir = Path::new(&row.git_dir);
    assert!(store_refs(dir).contains(&seed::patchset_ref(rid, 1)));

    run_daily(git_spawner, dir).expect("daily tasks");
    run_weekly(git_spawner, dir).expect("weekly tasks");
    run_monthly(git_spawner, dir, true).expect("monthly tasks (expiring)");
    run_monthly(git_spawner, dir, false).expect("monthly tasks (never-expire)");

    // The store is still a valid, connected object graph, and the ref this
    // test cares about survived every task untouched.
    fsck(dir);
    assert!(store_refs(dir).contains(&seed::patchset_ref(rid, 1)));
}

#[test]
fn run_pass_for_store_persists_last_maint_and_last_gc_dry_run() {
    let e = env();
    review_with_patchset(&e, &e.fx.feat_tip, &e.fx.main_tip);
    let row = ready_row(&e);
    let report = run_pass_for_store(
        &e.rs,
        &e.store,
        &row,
        &[MaintTask::Daily, MaintTask::Weekly, MaintTask::Monthly],
        12_345,
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(
        report.tasks_run,
        vec!["daily", "weekly", "monthly"],
        "{report:?}"
    );
    assert!(report.invariant.is_some());
    let gc = report.gc.as_ref().expect("a gc report");
    assert!(!gc.applied, "the scheduler must NEVER apply: {gc:?}");
    assert_eq!(gc.reason, "dry-run");
    assert!(!report.restore_guard_blocks_apply);

    let refreshed = e
        .store
        .store_for_repo_name("widgets-01")
        .unwrap()
        .expect("row");
    let sj: serde_json::Value =
        serde_json::from_str(refreshed.state_json.as_deref().unwrap()).unwrap();
    assert_eq!(sj["last_maint"]["daily"], 12345);
    assert_eq!(sj["last_maint"]["weekly"], 12345);
    assert_eq!(sj["last_maint"]["monthly"], 12345);
    assert!(sj["last_gc_dry_run"]["at"].is_i64());
    assert!(
        sj.get("last_gc_apply").is_none(),
        "a dry-run pass must never write last_gc_apply: {sj}"
    );
    // state stays `ready` — the merge must never change it.
    assert_eq!(refreshed.state, "ready");
}

// ── restore guard blocks scheduled GC apply, not the dry-run scan ──────

#[test]
fn scheduled_gc_never_applies_while_the_restore_guard_is_flagged_and_yes_acks_only_this_store() {
    let e = env();
    let r1 = review_with_patchset(&e, &e.fx.feat_tip, &e.fx.main_tip);
    let row = ready_row(&e);
    let dir = Path::new(&row.git_dir);
    let before: std::collections::BTreeSet<String> = store_refs(dir).into_iter().collect();
    assert!(before.contains(&seed::patchset_ref(r1, 1)));

    // The review "goes away" (refs left behind — GC's whole reason to
    // exist), then the restore guard is flagged (as if an epoch just
    // regressed on this boot).
    e.store.delete_review(r1).unwrap();
    let guard_path = &e.rs.settings().restore_guard_path;
    restore_guard::flag_manual(guard_path, "test", 1).unwrap();

    // The scheduler's own pass: candidates are still SCANNED and reported,
    // but the reason is a plain "dry-run" — the operator ruling is that
    // the scheduler NEVER applies at all, flagged or not.
    let report = run_pass_for_store(&e.rs, &e.store, &row, &[MaintTask::Daily], 2);
    let gc = report.gc.expect("a gc report");
    assert!(!gc.applied, "{gc:?}");
    assert_eq!(gc.reason, "dry-run");
    assert_eq!(gc.candidates, 1, "the orphaned ref was still SCANNED");
    assert!(report.restore_guard_blocks_apply);
    // Nothing was actually deleted.
    assert!(store_refs(dir).contains(&seed::patchset_ref(r1, 1)));

    // A dry-run `run_gc_now` (no --yes) is ALSO blocked-but-reported, same
    // shape as the scheduler.
    let dry = run_gc_now(&e.rs, &e.store, &row, false, false, 3).unwrap();
    assert!(!dry.applied);
    assert_eq!(dry.reason, "dry-run");

    // An explicit operator `--yes` (run_gc_now with bypass_guard=true)
    // acknowledges THIS store and applies.
    let gc2 = run_gc_now(&e.rs, &e.store, &row, true, true, 4).unwrap();
    assert!(gc2.applied, "{gc2:?}");
    assert_eq!(gc2.reason, "applied");
    assert!(!store_refs(dir).contains(&seed::patchset_ref(r1, 1)));
    let after = restore_guard::read(guard_path);
    assert!(
        after.flagged,
        "the GLOBAL flag is never cleared by a per-store ack"
    );
    assert!(
        !after.blocks(&row.uuid),
        "but THIS store is now acknowledged"
    );
}

#[test]
fn run_gc_now_default_is_a_dry_run() {
    let e = env();
    let r1 = review_with_patchset(&e, &e.fx.feat_tip, &e.fx.main_tip);
    let row = ready_row(&e);
    let dir = Path::new(&row.git_dir);
    e.store.delete_review(r1).unwrap();

    let report = run_gc_now(&e.rs, &e.store, &row, false, false, 1).unwrap();
    assert!(!report.applied);
    assert_eq!(report.reason, "dry-run");
    assert_eq!(report.candidates, 1);
    assert!(store_refs(dir).contains(&seed::patchset_ref(r1, 1)));
}

/// RS-U9 review finding B2's core new mechanism: a delete candidate whose
/// refname encodes a review id ABOVE this volume's high-water mark can
/// only mean the id was minted by a database this volume has since been
/// rolled BEHIND (a same-schema-epoch restore, invisible to the sentinel).
/// This check is UNCONDITIONAL — `bypass_guard: true` (an operator's
/// `--yes`) does not override it.
#[test]
fn an_impossible_review_id_refuses_the_whole_apply_even_with_yes() {
    let e = env();
    review_with_patchset(&e, &e.fx.feat_tip, &e.fx.main_tip); // review id 1
    let row = ready_row(&e);
    let dir = Path::new(&row.git_dir);

    let high_water = e.store.reviews_high_water_id().unwrap();
    assert_eq!(
        high_water, 1,
        "fixture sanity: exactly one review minted so far"
    );
    let impossible_ref = seed::patchset_ref(high_water + 1000, 1);
    git(dir, &["update-ref", &impossible_ref, &e.fx.feat_tip]);

    // bypass_guard=true (an operator's --yes): the SENTINEL guard is not
    // even flagged here, so nothing blocks on that axis — this isolates
    // the DB-truth check.
    let report = run_gc_now(&e.rs, &e.store, &row, true, true, 2).unwrap();
    assert!(!report.applied, "{report:?}");
    assert_eq!(report.reason, "restore-suspected");
    assert!(report
        .detail
        .as_deref()
        .is_some_and(|d| d.contains(&(high_water + 1000).to_string())));
    // Nothing was deleted — not even the refs that WOULD have been
    // legitimate candidates alongside the impossible one.
    assert!(store_refs(dir).contains(&impossible_ref));
}

/// RS-U9 review finding B2: acknowledging store R's restore suspicion via
/// `gc --repo R --yes` must never silently clear it for a DIFFERENT store
/// S nobody has looked at yet.
#[test]
fn an_acknowledgement_for_one_store_never_unblocks_a_different_store() {
    let tmp = tempfile::tempdir().unwrap();
    let p = restore_guard::path_for(tmp.path());
    restore_guard::flag_manual(&p, "test", 1).unwrap();
    restore_guard::acknowledge(&p, "store-r-uuid", 2).unwrap();
    let state = restore_guard::read(&p);
    assert!(!state.blocks("store-r-uuid"));
    assert!(
        state.blocks("store-s-uuid"),
        "a DIFFERENT store stays blocked"
    );
}

// ── backup bundles ──────────────────────────────────────────────────────

#[test]
fn bundle_contains_exactly_the_kbc_refs_and_excludes_nothing_it_should_not() {
    let e = env();
    let r1 = review_with_patchset(&e, &e.fx.feat_tip, &e.fx.main_tip);
    let row = ready_row(&e);
    let dir = Path::new(&row.git_dir);
    // A base tip the store holds credentialed-fetch-free (a plain local
    // ref write is enough for this test — write_bundle only reads refs).
    git(
        dir,
        &["update-ref", "refs/remotes/base/main", &e.fx.main_tip],
    );

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("out.bundle");
    let outcome = write_bundle(e.rs.git().unwrap(), dir, &dest).unwrap();
    assert_eq!(outcome, BundleOutcome::Written);
    assert!(dest.is_file());

    let heads = git(dir, &["bundle", "list-heads", dest.to_str().unwrap()]);
    let head_refs: Vec<&str> = heads
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(_, r)| r))
        .collect();
    assert_eq!(head_refs, vec![seed::patchset_ref(r1, 1)], "{heads:?}");
    // The base ref is never a head of the bundle (it is the EXCLUSION).
    assert!(!heads.contains("refs/remotes/base/main"));
    // Should-fix: the refs manifest carries every ref name even when the
    // bundle itself IS written.
    let manifest = PathBuf::from(format!("{}.refs", dest.display()));
    let text = std::fs::read_to_string(&manifest).unwrap();
    assert!(text.contains(&seed::patchset_ref(r1, 1)), "{text}");
}

/// Should-fix review finding: "nothing to bundle" (every kbc ref already
/// reachable from `refs/remotes/base/*`) is a recorded no-op, not a hard
/// error — and the ref NAME survives in the manifest either way.
#[test]
fn a_bundle_fully_reachable_from_base_is_a_recorded_no_op_not_an_error() {
    let e = env();
    // tip == base: the patchset ref names EXACTLY the commit base/main
    // will point at, so there is nothing new for the bundle to carry.
    let r1 = review_with_patchset(&e, &e.fx.main_tip, &e.fx.main_tip);
    let row = ready_row(&e);
    let dir = Path::new(&row.git_dir);
    git(
        dir,
        &["update-ref", "refs/remotes/base/main", &e.fx.main_tip],
    );

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("out.bundle");
    let outcome = write_bundle(e.rs.git().unwrap(), dir, &dest).unwrap();
    assert_eq!(
        outcome,
        BundleOutcome::Skipped {
            reason: "fully-reachable-from-base"
        }
    );
    assert!(
        !dest.exists(),
        "git never wrote a genuinely empty bundle file"
    );
    let manifest = PathBuf::from(format!("{}.refs", dest.display()));
    let text = std::fs::read_to_string(&manifest).unwrap();
    assert!(text.contains(&seed::patchset_ref(r1, 1)), "{text}");
}

#[test]
fn a_store_with_no_kbc_refs_yet_is_skipped_not_errored() {
    let e = env();
    let row = ready_row(&e); // no review at all
    let dir = Path::new(&row.git_dir);
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("out.bundle");
    let outcome = write_bundle(e.rs.git().unwrap(), dir, &dest).unwrap();
    assert_eq!(outcome, BundleOutcome::Skipped { reason: "no-refs" });
    assert!(!dest.exists());
}

#[test]
fn bundle_pruning_keeps_only_the_newest_three() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir).unwrap();
    let uuid = "11111111-1111-4111-8111-111111111111";
    for ts in [100, 200, 300, 400, 500] {
        std::fs::write(bundle_path(dir, uuid, ts), b"x").unwrap();
    }
    // A different store's bundle must never be touched by this prune.
    let other = "22222222-2222-4222-8222-222222222222";
    std::fs::write(bundle_path(dir, other, 999), b"x").unwrap();

    let removed = prune_bundles(dir, uuid, BUNDLES_KEPT).unwrap();
    assert_eq!(removed.len(), 2, "{removed:?}");
    let remaining: std::collections::BTreeSet<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(remaining.contains(&format!("store-{uuid}-300.bundle")));
    assert!(remaining.contains(&format!("store-{uuid}-400.bundle")));
    assert!(remaining.contains(&format!("store-{uuid}-500.bundle")));
    assert!(!remaining.contains(&format!("store-{uuid}-100.bundle")));
    assert!(!remaining.contains(&format!("store-{uuid}-200.bundle")));
    assert!(
        remaining.contains(&format!("store-{other}-999.bundle")),
        "another store's bundle survives untouched"
    );
}

#[test]
fn prune_bundles_on_a_missing_dir_is_a_harmless_empty_list() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("does-not-exist");
    assert_eq!(
        prune_bundles(&dir, "any", BUNDLES_KEPT).unwrap(),
        Vec::<PathBuf>::new()
    );
}

#[test]
fn backup_all_ready_stores_writes_and_prunes_across_a_real_volume() {
    let e = env();
    review_with_patchset(&e, &e.fx.feat_tip, &e.fx.main_tip);
    let row = ready_row(&e);

    // Direct sqlite handle onto the SAME volume `env()` opened — mirrors
    // how `Store::open`'s boot hook and `kb-code backup` see it (a
    // SEPARATE connection; sqlite/WAL supports true concurrent readers).
    let conn = rusqlite::Connection::open(e.fx.home.join("index.db")).unwrap();
    let git_spawner = e.rs.git().unwrap();

    let r1 = backup_all_ready_stores(&conn, git_spawner, &e.fx.home, 1000);
    assert_eq!(r1.stores.len(), 1, "{r1:?}");
    assert!(r1.stores[0].written.is_some(), "{r1:?}");
    assert_eq!(r1.stores[0].uuid, row.uuid);
    assert!(r1.stores[0].written.as_ref().unwrap().is_file());

    // A second pass at a LATER timestamp writes a second bundle and never
    // errors — both exist until pruning drops the older ones.
    let r2 = backup_all_ready_stores(&conn, git_spawner, &e.fx.home, 2000);
    assert!(r2.stores[0].written.is_some());
    let dest_dir = backups_dir(&e.fx.home);
    let bundles: Vec<_> = std::fs::read_dir(&dest_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "bundle"))
        .collect();
    assert_eq!(bundles.len(), 2, "{bundles:?}");
}

#[test]
fn backup_all_ready_stores_is_a_quiet_no_op_before_v0045_lands() {
    // A volume with no `review_stores` table at all (the FIRST V0045
    // crossing's own pre-migration point) must never error — it has
    // nothing to back up yet (README §10 step 1).
    let tmp = tempfile::tempdir().unwrap();
    let conn = rusqlite::Connection::open(tmp.path().join("index.db")).unwrap();
    conn.execute_batch("CREATE TABLE payload (x TEXT);")
        .unwrap();
    let git_home = tmp.path().join("git-home");
    let git_spawner = StoreGit::new(&git_home).unwrap();
    let report = backup_all_ready_stores(&conn, &git_spawner, tmp.path(), 1);
    assert!(report.stores.is_empty());
}

/// Should-fix review finding: NO git I/O on the boot path — `Store::open`
/// only ever WRITES this marker (filesystem only); the actual bundle pass
/// runs later, off `spawn_boot_bundle_backup`.
#[test]
fn mark_boot_backup_pending_writes_a_readable_marker() {
    let tmp = tempfile::tempdir().unwrap();
    mark_boot_backup_pending(tmp.path(), "gated-epoch-snapshot").unwrap();
    let marker = tmp.path().join(BOOT_BACKUP_PENDING_FILE);
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        "gated-epoch-snapshot"
    );
}

// ── repack after a multi-member seed (RS-U3's follow-up) ────────────────

#[test]
fn a_multi_member_seed_leaves_the_store_at_one_pack() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("state");
    let one = tmp.path().join("work/widgets-01");
    let two = tmp.path().join("work/widgets-02");
    std::fs::create_dir_all(&one).unwrap();
    git(&one, &["init", "-q", "-b", "main"]);
    // Disable auto-gc/-maintenance so the CLONE stays deliberately
    // many-packed too (mirrors seed/tests.rs's `a_many_pack_source_seeds_
    // one_pack`) — not load-bearing for THIS test (only the STORE's pack
    // count matters), but keeps the fixture's git behavior deterministic
    // across git versions.
    git(&one, &["config", "gc.auto", "0"]);
    git(&one, &["config", "maintenance.auto", "false"]);
    commit(&one, "a.txt", "a");
    git(
        &one,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    );
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            "--no-local",
            one.to_str().unwrap(),
            two.to_str().unwrap(),
        ],
    );
    git(
        &two,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    );
    commit(&two, "b.txt", "b"); // widgets-02 diverges: a distinct head to fetch.

    std::fs::create_dir_all(&home).unwrap();
    let store = Store::open(&home.join("index.db")).unwrap();
    let repos = vec![
        RepoEntry {
            name: "widgets-01".into(),
            path: one.clone(),
        },
        RepoEntry {
            name: "widgets-02".into(),
            path: two.clone(),
        },
    ];
    let mut ids = HashMap::new();
    for r in &repos {
        ids.insert(
            r.name.clone(),
            store
                .upsert_repo(&r.name, &r.path.to_string_lossy())
                .unwrap(),
        );
    }
    let rs = ReviewStores::new(&ReviewSection::default(), &home, &repos, &ids);
    // widgets-02 has ONE remote and resolves the store unambiguously;
    // widgets-01 joins by membership.
    let sid = member_id(&rs.register_repo(&store, "widgets-02", None));
    member_id(&rs.register_repo(&store, "widgets-01", None));
    rs.seed(&store, sid, false).unwrap();

    let row = store.store_for_repo_name("widgets-01").unwrap().unwrap();
    let stats = seed::store_stats(Path::new(&row.git_dir));
    assert_eq!(stats.packs, 1, "{stats:?}");
}

/// RS-U9 review finding B1 (data-loss blocker): `gc_pass`'s membership
/// MUST be DB truth (`Store::store_members`), never `ReviewStores::
/// members_of`'s config-filtered list — a member whose clone/config this
/// PROCESS currently cannot resolve is still a live member in the DB, and
/// its `refs/remotes/work-<id>/*` must survive a GC pass untouched, with
/// the pass reporting itself `partial` rather than silently "complete".
#[test]
fn a_member_unresolvable_this_process_keeps_its_refs_and_reports_partial() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("state");
    let one = tmp.path().join("work/widgets-01");
    let two = tmp.path().join("work/widgets-02");
    std::fs::create_dir_all(&one).unwrap();
    git(&one, &["init", "-q", "-b", "main"]);
    commit(&one, "a.txt", "a");
    git(
        &one,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    );
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            "--no-local",
            one.to_str().unwrap(),
            two.to_str().unwrap(),
        ],
    );
    git(
        &two,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    );
    commit(&two, "b.txt", "b");

    std::fs::create_dir_all(&home).unwrap();
    let store = Store::open(&home.join("index.db")).unwrap();
    let repos = vec![
        RepoEntry {
            name: "widgets-01".into(),
            path: one.clone(),
        },
        RepoEntry {
            name: "widgets-02".into(),
            path: two.clone(),
        },
    ];
    let mut ids = HashMap::new();
    for r in &repos {
        ids.insert(
            r.name.clone(),
            store
                .upsert_repo(&r.name, &r.path.to_string_lossy())
                .unwrap(),
        );
    }
    let id_two = ids["widgets-02"];
    let rs = ReviewStores::new(&ReviewSection::default(), &home, &repos, &ids);
    let sid = member_id(&rs.register_repo(&store, "widgets-02", None));
    member_id(&rs.register_repo(&store, "widgets-01", None));
    rs.seed(&store, sid, false).unwrap();
    let row = store.store_for_repo_name("widgets-01").unwrap().unwrap();
    let dir = Path::new(&row.git_dir);
    let work_two_prefix = format!("refs/remotes/work-{id_two}/");
    assert!(
        store_refs(dir)
            .iter()
            .any(|r| r.starts_with(&work_two_prefix)),
        "fixture sanity: widgets-02 must have a work ref before the test begins"
    );

    // Simulate widgets-02 becoming unresolvable THIS PROCESS ONLY (its
    // clone path moved, or it left [[repos]]) — a SECOND ReviewStores over
    // the SAME (still fully DB-registered) store, with a `repos` list that
    // omits it. DB membership (`repo_stores`) is untouched.
    let repos_missing_two = vec![RepoEntry {
        name: "widgets-01".into(),
        path: one.clone(),
    }];
    let rs_degraded = ReviewStores::new(&ReviewSection::default(), &home, &repos_missing_two, &ids);

    let report = run_pass_for_store(&rs_degraded, &store, &row, &[MaintTask::Daily], 100);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let gc = report.gc.expect("a gc report");
    assert!(gc.partial, "must self-report partial: {gc:?}");
    assert!(
        !gc.member_problems.is_empty(),
        "must name the unresolvable member: {gc:?}"
    );
    // The actual data-loss check: widgets-02's work ref is NOT a delete
    // candidate (it would show up in `report` as orphaned/deleted, and
    // it is still on disk either way since the scheduler never applies —
    // but the classification itself, exercised via `run_gc_now`, must
    // never orphan it).
    let gc_now = run_gc_now(&rs_degraded, &store, &row, false, false, 101).unwrap();
    assert!(gc_now.partial);
    assert_eq!(
        gc_now.candidates, 0,
        "widgets-02's work ref must be KEPT (DB truth), not classified as a candidate: {gc_now:?}"
    );
    assert!(store_refs(dir)
        .iter()
        .any(|r| r.starts_with(&work_two_prefix)));
}

#[test]
fn repack_full_consolidates_an_explicitly_many_pack_store() {
    let e = env();
    review_with_patchset(&e, &e.fx.feat_tip, &e.fx.main_tip);
    let row = ready_row(&e);
    let dir = Path::new(&row.git_dir);
    let git_spawner = e.rs.git().unwrap();
    // Force the store itself into more than one pack (independent of
    // however many `import_member` happened to leave behind): write a
    // handful of loose blobs directly, then pack them one at a time so
    // the store starts many-packed.
    git(dir, &["config", "gc.auto", "0"]);
    for i in 0..4u8 {
        let blob = e_write_blob(dir, i);
        let mut child = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(dir)
            .args(["pack-objects", "-q", "objects/pack/pack"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            writeln!(stdin, "{blob}").unwrap();
        }
        assert!(child.wait().unwrap().success());
        git(dir, &["prune-packed"]);
    }
    let before = seed::store_stats(dir);
    assert!(before.packs >= 2, "{before:?}");

    repack_full(git_spawner, dir).unwrap();
    let after = seed::store_stats(dir);
    assert_eq!(after.packs, 1, "{after:?}");
    // Every object survives the repack (fsck stays clean).
    fsck(dir);
}

/// Write ONE loose blob (content varies by `i`) into a bare repo and
/// return its oid.
fn e_write_blob(dir: &Path, i: u8) -> String {
    let mut child = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(dir)
        .args(["hash-object", "-w", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "blob-{i}").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}
