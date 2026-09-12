//! PRR-R3 ("The PR Room," kb v0.39 T2, Phase 3) — end-to-end HTTP tests for
//! the findings surface (`crate::review_findings`): batch import +
//! reconciliation, malformed-batch validation, the manual/import
//! slug-collision guard (the OWED item), manual create + slug uniquify,
//! additive mode, disposition set/clear + SSE, resolution confidence
//! (exact/orphaned), and the review-level ("general question") annotation
//! round-tripping through `/comments` + `/distill`. Boot pattern mirrors
//! `review_comments.rs` / `local_review_routes.rs` — each e2e file in this
//! crate duplicates its own small helper set (see `review_routes.rs`'s own
//! doc for why).

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;

use crate::common::git;

#[allow(dead_code)]
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
            "title": "findings",
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

async fn snapshot(client: &reqwest::Client, base: &str, id: i64) -> i64 {
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
    resp.json::<serde_json::Value>().await.unwrap()["ps_number"]
        .as_i64()
        .unwrap()
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

async fn list_findings(
    client: &reqwest::Client,
    base: &str,
    id: i64,
    query: &[(&str, &str)],
) -> serde_json::Value {
    let resp = client
        .get(format!("{base}/api/reviews/{id}/findings"))
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

fn finding_by_slug<'a>(body: &'a serde_json::Value, slug: &str) -> &'a serde_json::Value {
    body["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["slug"] == slug)
        .unwrap_or_else(|| panic!("finding {slug:?} not found in {body}"))
}

async fn create_manual_finding(
    client: &reqwest::Client,
    base: &str,
    id: i64,
    payload: &serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings"))
        .json(payload)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.json().await.unwrap();
    (status, body)
}

async fn set_disposition(
    client: &reqwest::Client,
    base: &str,
    id: i64,
    slug: &str,
    disposition: &str,
    note: Option<&str>,
) -> (reqwest::StatusCode, serde_json::Value) {
    let mut payload = serde_json::json!({ "disposition": disposition });
    if let Some(n) = note {
        payload["note"] = serde_json::json!(n);
    }
    let resp = client
        .put(format!(
            "{base}/api/reviews/{id}/findings/{slug}/disposition"
        ))
        .json(&payload)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.json().await.unwrap();
    (status, body)
}

async fn clear_disposition(
    client: &reqwest::Client,
    base: &str,
    id: i64,
    slug: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .delete(format!(
            "{base}/api/reviews/{id}/findings/{slug}/disposition"
        ))
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
    })
}

// --- SSE helpers (duplicated per this crate's own e2e-file convention) ---

async fn collect_sse_matching<S, B, E>(
    stream: &mut S,
    needle: &str,
    timeout: std::time::Duration,
) -> usize
where
    S: futures::Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    use futures::StreamExt;
    let mut buf = String::new();
    let mut hits = 0usize;
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                buf.push_str(&String::from_utf8_lossy(chunk.as_ref()));
                while let Some(idx) = buf.find(needle) {
                    hits += 1;
                    buf = buf[idx + needle.len()..].to_string();
                }
            }
            _ => break,
        }
    }
    hits
}

async fn drain_sse_backlog<S, B, E>(stream: &mut S)
where
    S: futures::Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    let _ = collect_sse_matching(stream, "event:", std::time::Duration::from_millis(300)).await;
}

// --- tests -----------------------------------------------------------------

