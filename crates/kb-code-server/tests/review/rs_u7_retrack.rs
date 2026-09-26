//! RS-U7 — end-to-end HTTP tests for `review retrack` (single + bulk),
//! README D17/D20. Fixture/mock pattern duplicated from `review_sweep.rs`'s
//! own (`boot`, `cfg_for`, `mock_github_server`) — see that file's module
//! doc for why each e2e file in this crate keeps its own small helper set.
//!
//! The "forge" here is a LOCAL bare repo (never `github.com`), exactly like
//! `review_base::tests`'s `Fx` fixture (RS-U6's own review-65-shaped unit
//! test): `store sync` seeds a `ready` store with NO credentials at all
//! (`Forge::Local`), and the default-branch rung resolves over
//! `ls-remote --symref` against the local path — real git protocol, no
//! GitHub API needed. This lets an explicit `--base main` retrack reproduce
//! review 65 (README §1) exactly, HTTP end to end.

use crate::common::{git, init_repo};
use axum::Router;
use kb_code_server::config::{
    GithubSection, KbCodeConfig, KbDaemonSection, RepoEntry, TranscriptsSection,
};
use kb_core::paths::KbPaths;
use std::net::SocketAddr;
use std::path::Path;
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

/// A bare "forge" (local path, never `github.com`) + an `author` clone (used
/// to push forge-side history without touching the member clone) + the
/// MEMBER clone the daemon's `[[repos]]` points at. Mirrors
/// `review_base::tests::fixture`'s exact recipe (RS-U6's review-65 unit
/// test), but the daemon (not direct `Store`/`ReviewStores` calls) drives
/// registration + seeding here, over `POST /api/repos/{name}/store/sync`.
struct Repo {
    _tmp: tempfile::TempDir,
    /// Kept for the fixture's own doc/shape (every push in these tests
    /// goes through `author`'s `origin` remote, which already points
    /// here) — no test reads the path back directly.
    _forge: std::path::PathBuf,
    author: std::path::PathBuf,
    clone: std::path::PathBuf,
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
    git(
        &author,
        &["remote", "add", "origin", forge.to_str().unwrap()],
    );
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
    // `git clone` doesn't inherit `author`'s repo-local identity — the
    // legacy-refs test commits directly in the clone.
    git(&clone, &["config", "user.email", "test@example.com"]);
    git(&clone, &["config", "user.name", "Test"]);
    (
        Repo {
            _tmp: tmp,
            _forge: forge,
            author,
            clone,
        },
        m1,
    )
}

impl Repo {
    /// Author a PR branch off `from` with `files`, force-push it as the
    /// forge's `refs/pull/<n>/head`. Returns the tip.
    fn push_pr(&self, from: &str, n: u32, files: &[&str], tag: &str) -> String {
        git(&self.author, &["checkout", "-q", "-B", "pr", from]);
        let mut tip = String::new();
        for f in files {
            tip = commit(&self.author, f, &format!("{f} {tag}"));
        }
        git(
            &self.author,
            &[
                "push",
                "-q",
                "-f",
                "origin",
                &format!("HEAD:refs/pull/{n}/head"),
            ],
        );
        git(&self.author, &["checkout", "-q", "main"]);
        tip
    }

    /// A commit that never merges into `main` — a branch off the very
    /// first forge commit, pushed to the forge as `sidetrack` (never
    /// `main`) — used to build a "custom" pin: a sha NOT an ancestor of
    /// `main`. Also fetched into the MEMBER clone's object database (a
    /// plain `git fetch`, no local ref/branch created) so the create
    /// route's own `--base <sha>` resolution — which reads the member
    /// clone first (`StoreProbe::resolve_rev`) — can find the object; a
    /// review-creation caller normally reaches a "custom" pin because IT
    /// was created against a rev the clone already had, not because kb-code
    /// fetched it on the caller's behalf.
    fn unrelated_commit(&self) -> String {
        let root = git_out(&self.author, &["rev-list", "--max-parents=0", "main"]);
        git(&self.author, &["checkout", "-q", &root]);
        let sha = commit(&self.author, "sidetrack.txt", "sidetrack");
        git(
            &self.author,
            &[
                "push",
                "-q",
                "origin",
                &format!("{sha}:refs/heads/sidetrack"),
            ],
        );
        git(&self.author, &["checkout", "-q", "main"]);
        git(&self.clone, &["fetch", "-q", "origin", "sidetrack"]);
        sha
    }

