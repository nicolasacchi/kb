//! PRR-R5 ("The PR Room," kb v0.39 T2, Phase 5) — end-to-end HTTP tests for
//! `crate::review_github_export`: verdict->event mapping, the candidate-set
//! exclusion defaults (waived/published) + their overrides, orphan honesty
//! (a genuinely orphaned OR imprecise-by-construction finding never lands
//! in `comments[]`), `commit_id` pinning to the latest patchset, the
//! `stale_export` flag, and publish-recording round-trip +
//! republish-idempotence. Boot pattern mirrors `review_findings.rs` (each
//! e2e file in this crate duplicates its own small helper set — see that
//! file's own doc for why).

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use tokio::sync::Mutex as AsyncMutex;

use crate::common::git;
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

async fn boot_with_repo(name: &str, path: &Path) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        review: ReviewSection::default(),
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"))
}

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(
        dir.join("order.rb"),
        "class Order\n  def checkout!\n    save!\n  end\nend\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(
        dir.join("order.rb"),
        "class Order\n  def checkout!\n    save!\n    log_checkout\n  end\nend\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature work"]);
    tmp
}

async fn create_review(client: &reqwest::Client, base: &str, repo: &str) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": repo,
            "head_ref": "feature",
            "base_ref": "main",
            "title": "github export",
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
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn snapshot(client: &reqwest::Client, base: &str, id: i64) -> serde_json::Value {
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
    resp.json().await.unwrap()
}

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

fn sample_finding(slug: &str, line: i64, title: &str) -> serde_json::Value {
    serde_json::json!({
        "slug": slug,
        "severity": "concern",
        "category": "Style",
        "location": { "path": "order.rb", "kind": "single", "lines": [line] },
        "title": title,
        "rationale": "Spotted on review.",
        "recommendation": "Fix it.",
    })
}

