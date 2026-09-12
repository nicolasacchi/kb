//! V72-H4a — the `aug-lane/1` ingest + facts routes over real HTTP.
//!
//! What these pin, all of them properties a unit test on the pure
//! functions cannot reach:
//!
//! * a lane the operator has not enabled REFUSES an ingest (4xx with a
//!   reason naming the config key), and an unknown lane is a 404 — a lane
//!   is never enabled by a request;
//! * a derived lane refuses too: `git.behavior` has no ingest at all;
//! * the fact cap REFUSES with the counts rather than truncating;
//! * a path outside the repository root is refused BY ROW, counted and
//!   named, while the rest of the batch still lands (the `read_repo_file`
//!   containment floor);
//! * an ingest is a POST under `/api`, so it appears in the V0027
//!   mutations ledger with its route and its `?repo=` — automatically,
//!   via the crate-wide `audit_mutations` middleware;
//! * the classing ladder end to end over a REAL repo: `exact` on the
//!   blob the tool named, `likely` after an unrelated edit above the
//!   line, `orphan` once the anchored text is gone.

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, LanesSection, RepoEntry};
use kb_core::paths::KbPaths;
use serde_json::{json, Value};
use std::path::Path;

use crate::common::git;

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(dir.join("app")).unwrap();
    std::fs::write(dir.join("app/widget.rb"), "one\ntwo\nthree\nfour\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

async fn boot(path: &Path, enabled: &[&str]) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let base = boot_at(tmp.path(), path, enabled).await;
    (tmp, base)
}

/// Boot against an EXISTING state dir — what
/// `a_disabled_lanes_stored_facts_are_withheld_and_counted` needs, since
/// enablement is boot-time config and the point is that the SAME store
/// reads differently under a different `[lanes]`.
async fn boot_at(state: &Path, path: &Path, enabled: &[&str]) -> String {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fx".to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        lanes: LanesSection {
            enabled: enabled.iter().map(|s| s.to_string()).collect(),
            retention_days: Default::default(),
        },
        ..KbCodeConfig::default()
    };
    let paths = KbPaths::rooted_at(state, "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    format!("http://{addr}")
}

async fn ingest(base: &str, lane: &str, body: &Value) -> (reqwest::StatusCode, Value) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/lanes/{lane}/ingest"))
        .query(&[("repo", "fx")])
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn get(base: &str, path: &str, query: &[(&str, &str)]) -> Value {
    reqwest::Client::new()
        .get(format!("{base}{path}"))
        .query(query)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn one_fact(path: &str, line: u32) -> Value {
    json!({
        "path": path,
        "range": [line, line],
        "kind": "diagnostic",
        "value": {"cop": "Style/X", "message": "m"},
        "severity": "warning"
    })
}

fn run_block() -> Value {
    json!({"tool": "rubocop", "tool_version": "1.66.1", "argv_redacted": "rubocop --format json"})
}

#[tokio::test]
async fn a_lane_that_is_not_enabled_refuses_an_ingest_and_says_which_key_turns_it_on() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path(), &[]).await;
    let (status, body) = ingest(
        &base,
        "rubocop",
        &json!({"schema": "lane-ingest/1", "run": run_block(), "facts": []}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    assert_eq!(body["reason"].as_str(), Some("lane-disabled"));
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("[lanes] enabled"),
        "{body}"
    );
}

