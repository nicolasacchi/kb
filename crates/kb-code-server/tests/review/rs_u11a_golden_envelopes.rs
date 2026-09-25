//! RS-U11a — golden envelope SHAPE tests, written BEFORE the base-model
//! change (README.md §12 / BUILD-BRIEF.md unit U11) so that change can
//! prove itself additive-only: `base{mode, remote?, branch?, ref, set_by,
//! source, state, code?, hint?, last_fetch, fetched_at?, tip_sha?,
//! merge_base_sha?, behind?, display}`, `warnings[]`, `minted`, and
//! per-patchset `kind`/`base_tip_sha` are all NEW keys U11 adds to the
//! review-facing envelopes this file pins today's shape of. `base_ref` is
//! explicitly KEPT per the design doc, so nothing here expects it to move.
//!
//! # What's pinned, and why these routes
//!
//! Every route BUILD-BRIEF.md's U11 row and design-general.md's own
//! "Before the ad hoc `json!` envelopes … move to typed structs, add
//! golden JSON snapshot tests of the current shapes" note name, located in
//! `reviews.rs`/`review_jobs.rs`/`review_sweep.rs`/`routes.rs` (verified at
//! this checkout — the design doc's own `reviews.rs` line numbers had
//! drifted, so they're not repeated here):
//!
//! - `POST /api/reviews` — [`create_review`], `reviews.rs`
//! - `POST /api/reviews/pr` sync — [`create_review_pr_sync`], `reviews.rs`
//! - `POST /api/reviews/pr?async=1` job result — `review_jobs.rs`
//! - `GET /api/reviews` (list) — `reviews.rs::list_reviews`
//! - `GET /api/reviews/{id}` (show, incl. `patchsets[]`) — `reviews.rs::get_review`
//! - `POST /api/reviews/{id}/snapshot` — `reviews.rs::snapshot_review`
//! - `GET /api/reviews/{id}/files` — `reviews.rs::review_files`
//! - `GET /api/reviews/{id}/pr-status` — `reviews.rs::pr_status_route`
//! - `POST /api/reviews/sweep` — `review_sweep.rs::sweep_route`
//! - `POST /api/prs/fetch` — `routes.rs::prs_fetch_route`
//!
//! # How the pin works
//!
//! [`crate::shape_util`] reduces a response to a wire SHAPE (every leaf
//! value replaced by its JSON type tag) and walks a committed shape
//! against a fresh one, requiring every committed key path to still
//! resolve at the same type — see that module's own doc for the full
//! contract (including why a `null` golden leaf is presence-only, and why
//! a brand-new key can never fail the check). The fixtures below are
//! chosen so every field that CAN be non-null in ordinary use IS non-null
//! here (a bound PR, a set verdict, an authored report, a populated
//! `pr_meta` with labels/checks/body) — a golden pinned entirely on
//! `null`s would only prove key PRESENCE, not type, for those fields.
//!
//! # Regenerating (this crate builds with NO local cargo — see
//! `BUILDER-RULES.md`; every golden here was minted on GitHub CI)
//!
//! ```text
//! RS_U11_UPDATE_GOLDEN=1 cargo test -p kb-code-server --profile fast \
//!   --test review -- rs_u11a --nocapture
//! git diff -- crates/kb-code-server/tests/fixtures/rs-u11a-envelopes/
//! ```
//! `RS_U11_UPDATE_GOLDEN=force` skips the additive-over-the-CURRENTLY-
//! COMMITTED-golden safety check `golden_check` otherwise runs first (the
//! "strict mode" this unit's brief asks for) — plain `=1` refuses to
//! silently regenerate OVER an actual regression; `=force` is the
//! deliberate override for a genuinely intended breaking change.
//!
//! On a missing-fixture run under `CI=1` (unset locally), each affected
//! test prints its computed shape between `===RS-U11A-GOLDEN-BEGIN:<name>===`
//! / `===RS-U11A-GOLDEN-END:<name>===` markers on stderr before failing —
//! `gh run view --log-failed` surfaces them (cargo shows captured output
//! for failed tests even without `--nocapture`); copy each block verbatim
//! into `tests/fixtures/rs-u11a-envelopes/<name>.json` (trailing newline)
//! and commit.