async fn set_disposition(
    client: &reqwest::Client,
    base: &str,
    id: i64,
    slug: &str,
    disposition: &str,
) {
    let resp = client
        .put(format!(
            "{base}/api/reviews/{id}/findings/{slug}/disposition"
        ))
        .json(&serde_json::json!({ "disposition": disposition }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
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

async fn clear_verdict(client: &reqwest::Client, base: &str, id: i64) {
    let resp = client
        .delete(format!("{base}/api/reviews/{id}/verdict"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
}

async fn export_github(
    client: &reqwest::Client,
    base: &str,
    id: i64,
    query: &[(&str, &str)],
) -> serde_json::Value {
    let resp = client
        .get(format!("{base}/api/reviews/{id}/export/github"))
        .query(query)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json().await.unwrap()
}

async fn publish_finding(
    client: &reqwest::Client,
    base: &str,
    id: i64,
    slug: &str,
    url: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings/{slug}/published"))
        .json(&serde_json::json!({ "github_comment_url": url }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.json().await.unwrap();
    (status, body)
}

async fn publish_verdict(
    client: &reqwest::Client,
    base: &str,
    id: i64,
    url: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{base}/api/reviews/{id}/verdict/published"))
        .json(&serde_json::json!({ "github_review_url": url }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.json().await.unwrap();
    (status, body)
}

fn comment_slugs(export: &serde_json::Value) -> Vec<String> {
    export["comments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["finding_slug"].as_str().unwrap().to_string())
        .collect()
}

// --- tests -------------------------------------------------------------

/// The verdict->GitHub-event mapping table (design doc §3.2), all four
/// cases: `approve`->`APPROVE`, `request-changes`->`REQUEST_CHANGES`,
/// `comment`->`COMMENT`, unset->`null` + `event_reason:"no_verdict_set"`.
/// Every case is `200`, never `400` — "nothing to export yet" is a
/// structural fact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verdict_to_event_mapping_covers_all_four_states() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    // Unset -> null + reason, 200.
    let out = export_github(&client, &base, id, &[]).await;
    assert!(out["event"].is_null());
    assert_eq!(out["event_reason"], "no_verdict_set");

    let cases = [
        ("approve", "APPROVE"),
        ("request-changes", "REQUEST_CHANGES"),
        ("comment", "COMMENT"),
    ];
    for (state, event) in cases {
        set_verdict(&client, &base, id, state).await;
        let out = export_github(&client, &base, id, &[]).await;
        assert_eq!(out["event"], event, "state={state}");
        assert!(out["event_reason"].is_null());
    }

    // Cleared back to unset.
    clear_verdict(&client, &base, id).await;
    let out = export_github(&client, &base, id, &[]).await;
    assert!(out["event"].is_null());
    assert_eq!(out["event_reason"], "no_verdict_set");
}

/// Default candidate-set exclusions: a `waive`d finding and an
/// already-published finding are BOTH excluded from `comments[]` by
/// default. `?include_waived=true` overrides the waived exclusion; there
/// is no override for "already published" (Risk #5's guard, not a lock).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn export_excludes_waived_and_published_by_default_and_include_waived_overrides() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let payload = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [
            sample_finding("f-normal", 3, "a normal finding"),
            sample_finding("f-waived", 3, "a finding someone waived"),
            sample_finding("f-published", 3, "a finding already published"),
        ],
    });
    let (status, body) = import_findings(&client, &base, id, &payload).await;
    assert_eq!(status, 200, "{body}");

    set_disposition(&client, &base, id, "f-waived", "waive").await;
    let (status, _) = publish_finding(&client, &base, id, "f-published", "https://x/1").await;
    assert_eq!(status, 200);

    let out = export_github(&client, &base, id, &[]).await;
    let slugs = comment_slugs(&out);
    assert!(slugs.contains(&"f-normal".to_string()));
    assert!(
        !slugs.contains(&"f-waived".to_string()),
        "waived must be excluded by default: {slugs:?}"
    );
    assert!(
        !slugs.contains(&"f-published".to_string()),
        "already-published must be excluded by default: {slugs:?}"
    );

    let out = export_github(&client, &base, id, &[("include_waived", "true")]).await;
    let slugs = comment_slugs(&out);
    assert!(
        slugs.contains(&"f-waived".to_string()),
        "include_waived=true must surface it: {slugs:?}"
    );
    assert!(
        !slugs.contains(&"f-published".to_string()),
        "published has no override, even with include_waived: {slugs:?}"
    );
}

/// `?finding_slugs=` narrows the candidate set to exactly the named slugs
/// (still subject to the published guard).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn export_finding_slugs_narrows_the_candidate_set() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let payload = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [
            sample_finding("f-a", 3, "finding a"),
            sample_finding("f-b", 3, "finding b"),
        ],
    });
    let (status, body) = import_findings(&client, &base, id, &payload).await;
    assert_eq!(status, 200, "{body}");

    let out = export_github(&client, &base, id, &[("finding_slugs", "f-a")]).await;
    assert_eq!(comment_slugs(&out), vec!["f-a".to_string()]);
}

/// Orphan honesty: a genuinely orphaned finding (missing file) AND the
/// imprecise-by-construction kinds (`whole_file`, `multi`) never land in
/// `comments[]` — they land in `skipped_orphaned[]` with the right reason
/// and an honest `original`. `?include_orphaned_as_general=true` degrades
/// them into `general_comments[]` instead (never both).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphaned_and_imprecise_findings_never_reach_comments_by_default() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let payload = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [
            sample_finding("f-exact", 3, "a precisely-anchored finding"),
            {
                "slug": "f-orphan",
                "severity": "ok",
                "category": "Style",
                "location": {"path": "nope/gone.rb", "kind": "single", "lines": [1]},
                "title": "cites a file never in this repo",
                "rationale": "n/a",
            },
            {
                "slug": "f-whole",
                "severity": "ok",
                "category": "Style",
                "location": {"path": "order.rb", "kind": "whole_file"},
                "title": "whole-file finding",
                "rationale": "n/a",
            },
            {
                "slug": "f-multi",
                "severity": "ok",
                "category": "Style",
                "location": {"path": "order.rb", "kind": "multi", "lines": [2, 3]},
                "title": "multi-line finding",
                "rationale": "n/a",
            },
        ],
    });
    let (status, body) = import_findings(&client, &base, id, &payload).await;
    assert_eq!(status, 200, "{body}");

    // Default: only f-exact reaches comments[]; the other three are
    // skipped_orphaned, never comments, never general_comments.
    let out = export_github(&client, &base, id, &[]).await;
    assert_eq!(comment_slugs(&out), vec!["f-exact".to_string()]);
    assert!(out["general_comments"].as_array().unwrap().is_empty());

    let skipped = out["skipped_orphaned"].as_array().unwrap().clone();
    let reason_of = |slug: &str| -> String {
        skipped
            .iter()
            .find(|s| s["finding_slug"] == slug)
            .unwrap_or_else(|| panic!("{slug} missing from skipped_orphaned: {skipped:?}"))
            ["reason"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(reason_of("f-orphan"), "orphaned");
    assert_eq!(reason_of("f-whole"), "whole_file");
    assert_eq!(reason_of("f-multi"), "multi_line");

    let whole_original = skipped
        .iter()
        .find(|s| s["finding_slug"] == "f-whole")
        .unwrap();
    assert!(
        whole_original["original"]["line"].is_null(),
        "whole_file never cited a line"
    );
    let multi_original = skipped
        .iter()
        .find(|s| s["finding_slug"] == "f-multi")
        .unwrap();
    assert_eq!(multi_original["original"]["line"], 2);

    // Opt-in: the three degrade into general_comments (no path/line at
    // all), skipped_orphaned goes empty, comments[] is UNCHANGED.
    let out = export_github(
        &client,
        &base,
        id,
        &[("include_orphaned_as_general", "true")],
    )
    .await;
    assert_eq!(comment_slugs(&out), vec!["f-exact".to_string()]);
    assert!(out["skipped_orphaned"].as_array().unwrap().is_empty());
    let general_slugs: Vec<String> = out["general_comments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["finding_slug"].as_str().unwrap().to_string())
        .collect();
    assert!(general_slugs.contains(&"f-orphan".to_string()));
    assert!(general_slugs.contains(&"f-whole".to_string()));
    assert!(general_slugs.contains(&"f-multi".to_string()));
    for g in out["general_comments"].as_array().unwrap() {
        assert!(
            g.get("path").is_none(),
            "general comment must carry no path: {g}"
        );
        assert!(
            g.get("line").is_none(),
            "general comment must carry no line: {g}"
        );
    }
}

/// `commit_id` is `latest_ps.tip_sha` VERBATIM — pinned to the review's
/// current latest patchset, not the first one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_id_pins_the_latest_patchset_tip() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let out = export_github(&client, &base, id, &[]).await;
    let ps1_tip = git_out(dir, &["rev-parse", "feature"]);
    assert_eq!(out["commit_id"], ps1_tip);
    assert_eq!(out["ps_number"], 1);

    std::fs::write(
        dir.join("order.rb"),
        "class Order\n  def checkout!\n    save!\n    log_checkout\n    notify!\n  end\nend\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature v2"]);
    let ps2 = snapshot(&client, &base, id).await;
    assert_eq!(ps2["ps_number"], 2);
    let ps2_tip = git_out(dir, &["rev-parse", "feature"]);
    assert_ne!(ps1_tip, ps2_tip);

    let out = export_github(&client, &base, id, &[]).await;
    assert_eq!(out["commit_id"], ps2_tip);
    assert_eq!(out["ps_number"], 2);
}