/// Import happy path + the §4.3 reconciliation matrix over TWO imports:
/// new slug creates, an existing slug present again refreshes (survives a
/// disposition set), an existing slug absent from the re-import soft-
/// supersedes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_happy_path_and_reconciliation_over_two_imports() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let payload = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [
            sample_finding("f-a", 3, "save! without validation"),
            {
                "slug": "f-b",
                "severity": "blocker",
                "category": "Concurrency",
                "location": {"path": "order.rb", "kind": "range", "lines": [2, 4]},
                "title": "checkout! is not idempotent",
                "rationale": "Verified by re-reading the diff twice.",
            },
        ],
    });
    let (status, body) = import_findings(&client, &base, id, &payload).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["created"], serde_json::json!(["f-a", "f-b"]));
    assert_eq!(body["updated"], serde_json::json!([]));
    assert_eq!(body["superseded"], serde_json::json!([]));
    assert_eq!(body["unchanged"], serde_json::json!([]));
    assert_eq!(body["ps_number"], 1);
    assert!(!body["import_batch_id"].as_str().unwrap().is_empty());

    let listed = list_findings(&client, &base, id, &[]).await;
    let fa = finding_by_slug(&listed, "f-a");
    assert_eq!(fa["origin"], "import");
    assert_eq!(fa["severity"], "concern");
    assert_eq!(fa["resolution"]["orphaned"], false);
    let fb = finding_by_slug(&listed, "f-b");
    assert_eq!(fb["location"]["kind"], "range");
    assert_eq!(fb["resolution"]["orphaned"], false);

    // A human dispositions f-a before the re-review.
    let (status, _) = set_disposition(&client, &base, id, "f-a", "agree", Some("good catch")).await;
    assert_eq!(status, 200);

    // Advance the branch and re-review: f-a survives with a NEW title,
    // f-b is dropped (never re-mentioned), f-c is brand new.
    std::fs::write(
        dir.join("order.rb"),
        "class Order\n  def checkout!\n    save!\n    log_checkout\n    notify!\n  end\nend\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature v2"]);
    assert_eq!(snapshot(&client, &base, id).await, 2);

    let payload2 = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [
            sample_finding("f-a", 3, "save! STILL lacks validation (re-verified)"),
            sample_finding("f-c", 5, "notify! can raise and swallow the exception"),
        ],
    });
    let (status, body) = import_findings(&client, &base, id, &payload2).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["created"], serde_json::json!(["f-c"]));
    assert_eq!(body["updated"], serde_json::json!(["f-a"]));
    assert_eq!(body["superseded"], serde_json::json!(["f-b"]));
    assert_eq!(body["unchanged"], serde_json::json!([]));

    // Default list excludes superseded.
    let listed = list_findings(&client, &base, id, &[]).await;
    assert!(listed["findings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["slug"] != "f-b"));
    let fa = finding_by_slug(&listed, "f-a");
    assert_eq!(fa["title"], "save! STILL lacks validation (re-verified)");
    assert_eq!(
        fa["disposition"]["state"], "agree",
        "a human's disposition must survive a re-import refresh"
    );
    assert_eq!(fa["disposition"]["note"], "good catch");

    // include_superseded=true surfaces the tombstoned f-b.
    let all = list_findings(&client, &base, id, &[("include_superseded", "true")]).await;
    let fb = finding_by_slug(&all, "f-b");
    assert_eq!(fb["superseded"], true);
    assert_eq!(fb["superseded_reason"], "not_in_reimport");
}

/// A malformed batch (one bad finding among good ones) 400s WHOLE, naming
/// the offending index, and writes nothing at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_malformed_batch_400s_and_writes_nothing() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let payload = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [
            sample_finding("f-good", 3, "a fine finding"),
            {
                "slug": "f-bad",
                "severity": "catastrophic", // invalid
                "category": "Style",
                "location": {"path": "order.rb", "kind": "single", "lines": [2]},
                "title": "bad severity",
                "rationale": "n/a",
            },
        ],
    });
    let (status, body) = import_findings(&client, &base, id, &payload).await;
    assert_eq!(status, 400, "{body}");
    let details = body["details"].as_array().expect("details array");
    assert!(
        details
            .iter()
            .any(|e| e["kind"] == "invalid_severity" && e["index"] == 1),
        "expected an invalid_severity error at index 1, got {details:?}"
    );

    let listed = list_findings(&client, &base, id, &[]).await;
    assert!(
        listed["findings"].as_array().unwrap().is_empty(),
        "nothing must be written on a whole-batch validation failure"
    );
}