use crate::common::{git, init_repo};
use crate::shape_util::{assert_additive, shape_of};
use axum::routing::get;
use axum::{Json, Router};
use kb_code_server::config::{
    GithubSection, KbCodeConfig, KbDaemonSection, RepoEntry, TranscriptsSection,
};
use kb_core::paths::KbPaths;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio::net::TcpListener;

// --- shared helpers (duplicated per this crate's own e2e-file convention —
// see `review_routes.rs`'s module doc for why: no `tests/support` module
// exists yet in this crate) ---------------------------------------------

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

/// `[kb_daemon]` disabled — this file never needs the kb sibling.
fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: Some("http://127.0.0.1:0".to_string()),
        token_file: None,
        public_url: None,
    }
}

/// `[transcripts]` explicitly disabled — see the module doc's own note in
/// BUILDER-RULES: `TranscriptsSection::default()` is `enabled: true` +
/// `root: "~/.claude/projects"` (a fresh install indexes the OPERATOR's own
/// local Claude Code transcripts out of the box), which every test daemon
/// in this file must never do. Every `KbCodeConfig` built below sets this
/// explicitly rather than relying on `..KbCodeConfig::default()`, which
/// would silently restore the enabled default.
fn disabled_transcripts() -> TranscriptsSection {
    TranscriptsSection {
        enabled: false,
        ..TranscriptsSection::default()
    }
}

async fn boot(cfg: KbCodeConfig) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"))
}

fn cfg_with_repo(name: &str, path: &Path) -> KbCodeConfig {
    KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: disabled_kb_daemon(),
        transcripts: disabled_transcripts(),
        ..KbCodeConfig::default()
    }
}

fn cfg_with_repo_and_github(name: &str, path: &Path, gh_addr: SocketAddr) -> KbCodeConfig {
    KbCodeConfig {
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..cfg_with_repo(name, path)
    }
}

async fn mock_github_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, handle)
}

/// One bare "origin" carrying a PR ref pushed straight to
/// `refs/pull/<n>/head` (real GitHub never lets a client push there
/// directly; this reproduces the same shape locally, same technique
/// `review_routes.rs`'s own `fixture_pr_repo` uses) plus `origin` on the
/// working repo configured as a github.com URL with a `url.*.insteadOf`
/// rewrite pointing the actual fetch at the local bare repo (so
/// `github::github_repo` resolves `acme/widget` while `git fetch origin`
/// stays fully offline).
fn fixture_pr_repo(pr_number: u32) -> (tempfile::TempDir, tempfile::TempDir, PathBuf, String) {
    let bare_tmp = tempfile::tempdir().unwrap();
    let bare_dir = std::fs::canonicalize(bare_tmp.path())
        .unwrap()
        .join("origin.git");
    std::fs::create_dir_all(&bare_dir).unwrap();
    git(&bare_dir, &["init", "-q", "--bare", "-b", "main"]);

    let repo_tmp = tempfile::tempdir().unwrap();
    let repo_dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&repo_dir);
    std::fs::write(repo_dir.join("base.txt"), "base\n").unwrap();
    git(&repo_dir, &["add", "-A"]);
    git(&repo_dir, &["commit", "-q", "-m", "base"]);
    git(
        &repo_dir,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    );
    git(
        &repo_dir,
        &[
            "config",
            &format!("url.{}.insteadOf", bare_dir.to_str().unwrap()),
            "https://github.com/acme/widget.git",
        ],
    );

    git(&repo_dir, &["checkout", "-q", "-b", "pr-branch"]);
    std::fs::write(repo_dir.join("feature.txt"), "feature\n").unwrap();
    git(&repo_dir, &["add", "-A"]);
    git(&repo_dir, &["commit", "-q", "-m", "pr commit"]);
    let pr_sha = git_out(&repo_dir, &["rev-parse", "HEAD"]);
    git(
        &repo_dir,
        &[
            "push",
            "-q",
            bare_dir.to_str().unwrap(),
            &format!("HEAD:refs/pull/{pr_number}/head"),
        ],
    );
    git(&repo_dir, &["checkout", "-q", "main"]);
    git(&repo_dir, &["branch", "-D", "pr-branch"]);

    (repo_tmp, bare_tmp, repo_dir, pr_sha)
}

