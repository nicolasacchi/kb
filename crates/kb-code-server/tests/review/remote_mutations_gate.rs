//! S2-B ("Mobile mutations," `/tmp/design-s2.md`) — end-to-end HTTP tests
//! for `[review] remote_mutations` (`crate::review_gate::
//! review_mutations_gate`), pinning the admission table `router.rs`'s
//! S2-B doc paragraph and `review_gate`'s own module doc describe:
//!
//! - gate OFF (default): non-loopback + a VALID bearer token still 404s on
//!   all five graduated route families — byte-identical to the pre-S2
//!   loopback-only posture (`gate_off_non_loopback_valid_token_404s_all_
//!   five_families`).
//! - gate ON: non-loopback + a valid token succeeds
//!   (`gate_on_non_loopback_valid_token_succeeds_on_every_family`);
//!   non-loopback + a missing/wrong token 401s
//!   (`gate_on_non_loopback_missing_or_bad_token_401s_every_family`).
//! - loopback is unaffected by the flag either way
//!   (`loopback_admits_regardless_of_gate_state`).
//! - the working-tree mutation lane NEVER moves — `checkout`, suggestion
//!   apply / apply-batch, `scip/ingest`, and `prs/fetch` all keep 404ing a
//!   non-loopback caller even with the gate ON and a valid token (one test
//!   per route, `never_moves_*`).
//!
//! Boot pattern mirrors `local_review_routes.rs` / `review_findings.rs`
//! (each e2e file in this crate duplicates its own small helper set — see
//! `review_routes.rs`'s own doc for why). `KB_CODE_TOKEN` is a process env
//! var, so every test that sets it serializes on this file's own `SERIAL`
//! guard — same *purpose* as `boot_e2e/boot.rs`'s `ENV_TEST_LOCK` (a
//! `tokio::sync::Mutex` held across `.await`).

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use std::path::Path;

use crate::common::git;

fn fixture_feature_branch() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature-1\n").unwrap();
    std::fs::write(dir.join("b.txt"), "new\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature work"]);
    tmp
}

async fn boot_with_repo(
    name: &str,
    path: &Path,
    review: ReviewSection,
) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
        review,
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"))
}

async fn create_review(client: &reqwest::Client, base: &str, repo: &str) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": repo,
            "head_ref": "feature",
            "base_ref": "main",
            "title": "remote mutations gate",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// Findings import stays loopback-only regardless of the gate (see the
/// module doc) — used here purely to SEED a target finding for the
/// disposition/publish tests, always via a real loopback connection (no
/// `X-Forwarded-For`), so it succeeds unconditionally.
async fn import_findings(
    client: &reqwest::Client,
    base: &str,
    id: i64,
    payload: &serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings/import"))
        .json(payload)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.json().await.unwrap();
    (status, body)
}