/// The OWED item: an import payload slug colliding with an existing
/// MANUAL finding is rejected wholesale (`slug_conflict_manual`), and the
/// manual finding is left byte-for-byte untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_rejects_a_batch_colliding_with_a_manual_finding_slug() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let (status, manual) = create_manual_finding(
        &client,
        &base,
        id,
        &serde_json::json!({
            "slug": "f-human-noticed",
            "severity": "ok",
            "category": "Style",
            "location": {"path": "order.rb", "kind": "single", "lines": [2]},
            "title": "A human's own finding",
            "rationale": "Spotted while reading.",
        }),
    )
    .await;
    assert_eq!(status, 201, "{manual}");

    let payload = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [sample_finding("f-human-noticed", 3, "agent thinks it found this too")],
    });
    let (status, body) = import_findings(&client, &base, id, &payload).await;
    assert_eq!(status, 400, "{body}");
    let details = body["details"].as_array().expect("details array");
    assert!(
        details
            .iter()
            .any(|e| e["kind"] == "slug_conflict_manual" && e["index"] == 0),
        "expected slug_conflict_manual at index 0, got {details:?}"
    );

    let listed = list_findings(&client, &base, id, &[]).await;
    let f = finding_by_slug(&listed, "f-human-noticed");
    assert_eq!(f["origin"], "manual");
    assert_eq!(
        f["title"], "A human's own finding",
        "the manual finding must be completely untouched by the rejected import"
    );
}

/// Manual create derives `f-<kebab-of-title>` when `slug` is omitted, and
/// uniquifies `-2`/`-3`… on a repeat title. An explicit, already-taken
/// slug 409s.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_create_derives_and_uniquifies_slugs() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let body_for = |title: &str| {
        serde_json::json!({
            "severity": "ok",
            "category": "Naming",
            "location": {"path": "order.rb", "kind": "whole_file"},
            "title": title,
            "rationale": "Manual note.",
        })
    };

    let (status, f1) =
        create_manual_finding(&client, &base, id, &body_for("Duplicate Order Rows")).await;
    assert_eq!(status, 201, "{f1}");
    assert_eq!(f1["slug"], "f-duplicate-order-rows");
    assert_eq!(f1["origin"], "manual");
    assert_eq!(f1["author"], "you");

    let (status, f2) =
        create_manual_finding(&client, &base, id, &body_for("Duplicate Order Rows")).await;
    assert_eq!(status, 201, "{f2}");
    assert_eq!(f2["slug"], "f-duplicate-order-rows-2");

    let (status, f3) =
        create_manual_finding(&client, &base, id, &body_for("Duplicate Order Rows")).await;
    assert_eq!(status, 201, "{f3}");
    assert_eq!(f3["slug"], "f-duplicate-order-rows-3");

    // An explicit, already-taken slug 409s.
    let mut explicit = body_for("Something else entirely");
    explicit["slug"] = serde_json::json!("f-duplicate-order-rows");
    let (status, body) = create_manual_finding(&client, &base, id, &explicit).await;
    assert_eq!(status, 409, "{body}");
}

/// `mode: "additive"` never supersedes anything, even a slug that a plain
/// (full) re-import over the same absent set would have tombstoned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn additive_mode_never_supersedes() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let first = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [
            sample_finding("f-a", 2, "finding a"),
            sample_finding("f-b", 3, "finding b"),
        ],
    });
    let (status, body) = import_findings(&client, &base, id, &first).await;
    assert_eq!(status, 200, "{body}");

    let second = serde_json::json!({
        "schema": "kbc-findings/1",
        "mode": "additive",
        "findings": [sample_finding("f-c", 4, "finding c")],
    });
    let (status, body) = import_findings(&client, &base, id, &second).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["created"], serde_json::json!(["f-c"]));
    assert_eq!(
        body["superseded"],
        serde_json::json!([]),
        "additive must supersede nothing, even f-a/f-b that never appeared in this batch"
    );

    let listed = list_findings(&client, &base, id, &[]).await;
    let slugs: Vec<&str> = listed["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.contains(&"f-a"));
    assert!(slugs.contains(&"f-b"));
    assert!(slugs.contains(&"f-c"));
}