/// A full-detail mock `GET /repos/acme/widget/pulls/{n}` + its
/// `check-runs` route — every field [`create_review_pr`]'s enrichment,
/// [`sweep_route`]'s refresh, and [`pr_status_route`]'s live half read,
/// populated NON-null (see the module doc's "how the pin works" section
/// for why: a null-heavy fixture would only pin key presence, not type).
fn pulls_and_checks_router(pr_number: u32, pr_sha: &str) -> Router {
    let pulls_path = format!("/repos/acme/widget/pulls/{pr_number}");
    let checks_path = format!("/repos/acme/widget/commits/{pr_sha}/check-runs");
    let pr_sha_owned = pr_sha.to_string();
    Router::new()
        .route(
            &pulls_path,
            get(move || {
                let pr_sha = pr_sha_owned.clone();
                async move {
                    Json(serde_json::json!({
                        "number": pr_number,
                        "title": "widgets: spin faster",
                        "user": {"login": "octocat"},
                        "head": {"ref": "pr-branch", "sha": pr_sha},
                        "base": {"ref": "main"},
                        "updated_at": "2024-01-01T00:00:00Z",
                        "draft": false,
                        "state": "open",
                        "merged": false,
                        "labels": [{"name": "needs-review"}],
                        "mergeable_state": "clean",
                        "body": "Adds a faster spin path.",
                    }))
                }
            }),
        )
        .route(
            &checks_path,
            get(|| async {
                Json(serde_json::json!({
                    "check_runs": [{
                        "name": "build",
                        "status": "completed",
                        "conclusion": "success",
                        "started_at": "2024-01-01T00:00:00Z",
                        "completed_at": "2024-01-01T00:05:00Z",
                    }]
                }))
            }),
        )
}

/// Where this unit's committed shape fixtures live.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rs-u11a-envelopes")
}