async fn list_findings(client: &reqwest::Client, base: &str, id: i64) -> serde_json::Value {
    let resp = client
        .get(format!("{base}/api/reviews/{id}/findings"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    resp.json().await.unwrap()
}

fn finding_by_slug<'a>(body: &'a serde_json::Value, slug: &str) -> &'a serde_json::Value {
    body["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["slug"] == slug)
        .unwrap_or_else(|| panic!("finding {slug:?} not found in {body}"))
}

fn sample_finding(slug: &str, line: i64, title: &str) -> serde_json::Value {
    serde_json::json!({
        "slug": slug,
        "severity": "concern",
        "category": "Style",
        "location": { "path": "a.txt", "kind": "single", "lines": [line] },
        "title": title,
        "rationale": "Spotted on review.",
    })
}

async fn seed_target_finding(client: &reqwest::Client, base: &str, id: i64) {
    let (status, body) = import_findings(
        client,
        base,
        id,
        &serde_json::json!({
            "schema": "kbc-findings/1",
            "findings": [sample_finding("f-target", 2, "seed for the gate test")],
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
}

// --- gate OFF (default) — byte-identical to the pre-S2 loopback-only ------

/// Gate OFF (`ReviewSection::default()`) + non-loopback + a VALID bearer
/// token still 404s all five graduated families — the gate decides before
/// `auth_bearer` ever sees the token, so presenting one changes nothing.
/// Confirms nothing was actually written by re-reading state after every
/// rejected call (same discipline as `review_findings.rs`'s
/// `findings_mutation_routes_404_for_non_loopback`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_off_non_loopback_valid_token_404s_all_five_families() {
    const FIXTURE_TOKEN: &str = "kb-code-remote-mutations-gate-off-token";
    let _guard = crate::ENV_SERIAL.lock().await;
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);

    let repo_tmp = fixture_feature_branch();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), ReviewSection::default()).await;
    std::env::remove_var("KB_CODE_TOKEN");

    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;
    seed_target_finding(&client, &base, id).await;

    let auth = format!("Bearer {FIXTURE_TOKEN}");

    // 1. manual finding create.
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .json(&serde_json::json!({
            "severity": "ok",
            "category": "Style",
            "location": {"path": "a.txt", "kind": "whole_file"},
            "title": "should never write",
            "rationale": "n/a",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "manual finding create, gate off");

    // 2/3. finding disposition set/clear.
    let resp = client
        .put(format!(
            "{base}/api/reviews/{id}/findings/f-target/disposition"
        ))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .json(&serde_json::json!({ "disposition": "agree" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "disposition PUT, gate off");
    let resp = client
        .delete(format!(
            "{base}/api/reviews/{id}/findings/f-target/disposition"
        ))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "disposition DELETE, gate off");

    // 4. finding publish recording.
    let resp = client
        .post(format!(
            "{base}/api/reviews/{id}/findings/f-target/published"
        ))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .json(&serde_json::json!({ "github_comment_url": "https://x/1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "finding publish, gate off");

    // 5. verdict publish recording.
    let resp = client
        .post(format!("{base}/api/reviews/{id}/verdict/published"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .json(&serde_json::json!({ "github_review_url": "https://x/review-1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "verdict publish, gate off");

    // 6/7. verdict set/clear.
    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .json(&serde_json::json!({ "state": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "verdict PUT, gate off");
    let resp = client
        .delete(format!("{base}/api/reviews/{id}/verdict"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "verdict DELETE, gate off");

    // Nothing above actually wrote.
    let listed = list_findings(&client, &base, id).await;
    let f = finding_by_slug(&listed, "f-target");
    assert!(
        f["disposition"].is_null(),
        "every 404'd mutation above must have left the finding untouched"
    );
    // `published_state` is a non-nullable `String` (`store::ReviewFinding`),
    // never `Null` — it defaults to `"unpublished"` at insert time and only
    // the (never-reached-here) publish route flips it to `"published"`.
    assert_eq!(f["published_state"], "unpublished", "{f}");
    let show: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(show["verdict"].is_null(), "verdict must stay unset: {show}");
}

// --- gate ON ----------------------------------------------------------------

/// Gate ON + non-loopback + a VALID bearer token succeeds on every one of
/// the five graduated families (both HTTP methods, where the family has a
/// pair).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_on_non_loopback_valid_token_succeeds_on_every_family() {
    const FIXTURE_TOKEN: &str = "kb-code-remote-mutations-gate-on-token";
    let _guard = crate::ENV_SERIAL.lock().await;
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);

    let repo_tmp = fixture_feature_branch();
    let review = ReviewSection {
        remote_mutations: true,
        ..ReviewSection::default()
    };
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), review).await;
    std::env::remove_var("KB_CODE_TOKEN");

    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;
    seed_target_finding(&client, &base, id).await;

    let auth = format!("Bearer {FIXTURE_TOKEN}");

    // 1. manual finding create.
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .json(&serde_json::json!({
            "severity": "ok",
            "category": "Style",
            "location": {"path": "a.txt", "kind": "whole_file"},
            "title": "manual finding via the gate-on test",
            "rationale": "n/a",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "manual finding create, gate on: {}",
        resp.text().await.unwrap()
    );

    // 2. finding disposition set.
    let resp = client
        .put(format!(
            "{base}/api/reviews/{id}/findings/f-target/disposition"
        ))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .json(&serde_json::json!({ "disposition": "agree" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "disposition PUT, gate on"
    );

    // 3. finding publish recording.
    let resp = client
        .post(format!(
            "{base}/api/reviews/{id}/findings/f-target/published"
        ))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .json(&serde_json::json!({ "github_comment_url": "https://x/1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "finding publish, gate on"
    );

    // 4. verdict publish recording.
    let resp = client
        .post(format!("{base}/api/reviews/{id}/verdict/published"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .json(&serde_json::json!({ "github_review_url": "https://x/review-1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "verdict publish, gate on"
    );

    // 5. verdict set.
    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .json(&serde_json::json!({ "state": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "verdict PUT, gate on"
    );

    // The DELETE half of both pairs also succeeds through the same gate.
    let resp = client
        .delete(format!("{base}/api/reviews/{id}/verdict"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "verdict DELETE, gate on"
    );
    let resp = client
        .delete(format!(
            "{base}/api/reviews/{id}/findings/f-target/disposition"
        ))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "disposition DELETE, gate on"
    );
}

/// Gate ON + non-loopback + a missing OR wrong bearer token 401s every
/// family — `auth_bearer` runs (the gate let the request through) and its
/// ordinary token check rejects it. Confirms nothing was written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_on_non_loopback_missing_or_bad_token_401s_every_family() {
    const FIXTURE_TOKEN: &str = "kb-code-remote-mutations-gate-on-401-token";
    let _guard = crate::ENV_SERIAL.lock().await;
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);

    let repo_tmp = fixture_feature_branch();
    let review = ReviewSection {
        remote_mutations: true,
        ..ReviewSection::default()
    };
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), review).await;
    std::env::remove_var("KB_CODE_TOKEN");

    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;
    seed_target_finding(&client, &base, id).await;

    // No Authorization header at all.
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings"))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({
            "severity": "ok",
            "category": "Style",
            "location": {"path": "a.txt", "kind": "whole_file"},
            "title": "should never write",
            "rationale": "n/a",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "manual finding create, no token"
    );

    let resp = client
        .put(format!(
            "{base}/api/reviews/{id}/findings/f-target/disposition"
        ))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({ "disposition": "agree" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "disposition PUT, no token"
    );

    let resp = client
        .delete(format!(
            "{base}/api/reviews/{id}/findings/f-target/disposition"
        ))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "disposition DELETE, no token"
    );

    let resp = client
        .post(format!(
            "{base}/api/reviews/{id}/findings/f-target/published"
        ))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "finding publish, no token"
    );

    let resp = client
        .post(format!("{base}/api/reviews/{id}/verdict/published"))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "verdict publish, no token"
    );

    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({ "state": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "verdict PUT, no token"
    );

    let resp = client
        .delete(format!("{base}/api/reviews/{id}/verdict"))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "verdict DELETE, no token"
    );

    // A WRONG (not merely absent) token 401s too — spot-check on one
    // representative route; the rejection is `auth_bearer`'s ordinary
    // bad-token path, shared byte-for-byte by every route behind this gate.
    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", "Bearer not-the-right-token")
        .json(&serde_json::json!({ "state": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "verdict PUT, wrong token"
    );

    // Nothing above actually wrote.
    let listed = list_findings(&client, &base, id).await;
    let f = finding_by_slug(&listed, "f-target");
    assert!(
        f["disposition"].is_null(),
        "every 401'd mutation above must have left the finding untouched"
    );
    let show: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(show["verdict"].is_null(), "verdict must stay unset: {show}");
}

// --- loopback is unaffected either way --------------------------------------

/// A loopback caller (real TCP peer, no `X-Forwarded-For` spoof, no bearer
/// token) is admitted regardless of `[review] remote_mutations` — both
/// with the flag at its default (off) and explicitly on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_admits_regardless_of_gate_state() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let client = reqwest::Client::new();

    let repo_tmp_off = fixture_feature_branch();
    let (_daemon_off, base_off) =
        boot_with_repo("r", repo_tmp_off.path(), ReviewSection::default()).await;
    let id_off = create_review(&client, &base_off, "r").await;
    let resp = client
        .put(format!("{base_off}/api/reviews/{id_off}/verdict"))
        .json(&serde_json::json!({ "state": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "loopback + gate OFF must still work, no token needed"
    );

    let repo_tmp_on = fixture_feature_branch();
    let review_on = ReviewSection {
        remote_mutations: true,
        ..ReviewSection::default()
    };
    let (_daemon_on, base_on) = boot_with_repo("r", repo_tmp_on.path(), review_on).await;
    let id_on = create_review(&client, &base_on, "r").await;
    let resp = client
        .put(format!("{base_on}/api/reviews/{id_on}/verdict"))
        .json(&serde_json::json!({ "state": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "loopback + gate ON must still work, no token needed"
    );
}

// --- the working-tree mutation lane NEVER moves (one test per route) -------

/// `POST /api/checkout` (the working-tree mutation lane) 404s a
/// non-loopback caller EVEN WITH the S2-B gate ON and a valid bearer
/// token — it stays on `transcripts::search::loopback_only`, which
/// `review_mutations_gate` never wraps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_moves_checkout_404s_non_loopback_even_with_gate_on_and_valid_token() {
    const FIXTURE_TOKEN: &str = "kb-code-never-moves-checkout";
    let _guard = crate::ENV_SERIAL.lock().await;
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);

    let repo_tmp = fixture_feature_branch();
    let review = ReviewSection {
        remote_mutations: true,
        ..ReviewSection::default()
    };
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), review).await;
    std::env::remove_var("KB_CODE_TOKEN");

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/checkout"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .json(&serde_json::json!({ "repo": "r", "ref": "feature" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "POST /api/checkout must 404 a non-loopback caller even with the \
         gate ON + a valid token — the working-tree lane never moves"
    );
}

/// `POST /api/annotations/{id}/apply` — same never-moves contract as
/// `checkout` above.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_moves_suggestion_apply_404s_non_loopback_even_with_gate_on_and_valid_token() {
    const FIXTURE_TOKEN: &str = "kb-code-never-moves-apply";
    let _guard = crate::ENV_SERIAL.lock().await;
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);

    let repo_tmp = fixture_feature_branch();
    let review = ReviewSection {
        remote_mutations: true,
        ..ReviewSection::default()
    };
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), review).await;
    std::env::remove_var("KB_CODE_TOKEN");

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/annotations/ann_doesnotexist/apply"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "POST /api/annotations/{{id}}/apply must 404 a non-loopback caller \
         even with the gate ON + a valid token"
    );
}

/// `POST /api/annotations/apply-batch` — same never-moves contract as
/// `checkout` above.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_moves_apply_batch_404s_non_loopback_even_with_gate_on_and_valid_token() {
    const FIXTURE_TOKEN: &str = "kb-code-never-moves-apply-batch";
    let _guard = crate::ENV_SERIAL.lock().await;
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);

    let repo_tmp = fixture_feature_branch();
    let review = ReviewSection {
        remote_mutations: true,
        ..ReviewSection::default()
    };
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), review).await;
    std::env::remove_var("KB_CODE_TOKEN");

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/annotations/apply-batch"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .json(&serde_json::json!({ "annotation_ids": ["ann_doesnotexist"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "POST /api/annotations/apply-batch must 404 a non-loopback caller \
         even with the gate ON + a valid token"
    );
}

/// `POST /api/scip/ingest` — same never-moves contract as `checkout` above.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_moves_scip_ingest_404s_non_loopback_even_with_gate_on_and_valid_token() {
    const FIXTURE_TOKEN: &str = "kb-code-never-moves-scip-ingest";
    let _guard = crate::ENV_SERIAL.lock().await;
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);

    let repo_tmp = fixture_feature_branch();
    let review = ReviewSection {
        remote_mutations: true,
        ..ReviewSection::default()
    };
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), review).await;
    std::env::remove_var("KB_CODE_TOKEN");

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/scip/ingest"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "POST /api/scip/ingest must 404 a non-loopback caller even with \
         the gate ON + a valid token"
    );
}

/// `POST /api/prs/fetch` — same never-moves contract as `checkout` above.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_moves_prs_fetch_404s_non_loopback_even_with_gate_on_and_valid_token() {
    const FIXTURE_TOKEN: &str = "kb-code-never-moves-prs-fetch";
    let _guard = crate::ENV_SERIAL.lock().await;
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);

    let repo_tmp = fixture_feature_branch();
    let review = ReviewSection {
        remote_mutations: true,
        ..ReviewSection::default()
    };
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), review).await;
    std::env::remove_var("KB_CODE_TOKEN");

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/prs/fetch"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .json(&serde_json::json!({ "repo": "r", "number": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "POST /api/prs/fetch must 404 a non-loopback caller even with the \
         gate ON + a valid token"
    );
}