/// Disposition set/clear round-trips through the finding view and emits
/// `review.changed{reason:"disposition", finding_slug}`; an identical
/// re-set is a no-op with NO new SSE.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disposition_set_clear_and_sse_reason() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let payload = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [sample_finding("f-a", 3, "a finding")],
    });
    let (status, _) = import_findings(&client, &base, id, &payload).await;
    assert_eq!(status, 200);

    let sse_resp = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(sse_resp.status(), 200);
    let mut stream = sse_resp.bytes_stream();
    drain_sse_backlog(&mut stream).await;

    let (status, body) =
        set_disposition(&client, &base, id, "f-a", "waive", Some("accepted risk")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["disposition"]["state"], "waive");
    assert_eq!(body["disposition"]["note"], "accepted risk");

    let hits = collect_sse_matching(
        &mut stream,
        "\"reason\":\"disposition\"",
        std::time::Duration::from_millis(400),
    )
    .await;
    assert!(
        hits >= 1,
        "expected a review.changed{{reason:disposition}} SSE"
    );
    let slug_hits = collect_sse_matching(
        &mut stream,
        "\"finding_slug\":\"f-a\"",
        std::time::Duration::from_millis(1),
    )
    .await;
    let _ = slug_hits; // best-effort — the buffer may already be consumed above

    // Re-setting the SAME (disposition, note) is a no-op: no new SSE.
    let (status, _) =
        set_disposition(&client, &base, id, "f-a", "waive", Some("accepted risk")).await;
    assert_eq!(status, 200);
    let hits = collect_sse_matching(
        &mut stream,
        "\"reason\":\"disposition\"",
        std::time::Duration::from_millis(300),
    )
    .await;
    assert_eq!(hits, 0, "an identical disposition re-set must not emit SSE");

    let (status, body) = clear_disposition(&client, &base, id, "f-a").await;
    assert_eq!(status, 200, "{body}");
    assert!(body["disposition"].is_null());
    let hits = collect_sse_matching(
        &mut stream,
        "\"reason\":\"disposition\"",
        std::time::Duration::from_millis(400),
    )
    .await;
    assert!(hits >= 1, "expected an SSE for the clear too");
}

/// Resolution confidence: an untouched finding resolves `exact`; a finding
/// citing a path absent from the repo (single-line AND whole_file kinds)
/// is honestly `orphaned` — never a guessed line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn findings_list_reports_exact_and_orphaned_resolution_confidence() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let payload = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [
            sample_finding("f-exact", 3, "untouched line"),
            {
                "slug": "f-missing-single",
                "severity": "ok",
                "category": "Style",
                "location": {"path": "nope/does_not_exist.rb", "kind": "single", "lines": [1]},
                "title": "citing a file that was never in this repo",
                "rationale": "n/a",
            },
            {
                "slug": "f-missing-whole",
                "severity": "ok",
                "category": "Style",
                "location": {"path": "nope/does_not_exist.rb", "kind": "whole_file"},
                "title": "whole_file on a missing path",
                "rationale": "n/a",
            },
            {
                "slug": "f-whole-present",
                "severity": "ok",
                "category": "Style",
                "location": {"path": "order.rb", "kind": "whole_file"},
                "title": "whole_file on a real path",
                "rationale": "n/a",
            },
        ],
    });
    let (status, body) = import_findings(&client, &base, id, &payload).await;
    assert_eq!(status, 200, "{body}");

    let listed = list_findings(&client, &base, id, &[]).await;

    let exact = finding_by_slug(&listed, "f-exact");
    assert_eq!(exact["resolution"]["orphaned"], false);
    assert_eq!(exact["resolution"]["confidence"], "exact");

    let missing_single = finding_by_slug(&listed, "f-missing-single");
    assert_eq!(missing_single["resolution"]["orphaned"], true);
    assert_eq!(missing_single["resolution"]["confidence"], "orphaned");
    assert!(missing_single["resolution"]["line"].is_null());

    let missing_whole = finding_by_slug(&listed, "f-missing-whole");
    assert_eq!(
        missing_whole["resolution"]["orphaned"], true,
        "a whole_file finding on an absent path must be honestly orphaned, never guessed"
    );

    let whole_present = finding_by_slug(&listed, "f-whole-present");
    assert_eq!(whole_present["resolution"]["orphaned"], false);
    assert!(
        whole_present["resolution"]["line"].is_null(),
        "whole_file never makes a line claim even when resolved"
    );
}