#[tokio::test]
async fn an_unknown_lane_is_a_404_and_a_derived_lane_has_no_ingest() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path(), &["git.behavior"]).await;

    let (status, _) = ingest(
        &base,
        "no.such.lane",
        &json!({"run": run_block(), "facts": []}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

    let (status, body) = ingest(
        &base,
        "git.behavior",
        &json!({"run": run_block(), "facts": []}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(body["reason"].as_str(), Some("lane-not-ingestable"));
}

#[tokio::test]
async fn the_fact_cap_refuses_with_counts_rather_than_truncating() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path(), &["rubocop"]).await;
    let cap = kb_code_server::lanes::ingest::MAX_FACTS_PER_RUN;
    let facts: Vec<Value> = (0..cap + 1)
        .map(|i| one_fact("app/widget.rb", (i % 4) as u32 + 1))
        .collect();
    let (status, body) = ingest(
        &base,
        "rubocop",
        &json!({"run": run_block(), "facts": facts}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["reason"].as_str(), Some("lane-facts-cap"));
    let msg = body["error"].as_str().unwrap_or("");
    assert!(msg.contains(&(cap + 1).to_string()), "{msg}");
    assert!(msg.contains("refused, not"), "{msg}");
    // Nothing landed.
    let facts = get(
        &base,
        "/api/lanes/facts",
        &[("repo", "fx"), ("path", "app/widget.rb")],
    )
    .await;
    assert_eq!(facts["returned"].as_u64(), Some(0));
}

#[tokio::test]
async fn a_path_outside_the_repo_is_refused_by_row_while_the_batch_lands() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path(), &["rubocop"]).await;
    let (status, body) = ingest(
        &base,
        "rubocop",
        &json!({
            "run": run_block(),
            "facts": [
                one_fact("app/widget.rb", 2),
                one_fact("../../etc/passwd", 1),
                one_fact("/etc/shadow", 1),
                one_fact("app/missing.rb", 1),
            ]
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["accepted"].as_u64(), Some(1));
    assert_eq!(body["refused"].as_u64(), Some(3));
    let refusals = body["refusals"].as_array().unwrap();
    let paths: Vec<&str> = refusals
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"../../etc/passwd"), "{body}");
    assert!(paths.contains(&"/etc/shadow"), "{body}");
    // A path INSIDE the root that simply does not exist is refused too —
    // there is no readable blob to anchor a fact to — but for a different,
    // stated reason.
    assert!(paths.contains(&"app/missing.rb"), "{body}");
}

#[tokio::test]
async fn an_ingest_lands_in_the_mutations_ledger_with_its_route_and_repo() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path(), &["rubocop"]).await;
    ingest(
        &base,
        "rubocop",
        &json!({"run": run_block(), "facts": [one_fact("app/widget.rb", 2)]}),
    )
    .await;
    let audit = get(&base, "/api/audit", &[]).await;
    let rows = audit["entries"].as_array().expect("entries");
    let row = rows
        .iter()
        .find(|r| r["route"].as_str() == Some("/api/lanes/rubocop/ingest"))
        .unwrap_or_else(|| panic!("no audit row for the ingest: {audit}"));
    assert_eq!(row["method"].as_str(), Some("POST"));
    assert_eq!(row["repo"].as_str(), Some("fx"));
    assert_eq!(row["admission"].as_str(), Some("loopback"));
    assert_eq!(row["outcome"].as_str(), Some("200"));
}

