//! RS-U5 — the pre-existing `POST /api/reviews/refs/gc --apply` route
//! (`kb-code review refs gc --repo R --apply`) against a READY review
//! store.
//!
//! RS-U5 gave that route a store-WIDE keep-set; this file pins that its
//! apply half then went through the SAME guards `kb-code store gc` uses
//! (`review_store::maint::run_gc_pass`). It used to reach
//! `review_store::gc::apply` DIRECTLY, so a store-wide delete ran with no
//! pre-apply bundle, no restore-guard check, no DB-truth high-water check
//! and without recording `state_json.last_gc_apply`.
//!
//! Fixture/mock pattern copied from `rs_u7_retrack.rs` (its module doc
//! records why each e2e file in this crate keeps its own small helper
//! set): a LOCAL bare "forge" (never `github.com`) + a member clone the
//! daemon's `[[repos]]` points at, and `POST /api/repos/{name}/store/sync`
//! driving registration + seeding. The review-store location is read back
//! over the LOOPBACK store card, the same way an operator would.

use crate::common::{git, init_repo};
use axum::Router;
use kb_code_server::config::{
    GithubSection, KbCodeConfig, KbDaemonSection, RepoEntry, TranscriptsSection,
};
use kb_code_server::review_store::maint::restore_guard;
use kb_core::paths::KbPaths;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio::net::TcpListener;
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit(dir: &Path, file: &str, body: &str) -> String {
    std::fs::write(dir.join(file), body).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
    git_out(dir, &["rev-parse", "HEAD"])
}

async fn boot(cfg: KbCodeConfig) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"))
}

async fn mock_github_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, handle)
}

fn cfg_for(dir: &Path, gh_addr: SocketAddr) -> KbCodeConfig {
    KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.to_path_buf(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        transcripts: TranscriptsSection {
            enabled: false,
            ..TranscriptsSection::default()
        },
        ..KbCodeConfig::default()
    }
}

/// A bare local "forge" + a MEMBER clone the daemon's `[[repos]]` points
/// at — `rs_u7_retrack::fixture_repo`'s recipe (itself a copy of
/// `review_base::tests::fixture`, README §1's review-65 shape). The
/// member commits a `feature-a` branch so a review has a head to pin.
struct Repo {
    _tmp: tempfile::TempDir,
    clone: PathBuf,
}