/// `stale_export` flips true the moment the review's local latest patchset
/// tip diverges from the STORED `pr_head_sha` snapshot (no live GitHub
/// call — the design doc §3.2 point 4 contract). No `pr_head_sha` at all
/// (never bound) means "nothing to have drifted from", so `false`, never a
/// guessed staleness. `pr_head_sha` is set directly via the store (no
/// lightweight HTTP route exists for this outside the full `POST
/// /api/reviews/pr` GitHub-mock flow `review_routes.rs` already covers) —
/// mirrors that file's own `Store::open(&db)` pattern for reaching
/// otherwise-unreachable-via-HTTP setup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_export_flips_when_the_local_tip_moves_past_the_stored_pr_head() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (daemon_tmp, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    // No PR binding at all -> never stale.
    let out = export_github(&client, &base, id, &[]).await;
    assert_eq!(out["stale_export"], false);

    let ps1_tip = git_out(dir, &["rev-parse", "feature"]);
    let db = daemon_tmp.path().join("state/kb-code/index.db");
    {
        let store = kb_code_server::store::Store::open(&db).unwrap();
        store
            .set_review_pr_binding(id, 99, "acme/widget", Some(&ps1_tip), None, None)
            .unwrap();
    }

    // pr_head_sha matches the local tip -> not stale.
    let out = export_github(&client, &base, id, &[]).await;
    assert_eq!(out["stale_export"], false);

    // Advance the local branch + snapshot a new ps WITHOUT updating the
    // stored pr_head_sha (simulating a re-review nobody re-fetched yet).
    std::fs::write(
        dir.join("order.rb"),
        "class Order\n  def checkout!\n    save!\n    log_checkout\n    notify!\n  end\nend\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature v2"]);
    snapshot(&client, &base, id).await;

    let out = export_github(&client, &base, id, &[]).await;
    assert_eq!(out["stale_export"], true);
}