#[tokio::test]
async fn classing_walks_exact_then_likely_then_orphan_as_the_file_changes() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path(), &["rubocop"]).await;
    let file = repo.path().join("app/widget.rb");

    // The blob the fact will name: whatever `GET /api/file` reports now.
    let f = get(
        &base,
        "/api/file",
        &[("repo", "fx"), ("path", "app/widget.rb")],
    )
    .await;
    let blob = f["blob_hash"].as_str().unwrap().to_string();

    let mut fact = one_fact("app/widget.rb", 2);
    fact["blob_sha"] = json!(blob);
    let (status, body) = ingest(
        &base,
        "rubocop",
        &json!({"run": run_block(), "facts": [fact]}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["anchored"].as_u64(), Some(1), "{body}");
    assert_eq!(body["sha_source"]["tool"].as_u64(), Some(1), "{body}");

    let facts = get(
        &base,
        "/api/lanes/facts",
        &[("repo", "fx"), ("path", "app/widget.rb")],
    )
    .await;
    assert_eq!(
        facts["facts"][0]["class"].as_str(),
        Some("exact"),
        "{facts}"
    );
    assert_eq!(facts["facts"][0]["reason"].as_str(), Some("blob-current"));
    assert_eq!(facts["facts"][0]["line"].as_u64(), Some(2));

    // An unrelated edit ABOVE the anchored line: the blob moves and the
    // line shifts, so the fact carries forward at `likely`.
    std::fs::write(&file, "zero\none\ntwo\nthree\nfour\n").unwrap();
    let facts = get(
        &base,
        "/api/lanes/facts",
        &[("repo", "fx"), ("path", "app/widget.rb")],
    )
    .await;
    assert_eq!(
        facts["facts"][0]["class"].as_str(),
        Some("likely"),
        "an edit must degrade the class, never keep it exact: {facts}"
    );
    assert_eq!(
        facts["facts"][0]["reason"].as_str(),
        Some("reanchored-exact")
    );
    assert_eq!(facts["facts"][0]["line"].as_u64(), Some(3));
    assert_eq!(facts["facts"][0]["shifted"].as_bool(), Some(true));

    // The anchored text is deleted: an honest orphan that keeps its
    // original line, never a guess onto a nearby one.
    std::fs::write(&file, "zero\none\nthree\nfour\n").unwrap();
    let facts = get(
        &base,
        "/api/lanes/facts",
        &[("repo", "fx"), ("path", "app/widget.rb")],
    )
    .await;
    assert_eq!(
        facts["facts"][0]["class"].as_str(),
        Some("orphan"),
        "{facts}"
    );
    assert_eq!(facts["facts"][0]["reason"].as_str(), Some("no-anchor"));

    // And once the file is gone entirely.
    std::fs::remove_file(&file).unwrap();
    let facts = get(
        &base,
        "/api/lanes/facts",
        &[("repo", "fx"), ("path", "app/widget.rb")],
    )
    .await;
    assert_eq!(
        facts["facts"][0]["class"].as_str(),
        Some("orphan"),
        "{facts}"
    );
    assert_eq!(facts["facts"][0]["reason"].as_str(), Some("path-gone"));
}

#[tokio::test]
async fn a_fact_with_no_tool_named_blob_can_never_reach_exact() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path(), &["rubocop"]).await;
    let (status, body) = ingest(
        &base,
        "rubocop",
        &json!({"run": run_block(), "facts": [one_fact("app/widget.rb", 2)]}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["sha_source"]["mirror_at_ingest"].as_u64(), Some(1));

    let facts = get(
        &base,
        "/api/lanes/facts",
        &[("repo", "fx"), ("path", "app/widget.rb")],
    )
    .await;
    assert_eq!(
        facts["facts"][0]["class"].as_str(),
        Some("likely"),
        "a blob the DAEMON attributed is not a blob the tool named: {facts}"
    );
    assert_eq!(
        facts["facts"][0]["reason"].as_str(),
        Some("blob-current-sha-attributed")
    );
}