    fn refs_snapshot(&self) -> String {
        git_out(
            &self.clone,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
    }
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

async fn create_pr_review(
    client: &reqwest::Client,
    base: &str,
    repo: &str,
    pr_number: u32,
    base_ref: Option<&str>,
) -> i64 {
    let mut body = serde_json::json!({ "repo": repo, "pr_number": pr_number });
    if let Some(b) = base_ref {
        body["base_ref"] = serde_json::json!(b);
    }
    let resp = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert_eq!(status, 201, "{text}");
    serde_json::from_str::<serde_json::Value>(&text).unwrap()["id"]
        .as_i64()
        .unwrap()
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

async fn retrack(
    client: &reqwest::Client,
    base: &str,
    id: i64,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{base}/api/reviews/{id}/retrack"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    let json = if text.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"error": text}))
    };
    (status, json)
}

async fn retrack_all(
    client: &reqwest::Client,
    base: &str,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{base}/api/reviews/retrack-bulk"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.unwrap();
    (status, json)
}

async fn set_verdict(client: &reqwest::Client, base: &str, id: i64, state: &str) {
    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .json(&serde_json::json!({ "state": state }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
}

// --- tests -------------------------------------------------------------------

/// README §1 review-65 shape, HTTP end to end (D17/D20). The review is
/// pinned one commit TOO FAR BACK (`M0`, the forge's very first commit)
/// instead of the PR's real fork point (`M1`) — the same shape a stale
/// `--base <sha>` produces: `merge-base(pin, tip)` (`M0`) drags M1 into the
/// diff too, one commit more than GitHub shows. `retrack --dry-run`
/// reports `stale-pin`; `retrack` mints `kind=base-corrected` with EXACTLY
/// the PR's own 3 commits/files (GitHub-equal) at the SAME tip (no rebase
/// happened — only the base moved); a published verdict stays on the OLD
/// patchset with `verdict_stale=true` AND the new, additive
/// `verdict_scope_changed=true` (same tip, D20); a second retrack with the
/// SAME `--base` is `equivalent` and mints nothing; the user clone's refs
/// never move (invariance, item 6).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrack_review_65_shape_stale_pin_to_base_corrected_and_then_equivalent() {
    let _guard = SERIAL.lock().await;
    let (repo, m1) = fixture_repo();
    let m0 = git_out(&repo.author, &["rev-list", "--max-parents=0", "main"]);
    let gh_router = Router::new();
    let (gh_addr, _gh) = mock_github_server(gh_router).await;
    let (_tmp, base) = boot(cfg_for(&repo.clone, gh_addr)).await;
    let client = reqwest::Client::new();

    sync_store(&client, &base, "fixture").await;

    let pr_files = ["p1.rs", "p2.rs", "p3.rs"];
    let tip0 = repo.push_pr(&m1, 7, &pr_files, "v1");
    // A legacy-shaped review: pinned at M0 — one commit too far back (the
    // morning skill's `--base <sha>`, computed once and never refreshed).
    let id = create_pr_review(&client, &base, "fixture", 7, Some(&m0)).await;
    let before = repo.refs_snapshot();

    let body0 = get_review(&client, &base, id).await;
    assert_eq!(body0["patchsets"][0]["tip_sha_full"], tip0);
    assert_eq!(
        body0["patchsets"][0]["base_sha_full"], m0,
        "the pin drags M1 into the diff too — one commit more than GitHub"
    );
    assert_eq!(body0["patchsets"][0]["commit_count"], 4, "{body0}");

    set_verdict(&client, &base, id, "approve").await;
    let verdict_ps = get_review(&client, &base, id).await["verdict"]["ps"]
        .as_i64()
        .unwrap();
    assert_eq!(verdict_ps, 1);

    // Dry run: classify BEFORE touching anything. No PR activity, no main
    // advance — retrack alone must recognize the pin is one commit stale.
    let (status, dry) = retrack(
        &client,
        &base,
        id,
        serde_json::json!({ "base": "main", "dry_run": true }),
    )
    .await;
    assert_eq!(status, 200, "{dry}");
    assert_eq!(dry["class"], "stale-pin", "{dry}");
    assert_eq!(dry["minted"], false, "{dry}");
    assert!(dry.get("id").is_some(), "{dry}");
    assert!(dry.get("base").is_some(), "{dry}");
    assert!(dry.get("warnings").is_some(), "{dry}");
    // A dry run must not have written anything.
    assert_eq!(repo.refs_snapshot(), before);

    // Apply.
    let (status, applied) = retrack(
        &client,
        &base,
        id,
        serde_json::json!({ "base": "main", "dry_run": false }),
    )
    .await;
    assert_eq!(status, 200, "{applied}");
    assert_eq!(applied["class"], "stale-pin", "{applied}");
    assert_eq!(applied["minted"], true, "{applied}");
    assert_eq!(applied["kind"], "base-corrected", "{applied}");
    assert_eq!(applied["base"]["mode"], "track", "{applied}");
    assert_eq!(applied["base"]["branch"], "main", "{applied}");

    // GitHub-equal: exactly the PR's own 3 commits / 3 files — M0 is gone;
    // the tip is UNCHANGED (nobody rebased — only the base moved).
    let show = get_review(&client, &base, id).await;
    let ps = show["patchsets"].as_array().unwrap().last().unwrap();
    assert_eq!(ps["tip_sha_full"], tip0, "no rebase happened — same tip");
    assert_eq!(ps["base_sha_full"], m1);
    assert_eq!(ps["commit_count"], 3, "{ps}");
    let files: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/files"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut names: Vec<String> = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec!["p1.rs", "p2.rs", "p3.rs"]);

    // D20 — findings/verdict stayed on the OLD patchset (still ps1);
    // verdict_stale AND the new, additive verdict_scope_changed (same
    // tip, only the base moved).
    assert_eq!(show["verdict"]["ps"], 1, "{show}");
    assert_eq!(show["verdict_stale"], true, "{show}");
    assert_eq!(show["verdict_scope_changed"], true, "{show}");

    // A second retrack with the SAME --base: equivalent, mints nothing.
    let (status, again) = retrack(
        &client,
        &base,
        id,
        serde_json::json!({ "base": "main", "dry_run": false }),
    )
    .await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["class"], "equivalent", "{again}");
    assert_eq!(again["minted"], false, "{again}");

    // The user clone's refs never moved across create/retrack ×2.
    assert_eq!(repo.refs_snapshot(), before);
}

/// A pin OUTSIDE the target branch's history is `custom` — never conflated
/// with `stale-pin`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrack_a_pin_not_on_the_target_branch_is_custom() {
    let _guard = SERIAL.lock().await;
    let (repo, m1) = fixture_repo();
    let gh_router = Router::new();
    let (gh_addr, _gh) = mock_github_server(gh_router).await;
    let (_tmp, base) = boot(cfg_for(&repo.clone, gh_addr)).await;
    let client = reqwest::Client::new();
    sync_store(&client, &base, "fixture").await;

    let unrelated = repo.unrelated_commit();
    repo.push_pr(&m1, 8, &["p1.rs"], "v1");
    let id = create_pr_review(&client, &base, "fixture", 8, Some(&unrelated)).await;

    let (status, dry) = retrack(
        &client,
        &base,
        id,
        serde_json::json!({ "base": "main", "dry_run": true }),
    )
    .await;
    assert_eq!(status, 200, "{dry}");
    assert_eq!(dry["class"], "custom", "{dry}");
}

/// `retrack --all --pinned`: two pinned reviews, one `stale-pin` and one
/// `custom` — dry-run lists both classes; `--yes` applies ONLY the
/// stale-pin row, leaving the custom one untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrack_all_pinned_applies_only_stale_pin() {
    let _guard = SERIAL.lock().await;
    let (repo, m1) = fixture_repo();
    let m0 = git_out(&repo.author, &["rev-list", "--max-parents=0", "main"]);
    let gh_router = Router::new();
    let (gh_addr, _gh) = mock_github_server(gh_router).await;
    let (_tmp, base) = boot(cfg_for(&repo.clone, gh_addr)).await;
    let client = reqwest::Client::new();
    sync_store(&client, &base, "fixture").await;