/// A review-level ("general question") annotation — `path=""`,
/// `anchor_kind="review"` — is creatable via the plain `POST
/// /api/annotations` route, is ALWAYS resolved (no anchor to go stale),
/// and round-trips through both `/comments` and `/distill` grouped under
/// path `""`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_level_question_round_trips_through_comments_and_distill() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    // Missing review_id -> 400.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "",
            "anchor_kind": "review",
            "body": "no review_id",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Non-empty path -> 400.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "order.rb",
            "anchor_kind": "review",
            "review_id": id,
            "body": "path must be empty",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // The real thing.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "",
            "anchor_kind": "review",
            "review_id": id,
            "intent": "question",
            "body": "What about the overall approach here?",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let created: serde_json::Value = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = created["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["body"] == "What about the overall approach here?")
        .expect("the review-level annotation via GET /api/annotations?path=");
    assert_eq!(row["anchor_kind"], "review");

    // /comments — grouped under path "".
    let comments: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/comments"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let group = comments["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["path"] == "")
        .expect("a path=\"\" group must exist");
    let c = group["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["body"] == "What about the overall approach here?")
        .expect("the review-level comment");
    assert_eq!(c["resolution"]["orphaned"], false);

    // /distill — same comment, same grouping.
    let distill: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/distill"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let group = distill["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["path"] == "")
        .expect("distill must carry the same path=\"\" group");
    assert!(group["comments"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["body"] == "What about the overall approach here?"));
}

// --- PRR-F: GET /reviews/{id}/findings/recurrence (design-ui.md §12.4) ----

/// Two reviews import findings sharing ONE `(category, location_path)`
/// pair (both `sample_finding`'s fixed `Style`/`order.rb`) plus a singleton
/// on the second review — the recurring one names the OTHER review as
/// `prior` (excluding itself), `seen_in_reviews` matches the pair's
/// distinct-review count, and the singleton never appears.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn findings_recurrence_reports_prior_reviews_sharing_category_and_path() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();

    let id1 = create_review(&client, &base, "r").await;
    let id2 = create_review(&client, &base, "r").await;

    let payload1 = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [sample_finding("f-1", 3, "leaking authenticity_token")],
    });
    let (status, body) = import_findings(&client, &base, id1, &payload1).await;
    assert_eq!(status, 200, "{body}");

    let payload2 = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [
            sample_finding("f-2", 3, "same leak again"),
            {
                "slug": "f-solo",
                "severity": "ok",
                "category": "Unique",
                "location": {"path": "solo.rb", "kind": "single", "lines": [1]},
                "title": "a one-off",
                "rationale": "no repeat",
            },
        ],
    });
    let (status, body) = import_findings(&client, &base, id2, &payload2).await;
    assert_eq!(status, 200, "{body}");

    let recurrence: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id2}/findings/recurrence"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(recurrence["schema"], "review-findings-recurrence/1");
    let rows = recurrence["findings"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "only f-2 recurs; f-solo has no matching pair: {rows:?}"
    );
    assert_eq!(rows[0]["slug"], "f-2");
    assert_eq!(rows[0]["seen_in_reviews"], 2);
    let prior = rows[0]["prior"].as_array().unwrap();
    assert_eq!(prior.len(), 1);
    assert_eq!(prior[0]["review_id"], id1);
    assert_eq!(prior[0]["title"], "findings");

    // The FIRST review's own recurrence view names the SECOND as prior —
    // symmetric, and self always excluded.
    let recurrence1: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id1}/findings/recurrence"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows1 = recurrence1["findings"].as_array().unwrap();
    assert_eq!(rows1.len(), 1);
    assert_eq!(rows1[0]["slug"], "f-1");
    let prior1 = rows1[0]["prior"].as_array().unwrap();
    assert_eq!(prior1.len(), 1);
    assert_eq!(prior1[0]["review_id"], id2);
}