/// Compare `actual`'s wire shape against the committed golden fixture
/// `name`, print-and-report on a genuinely missing fixture (CI markers —
/// see the module doc), or regenerate under `RS_U11_UPDATE_GOLDEN`. Never
/// panics itself — callers collect every `Err` across one test's several
/// golden checks and panic ONCE at the end (via [`finish`]), so a single
/// CI run surfaces every missing/mismatched shape at once rather than
/// stopping at the first.
fn golden_check(name: &str, actual: &serde_json::Value) -> Result<(), String> {
    let shape = shape_of(actual);
    let pretty = serde_json::to_string_pretty(&shape).expect("shape serializes");
    let path = fixtures_dir().join(format!("{name}.json"));

    if let Some(mode) = std::env::var_os("RS_U11_UPDATE_GOLDEN") {
        let force = mode == "force";
        if !force {
            if let Ok(old_text) = std::fs::read_to_string(&path) {
                if let Ok(old) = serde_json::from_str::<serde_json::Value>(&old_text) {
                    let errs = assert_additive(&old, actual, "");
                    if !errs.is_empty() {
                        return Err(format!(
                            "refusing to regenerate {name:?}: the fresh response is NOT \
                             additive over the CURRENTLY COMMITTED golden ({} problem(s)):\n{}\n\
                             If this break is genuinely intended, rerun with \
                             RS_U11_UPDATE_GOLDEN=force.",
                            errs.len(),
                            errs.join("\n"),
                        ));
                    }
                }
            }
        }
        std::fs::create_dir_all(fixtures_dir()).expect("mkdir golden fixtures dir");
        std::fs::write(&path, format!("{pretty}\n")).expect("write golden fixture");
        eprintln!("RS_U11_UPDATE_GOLDEN: wrote {}", path.display());
        return Ok(());
    }

    let golden_text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            let ci_note = if std::env::var_os("CI").is_some() {
                eprintln!("===RS-U11A-GOLDEN-MISSING:{name}===");
                eprintln!("===RS-U11A-GOLDEN-BEGIN:{name}===");
                eprintln!("{pretty}");
                eprintln!("===RS-U11A-GOLDEN-END:{name}===");
                format!(
                    "This CI run printed the computed shape between the \
                     RS-U11A-GOLDEN-BEGIN/END:{name} markers above (see `gh run view \
                     --log-failed`) — copy that JSON verbatim into {} (with a trailing \
                     newline) and commit it.\n",
                    path.display()
                )
            } else {
                String::new()
            };
            return Err(format!(
                "missing golden fixture {}.\n{ci_note}Regenerate with:\n  \
                 RS_U11_UPDATE_GOLDEN=1 cargo test -p kb-code-server --profile fast \
                 --test review -- rs_u11a --nocapture",
                path.display(),
            ));
        }
    };
    let golden: serde_json::Value =
        serde_json::from_str(&golden_text).map_err(|e| format!("parse {}: {e}", path.display()))?;

    let errs = assert_additive(&golden, actual, "");
    if errs.is_empty() {
        Ok(())
    } else {
        let actual_tmp = std::env::temp_dir().join(format!("rs-u11a-actual-{name}.json"));
        std::fs::write(&actual_tmp, &pretty).ok();
        Err(format!(
            "golden envelope shape mismatch for {name:?} ({} problem(s)):\n{}\n\
             Actual shape written to: {}\n\
             If intentional, regenerate with RS_U11_UPDATE_GOLDEN=1 (see the module doc).",
            errs.len(),
            errs.join("\n"),
            actual_tmp.display(),
        ))
    }
}

/// Panic once with every collected problem, or do nothing when `problems`
/// is empty — see [`golden_check`]'s own doc for why callers batch rather
/// than panicking per check.
fn finish(problems: Vec<String>) {
    if !problems.is_empty() {
        panic!("\n\n{}\n", problems.join("\n\n---\n\n"));
    }
}

// --- POST /api/reviews (plain, non-PR create) ---------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_review_golden_envelope_shape() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "feature"]);

    let (_tmp, base) = boot(cfg_with_repo("fixture", &dir)).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "fixture",
            "head_ref": "feature",
            "base_ref": "main",
            "title": "widgets: spin faster",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    let body: serde_json::Value = resp.json().await.unwrap();

    let mut problems = Vec::new();
    if let Err(e) = golden_check("create_review", &body) {
        problems.push(e);
    }
    finish(problems);
}

