//! RS-U3 — review-store seeding, registration and boot tests. Hermetic:
//! real git over local fixture clones (synthetic `acme/widgets`), a temp
//! DB, no network. Test-only direct `git` spawns build the fixtures (this
//! file is in SEC-17's `GIT_SPAWNING_FILES`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::config::{RepoEntry, ReviewSection};
use crate::git::roots::{GitCtx, WorkTreeRoot};
use crate::review_store::gc;
use crate::review_store::registry::{Registration, ReviewStores, StoreUnavailable};
// `patchset_ref`/`patchset_base_ref` come from `super::*` below (this
// module's OWN copies, `seed.rs:patchset_ref`/`patchset_base_ref` —
// byte-identical strings to `crate::reviews`'s, RS-U3's existing
// duplication, not a new one).
use crate::reviews::{delete_review_with_refs, pr_ref, PrRefScope};
use crate::store::Store;

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

/// Write the objects HEAD introduced into a pack of their own, then drop the
/// loose copies. `git repack` may consolidate packs depending on the git
/// version and its defaults (CI's git left 2 packs for 6 repacks), so the
/// many-pack fixture builds each pack explicitly with `pack-objects`.
fn pack_head_objects(dir: &Path) {
    let objects = git(dir, &["rev-list", "--objects", "HEAD", "--not", "HEAD~1"]);
    let mut child = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(dir)
        .args(["pack-objects", "-q", ".git/objects/pack/pack"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(objects.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    assert!(child.wait().unwrap().success(), "pack-objects failed");
    git(dir, &["prune-packed"]);
}

/// `widgets.01` (origin = acme/widgets, plus a personal fork remote) and
/// `widgets.02` cloned from it (origin re-pointed at acme/widgets). No
/// network is ever touched: the forge URLs are config only.
struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    one: PathBuf,
    two: PathBuf,
    main_tip: String,
    feat_tip: String,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("state");
    let one = tmp.path().join("work/widgets.01");
    let two = tmp.path().join("work/widgets.02");
    std::fs::create_dir_all(&one).unwrap();
    git(&one, &["init", "-q", "-b", "main"]);
    commit(&one, "a.txt", "a");
    let main_tip = commit(&one, "b.txt", "b");
    git(&one, &["checkout", "-q", "-b", "feature/x"]);
    let feat_tip = commit(&one, "c.txt", "c");
    git(&one, &["checkout", "-q", "main"]);
    git(
        &one,
        &["remote", "add", "origin", "git@github.com:acme/widgets.git"],
    );
    git(
        &one,
        &[
            "remote",
            "add",
            "mine",
            "https://github.com/someone/widgets.git",
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
            "https://github.com/Acme/Widgets.git",
        ],
    );
    git(&two, &["checkout", "-q", "-b", "only-in-two"]);
    commit(&two, "d.txt", "d");
    git(&two, &["checkout", "-q", "main"]);
    Fixture {
        _tmp: tmp,
        home,
        one,
        two,
        main_tip,
        feat_tip,
    }
}

struct Env {
    fx: Fixture,
    store: Store,
    rs: ReviewStores,
    ids: HashMap<String, i64>,
}

fn env_with(fx: Fixture, review: ReviewSection) -> Env {
    std::fs::create_dir_all(&fx.home).unwrap();
    let store = Store::open(&fx.home.join("index.db")).unwrap();
    let repos = vec![
        RepoEntry {
            name: "widgets-01".into(),
            path: fx.one.clone(),
        },
        RepoEntry {
            name: "widgets-02".into(),
            path: fx.two.clone(),
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
    let rs = ReviewStores::new(&review, &fx.home, &repos, &ids);
    Env { fx, store, rs, ids }
}

fn env() -> Env {
    env_with(fixture(), ReviewSection::default())
}

/// A review with one patchset whose ref is pinned in `clone`.
fn review_in(e: &Env, repo: &str, clone: &Path, tip: &str, base: &str) -> i64 {
    let id = e
        .store
        .create_review(
            repo,
            Some("t"),
            "refs/remotes/origin/main",
            "feature/x",
            None,
            1,
        )
        .unwrap();
    e.store.insert_patchset(id, 1, tip, base, 1).unwrap();
    git(clone, &["update-ref", &patchset_ref(id, 1), tip]);
    id
}

fn row_for(e: &Env, repo: &str) -> crate::store::ReviewStoreRow {
    e.store.store_for_repo_name(repo).unwrap().expect("a store")
}

fn store_refs(dir: &Path) -> Vec<String> {
    let out = git(dir, &["for-each-ref", "--format=%(refname)"]);
    out.lines().map(str::to_string).collect()
}

fn member_id(r: &Registration) -> i64 {
    match r {
        Registration::Member { store_id, .. } => *store_id,
        other => panic!("expected Member, got {other:?}"),
    }
}

// ---------------------------------------------------------------------

#[test]
fn common_dir_resolves_worktrees_and_bare_repos() {
    let fx = fixture();
    let common = common_dir_of(&fx.one).unwrap();
    assert_eq!(common, std::fs::canonicalize(fx.one.join(".git")).unwrap());
    let wt = fx.one.parent().unwrap().join("widgets.01-wt");
    git(
        &fx.one,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "feature/x"],
    );
    assert_eq!(common_dir_of(&wt).unwrap(), common);
    let bare = fx.one.parent().unwrap().join("bare.git");
    git(
        fx.one.parent().unwrap(),
        &["init", "-q", "--bare", bare.to_str().unwrap()],
    );
    assert_eq!(
        common_dir_of(&bare).unwrap(),
        std::fs::canonicalize(&bare).unwrap()
    );
    assert!(common_dir_of(fx.one.parent().unwrap()).is_err());
}

#[test]
fn remotes_are_read_from_the_member_config() {
    let e = env();
    let g = e.rs.git().unwrap();
    let r = read_remotes(g, &common_dir_of(&e.fx.one).unwrap()).unwrap();
    let names: Vec<&str> = r.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, vec!["mine", "origin"]);
    git(&e.fx.one, &["config", "remote.origin.gh-resolved", "base"]);
    let r = read_remotes(g, &common_dir_of(&e.fx.one).unwrap()).unwrap();
    assert_eq!(
        r.iter()
            .find(|x| x.name == "origin")
            .unwrap()
            .gh_resolved
            .as_deref(),
        Some("base")
    );
}

/// README §15.1: two member clones of one project resolve to ONE store,
/// each seeded under its own `work-<id>` namespace, reviews from both.
#[test]
fn two_member_clones_share_one_store() {
    let e = env();
    let r1 = review_in(&e, "widgets-01", &e.fx.one, &e.fx.feat_tip, &e.fx.main_tip);
    e.store
        .set_review_pr_binding(r1, 7, "acme/widgets", None, None, None)
        .unwrap();
    let two_tip = git(&e.fx.two, &["rev-parse", "only-in-two"]);
    let r2 = review_in(&e, "widgets-02", &e.fx.two, &two_tip, &e.fx.main_tip);

    // widgets-01 has two forge remotes; its PR binding's slug decides (rung 3).
    let reg1 = e.rs.register_repo(&e.store, "widgets-01", None);
    match &reg1 {
        Registration::Member {
            store_key,
            source,
            joined_existing,
            ..
        } => {
            assert_eq!(store_key, "github.com/acme/widgets");
            assert_eq!(source, "pr-slug");
            assert!(!joined_existing);
        }
        other => panic!("{other:?}"),
    }
    // widgets-02's origin normalizes to the same key → joins.
    let reg2 = e.rs.register_repo(&e.store, "widgets-02", None);
    assert_eq!(member_id(&reg1), member_id(&reg2));
    assert!(
        matches!(&reg2, Registration::Member { source, joined_existing: true, .. } if source == "member")
    );
    assert_eq!(e.store.list_review_stores().unwrap().len(), 1);
    let row = row_for(&e, "widgets-02");
    assert_eq!(row.state, "absent");
    assert_eq!(
        row.base_url.as_deref(),
        Some("https://github.com/acme/widgets.git")
    );
    assert_eq!(row.forge_kind.as_deref(), Some("github"));
    assert_eq!(row.forge_verified, "verified");
    assert!(row.git_dir.ends_with(&format!("{}.git", row.uuid)));

    let report = e.rs.seed(&e.store, row.id, false).unwrap();
    assert!(matches!(report.base, BaseFetch::Skipped { ref code } if code == "offline-seed"));
    assert!(
        report.objects_missing.is_empty(),
        "{:?}",
        report.objects_missing
    );
    let row = row_for(&e, "widgets-01");
    assert_eq!(row.state, "ready");
    let dir = PathBuf::from(&row.git_dir);
    let refs = store_refs(&dir);
    let (i1, i2) = (e.ids["widgets-01"], e.ids["widgets-02"]);
    for want in [
        format!("refs/remotes/work-{i1}/main"),
        format!("refs/remotes/work-{i1}/feature/x"),
        format!("refs/remotes/work-{i2}/main"),
        format!("refs/remotes/work-{i2}/only-in-two"),
        patchset_ref(r1, 1),
        patchset_ref(r2, 1),
    ] {
        assert!(refs.contains(&want), "missing {want} in {refs:?}");
    }
    assert!(!tmp_dir(&e.rs.settings().root, &row.uuid).exists());
    manifest::check(&dir, &row.uuid, &row.store_key).unwrap();
    // Store config: hooks neutralised, no push, gc off, HEAD parked.
    let cfg = |k: &str| git(&dir, &["config", "--get", k]);
    assert_eq!(cfg("gc.auto"), "0");
    assert_eq!(cfg("maintenance.auto"), "false");
    assert_eq!(cfg("fetch.prune"), "false");
    assert_eq!(cfg("core.hooksPath"), "/dev/null");
    assert_eq!(cfg("protocol.version"), "2");
    assert_eq!(cfg("uploadpack.allowAnySHA1InWant"), "true");
    assert_eq!(
        cfg(&format!("remote.work-{i1}.pushurl")),
        crate::review_store::NO_PUSH_URL
    );
    assert_eq!(cfg("remote.base.pushurl"), crate::review_store::NO_PUSH_URL);
    assert_eq!(
        cfg("remote.base.url"),
        "https://github.com/acme/widgets.git"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("HEAD")).unwrap().trim(),
        "ref: refs/kbc/none"
    );
    // The handle API opens it.
    let h = e.rs.handle_for_repo(&e.store, "widgets-02").unwrap();
    assert_eq!(h.git_dir, dir);
}

/// README §15.2: `refs/kbc/pr/N` disagreeing between two clones is never
/// imported, so it cannot conflict.
#[test]
fn legacy_pr_refs_are_ignored_even_when_clones_disagree() {
    let e = env();
    git(&e.fx.one, &["update-ref", "refs/kbc/pr/15", &e.fx.main_tip]);
    git(&e.fx.two, &["update-ref", "refs/kbc/pr/15", &e.fx.feat_tip]);
    review_in(&e, "widgets-01", &e.fx.one, &e.fx.feat_tip, &e.fx.main_tip);
    let id = member_id(&e.rs.register_repo(&e.store, "widgets-02", None));
    // widgets-02 has ONE forge remote → rung 5; then 01 joins by membership.
    member_id(&e.rs.register_repo(&e.store, "widgets-01", None));
    let rep = e.rs.seed(&e.store, id, false).unwrap();
    assert!(rep.members.iter().all(|m| m.conflicts.is_empty()));
    let refs = store_refs(Path::new(&row_for(&e, "widgets-01").git_dir));
    assert!(
        !refs.iter().any(|r| r.starts_with("refs/kbc/pr/")),
        "{refs:?}"
    );
}

/// README §15 / U3 done-when: reviews whose tips exist nowhere go
/// `objects-missing` while the store itself goes `ready`; a review whose
/// ref was lost but whose commit survives is recovered by sha.
#[test]
fn missing_tips_mark_reviews_while_the_store_goes_ready() {
    let e = env();
    let ok = review_in(&e, "widgets-01", &e.fx.one, &e.fx.feat_tip, &e.fx.main_tip);
    let gone_a = e
        .store
        .create_review("widgets-01", None, "main", "x", None, 1)
        .unwrap();
    e.store
        .insert_patchset(gone_a, 1, &"1".repeat(40), &e.fx.main_tip, 1)
        .unwrap();
    let gone_b = e
        .store
        .create_review("widgets-02", None, "main", "y", None, 1)
        .unwrap();
    e.store
        .insert_patchset(gone_b, 1, &"2".repeat(40), &"3".repeat(40), 1)
        .unwrap();
    // A dangling commit: reachable from no ref in the clone.
    git(&e.fx.one, &["checkout", "-q", "-b", "tmp-dangling"]);
    let dangling = commit(&e.fx.one, "e.txt", "e");
    git(&e.fx.one, &["checkout", "-q", "main"]);
    git(&e.fx.one, &["branch", "-q", "-D", "tmp-dangling"]);
    let lost = e
        .store
        .create_review("widgets-01", None, "main", "z", None, 1)
        .unwrap();
    e.store
        .insert_patchset(lost, 1, &dangling, &e.fx.main_tip, 1)
        .unwrap();
    e.store
        .set_review_objects_state(ok, Some(OBJECTS_MISSING))
        .unwrap();

    let id = member_id(&e.rs.register_repo(&e.store, "widgets-02", None));
    e.rs.register_repo(&e.store, "widgets-01", None);
    let rep = e.rs.seed(&e.store, id, false).unwrap();
    assert_eq!(rep.objects_missing, vec![gone_a, gone_b]);
    assert_eq!(rep.recovered_by_sha, 1);
    assert_eq!(row_for(&e, "widgets-01").state, "ready");
    let st = |id| e.store.get_review_base(id).unwrap().unwrap().objects_state;
    assert_eq!(st(gone_a).as_deref(), Some(OBJECTS_MISSING));
    assert_eq!(st(gone_b).as_deref(), Some(OBJECTS_MISSING));
    assert_eq!(
        st(ok),
        None,
        "a previously-missing review that is whole clears"
    );
    assert_eq!(st(lost), None);
    let refs = store_refs(Path::new(&row_for(&e, "widgets-01").git_dir));
    assert!(
        refs.contains(&patchset_ref(lost, 1)),
        "recovered ref recreated"
    );
    // The member clone was never written to by the recovery.
    assert!(
        !git(&e.fx.one, &["for-each-ref", "--format=%(refname)"]).contains(&patchset_ref(lost, 1))
    );
}

/// README §15.3: a many-pack source seeds into ONE pack.
#[test]
fn a_many_pack_source_seeds_one_pack() {
    let e = env();
    // Commits trigger auto-gc / auto-maintenance, which on some git versions
    // consolidates small packs (CI's git merged 6 into 4): switch both off.
    git(&e.fx.one, &["config", "gc.auto", "0"]);
    git(&e.fx.one, &["config", "maintenance.auto", "false"]);
    for i in 0..6 {
        commit(&e.fx.one, &format!("p{i}.txt"), &format!("{i}"));
        pack_head_objects(&e.fx.one);
    }
    let packs = std::fs::read_dir(e.fx.one.join(".git/objects/pack"))
        .unwrap()
        .flatten()
        .filter(|p| p.path().extension().is_some_and(|x| x == "pack"))
        .count();
    assert!(packs >= 5, "fixture should be many-pack, got {packs}");
    // Only widgets-01 is a member.
    let id = member_id(&e.rs.register_repo(
        &e.store,
        "widgets-01",
        Some("https://github.com/acme/widgets.git"),
    ));
    e.rs.seed(&e.store, id, false).unwrap();
    let s = store_stats(Path::new(&row_for(&e, "widgets-01").git_dir));
    assert_eq!(s.packs, 1, "{s:?}");
    assert_eq!(s.loose_objects, 0, "{s:?}");
}

/// README §15.3: an injected failure mid-seed leaves no `.tmp`, and the
/// store stays `absent` (with the failure recorded) for the next attempt.
#[test]
fn an_injected_failure_mid_seed_leaves_no_tmp() {
    let e = env();
    let id = member_id(&e.rs.register_repo(&e.store, "widgets-02", None));
    e.rs.register_repo(&e.store, "widgets-01", None);
    let row = e.store.get_review_store(id).unwrap().unwrap();
    let (mut plan, _) = e.rs.plan_for(&e.store, &row).unwrap();
    plan.fail_after_members = Some(1);
    let err = seed_store(e.rs.git().unwrap(), &plan, None, 1).unwrap_err();
    assert_eq!(err.stage, "member-fetch");
    assert!(!tmp_dir(&plan.root, &plan.uuid).exists());
    assert!(!store_dir(&plan.root, &plan.uuid).exists());
    // A leftover tmp from a "crashed" process is swept at boot.
    std::fs::create_dir_all(tmp_dir(&plan.root, "dead-uuid")).unwrap();
    assert_eq!(sweep_stale_tmp(&plan.root), 1);
    // A real seed afterwards succeeds.
    e.rs.seed(&e.store, id, false).unwrap();
}

#[test]
fn a_manifest_mismatch_breaks_the_store_instead_of_adopting_it() {
    let e = env();
    let id = member_id(&e.rs.register_repo(&e.store, "widgets-02", None));
    e.rs.seed(&e.store, id, false).unwrap();
    let row = e.store.get_review_store(id).unwrap().unwrap();
    let dir = PathBuf::from(&row.git_dir);
    let mut m = manifest::read(&dir).unwrap();
    m.store_key = "github.com/acme/gadgets".into();
    manifest::write(&dir, &m).unwrap();
    // A fresh process (no lock held yet) opens it.
    let repos = vec![RepoEntry {
        name: "widgets-02".into(),
        path: e.fx.two.clone(),
    }];
    let fresh = ReviewStores::new(&ReviewSection::default(), &e.fx.home, &repos, &e.ids);
    drop(e.rs); // releases the first registry's flock
    match fresh.handle_for_repo(&e.store, "widgets-02") {
        Err(StoreUnavailable::Broken { code }) => assert_eq!(code, "manifest-mismatch"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        e.store.get_review_store(id).unwrap().unwrap().state,
        "broken"
    );
}

#[test]
fn a_second_process_sees_the_store_locked() {
    let e = env();
    let id = member_id(&e.rs.register_repo(&e.store, "widgets-02", None));
    e.rs.seed(&e.store, id, false).unwrap();
    let repos = vec![RepoEntry {
        name: "widgets-02".into(),
        path: e.fx.two.clone(),
    }];
    let other = ReviewStores::new(&ReviewSection::default(), &e.fx.home, &repos, &e.ids);
    assert_eq!(
        other.handle_for_repo(&e.store, "widgets-02").unwrap_err(),
        StoreUnavailable::LockedElsewhere
    );
    assert!(other.admit_mutation(&e.store, "widgets-02").is_err());
    assert!(e.rs.handle_for_repo(&e.store, "widgets-02").is_ok());
}

#[test]
fn an_ambiguous_repo_refuses_and_an_explicit_url_resolves_it() {
    let e = env();
    // widgets-01 has two forge remotes, no PR binding, no store yet.
    match e.rs.register_repo(&e.store, "widgets-01", None) {
        Registration::Refused {
            code, candidates, ..
        } => {
            assert_eq!(code, "base-url-ambiguous");
            assert_eq!(candidates.len(), 2);
        }
        other => panic!("{other:?}"),
    }
    assert!(e.store.store_for_repo_name("widgets-01").unwrap().is_none());
    let r = e.rs.register_repo(
        &e.store,
        "widgets-01",
        Some("git@github.com:acme/widgets.git"),
    );
    assert!(matches!(r, Registration::Member { ref source, .. } if source == "explicit"));
    // Never a re-key.
    match e.rs.register_repo(
        &e.store,
        "widgets-01",
        Some("https://github.com/acme/gadgets"),
    ) {
        Registration::Refused { code, .. } => assert_eq!(code, "base-url-key-mismatch"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_repo_with_no_forge_remote_gets_a_local_store() {
    let e = env();
    git(&e.fx.two, &["remote", "remove", "origin"]);
    let r = e.rs.register_repo(&e.store, "widgets-02", None);
    let row = row_for(&e, "widgets-02");
    assert!(matches!(r, Registration::Member { ref source, .. } if source == "local"));
    assert_eq!(row.store_key, format!("local:{}", row.uuid));
    assert_eq!(row.base_url, None);
    let rep = e.rs.seed(&e.store, row.id, true).unwrap();
    assert!(matches!(rep.base, BaseFetch::Skipped { ref code } if code == "no-base-remote"));
}

#[test]
fn the_boot_job_registers_and_seeds_only_repos_with_reviews() {
    let e = env();
    review_in(&e, "widgets-02", &e.fx.two, &e.fx.main_tip, &e.fx.main_tip);
    let s = crate::review_store::boot::run_boot(&e.rs, &e.store);
    assert_eq!(s.registered, vec!["widgets-02".to_string()]);
    assert_eq!(s.seeded, 1, "{s:?}");
    assert!(e.store.store_for_repo_name("widgets-01").unwrap().is_none());
    assert_eq!(row_for(&e, "widgets-02").state, "ready");
    // A second boot pass only opens it.
    let s2 = crate::review_store::boot::run_boot(&e.rs, &e.store);
    assert_eq!((s2.seeded, s2.opened), (0, 1));
    // `seed_on_boot = false` registers but leaves it absent.
    let mut review = ReviewSection::default();
    review.store.seed_on_boot = false;
    let e2 = env_with(fixture(), review);
    review_in(
        &e2,
        "widgets-02",
        &e2.fx.two,
        &e2.fx.main_tip,
        &e2.fx.main_tip,
    );
    let s3 = crate::review_store::boot::run_boot(&e2.rs, &e2.store);
    assert_eq!(s3.seeded, 0);
    assert_eq!(row_for(&e2, "widgets-02").state, "absent");
}

#[test]
fn a_seeding_store_refuses_mutations() {
    let e = env();
    let id = member_id(&e.rs.register_repo(&e.store, "widgets-02", None));
    e.store.set_review_store_state(id, "seeding", None).unwrap();
    match e.rs.admit_mutation(&e.store, "widgets-02") {
        Err(r) => assert_eq!(r.0, StoreUnavailable::Seeding),
        Ok(h) => panic!("admitted {h:?}"),
    }
    // A second concurrent seed of the same store is refused in-process.
    e.store.set_review_store_state(id, "absent", None).unwrap();
    assert!(e
        .rs
        .admit_mutation(&e.store, "widgets-02")
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn the_sync_route_answers_503_while_seeding() {
    use axum::extract::{Path as AxPath, Query, State};
    use axum::response::IntoResponse;
    let fx = fixture();
    let cfg = crate::config::KbCodeConfig {
        repos: vec![RepoEntry {
            name: "widgets-02".into(),
            path: fx.two.clone(),
        }],
        transcripts: crate::config::TranscriptsSection {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let paths = kb_core::paths::KbPaths::rooted_at(&fx.home, "kb-code");
    let state = crate::build_state_for_test(cfg, paths).await.unwrap();
    let st = state.clone();
    let id = tokio::task::spawn_blocking(move || {
        member_id(
            &st.review_stores
                .register_repo(&st.store, "widgets-02", None),
        )
    })
    .await
    .unwrap();
    state
        .store
        .set_review_store_state(id, "seeding", None)
        .unwrap();
    let resp = crate::review_store::routes::store_sync_route(
        State(state.clone()),
        AxPath("widgets-02".into()),
        Query(crate::review_store::routes::SyncQuery::default()),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
    assert!(resp
        .headers()
        .get(axum::http::header::RETRY_AFTER)
        .is_some());
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["type"], "urn:kb:errors:store-seeding");
    assert_eq!(v["retry_after"], 30);

    // Once absent, the same route seeds it (offline) and the card says ready.
    state
        .store
        .set_review_store_state(id, "absent", None)
        .unwrap();
    let resp = crate::review_store::routes::store_sync_route(
        State(state.clone()),
        AxPath("widgets-02".into()),
        Query(crate::review_store::routes::SyncQuery { offline: true }),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let resp = crate::review_store::routes::store_show_route(
        State(state.clone()),
        AxPath("widgets-02".into()),
    )
    .await;
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let card: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(card["schema"], "kbc-store/1");
    assert_eq!(card["store"]["state"], "ready");
    assert_eq!(card["store"]["store_key"], "github.com/acme/widgets");
    assert_eq!(card["disk"]["packs"], 1);
    // A sync of the now-ready store takes the fetch locks and reports.
    let resp = crate::review_store::routes::store_sync_route(
        State(state.clone()),
        AxPath("widgets-02".into()),
        Query(crate::review_store::routes::SyncQuery { offline: true }),
    )
    .await
    .into_response();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    // Unknown repo: 404.
    let resp =
        crate::review_store::routes::store_show_route(State(state), AxPath("nope".into())).await;
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
}

// --- review fixes (RS-U3 follow-up) ------------------------------------

/// BLOCKER 1: a repo that joins an already-`ready` store is imported, and
/// until then its handle falls back (`MemberPending`, admission `None`).
#[test]
fn a_member_joining_a_ready_store_is_imported_and_falls_back_until_then() {
    let e = env();
    let id = member_id(&e.rs.register_repo(&e.store, "widgets-02", None));
    e.rs.seed(&e.store, id, false).unwrap();
    assert!(e.rs.handle_for_repo(&e.store, "widgets-02").is_ok());
    // widgets-01 gets a review AFTER the seed, then joins.
    let late = review_in(&e, "widgets-01", &e.fx.one, &e.fx.feat_tip, &e.fx.main_tip);
    assert_eq!(
        member_id(&e.rs.register_repo(&e.store, "widgets-01", None)),
        id
    );
    assert_eq!(
        e.rs.handle_for_repo(&e.store, "widgets-01").unwrap_err(),
        StoreUnavailable::MemberPending
    );
    assert!(e
        .rs
        .admit_mutation(&e.store, "widgets-01")
        .unwrap()
        .is_none());
    let dir = PathBuf::from(&row_for(&e, "widgets-01").git_dir);
    assert!(!store_refs(&dir).contains(&patchset_ref(late, 1)));
    let imported = e.rs.import_pending_members(&e.store, id).unwrap();
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].repo_id, e.ids["widgets-01"]);
    let refs = store_refs(&dir);
    assert!(refs.contains(&patchset_ref(late, 1)), "{refs:?}");
    assert!(refs.contains(&format!(
        "refs/remotes/work-{}/feature/x",
        e.ids["widgets-01"]
    )));
    assert!(e.rs.handle_for_repo(&e.store, "widgets-01").is_ok());
    // Idempotent: nothing pending any more.
    assert!(e
        .rs
        .import_pending_members(&e.store, id)
        .unwrap()
        .is_empty());
    // The boot job does the same for a ready store.
    let e2 = env();
    let id2 = member_id(&e2.rs.register_repo(&e2.store, "widgets-02", None));
    e2.rs.seed(&e2.store, id2, false).unwrap();
    review_in(
        &e2,
        "widgets-01",
        &e2.fx.one,
        &e2.fx.feat_tip,
        &e2.fx.main_tip,
    );
    review_in(
        &e2,
        "widgets-02",
        &e2.fx.two,
        &e2.fx.main_tip,
        &e2.fx.main_tip,
    );
    let s = crate::review_store::boot::run_boot(&e2.rs, &e2.store);
    assert_eq!((s.opened, s.imported), (1, 1), "{s:?}");
    assert!(e2.rs.handle_for_repo(&e2.store, "widgets-01").is_ok());
}

/// Fix 3: kb prunes `work-<id>/*` refs the member no longer has — before
/// the fetch, so a rename across a directory boundary cannot D/F-clash.
#[test]
fn renamed_branches_are_pruned_from_the_work_namespace() {
    let e = env();
    let id = member_id(&e.rs.register_repo(
        &e.store,
        "widgets-01",
        Some("https://github.com/acme/widgets.git"),
    ));
    e.rs.seed(&e.store, id, false).unwrap();
    let w = e.ids["widgets-01"];
    let dir = PathBuf::from(&row_for(&e, "widgets-01").git_dir);
    // feature/x → feature (the file-over-directory direction) ...
    git(&e.fx.one, &["branch", "-m", "feature/x", "feature"]);
    let h = e.rs.handle_for_repo(&e.store, "widgets-01").unwrap();
    let rep = e.rs.sync_ready(&e.store, &h, false).unwrap();
    assert!(rep.member_errors.is_empty(), "{:?}", rep.member_errors);
    assert_eq!(rep.members[0].pruned, 1);
    let refs = store_refs(&dir);
    assert!(
        refs.contains(&format!("refs/remotes/work-{w}/feature")),
        "{refs:?}"
    );
    assert!(!refs.contains(&format!("refs/remotes/work-{w}/feature/x")));
    // ... and back: feature → feature/x (directory-over-file).
    git(&e.fx.one, &["branch", "-m", "feature", "feature/x"]);
    let rep = e.rs.sync_ready(&e.store, &h, false).unwrap();
    assert!(rep.member_errors.is_empty(), "{:?}", rep.member_errors);
    let refs = store_refs(&dir);
    assert!(
        refs.contains(&format!("refs/remotes/work-{w}/feature/x")),
        "{refs:?}"
    );
    assert!(!refs.contains(&format!("refs/remotes/work-{w}/feature")));
    // A deleted branch disappears too; `main` is untouched.
    git(&e.fx.one, &["branch", "-D", "feature/x"]);
    e.rs.sync_ready(&e.store, &h, false).unwrap();
    let refs = store_refs(&dir);
    assert!(!refs
        .iter()
        .any(|r| r.starts_with(&format!("refs/remotes/work-{w}/feature"))));
    assert!(refs.contains(&format!("refs/remotes/work-{w}/main")));
}

/// Fix 4: a store minted first for the personal fork never captures a
/// clone whose PR bindings target the org.
#[test]
fn a_fork_store_that_exists_first_does_not_capture_an_org_clone() {
    let e = env();
    let fork_uuid = crate::review_store::registry::new_store_uuid().unwrap();
    e.store
        .create_review_store(
            &fork_uuid,
            "github.com/someone/widgets",
            "/nonexistent/fork.git",
            Some("https://github.com/someone/widgets.git"),
            Some("explicit"),
            1,
        )
        .unwrap();
    let r = review_in(&e, "widgets-01", &e.fx.one, &e.fx.feat_tip, &e.fx.main_tip);
    e.store
        .set_review_pr_binding(r, 7, "acme/widgets", None, None, None)
        .unwrap();
    match e.rs.register_repo(&e.store, "widgets-01", None) {
        Registration::Member {
            store_key, source, ..
        } => {
            assert_eq!(store_key, "github.com/acme/widgets");
            assert_eq!(source, "pr-slug");
        }
        other => panic!("{other:?}"),
    }
}

/// Fix 2: concurrent same-process opens never flock-conflict.
#[test]
fn concurrent_opens_in_one_process_do_not_lock_each_other_out() {
    let e = env();
    let id = member_id(&e.rs.register_repo(&e.store, "widgets-02", None));
    e.rs.seed(&e.store, id, false).unwrap();
    let repos = vec![RepoEntry {
        name: "widgets-02".into(),
        path: e.fx.two.clone(),
    }];
    let fresh = std::sync::Arc::new(ReviewStores::new(
        &ReviewSection::default(),
        &e.fx.home,
        &repos,
        &e.ids,
    ));
    drop(e.rs);
    let store = std::sync::Arc::new(e.store);
    let hs: Vec<_> = (0..8)
        .map(|_| {
            let (f, st) = (fresh.clone(), store.clone());
            std::thread::spawn(move || f.handle_for_repo(&st, "widgets-02"))
        })
        .collect();
    for h in hs {
        h.join().unwrap().expect("every concurrent open succeeds");
    }
}

#[test]
fn a_missing_base_tip_marks_the_review() {
    let e = env();
    let id = e
        .store
        .create_review("widgets-02", None, "main", "x", None, 1)
        .unwrap();
    e.store
        .insert_patchset_with_base(
            id,
            1,
            &e.fx.main_tip,
            &e.fx.main_tip,
            Some(&"4".repeat(40)),
            Some("push"),
            1,
        )
        .unwrap();
    let sid = member_id(&e.rs.register_repo(&e.store, "widgets-02", None));
    let rep = e.rs.seed(&e.store, sid, false).unwrap();
    assert_eq!(rep.objects_missing, vec![id]);
}

/// RS-U5 — README §5.4/§8's invariant extended to the `-base` pin: a
/// patchset's `base_tip_sha` whose object IS present (reachable via the
/// member's own imported heads, same as the tip) gets its
/// `ps<n>-base` ref recreated alongside `ps<n>`, even though neither was
/// ever written into the member clone itself.
#[test]
fn a_present_base_tip_gets_its_base_ref_recreated() {
    let e = env();
    let id = e
        .store
        .create_review("widgets-01", None, "main", "x", None, 1)
        .unwrap();
    // main_tip and feat_tip are both reachable in widgets-01 (the fixture's
    // `main`/`feature/x` branches), so both objects land in the store via
    // the ordinary heads import — no sha-recovery needed for either.
    e.store
        .insert_patchset_with_base(
            id,
            1,
            &e.fx.main_tip,
            &e.fx.main_tip,
            Some(&e.fx.feat_tip),
            Some("push"),
            1,
        )
        .unwrap();
    // widgets-01 alone carries TWO forge remotes (`origin` + `mine`), so it
    // cannot resolve on its own; register widgets-02 (a single remote)
    // FIRST to mint the store unambiguously, then widgets-01 joins by
    // matching membership (README §5.1 "Joining") — same order
    // `missing_tips_mark_reviews_while_the_store_goes_ready` uses.
    let sid = member_id(&e.rs.register_repo(&e.store, "widgets-02", None));
    member_id(&e.rs.register_repo(&e.store, "widgets-01", None));
    let rep = e.rs.seed(&e.store, sid, false).unwrap();
    assert!(rep.objects_missing.is_empty(), "{:?}", rep.objects_missing);
    let refs = store_refs(Path::new(&row_for(&e, "widgets-01").git_dir));
    assert!(
        refs.contains(&patchset_ref(id, 1)),
        "tip ref recreated: {refs:?}"
    );
    assert!(
        refs.contains(&patchset_base_ref(id, 1)),
        "base_tip_sha's object is present, so its -base ref is recreated: {refs:?}"
    );
    // The member clone was never written to.
    assert!(!git(&e.fx.one, &["for-each-ref", "--format=%(refname)"])
        .contains(&patchset_base_ref(id, 1)));
}

#[test]
fn boot_never_resets_a_seed_that_is_live_in_process() {
    let e = env();
    let id = member_id(&e.rs.register_repo(&e.store, "widgets-02", None));
    e.store.set_review_store_state(id, "seeding", None).unwrap();
    assert_eq!(e.store.reset_interrupted_seeding(&[id]).unwrap(), 0);
    assert_eq!(
        e.store.get_review_store(id).unwrap().unwrap().state,
        "seeding"
    );
    assert_eq!(e.store.reset_interrupted_seeding(&[]).unwrap(), 1);
    assert_eq!(
        e.store.get_review_store(id).unwrap().unwrap().state,
        "absent"
    );
}

#[test]
fn store_uuids_and_git_versions_are_validated() {
    use crate::review_store::registry::is_store_uuid;
    let u = crate::review_store::registry::new_store_uuid().unwrap();
    assert!(is_store_uuid(&u));
    for bad in [
        "",
        "../../etc",
        "9cdee070-5909-12a4-b045-05e79aa5b7f3",
        "9CDEE070-5909-42A4-B045-05E79AA5B7F3",
        "9cdee070-5909-42a4-b045-05e79aa5b7f3\n",
    ] {
        assert!(!is_store_uuid(bad), "{bad:?}");
    }
    // A row with a hostile uuid is refused before it becomes a path.
    let e = env();
    let id = e
        .store
        .create_review_store("../x", "github.com/acme/widgets", "/tmp/x", None, None, 1)
        .unwrap();
    e.store.add_repo_to_store(e.ids["widgets-02"], id).unwrap();
    assert_eq!(
        e.rs.seed(&e.store, id, false).unwrap_err(),
        StoreUnavailable::Broken {
            code: "bad-uuid".into()
        }
    );
    assert_eq!(parse_git_version("git version 2.55.0"), Some((2, 55)));
    assert_eq!(
        parse_git_version("git version 2.41.0.windows.1"),
        Some((2, 41))
    );
    assert_eq!(parse_git_version("git version 2.9"), Some((2, 9)));
    assert!(parse_git_version("git version 2.9").unwrap() < MIN_GIT);
    assert_eq!(parse_git_version("nope"), None);
    assert_eq!(
        git_too_old(e.rs.git().unwrap(), MIN_GIT),
        None,
        "this box runs a new git"
    );
    assert!(git_too_old(e.rs.git().unwrap(), (999, 0)).is_some());
}

/// RS-U5 / README §15.1, end to end (not `gc.rs`'s pure unit tests): two
/// REAL member clones sharing one store, reviews in both — the store-wide
/// GC engine (`gc::keep_set`/`attribute`/`delete_candidates`/`apply`)
/// deletes a gone review's refs and NEVER touches the other member's.
#[test]
fn store_wide_gc_deletes_a_gone_reviews_refs_and_never_a_siblings() {
    let e = env();
    let r1 = review_in(&e, "widgets-01", &e.fx.one, &e.fx.feat_tip, &e.fx.main_tip);
    let two_tip = git(&e.fx.two, &["rev-parse", "only-in-two"]);
    let r2 = review_in(&e, "widgets-02", &e.fx.two, &two_tip, &e.fx.main_tip);

    // widgets-01 alone carries TWO forge remotes and cannot resolve on its
    // own; register widgets-02 (a single remote) FIRST to mint the store
    // unambiguously, then widgets-01 joins by matching membership (README
    // §5.1 "Joining").
    let sid = member_id(&e.rs.register_repo(&e.store, "widgets-02", None));
    member_id(&e.rs.register_repo(&e.store, "widgets-01", None));
    let rep = e.rs.seed(&e.store, sid, false).unwrap();
    assert!(rep.objects_missing.is_empty(), "{:?}", rep.objects_missing);
    let row = row_for(&e, "widgets-01");
    let dir = PathBuf::from(&row.git_dir);
    let before = store_refs(&dir);
    assert!(before.contains(&patchset_ref(r1, 1)), "{before:?}");
    assert!(before.contains(&patchset_ref(r2, 1)), "{before:?}");
    // `review_in` pinned `patchset_ref(r1, 1)`/`(r2, 1)` in each CLONE as
    // fixture setup (so seeding's member import had something to pick up)
    // — those refs stay in the clones forever; GC never reaches a clone at
    // all. The invariance check below is therefore "unchanged", not
    // "absent": snapshot both clones NOW, before GC runs.
    let clone_refs_before: Vec<String> = [&e.fx.one, &e.fx.two]
        .into_iter()
        .map(|c| git(c, &["for-each-ref", "--format=%(refname) %(objectname)"]))
        .collect();

    // r1 "goes away" (deleted through some other path, refs left behind —
    // exactly the shape GC exists to reconcile).
    e.store.delete_review(r1).unwrap();

    let member_names = vec!["widgets-01".to_string(), "widgets-02".to_string()];
    let member_ids = e.store.store_members(row.id).unwrap();
    let keep = gc::keep_set(&e.store, &member_names, &member_ids).unwrap();
    let refs = crate::review_store::seed::list_refs(
        e.rs.git().unwrap(),
        &dir,
        &["refs/kbc/", "refs/remotes/work-"],
    )
    .unwrap();
    let attributed = gc::attribute(&refs, &keep);
    let delete = gc::delete_candidates(&attributed);
    let delete_names: Vec<String> = delete.iter().map(|c| c.refname.clone()).collect();
    assert_eq!(delete_names, vec![patchset_ref(r1, 1)], "{delete_names:?}");
    gc::apply(e.rs.git().unwrap(), &dir, &delete).unwrap();

    let after = store_refs(&dir);
    assert!(!after.contains(&patchset_ref(r1, 1)), "{after:?}");
    assert!(
        after.contains(&patchset_ref(r2, 1)),
        "member B's ref must survive member A's review going away: {after:?}"
    );
    let (i1, i2) = (e.ids["widgets-01"], e.ids["widgets-02"]);
    assert!(after.contains(&format!("refs/remotes/work-{i1}/main")));
    assert!(after.contains(&format!("refs/remotes/work-{i2}/main")));
    // Neither member clone was written to by the GC: byte-identical
    // for-each-ref before and after (invariance, not absence — see above).
    for (clone, want) in [&e.fx.one, &e.fx.two].into_iter().zip(&clone_refs_before) {
        let got = git(
            clone,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        );
        assert_eq!(&got, want, "{clone:?} was written to by the GC");
    }
}

/// RS-U5 — `delete_review_with_refs` on a READY store deletes the
/// review's `ps<n>`/`ps<n>-base` refs from the STORE (never the user
/// clone). Under `PrRefScope::StoreWide` it leaves `refs/kbc/pr/<n>`
/// alone — that decision is the store-wide GC's job (see the test above),
/// since a shared store's PR ref can be bound by ANOTHER member too.
#[test]
fn delete_review_with_refs_on_a_ready_store_removes_store_refs_never_the_clone() {
    let e = env();
    // widgets-01 alone carries TWO forge remotes and cannot resolve on its
    // own; register widgets-02 (a single remote) FIRST to mint the store
    // unambiguously, then widgets-01 joins by matching membership (README
    // §5.1 "Joining").
    let sid = member_id(&e.rs.register_repo(&e.store, "widgets-02", None));
    member_id(&e.rs.register_repo(&e.store, "widgets-01", None));
    let rep = e.rs.seed(&e.store, sid, false).unwrap();
    assert!(rep.objects_missing.is_empty(), "{:?}", rep.objects_missing);

    let id = e
        .store
        .create_review("widgets-01", None, "main", "feature/x", None, 1)
        .unwrap();
    e.store
        .insert_patchset_with_base(
            id,
            1,
            &e.fx.feat_tip,
            &e.fx.main_tip,
            Some(&e.fx.feat_tip),
            Some("push"),
            1,
        )
        .unwrap();
    e.store
        .set_review_pr_binding(id, 9, "acme/widgets", None, None, None)
        .unwrap();
    let dir = PathBuf::from(&row_for(&e, "widgets-01").git_dir);
    git(&dir, &["update-ref", &patchset_ref(id, 1), &e.fx.feat_tip]);
    git(
        &dir,
        &["update-ref", &patchset_base_ref(id, 1), &e.fx.feat_tip],
    );
    git(&dir, &["update-ref", &pr_ref(9), &e.fx.feat_tip]);

    let review = e.store.get_review(id).unwrap().unwrap();
    let ctx = GitCtx::for_repo(&e.store, "widgets-01", WorkTreeRoot::user_clone(&e.fx.one));
    assert!(
        !ctx.is_fallback(),
        "the store must be ready for this test to mean anything"
    );
    let bus = kb_core::events::EventBus::default();
    delete_review_with_refs(
        &e.store,
        &bus,
        ctx.primary(),
        &review,
        PrRefScope::StoreWide,
    )
    .unwrap();

    assert!(e.store.get_review(id).unwrap().is_none(), "row deleted");
    let refs = store_refs(&dir);
    assert!(!refs.contains(&patchset_ref(id, 1)), "{refs:?}");
    assert!(!refs.contains(&patchset_base_ref(id, 1)), "{refs:?}");
    assert!(
        refs.contains(&pr_ref(9)),
        "StoreWide scope leaves refs/kbc/pr/<n> for the store-wide GC to decide: {refs:?}"
    );
    // The member clone was never written to.
    let clone_refs = git(&e.fx.one, &["for-each-ref", "--format=%(refname)"]);
    assert!(!clone_refs.contains(&patchset_ref(id, 1)));
    assert!(!clone_refs.contains(&patchset_base_ref(id, 1)));
}

/// RS-U3 benchmark (not part of the suite): seed a store from REAL local
/// clones named by env vars, into a scratch root, local-only. Run with
/// `KBRS_BENCH_CLONES=/a,/b KBRS_BENCH_ROOT=/scratch cargo test … --
/// --ignored bench_seed_from_env --nocapture`.
#[test]
#[ignore]
fn bench_seed_from_env() {
    let clones: Vec<PathBuf> = std::env::var("KBRS_BENCH_CLONES")
        .expect("KBRS_BENCH_CLONES")
        .split(',')
        .map(PathBuf::from)
        .collect();
    let root = PathBuf::from(std::env::var("KBRS_BENCH_ROOT").expect("KBRS_BENCH_ROOT"));
    std::fs::create_dir_all(&root).unwrap();
    let g = StoreGit::new(root.join("git-home")).unwrap();
    let uuid = crate::review_store::registry::new_store_uuid().unwrap();
    let members: Vec<SeedMember> = clones
        .iter()
        .enumerate()
        .map(|(i, c)| SeedMember {
            repo_id: i as i64 + 1,
            common_dir: common_dir_of(c).unwrap(),
        })
        .collect();
    let plan = SeedPlan {
        root: root.join("stores"),
        uuid: uuid.clone(),
        store_key: "bench/local".into(),
        base_url: None,
        members,
        base_branches: vec![],
        patchsets: vec![],
        fail_after_members: None,
    };
    let t = std::time::Instant::now();
    let rep = seed_store(&g, &plan, None, 0).unwrap();
    let wall = t.elapsed();
    let s = store_stats(&rep.git_dir);
    let review_refs = store_refs_count(&rep.git_dir, "refs/kbc/review/");
    let pr_refs = store_refs_count(&rep.git_dir, "refs/kbc/pr/");
    println!(
        "BENCH seed wall={:.1}s packs={} pack_bytes={} total_bytes={} loose={} review_refs={} pr_refs={} members={}",
        wall.as_secs_f64(),
        s.packs,
        s.pack_bytes,
        s.total_bytes,
        s.loose_objects,
        review_refs,
        pr_refs,
        serde_json::to_string(&rep.members).unwrap()
    );
}

fn store_refs_count(dir: &Path, prefix: &str) -> usize {
    store_refs(dir)
        .iter()
        .filter(|r| r.starts_with(prefix))
        .count()
}