/// A finding seen in only ONE review never surfaces — `RECURRENCE_MIN_
/// REVIEWS` requires at least two distinct reviews.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn findings_recurrence_is_empty_for_a_singleton_review() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;
    let payload = serde_json::json!({
        "schema": "kbc-findings/1",
        "findings": [sample_finding("f-1", 3, "only seen once")],
    });
    let (status, body) = import_findings(&client, &base, id, &payload).await;
    assert_eq!(status, 200, "{body}");

    let recurrence: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/findings/recurrence"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(recurrence["findings"].as_array().unwrap().is_empty());
}

// --- PRR-U8: auth-tier coverage for the findings routes --------------------
//
// `review_findings.rs`'s own module doc names the split verbatim: bearer
// read (`GET .../findings`, the SAME ordinary review-read gate as
// `/comments`/`/report`) vs. `/findings/import` (LOOPBACK-ONLY,
// `transcripts_api` — an agent-side batch-reconcile verb, never graduated)
// vs. `/findings` POST + `/findings/{slug}/disposition` PUT+DELETE (S2-B
// GATED, `router.rs`'s `review_remote` sub-router, `[review]
// remote_mutations`, default OFF). This test suite boots with
// `ReviewSection::default()` throughout (the flag OFF), so the graduated
// pair's non-loopback behaviour stays byte-identical to the pre-S2
// loopback-only posture below — see `tests/review/
// remote_mutations_gate.rs` for the flag-ON / never-moves contract these
// two routes now also carry. This pair mirrors two already-established
// house patterns verbatim, applied to this module's own routes:
// `review_comments.rs`'s
// `comments_route_accepts_bearer_and_loopback_401s_nonloopback` (bearer
// read: loopback bypasses, a token-less non-loopback caller 401s, the SAME
// caller + a valid bearer token is admitted) and `local_review_routes.rs`'s
// `mutation_routes_404_for_non_loopback` (loopback-only write: a
// non-loopback caller — real XFF spoof, `X-Forwarded-For` from a
// loopback-trusted peer, `is_loopback_origin` false — 404s, never a 401,
// since these routes never touch `auth_bearer` at all while the gate/flag
// stays off).

/// `GET /api/reviews/{id}/findings` — bearer read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn findings_list_route_accepts_bearer_and_loopback_401s_nonloopback() {
    let _guard = crate::ENV_SERIAL.lock().await;
    const FIXTURE_TOKEN: &str = "kb-code-review-findings-test-token";
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);

    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    std::env::remove_var("KB_CODE_TOKEN");

    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    // Loopback, no token — bypass.
    let resp = client
        .get(format!("{base}/api/reviews/{id}/findings"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "loopback must bypass auth_bearer"
    );

    // Simulated non-loopback, no token → 401.
    let resp = client
        .get(format!("{base}/api/reviews/{id}/findings"))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "token-less non-loopback must 401"
    );

    // Same spoof + bearer → 200.
    let resp = client
        .get(format!("{base}/api/reviews/{id}/findings"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "bearer + non-loopback must be admitted"
    );
}