fn fixture_repo() -> (Repo, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let forge = root.join("forge/widgets.git");
    std::fs::create_dir_all(&forge).unwrap();
    git(&forge, &["init", "-q", "--bare", "-b", "main"]);
    let author = root.join("author");
    std::fs::create_dir_all(&author).unwrap();
    init_repo(&author);
    git(&author, &["remote", "add", "origin", forge.to_str().unwrap()]);
    commit(&author, "m0.txt", "m0");
    let m1 = commit(&author, "m1.txt", "m1");
    git(&author, &["push", "-q", "origin", "main"]);
    let clone = root.join("work/widgets");
    git(
        &root,
        &[
            "clone",
            "-q",
            forge.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    (
        Repo {
            _tmp: tmp,
            clone: clone.clone(),
        },
        m1,
    )
}

async fn sync_store(client: &reqwest::Client, base: &str, repo: &str) {
    let resp = client
        .post(format!("{base}/api/repos/{repo}/store/sync"))
        .json(&serde_json::Value::Null)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
}

async fn get_review(client: &reqwest::Client, base: &str, id: i64) -> serde_json::Value {
    client
        .get(format!("{base}/api/reviews/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// `POST /api/reviews/{repo}/store` — the loopback store card, the same
/// read an operator uses to find the store's own git dir.
async fn store_card(client: &reqwest::Client, base: &str) -> serde_json::Value {
    let resp = client
        .get(format!("{base}/api/repos/fixture/store"))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert_eq!(status, 200, "{text}");
    serde_json::from_str(&text).unwrap()
}

async fn store_git_dir(client: &reqwest::Client, base: &str) -> String {
    let card = store_card(client, base).await;
    assert_eq!(card["store"]["state"], "ready", "{card}");
    card["store"]["git_dir"]
        .as_str()
        .expect("a store git_dir")
        .to_string()
}

/// `POST /api/reviews/refs/gc?repo=fixture[&dry_run=0]`. `dry_run`
/// defaults ON (the route's own `dry_run_default_on`).
async fn refs_gc(client: &reqwest::Client, base: &str, apply: bool) -> serde_json::Value {
    let query: &[(&str, &str)] = if apply {
        &[("repo", "fixture"), ("dry_run", "0")]
    } else {
        &[("repo", "fixture")]
    };
    let resp = client
        .post(format!("{base}/api/reviews/refs/gc"))
        .query(query)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert_eq!(status, 200, "{text}");
    serde_json::from_str(&text).unwrap()
}

fn ref_exists(store_dir: &Path, refname: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(store_dir)
        .args(["show-ref", "--verify", "--quiet", refname])
        .output()
        .unwrap()
        .status
        .success()
}

/// Every `<bundle>.refs` manifest under `root` (the daemon's whole state
/// dir), so a test can assert WHICH bundle covered a deleted ref without
/// assuming a timestamped bundle filename.
fn refs_manifests_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "refs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// A `ready` store holding ONE bound review (its `refs/kbc/review/<id>/ps1`
/// is a live ref every GC pass must keep), plus the sha the store's own
/// patchset ref points at — the only object this file ever plants
/// candidates at, so the pre-apply bundle is guaranteed to be non-empty
/// even though the candidate itself is not in `refs/kbc/*`.
struct Ready {
    _tmp: tempfile::TempDir,
    base: String,
    client: reqwest::Client,
    store_dir: PathBuf,
    review_id: i64,
    tip: String,
    _repo: Repo,
}

async fn ready_store_with_one_review() -> Ready {
    let (repo, _m1) = fixture_repo();
    let gh_router = Router::new();
    let (gh_addr, _gh) = mock_github_server(gh_router).await;
    let (tmp, base) = boot(cfg_for(&repo.clone, gh_addr)).await;
    let client = reqwest::Client::new();
    sync_store(&client, &base, "fixture").await;

    // One ordinary review on a branch — its ps1 ref is captured INTO the
    // store (so the store is not empty and carries a BOUND ref the
    // passes below must leave alone).
    git(&repo.clone, &["checkout", "-q", "-b", "feature-a"]);
    let tip = commit(&repo.clone, "a.txt", "a\n");
    git(&repo.clone, &["checkout", "-q", "main"]);
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "fixture",
            "head_ref": "feature-a",
            "base_ref": "main",
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert_eq!(status, 201, "{text}");
    let review_id = serde_json::from_str::<serde_json::Value>(&text).unwrap()["id"]
        .as_i64()
        .unwrap();
    let captured = get_review(&client, &base, review_id).await;
    assert_eq!(
        captured["patchsets"][0]["tip_sha_full"], tip,
        "fixture sanity: the review pinned the branch tip"
    );

    let store_dir = PathBuf::from(store_git_dir(&client, &base).await);
    assert!(
        ref_exists(&store_dir, &format!("refs/kbc/review/{review_id}/ps1")),
        "fixture sanity: the store carries the review's own ps1 ref"
    );
    Ready {
        _tmp: tmp,
        base,
        client,
        store_dir,
        review_id,
        tip,
        _repo: repo,
    }
}

/// The route's `--apply` must be refused while the store's restore guard
/// is flagged, and the refusal must be REPORTED (`applied: false` plus a
/// reason) instead of reported as a clean success — while the candidate
/// ref stays exactly where it was.
///
/// Pins the `guard_blocked` arm of `maint::apply_gc_candidates` being
/// reachable from `reviews::gc_review_refs_inner` at all: before the fix
/// the route called `gc::apply` directly, so the guard never ran, the
/// response said `applied: true`, and the ref was gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_refs_gc_route_refuses_a_restore_flagged_store_wide_apply() {
    let _g = SERIAL.lock().await;
    let e = ready_store_with_one_review().await;

    // A DE-REGISTERED member's mirror ref: `gc::attribute` marks it
    // `kind: "work"`, `status: "orphan"` (its repo id is not in the
    // store's DB-truth member list) and `delete_candidates` filters on
    // `status` alone — so it is a real delete candidate while living
    // entirely OUTSIDE `refs/kbc/*`.
    let work_ref = "refs/remotes/work-99999/main";
    git(&e.store_dir, &["update-ref", work_ref, &e.tip]);
    assert!(ref_exists(&e.store_dir, work_ref), "planted");

    // A dry run first: the CLI's response contract is pinned, and the
    // candidate is reported WITHOUT being applied.
    let dry = refs_gc(&e.client, &e.base, false).await;
    for key in ["deleted", "deleted_count", "applied", "reason", "detail"] {
        assert!(dry.get(key).is_some(), "dry-run shape lost `{key}`: {dry}");
    }
    assert_eq!(dry["dry_run"], true, "{dry}");
    assert_eq!(dry["applied"], false, "{dry}");
    assert_eq!(dry["reason"], "dry-run", "{dry}");
    assert_eq!(dry["deleted_count"], 1, "{dry}");
    assert!(
        dry["deleted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == work_ref),
        "{dry}"
    );
    assert!(
        ref_exists(&e.store_dir, work_ref),
        "the dry run deleted nothing"
    );

    // The guard goes off (as if an epoch regressed on this boot).
    let guard_path = restore_guard::path_for(&KbPaths::rooted_at(e._tmp.path(), "kb-code").state);
    restore_guard::flag_manual(&guard_path, "test", 1).unwrap();

    let applied = refs_gc(&e.client, &e.base, true).await;
    assert_eq!(applied["applied"], false, "{applied}");
    assert_eq!(applied["reason"], "restore-guard", "{applied}");
    assert!(
        applied.get("detail").is_some(),
        "the response contract carries a `detail` key even when empty: {applied}"
    );
    // A refusal still reports WHAT it refused — the candidate list is
    // captured before any decision is acted on.
    assert!(
        applied["deleted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == work_ref),
        "the refusal must name the candidate it did not delete: {applied}"
    );
    assert!(
        ref_exists(&e.store_dir, work_ref),
        "a guard-blocked pass must leave every ref in place"
    );
    // The live review's own ref is untouched too.
    assert!(ref_exists(
        &e.store_dir,
        &format!("refs/kbc/review/{}/ps1", e.review_id)
    ));
    // And nothing was recorded as an apply.
    let card = store_card(&e.client, &e.base).await;
    assert!(
        card["store"]["state_json"]["last_gc_apply"].is_null(),
        "a refused pass must never record last_gc_apply: {card}"
    );
    assert!(
        card["store"]["state_json"]["last_gc_dry_run"].is_object(),
        "the refusal is recorded as a dry run: {card}"
    );
}

/// The UNCONDITIONAL, never-`--yes`-bypassable DB-truth high-water check
/// must run on this route too: a delete candidate naming a review id above
/// `reviews_high_water_id()` can only have been minted by a database this
/// volume was rolled BEHIND, so the WHOLE apply is refused — the
/// legitimate candidates alongside it included.
///
/// Pins `maint::restore_suspected_review_id` being consulted from the
/// route's pass; before the fix this route never ran it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_refs_gc_route_refuses_a_candidate_above_the_high_water_mark() {
    let _g = SERIAL.lock().await;
    let e = ready_store_with_one_review().await;
    let impossible = e.review_id + 1000;
    let impossible_ref = format!("refs/kbc/review/{impossible}/ps1");
    // A perfectly legitimate orphan alongside it — the refusal is
    // whole-apply, not a skip of just the impossible one.
    let orphan_ref = "refs/kbc/pr/4242";
    git(&e.store_dir, &["update-ref", &impossible_ref, &e.tip]);
    git(&e.store_dir, &["update-ref", orphan_ref, &e.tip]);

    let applied = refs_gc(&e.client, &e.base, true).await;
    assert_eq!(applied["applied"], false, "{applied}");
    assert_eq!(applied["reason"], "restore-suspected", "{applied}");
    assert!(
        applied["detail"]
            .as_str()
            .is_some_and(|d| d.contains(&impossible.to_string())),
        "the detail must name the impossible review id: {applied}"
    );
    assert_eq!(applied["deleted_count"], 2, "both refs were candidates: {applied}");
    // Nothing at all was deleted — not even the legitimate orphan.
    assert!(ref_exists(&e.store_dir, &impossible_ref), "{applied}");
    assert!(ref_exists(&e.store_dir, orphan_ref), "{applied}");
}

/// The apply half of the same route, when no guard blocks it: it must now
/// run through `maint::run_gc_pass`, so (a) a pre-apply bundle exists and
/// COVERS the ref the apply deleted, and (b) `state_json.last_gc_apply`
/// is recorded so the monthly cruft cooldown is judged from a truth.
///
/// Pins the `store gc` funnel itself: the route reaching `gc::apply` at
/// all now means going through `apply_gc_candidates`. Before the fix the
/// ref was deleted with no bundle on disk and no timestamp written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_refs_gc_route_applies_only_through_a_bundle_covering_pass() {
    let _g = SERIAL.lock().await;
    let e = ready_store_with_one_review().await;
    let work_ref = "refs/remotes/work-99999/main";
    git(&e.store_dir, &["update-ref", work_ref, &e.tip]);

    let applied = refs_gc(&e.client, &e.base, true).await;
    assert_eq!(applied["applied"], true, "{applied}");
    assert_eq!(applied["reason"], "applied", "{applied}");
    assert_eq!(applied["deleted_count"], 1, "{applied}");
    assert!(
        !ref_exists(&e.store_dir, work_ref),
        "the orphan really was deleted: {applied}"
    );
    assert!(ref_exists(
        &e.store_dir,
        &format!("refs/kbc/review/{}/ps1", e.review_id)
    ));

    // A pre-apply bundle exists and covers the deleted ref. The ref lives
    // OUTSIDE `refs/kbc/*`, so no routine `refs/kbc/*`-only backup could
    // have covered it — this is the route taking the pre-apply bundle at
    // all, which is what the fix is.
    let manifests = refs_manifests_under(e._tmp.path());
    let covering: Vec<&PathBuf> = manifests
        .iter()
        .filter(|m| {
            std::fs::read_to_string(m)
                .map(|t| t.contains(work_ref))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(
        covering.len(),
        1,
        "exactly one pre-apply bundle must cover {work_ref}; manifests: {manifests:?}"
    );
    let bundle = covering[0].with_extension("");
    assert!(
        bundle.is_file(),
        "the covering manifest {covering:?} has no bundle beside it"
    );

    // And the pass recorded what it did, under the store's ops lock.
    let card = store_card(&e.client, &e.base).await;
    let last = &card["store"]["state_json"]["last_gc_apply"];
    assert!(last.is_object(), "last_gc_apply was never recorded: {card}");
    assert_eq!(last["reason"], "applied", "{card}");
    assert_eq!(last["candidates"], 1, "{card}");
    assert!(last["at"].is_i64(), "{card}");
}