    // Pinned one commit too far back (M0 instead of the real fork point
    // M1) — the review-65 shape: `merge-base(pin, tip)` still resolves,
    // it's just wrong, so `stale-pin` is the correct class (never
    // `equivalent` — a pin EQUAL to the real merge-base is what makes a
    // row `equivalent`, and M1 itself would be exactly that).
    repo.push_pr(&m1, 9, &["a.rs"], "v1");
    let stale_id = create_pr_review(&client, &base, "fixture", 9, Some(&m0)).await;

    let unrelated = repo.unrelated_commit();
    repo.push_pr(&m1, 10, &["b.rs"], "v1");
    let custom_id = create_pr_review(&client, &base, "fixture", 10, Some(&unrelated)).await;

    let before_custom = get_review(&client, &base, custom_id).await;

    let (status, dry) = retrack_all(
        &client,
        &base,
        serde_json::json!({ "repo": "fixture", "pinned": true, "dry_run": true }),
    )
    .await;
    assert_eq!(status, 200, "{dry}");
    let rows = dry["rows"].as_array().unwrap();
    let row = |id: i64| rows.iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(row(stale_id)["class"], "stale-pin", "{dry}");
    assert_eq!(row(custom_id)["class"], "custom", "{dry}");
    // Dry run: nothing minted, nothing applied.
    assert!(rows.iter().all(|r| r["minted"] == false), "{dry}");