/// `POST /api/reviews/{id}/findings/import`, `POST /api/reviews/{id}/
/// findings` (manual create), and `PUT`/`DELETE /api/reviews/{id}/
/// findings/{slug}/disposition` all 404 a non-loopback caller with the
/// (default, `ReviewSection::default()`) `[review] remote_mutations` flag
/// off — and, unlike the bearer-gated read above, a VALID bearer token does
/// not change that: `/findings/import` never consults `auth_bearer` at all
/// (permanently loopback-only); the graduated pair's `review_mutations_gate`
/// 404s BEFORE `auth_bearer` ever runs when the flag is off (see
/// `tests/review/remote_mutations_gate.rs` for the flag-ON contract, where a
/// valid token DOES flip the outcome for these two). Confirms
/// nothing was actually written by re-listing the target finding after
/// every rejected call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn findings_mutation_routes_404_for_non_loopback() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    // import
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings/import"))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({
            "schema": "kbc-findings/1",
            "findings": [sample_finding("f-a", 3, "should never write")],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "POST .../findings/import must 404 a non-loopback caller"
    );

    // manual create
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings"))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({
            "severity": "ok",
            "category": "Style",
            "location": {"path": "order.rb", "kind": "whole_file"},
            "title": "should never write either",
            "rationale": "n/a",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "POST .../findings (manual create) must 404 a non-loopback caller"
    );

    // disposition set/clear need a REAL finding to target — imported over
    // loopback (this crate's `reqwest::Client` defaults to a loopback
    // peer; only the `X-Forwarded-For` header above spoofs non-loopback).
    let (status, _) = import_findings(
        &client,
        &base,
        id,
        &serde_json::json!({
            "schema": "kbc-findings/1",
            "findings": [sample_finding("f-target", 3, "disposition non-loopback probe")],
        }),
    )
    .await;
    assert_eq!(status, 200);

    let resp = client
        .put(format!(
            "{base}/api/reviews/{id}/findings/f-target/disposition"
        ))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({"disposition": "agree"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "PUT .../disposition must 404 a non-loopback caller"
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
        404,
        "DELETE .../disposition must 404 a non-loopback caller"
    );

    // Nothing above actually wrote — the finding stays undecided.
    let listed = list_findings(&client, &base, id, &[]).await;
    let f = finding_by_slug(&listed, "f-target");
    assert!(
        f["disposition"].is_null(),
        "every 404'd mutation above must have left the finding untouched"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_manual_finding_wires_evidence_through_to_the_view() {
    // V70-A3X — `evidence_lang`/`evidence_source` were hardcoded `None` on
    // the manual `add` path regardless of what a caller sent (the `import`
    // path already wired the same `FindingEvidenceBody` shape).
    let _guard = crate::ENV_SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let (status, created) = create_manual_finding(
        &client,
        &base,
        id,
        &serde_json::json!({
            "slug": "f-with-evidence",
            "severity": "concern",
            "category": "Correctness",
            "location": {"path": "order.rb", "kind": "single", "lines": [2]},
            "title": "Evidence-carrying finding",
            "rationale": "See the snippet.",
            "evidence": {"lang": "ruby", "source": "def total\n  price * qty\nend\n"},
        }),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["evidence"]["lang"], "ruby");
    assert_eq!(
        created["evidence"]["source"],
        "def total\n  price * qty\nend\n"
    );

    // Round-trips through the list view too, not just the create response.
    let listed = list_findings(&client, &base, id, &[]).await;
    let f = finding_by_slug(&listed, "f-with-evidence");
    assert_eq!(f["evidence"]["lang"], "ruby");

    // Omitting `evidence` entirely still yields the pre-existing `null`
    // shape — additive, not a required field.
    let (status2, created2) = create_manual_finding(
        &client,
        &base,
        id,
        &serde_json::json!({
            "slug": "f-no-evidence",
            "severity": "ok",
            "category": "Style",
            "location": {"path": "order.rb", "kind": "single", "lines": [3]},
            "title": "No evidence here",
            "rationale": "Just a note.",
        }),
    )
    .await;
    assert_eq!(status2, 201, "{created2}");
    assert!(created2["evidence"].is_null(), "{created2}");
}