#[tokio::test]
async fn a_re_ingest_replaces_and_clear_paths_erase_a_fixed_offense() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path(), &["rubocop"]).await;
    ingest(
        &base,
        "rubocop",
        &json!({"run": run_block(), "facts": [one_fact("app/widget.rb", 2)]}),
    )
    .await;
    // Re-running the same tool must not double-report.
    ingest(
        &base,
        "rubocop",
        &json!({"run": run_block(), "facts": [one_fact("app/widget.rb", 2)]}),
    )
    .await;
    let facts = get(
        &base,
        "/api/lanes/facts",
        &[("repo", "fx"), ("path", "app/widget.rb")],
    )
    .await;
    assert_eq!(facts["returned"].as_u64(), Some(1), "{facts}");

    // The offense is fixed: the tool inspected the file and said nothing,
    // which `clear_paths` is what expresses.
    let (status, body) = ingest(
        &base,
        "rubocop",
        &json!({
            "run": run_block(),
            "facts": [],
            "clear_paths": ["app/widget.rb"]
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["cleared"].as_u64(), Some(1));
    let facts = get(
        &base,
        "/api/lanes/facts",
        &[("repo", "fx"), ("path", "app/widget.rb")],
    )
    .await;
    assert_eq!(facts["returned"].as_u64(), Some(0), "{facts}");
    assert_eq!(
        facts["absent"][0]["lane"].as_str(),
        Some("rubocop"),
        "an enabled lane with nothing to say says so: {facts}"
    );
}

#[tokio::test]
async fn a_disabled_lanes_stored_facts_are_withheld_and_counted() {
    let repo = fixture_repo();
    let state = tempfile::tempdir().unwrap();
    let base = boot_at(state.path(), repo.path(), &["rubocop"]).await;
    ingest(
        &base,
        "rubocop",
        &json!({"run": run_block(), "facts": [one_fact("app/widget.rb", 2)]}),
    )
    .await;

    // The SAME store, read by a daemon whose config no longer enables the
    // lane: the rows are still there and are deliberately not served.
    let off = boot_at(state.path(), repo.path(), &[]).await;
    let facts = get(
        &off,
        "/api/lanes/facts",
        &[("repo", "fx"), ("path", "app/widget.rb")],
    )
    .await;
    assert_eq!(facts["returned"].as_u64(), Some(0), "{facts}");
    assert_eq!(
        facts["withheld_disabled"].as_u64(),
        Some(1),
        "a withheld fact is COUNTED, never a silently shorter list: {facts}"
    );
    assert!(
        facts["absent"].as_array().unwrap().is_empty(),
        "a disabled lane is not 'absent', it is off: {facts}"
    );
}

#[tokio::test]
async fn the_registry_route_lists_every_row_with_its_enablement() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path(), &["rubocop", "sarif.brakeman", "nope"]).await;
    let body = get(&base, "/api/lanes", &[("repo", "fx")]).await;
    assert_eq!(body["schema"].as_str(), Some("aug-lane/1"));
    let lanes = body["lanes"].as_array().unwrap();
    let by_id = |id: &str| -> Value {
        lanes
            .iter()
            .find(|l| l["id"].as_str() == Some(id))
            .cloned()
            .unwrap_or_else(|| panic!("no lane {id} in {body}"))
    };
    assert_eq!(by_id("rubocop")["enabled"].as_bool(), Some(true));
    assert_eq!(by_id("git.behavior")["enabled"].as_bool(), Some(false));
    assert_eq!(by_id("sarif.*")["family"].as_bool(), Some(true));
    assert_eq!(by_id("sarif.*")["enabled"].as_bool(), Some(false));
    assert_eq!(by_id("sarif.brakeman")["enabled"].as_bool(), Some(true));
    assert_eq!(
        body["unknown_enabled"].as_array().unwrap(),
        &vec![Value::from("nope")],
        "a typo in [lanes] enabled is named, not ignored"
    );
}

#[tokio::test]
async fn the_derived_lane_answers_through_the_same_facts_route() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path(), &["git.behavior"]).await;
    let body = get(
        &base,
        "/api/lanes/facts",
        &[("repo", "fx"), ("path", "app/widget.rb")],
    )
    .await;
    let kinds: Vec<&str> = body["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"churn"), "{body}");
    assert!(kinds.contains(&"last_touch"), "{body}");
    let last = body["facts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["kind"].as_str() == Some("last_touch"))
        .unwrap();
    assert_eq!(last["value"]["author_kind"].as_str(), Some("human"));
    assert_eq!(
        last["class"].as_str(),
        Some("likely"),
        "a human call rests on an ABSENT trailer, which is not evidence: {last}"
    );
    let churn = body["facts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["kind"].as_str() == Some("churn"))
        .unwrap();
    assert_eq!(churn["class"].as_str(), Some("exact"));
    assert_eq!(churn["run"]["origin"].as_str(), Some("daemon"));
}

#[tokio::test]
async fn the_summary_route_states_its_own_bound() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path(), &["rubocop"]).await;
    ingest(
        &base,
        "rubocop",
        &json!({
            "run": run_block(),
            "facts": [one_fact("app/widget.rb", 2), one_fact("app/widget.rb", 3)]
        }),
    )
    .await;
    let body = get(&base, "/api/lanes/summary", &[("repo", "fx")]).await;
    assert_eq!(body["buckets"][0]["lane"].as_str(), Some("rubocop"));
    assert_eq!(body["buckets"][0]["kind"].as_str(), Some("diagnostic"));
    assert_eq!(body["buckets"][0]["count"].as_i64(), Some(2));
    assert_eq!(body["scanned"].as_i64(), Some(2));
    assert_eq!(
        body["scan_cap"].as_u64(),
        Some(kb_code_server::store::LANE_SUMMARY_SCAN_CAP as u64)
    );
    assert_eq!(body["capped"].as_bool(), Some(false));
}