/// Publish recording round-trips through the finding view and is
/// idempotent by overwrite: recording twice UPDATES (never errors), and
/// the very next export naturally excludes the published finding (the
/// `published_state=="published"` guard). Verdict-publish round-trips the
/// same way at the review level.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_recording_round_trips_and_is_idempotent_then_export_excludes() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let payload = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [sample_finding("f-a", 3, "a finding")],
    });
    let (status, body) = import_findings(&client, &base, id, &payload).await;
    assert_eq!(status, 200, "{body}");

    // Present in the export before any publish record.
    let out = export_github(&client, &base, id, &[]).await;
    assert_eq!(comment_slugs(&out), vec!["f-a".to_string()]);

    let (status, view) = publish_finding(&client, &base, id, "f-a", "https://x/1").await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["published_state"], "published");
    assert_eq!(view["published_url"], "https://x/1");
    let first_at = view["published_at"].as_i64().unwrap();

    // Republish (a second `gh` call, or a retry) updates rather than
    // erroring — idempotent by overwrite.
    let (status, view2) = publish_finding(&client, &base, id, "f-a", "https://x/2").await;
    assert_eq!(status, 200, "{view2}");
    assert_eq!(view2["published_state"], "published");
    assert_eq!(view2["published_url"], "https://x/2");
    assert!(view2["published_at"].as_i64().unwrap() >= first_at);

    // The next export excludes it.
    let out = export_github(&client, &base, id, &[]).await;
    assert!(comment_slugs(&out).is_empty());

    // An unknown slug 404s.
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings/nope/published"))
        .json(&serde_json::json!({ "github_comment_url": "https://x/3" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // Verdict-level publish round-trips the same way.
    set_verdict(&client, &base, id, "approve").await;
    let (status, vbody) = publish_verdict(&client, &base, id, "https://x/review-1").await;
    assert_eq!(status, 200, "{vbody}");
    assert_eq!(vbody["verdict_published_url"], "https://x/review-1");
    let (status, vbody2) = publish_verdict(&client, &base, id, "https://x/review-2").await;
    assert_eq!(status, 200, "{vbody2}");
    assert_eq!(vbody2["verdict_published_url"], "https://x/review-2");
}