    let (status, applied) = retrack_all(
        &client,
        &base,
        serde_json::json!({ "repo": "fixture", "pinned": true, "dry_run": false }),
    )
    .await;
    assert_eq!(status, 200, "{applied}");
    let rows = applied["rows"].as_array().unwrap();
    let stale_row = rows.iter().find(|r| r["id"] == stale_id).unwrap();
    assert_eq!(stale_row["minted"], true, "{applied}");

    // The custom row was NEVER applied by --all --yes.
    let after_custom = get_review(&client, &base, custom_id).await;
    assert_eq!(
        after_custom["patchsets"], before_custom["patchsets"],
        "a custom pin must never be auto-applied by --all --yes"
    );
    assert_eq!(after_custom["base_ref"], unrelated);
}

// --- D19: `store legacy-refs` / `store export-legacy` -----------------------

async fn create_review(
    client: &reqwest::Client,
    base: &str,
    repo: &str,
    head_ref: &str,
    base_ref: &str,
) -> (i64, String) {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": repo,
            "head_ref": head_ref,
            "base_ref": base_ref,
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert_eq!(status, 201, "{text}");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    let id = body["id"].as_i64().unwrap();
    let show = get_review(client, base, id).await;
    let tip = show["patchsets"][0]["tip_sha_full"]
        .as_str()
        .unwrap()
        .to_string();
    (id, tip)
}

fn update_ref(dir: &Path, refname: &str, sha: &str) {
    git(dir, &["update-ref", refname, sha]);
}

fn ref_sha(dir: &Path, refname: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--verify", "-q", refname])
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap().trim().to_string())
}