// --- POST /api/reviews/pr (sync bind) + snapshot + list + show + files +
// pr-status — one PR-bound review carries all six through its lifecycle
// so each golden pins fields fully populated (bound PR meta, a set
// verdict, an authored report) rather than mostly `null`. --------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_review_pr_sync_snapshot_list_show_files_and_pr_status_golden_envelope_shapes() {
    let (_repo_tmp, _bare_tmp, dir, pr_sha) = fixture_pr_repo(100);
    let gh_router = pulls_and_checks_router(100, &pr_sha);
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;
    let (_tmp, base) = boot(cfg_with_repo_and_github("fixture", &dir, gh_addr)).await;
    let client = reqwest::Client::new();

    let mut problems = Vec::new();

    // --- bind (sync start-pr) --------------------------------------------
    let resp = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": "fixture", "pr_number": 100 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    let bind_body: serde_json::Value = resp.json().await.unwrap();
    let id = bind_body["id"].as_i64().expect("bound review has an id");
    if let Err(e) = golden_check("create_review_pr_sync", &bind_body) {
        problems.push(e);
    }

    // --- snapshot: a second patchset (capture_patchset always mints one,
    // dedup-on-unchanged-tip is the FUTURE `minted` behaviour U11 adds) ---
    let resp = client
        .post(format!("{base}/api/reviews/{id}/snapshot"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let snapshot_body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(snapshot_body["ps_number"], 2, "{snapshot_body}");
    if let Err(e) = golden_check("snapshot_review", &snapshot_body) {
        problems.push(e);
    }

    // --- verdict + report, so list/show pin non-null nested shapes -------
    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .json(&serde_json::json!({"state": "approve", "note": "fixture: looks good"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let resp = client
        .put(format!("{base}/api/reviews/{id}/report"))
        .json(&serde_json::json!({
            "schema": "kbc-review-report/1",
            "summary": "Looks good, one nit",
            "risk_score": 2,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );

    // --- GET /api/reviews (list) ------------------------------------------
    let resp = client
        .get(format!("{base}/api/reviews"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let list_body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        list_body["reviews"].as_array().unwrap().len(),
        1,
        "{list_body}"
    );
    if let Err(e) = golden_check("list_reviews", &list_body) {
        problems.push(e);
    }

    // --- GET /api/reviews/{id} (show, incl. patchsets) ---------------------
    let resp = client
        .get(format!("{base}/api/reviews/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let show_body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        show_body["patchsets"].as_array().unwrap().len(),
        2,
        "{show_body}"
    );
    if let Err(e) = golden_check("get_review", &show_body) {
        problems.push(e);
    }

    // --- GET /api/reviews/{id}/files ---------------------------------------
    let resp = client
        .get(format!("{base}/api/reviews/{id}/files"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let files_body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        !files_body["files"].as_array().unwrap().is_empty(),
        "{files_body}"
    );
    if let Err(e) = golden_check("review_files", &files_body) {
        problems.push(e);
    }

    // --- GET /api/reviews/{id}/pr-status ------------------------------------
    let resp = client
        .get(format!("{base}/api/reviews/{id}/pr-status"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let pr_status_body: serde_json::Value = resp.json().await.unwrap();
    if let Err(e) = golden_check("pr_status", &pr_status_body) {
        problems.push(e);
    }

    finish(problems);
}

// --- POST /api/reviews/pr?async=1 (start-pr job) -------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_review_pr_async_job_golden_envelope_shape() {
    let (_repo_tmp, _bare_tmp, dir, pr_sha) = fixture_pr_repo(101);
    let gh_router = pulls_and_checks_router(101, &pr_sha);
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;
    let (_tmp, base) = boot(cfg_with_repo_and_github("fixture", &dir, gh_addr)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/reviews/pr?async=1"))
        .json(&serde_json::json!({ "repo": "fixture", "pr_number": 101 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::ACCEPTED,
        "{}",
        resp.text().await.unwrap()
    );
    let job_id = resp.json::<serde_json::Value>().await.unwrap()["job_id"]
        .as_str()
        .expect("job_id")
        .to_string();

    let mut job_body = serde_json::Value::Null;
    for _ in 0..200 {
        job_body = client
            .get(format!("{base}/api/reviews/jobs/{job_id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if job_body["status"] != "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(job_body["status"], "done", "{job_body}");

    let mut problems = Vec::new();
    if let Err(e) = golden_check("create_review_pr_async_job", &job_body) {
        problems.push(e);
    }
    finish(problems);
}

// --- POST /api/reviews/sweep ----------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweep_golden_envelope_shape() {
    let (_repo_tmp, _bare_tmp, dir, pr_sha) = fixture_pr_repo(102);
    let pulls_path = "/repos/acme/widget/pulls/102".to_string();
    let checks_path = format!("/repos/acme/widget/commits/{pr_sha}/check-runs");
    let reviews_path = "/repos/acme/widget/pulls/102/reviews".to_string();
    let requested_path = "/repos/acme/widget/pulls/102/requested_reviewers".to_string();
    let pr_sha_for_pull = pr_sha.clone();
    let gh_router = Router::new()
        .route(
            &pulls_path,
            get(move || {
                let pr_sha = pr_sha_for_pull.clone();
                async move {
                    Json(serde_json::json!({
                        "number": 102,
                        "title": "widgets: spin faster",
                        "user": {"login": "octocat"},
                        "head": {"ref": "pr-branch", "sha": pr_sha},
                        "base": {"ref": "main"},
                        "updated_at": "2024-01-01T00:00:00Z",
                        "draft": false,
                        "state": "open",
                        "merged": false,
                        "labels": [{"name": "needs-review"}],
                        "mergeable_state": "clean",
                        "body": "Adds a faster spin path.",
                    }))
                }
            }),
        )
        .route(
            &checks_path,
            get(|| async {
                Json(serde_json::json!({
                    "check_runs": [{
                        "name": "build",
                        "status": "completed",
                        "conclusion": "success",
                        "started_at": "2024-01-01T00:00:00Z",
                        "completed_at": "2024-01-01T00:05:00Z",
                    }]
                }))
            }),
        )
        .route(
            &reviews_path,
            get(|| async {
                Json(serde_json::json!([
                    {"user": {"login": "alice"}, "state": "APPROVED", "submitted_at": "2024-01-01T00:00:00Z"}
                ]))
            }),
        )
        .route(
            &requested_path,
            get(|| async { Json(serde_json::json!({"users": []})) }),
        );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;
    let (_tmp, base) = boot(cfg_with_repo_and_github("fixture", &dir, gh_addr)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": "fixture", "pr_number": 102 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );

    let resp = client
        .post(format!("{base}/api/reviews/sweep"))
        .json(&serde_json::json!({ "repo": "fixture" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let sweep_body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        sweep_body["rows"].as_array().unwrap().len(),
        1,
        "{sweep_body}"
    );

    let mut problems = Vec::new();
    if let Err(e) = golden_check("sweep", &sweep_body) {
        problems.push(e);
    }
    finish(problems);
}

// --- POST /api/prs/fetch ---------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prs_fetch_golden_envelope_shape() {
    let bare_tmp = tempfile::tempdir().unwrap();
    let bare_dir = std::fs::canonicalize(bare_tmp.path())
        .unwrap()
        .join("origin.git");
    std::fs::create_dir_all(&bare_dir).unwrap();
    git(&bare_dir, &["init", "-q", "--bare", "-b", "main"]);

    let work_tmp = tempfile::tempdir().unwrap();
    let work_dir = std::fs::canonicalize(work_tmp.path()).unwrap();
    init_repo(&work_dir);
    std::fs::write(work_dir.join("a.txt"), "hello\n").unwrap();
    git(&work_dir, &["add", "-A"]);
    git(&work_dir, &["commit", "-q", "-m", "pr commit"]);
    git(
        &work_dir,
        &[
            "push",
            "-q",
            bare_dir.to_str().unwrap(),
            "HEAD:refs/pull/103/head",
        ],
    );

    let repo_tmp = tempfile::tempdir().unwrap();
    let repo_dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&repo_dir);
    std::fs::write(repo_dir.join("base.txt"), "base\n").unwrap();
    git(&repo_dir, &["add", "-A"]);
    git(&repo_dir, &["commit", "-q", "-m", "base"]);
    git(
        &repo_dir,
        &["remote", "add", "origin", bare_dir.to_str().unwrap()],
    );

    let (_tmp, base) = boot(cfg_with_repo("fixture", &repo_dir)).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/prs/fetch"))
        .json(&serde_json::json!({ "repo": "fixture", "number": 103 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let body: serde_json::Value = resp.json().await.unwrap();

    let mut problems = Vec::new();
    if let Err(e) = golden_check("prs_fetch", &body) {
        problems.push(e);
    }
    finish(problems);
}