async fn legacy_refs(
    client: &reqwest::Client,
    base: &str,
    repo: &str,
    yes: bool,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{base}/api/repos/{repo}/store/legacy-refs"))
        .json(&serde_json::json!({ "dry_run": !yes }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.unwrap();
    (status, json)
}

async fn export_legacy(
    client: &reqwest::Client,
    base: &str,
    repo: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{base}/api/repos/{repo}/store/export-legacy"))
        .json(&serde_json::Value::Null)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.unwrap();
    (status, json)
}

/// D19 round trip: a user clone with TWO "legacy" review refs left over
/// from before the store existed. `refs/kbc/review/<id1>/ps1` matches the
/// store's OWN copy (same sha, `X`) exactly — `deletable`. `refs/kbc/
/// review/<id2>/ps1` was hand-edited/stale in the clone (`Y`) and differs
/// from what the store holds (`Z`) — `kept(sha-differs)`, item 6:
/// user-clone invariance for the KEPT ref. `--yes` deletes ONLY the first,
/// in one transaction; `export-legacy` then restores it CREATE-ONLY —
/// review 2's clone ref is left untouched (it already exists, wrong sha
/// and all) and is reported, never silently skipped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_refs_round_trip_deletes_only_the_matching_ref_and_export_restores_create_only() {
    let _guard = SERIAL.lock().await;
    let (repo, m1) = fixture_repo();
    let gh_router = Router::new();
    let (gh_addr, _gh) = mock_github_server(gh_router).await;
    let (_tmp, base) = boot(cfg_for(&repo.clone, gh_addr)).await;
    let client = reqwest::Client::new();
    sync_store(&client, &base, "fixture").await;

    // Two ordinary (non-PR) reviews, both captured to ps1 in the STORE.
    git(&repo.clone, &["checkout", "-q", "-b", "feature-a"]);
    std::fs::write(repo.clone.join("a.txt"), "a\n").unwrap();
    git(&repo.clone, &["add", "-A"]);
    git(&repo.clone, &["commit", "-q", "-m", "a"]);
    git(&repo.clone, &["checkout", "-q", "main"]);
    let (id1, tip1) = create_review(&client, &base, "fixture", "feature-a", "main").await;

    git(&repo.clone, &["checkout", "-q", "-b", "feature-b"]);
    std::fs::write(repo.clone.join("b.txt"), "b\n").unwrap();
    git(&repo.clone, &["add", "-A"]);
    git(&repo.clone, &["commit", "-q", "-m", "b"]);
    git(&repo.clone, &["checkout", "-q", "main"]);
    let (id2, tip2) = create_review(&client, &base, "fixture", "feature-b", "main").await;

    // Leftover legacy refs in the user clone: review 1's matches the
    // store exactly (X); review 2's is stale — the clone still has the
    // OLD sha (Y = m1) while the store's own copy is the current tip (Z
    // = tip2).
    let ref1 = format!("refs/kbc/review/{id1}/ps1");
    let ref2 = format!("refs/kbc/review/{id2}/ps1");
    update_ref(&repo.clone, &ref1, &tip1);
    update_ref(&repo.clone, &ref2, &m1);
    assert_ne!(
        m1, tip2,
        "the fixture's stale value must differ from the store's"
    );

    let before = repo.refs_snapshot();

    let (status, dry) = legacy_refs(&client, &base, "fixture", false).await;
    assert_eq!(status, 200, "{dry}");
    assert_eq!(dry["deletable"], 1, "{dry}");
    assert_eq!(dry["kept"], 1, "{dry}");
    let refs = dry["refs"].as_array().unwrap();
    let row = |r: &str| refs.iter().find(|x| x["ref"] == r).unwrap();
    assert_eq!(row(&ref1)["deletable"], true, "{dry}");
    assert_eq!(row(&ref2)["deletable"], false, "{dry}");
    assert_eq!(row(&ref2)["reason"], "sha-differs", "{dry}");
    // A dry run wrote nothing.
    assert_eq!(repo.refs_snapshot(), before);

    let (status, applied) = legacy_refs(&client, &base, "fixture", true).await;
    assert_eq!(status, 200, "{applied}");
    assert_eq!(applied["deleted"], 1, "{applied}");
    assert_eq!(
        applied["legacy_refs_state"],
        serde_json::Value::Null,
        "kept row remains, never `cleaned`: {applied}"
    );

    // Only review 1's ref was removed; review 2's (still wrong) survives
    // untouched — invariance for the KEPT ref.
    assert_eq!(
        ref_sha(&repo.clone, &ref1),
        None,
        "review 1's ref must be gone"
    );
    assert_eq!(
        ref_sha(&repo.clone, &ref2).as_deref(),
        Some(m1.as_str()),
        "review 2's ref must be untouched, wrong sha and all"
    );

    // export-legacy restores review 1's ref CREATE-ONLY; review 2's
    // (already present, wrong sha) is left alone and reported, not
    // silently skipped.
    let (status, exported) = export_legacy(&client, &base, "fixture").await;
    assert_eq!(status, 200, "{exported}");
    assert_eq!(exported["candidates"], 2, "{exported}");
    assert_eq!(exported["created"], 1, "{exported}");
    let created: Vec<&str> = exported["refs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(created, vec![ref1.as_str()], "{exported}");
    assert_eq!(
        ref_sha(&repo.clone, &ref1).as_deref(),
        Some(tip1.as_str()),
        "export-legacy restored review 1's ref at the store's own sha"
    );
    assert_eq!(
        ref_sha(&repo.clone, &ref2).as_deref(),
        Some(m1.as_str()),
        "review 2's stale ref was never overwritten — create-only"
    );

    // A second dry run: review 1's restored ref matches the store again
    // (export-legacy recreated it at the store's own sha) — back to
    // `deletable`; review 2 still `kept(sha-differs)`, unchanged.
    let (status, dry2) = legacy_refs(&client, &base, "fixture", false).await;
    assert_eq!(status, 200, "{dry2}");
    assert_eq!(dry2["deletable"], 1, "{dry2}");
    assert_eq!(dry2["kept"], 1, "{dry2}");
}
