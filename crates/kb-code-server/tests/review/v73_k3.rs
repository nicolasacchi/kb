//! V73-K3 — HTTP+store wiring for the four surfaces this unit adds:
//! `kbc-claim/1` (`/api/claims`), `kbc-pseudo/1`
//! (`/api/reviews/{id}/pseudo[/{name}]`), the widened
//! `review-timeline/2`, and the hunk↔turn join
//! (`/api/reviews/{id}/hunks/{hunk}/turns`).
//!
//! The pure logic is unit-tested next to each module (`claims`,
//! `review_pseudo`, `review_hunks`, `review_turns`, `review_timeline`,
//! `review_legacy`); these tests exist to prove the WIRING — routing,
//! posture, store round-trip, and the ladder-through-the-wire path a ref
//! and a comment take onto a pseudo-file.
//!
//! Boot pattern mirrors `review_inbox_timeline.rs` (each e2e file in this
//! crate carries its own small helper set — see `review_routes.rs`'s own
//! doc for why).

use kb_code_server::config::{
    KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection, TranscriptsSection,
};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::common::git;

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

struct Boot {
    _tmp: tempfile::TempDir,
    base: String,
}

async fn boot(repos: Vec<RepoEntry>, transcripts: Option<TranscriptsSection>) -> Boot {
    let cfg = KbCodeConfig {
        repos,
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        review: ReviewSection::default(),
        transcripts: transcripts.unwrap_or(TranscriptsSection {
            enabled: false,
            root: "/nonexistent".into(),
            exclude_projects: Vec::new(),
            index_thinking: false,
        }),
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    Boot {
        _tmp: tmp,
        base: format!("http://{addr}"),
    }
}

/// A repo whose feature branch replaces one long, unmistakable line — long
/// enough to clear `review_turns::MIN_MATCH_BYTES`, which is the whole
/// point of the fixture.
const OLD_LINE: &str = "    total = items.sum { |i| i.price } # the legacy float total";
const NEW_LINE: &str = "    total = items.sum { |i| i.price_cents } # integer cents now";

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(
        dir.join("order.rb"),
        format!("class Order\n  def total\n{OLD_LINE}\n  end\nend\n"),
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(
        dir.join("order.rb"),
        format!("class Order\n  def total\n{NEW_LINE}\n  end\nend\n"),
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "cents"]);
    tmp
}

async fn create_review(client: &reqwest::Client, base: &str, repo: &str) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": repo,
            "head_ref": "feature",
            "base_ref": "main",
            "title": "cents",
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

fn repo_entry(dir: &Path) -> RepoEntry {
    RepoEntry {
        name: "fixture".to_string(),
        path: std::fs::canonicalize(dir).unwrap(),
    }
}

// --- kbc-claim/1 -----------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_round_trips_and_rides_the_ladder() {
    let repo = fixture_repo();
    let b = boot(vec![repo_entry(repo.path())], None).await;
    let client = reqwest::Client::new();

    // A claim with no blob is honestly UNANCHORED, not a failure.
    let resp = client
        .post(format!("{}/api/claims", b.base))
        .json(&serde_json::json!({
            "schema": "kbc-claim/1",
            "repo": "fixture",
            "subject_kind": "path",
            "subject": "order.rb",
            "kind": "explain",
            "body_md": "cents everywhere; the float column stays for one release.",
            "confidence": 0.7,
            "evidence": ["code:order.rb:3", "finding:f-1"],
            "session_id": "sess-1",
            "model": "claude",
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
    let created: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(created["state"], "unanchored");
    assert_eq!(created["confidence"], 0.7);
    assert_eq!(created["evidence"][0], "code:order.rb:3");
    let id = created["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("clm_"), "{id}");

    // A claim pinned to a blob that is NOT the file's current one is
    // DRIFTED, with both shas in the caption — never re-anchored, never
    // hidden.
    let resp = client
        .post(format!("{}/api/claims", b.base))
        .json(&serde_json::json!({
            "schema": "kbc-claim/1",
            "repo": "fixture",
            "subject_kind": "path",
            "subject": "order.rb",
            "kind": "decision",
            "body_md": "we chose integer cents over a Money gem.",
            "blob_sha": "0000000000000000000000000000000000000000",
        }))
        .send()
        .await
        .unwrap();
    let drifted: serde_json::Value = resp.json().await.unwrap();
    // With no indexed blob for the path yet, the honest answer is still
    // `drifted` — the claim named bytes this daemon cannot confirm.
    assert_eq!(drifted["state"], "drifted");
    assert!(drifted["caption"]
        .as_str()
        .unwrap()
        .contains("0000000000000000000000000000000000000000"));

    // The list read: true total, and every row carries its own state.
    let body: serde_json::Value = client
        .get(format!("{}/api/claims?repo=fixture&path=order.rb", b.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["schema"], "kbc-claim/1");
    assert_eq!(body["total"], 2);
    assert_eq!(body["returned"], 2);
    assert_eq!(body["claims"].as_array().unwrap().len(), 2);

    // Paging reports what it is a page OF.
    let page: serde_json::Value = client
        .get(format!("{}/api/claims?repo=fixture&limit=1", b.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["total"], 2);
    assert_eq!(page["returned"], 1);

    // The single read.
    let one: serde_json::Value = client
        .get(format!("{}/api/claims/{id}", b.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(one["id"], id.as_str());
    assert_eq!(one["kind"], "explain");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claim_outside_the_closed_vocabularies_is_refused_by_name() {
    let repo = fixture_repo();
    let b = boot(vec![repo_entry(repo.path())], None).await;
    let client = reqwest::Client::new();

    for (patch, needle) in [
        (serde_json::json!({"kind": "vibes"}), "kind must be one of"),
        (
            serde_json::json!({"subject_kind": "planet"}),
            "subject_kind must be one of",
        ),
        (serde_json::json!({"confidence": 2.0}), "confidence must be"),
        (
            serde_json::json!({"evidence": ["Order"]}),
            "names no kbc scheme",
        ),
        (serde_json::json!({"evidence": ["code:"]}), "is malformed"),
        (serde_json::json!({"body_md": "   "}), "must not be empty"),
        (
            serde_json::json!({"schema": "kbc-claim/2"}),
            "schema must be",
        ),
    ] {
        let mut payload = serde_json::json!({
            "schema": "kbc-claim/1",
            "repo": "fixture",
            "subject_kind": "path",
            "subject": "order.rb",
            "kind": "note",
            "body_md": "prose",
        });
        for (k, v) in patch.as_object().unwrap() {
            payload[k] = v.clone();
        }
        let resp = client
            .post(format!("{}/api/claims", b.base))
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{patch} should be refused"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(
            body["error"].as_str().unwrap_or("").contains(needle),
            "{patch}: expected {needle:?}, got {body}"
        );
    }
}

// --- kbc-pseudo/1 ----------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_four_pseudo_files_always_exist_and_carry_real_blob_hashes() {
    let repo = fixture_repo();
    let b = boot(vec![repo_entry(repo.path())], None).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &b.base, "fixture").await;

    let body: serde_json::Value = client
        .get(format!("{}/api/reviews/{id}/pseudo", b.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["schema"], "kbc-pseudo/1");
    let files = body["files"].as_array().unwrap();
    let names: Vec<&str> = files.iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec!["pr-body.md", "review.md", "findings.json", "commits.md"]
    );
    for f in files {
        assert!(f["path"].as_str().unwrap().starts_with("~review/"));
        assert_eq!(f["blob_sha"].as_str().unwrap().len(), 40, "a git blob hash");
        assert!(f["content"].is_null(), "the LIST read carries no bytes");
        if !f["present"].as_bool().unwrap() {
            assert!(f["reason"].is_string(), "an empty file always says why");
        }
    }

    // The commit list is really the range's commits, with their trailers.
    let one: serde_json::Value = client
        .get(format!("{}/api/reviews/{id}/pseudo/commits.md", b.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let f = &one["file"];
    assert!(f["present"].as_bool().unwrap());
    let content = f["content"].as_str().unwrap();
    assert!(content.contains("# Commits"));
    assert!(content.contains("cents"), "{content}");
    assert!(content.contains("trailers"));
    // The hash the LIST reported is the hash of the bytes served here.
    let listed = files.iter().find(|x| x["name"] == "commits.md").unwrap();
    assert_eq!(listed["blob_sha"], f["blob_sha"]);

    // An unknown name 404s NAMING the closed set.
    let resp = client
        .get(format!("{}/api/reviews/{id}/pseudo/nope.md", b.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    let err: serde_json::Value = resp.json().await.unwrap();
    assert!(err["error"].as_str().unwrap().contains("pr-body.md"));

    // An unbound review's PR body is EMPTY with a reason, not absent.
    let pr: serde_json::Value = client
        .get(format!("{}/api/reviews/{id}/pseudo/pr-body.md", b.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!pr["file"]["present"].as_bool().unwrap());
    assert!(pr["file"]["reason"]
        .as_str()
        .unwrap()
        .contains("not bound to a pull request"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_comment_on_a_pseudo_file_carries_forward_through_the_same_ladder() {
    let repo = fixture_repo();
    let b = boot(vec![repo_entry(repo.path())], None).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &b.base, "fixture").await;

    // Compose a document so `~review/review.md` has bytes with a stable
    // line to anchor to.
    let doc_md = "---\nschema: kbc-review/1\nsummary_md: |\n  Cents, everywhere.\n---\n\n\
                  # Review\n\nA LINE WORTH ANCHORING TO.\n";
    let resp = client
        .post(format!("{}/api/reviews/{id}/compose", b.base))
        .json(&serde_json::json!({
            "doc_md": doc_md,
            "tier": "minimal",
            "findings_v2": [],
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

    let one: serde_json::Value = client
        .get(format!("{}/api/reviews/{id}/pseudo/review.md", b.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let file = &one["file"];
    assert!(file["present"].as_bool().unwrap());
    assert_eq!(file["content"].as_str().unwrap(), doc_md);
    let blob = file["blob_sha"].as_str().unwrap().to_string();

    // The SAME hash a `code:` ref would pin against — a git blob hash of
    // the served bytes, computed the way every real file's is.
    assert_eq!(
        blob,
        kb_code_server::ingest::git_blob_hash(doc_md.as_bytes()),
        "a pseudo-file's hash must be a real git blob hash, or a `@sha` ref \
         could never be `pinned`"
    );

    // A `code:` ref to a pseudo-file resolves through the SAME card ladder
    // a tracked path takes. `~review/review.md` HAS bytes (the compose
    // above), so the ref is `pinned` at the line the author cited.
    let line = doc_md
        .lines()
        .position(|l| l.contains("ANCHORING"))
        .expect("the fixture line")
        + 1;
    let doc_with_ref = format!(
        "---\nschema: kbc-review/1\nsummary_md: |\n  Cents, everywhere.\n---\n\n\
         # Review\n\nA LINE WORTH ANCHORING TO.\n\n\
         See [[code:~review/review.md:{line}]].\n"
    );
    let resp = client
        .post(format!("{}/api/reviews/{id}/compose", b.base))
        .json(&serde_json::json!({
            "doc_md": doc_with_ref,
            "tier": "minimal",
            "findings_v2": [],
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

    let resolved: serde_json::Value = client
        .get(format!("{}/api/reviews/{id}/doc?resolve=true", b.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cards = resolved["cards"].as_array().unwrap();
    let card = cards
        .iter()
        .find(|c| {
            c["ref"]
                .as_str()
                .unwrap_or("")
                .contains("~review/review.md")
        })
        .unwrap_or_else(|| panic!("no pseudo card in {cards:#?}"));
    assert_eq!(card["state"], "pinned");
    assert_eq!(card["line"].as_u64(), Some(line as u64));
    assert!(
        card["snippet"].as_str().unwrap().contains("ANCHORING"),
        "{card:#?}"
    );
    // The hash the card reports IS the git blob hash of the served bytes —
    // one notion of identity, shared with every tracked file.
    assert_eq!(card["current_blob"].as_str().unwrap().len(), 40);

    // …and an EMPTY pseudo-file is an honest ORPHAN that FAILS the lint,
    // with nothing written. An absent PR description must not silently
    // resolve to an empty card.
    let doc_bad_ref = "---\nschema: kbc-review/1\nsummary_md: |\n  Cents.\n---\n\n\
                       See [[code:~review/pr-body.md:1]].\n";
    let resp = client
        .post(format!("{}/api/reviews/{id}/compose", b.base))
        .json(&serde_json::json!({
            "doc_md": doc_bad_ref,
            "tier": "minimal",
            "findings_v2": [],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    let rows = body["lint"]["rows"].as_array().unwrap();
    assert!(
        rows.iter().any(|r| r["rule"] == "ref_orphan"
            && r["message"]
                .as_str()
                .unwrap_or("")
                .contains("not bound to a pull request")),
        "{body:#?}"
    );
}

// --- review-timeline/2 -----------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_timeline_merges_every_lane_and_reports_each_one_honestly() {
    let repo = fixture_repo();
    let b = boot(vec![repo_entry(repo.path())], None).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &b.base, "fixture").await;

    client
        .post(format!("{}/api/reviews/{id}/compose", b.base))
        .json(&serde_json::json!({
            "doc_md": "---\nschema: kbc-review/1\nsummary_md: |\n  Cents.\n---\n\nbody\n",
            "tier": "minimal",
            "findings_v2": [{
                "severity": "concern",
                "category": "correctness",
                "title": "the float column is stale",
                "rationale": "after the backfill",
                "location": {"path": "order.rb", "kind": "whole_file"},
            }],
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    client
        .post(format!("{}/api/claims", b.base))
        .json(&serde_json::json!({
            "schema": "kbc-claim/1",
            "repo": "fixture",
            "subject_kind": "review",
            "subject": id.to_string(),
            "kind": "story",
            "body_md": "why this branch exists",
            "review_id": id,
            "model": "claude",
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let body: serde_json::Value = client
        .get(format!("{}/api/reviews/{id}/timeline", b.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["schema"], "review-timeline/2");

    let events = body["events"].as_array().unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"review_created"), "{kinds:?}");
    assert!(kinds.contains(&"patchset"), "{kinds:?}");
    assert!(kinds.contains(&"findings_import"), "{kinds:?}");
    assert!(kinds.contains(&"doc_revision"), "{kinds:?}");
    assert!(kinds.contains(&"report"), "{kinds:?}");
    assert!(kinds.contains(&"claim"), "{kinds:?}");

    // Ascending, and every event carries the v2 envelope beside its v1
    // payload.
    let mut last = i64::MIN;
    for e in events {
        let at = e["at"].as_i64().unwrap();
        assert!(at >= last, "not ascending: {events:#?}");
        last = at;
        assert_eq!(e["ts"], e["at"]);
        assert!(e["author"]["kind"].is_string());
        assert!(e["lane"].is_string());
    }

    // Every lane reports its own state; a lane that did not run says why.
    let sources = body["sources"].as_array().unwrap();
    let by_lane: std::collections::HashMap<&str, &serde_json::Value> = sources
        .iter()
        .map(|s| (s["lane"].as_str().unwrap(), s))
        .collect();
    for lane in [
        "lifecycle",
        "findings",
        "verdict",
        "comments",
        "pr_body",
        "document",
        "report",
        "wt_comments",
        "claims",
        "github",
        "turns",
    ] {
        let s = by_lane.get(lane).unwrap_or_else(|| panic!("lane {lane}"));
        if s["state"] != "ok" {
            assert!(
                s["reason"].is_string(),
                "lane {lane} is {} with no reason",
                s["state"]
            );
        }
    }
    assert_eq!(by_lane["github"]["state"], "skipped", "no PR binding");
    assert_eq!(by_lane["turns"]["state"], "skipped", "no ?hunk=");

    // Filters narrow the view and report the true total.
    let filtered: serde_json::Value = client
        .get(format!("{}/api/reviews/{id}/timeline?kind=claim", b.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(filtered["total"], 1);
    assert_eq!(filtered["events"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["events"][0]["kind"], "claim");
    assert_eq!(filtered["events"][0]["author"]["kind"], "agent");

    let by_author: serde_json::Value = client
        .get(format!(
            "{}/api/reviews/{id}/timeline?author=system",
            b.base
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(by_author["total"].as_i64().unwrap() >= 2);
    for e in by_author["events"].as_array().unwrap() {
        assert_eq!(e["author"]["kind"], "system");
    }

    // Paging: `total` is the count it is a page OF.
    let paged: serde_json::Value = client
        .get(format!("{}/api/reviews/{id}/timeline?limit=1", b.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(paged["returned"], 1);
    assert!(paged["total"].as_i64().unwrap() > 1);

    // An unknown kind is a 400 naming the vocabulary, never an empty page.
    let resp = client
        .get(format!("{}/api/reviews/{id}/timeline?kind=nope", b.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let err: serde_json::Value = resp.json().await.unwrap();
    assert!(err["error"].as_str().unwrap().contains("review_created"));
}

// --- the hunk↔turn join ----------------------------------------------------

/// A transcript whose ONE `Edit` reproduces the fixture's hunk exactly, and
/// whose commit trailer joins it to the feature commit.
fn seed_edit_session(root: &Path, session_id: &str, repo_dir: &Path) {
    let proj = root.join("-fixture-project");
    std::fs::create_dir_all(&proj).unwrap();
    let abs = serde_json::to_string(&repo_dir.join("order.rb").display().to_string()).unwrap();
    let old = serde_json::to_string(OLD_LINE).unwrap();
    let new = serde_json::to_string(NEW_LINE).unwrap();
    let lines = [
        format!(
            r#"{{"type":"user","uuid":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","parentUuid":null,"sessionId":"{session_id}","timestamp":"2026-07-17T10:00:00.000Z","isSidechain":false,"message":{{"role":"user","content":"move to cents"}}}}"#
        ),
        format!(
            r#"{{"type":"assistant","uuid":"11111111-2222-3333-4444-555555555555","parentUuid":null,"sessionId":"{session_id}","timestamp":"2026-07-17T10:00:01.000Z","isSidechain":false,"message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Edit","input":{{"file_path":{abs},"old_string":{old},"new_string":{new}}}}}]}}}}"#
        ),
    ];
    std::fs::write(
        proj.join(format!("{session_id}.jsonl")),
        lines.join("\n") + "\n",
    )
    .unwrap();
}

async fn wait_for_transcript_turns(base: &str, expected: u64) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let body: serde_json::Value = client
            .get(format!("{base}/api/transcripts/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if body["turns"].as_u64().unwrap_or(0) >= expected || Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Mint the hunk address the way the SPA does — through the Rust mirror,
/// over the same `git diff` the daemon will re-derive.
fn hunk_id_for(repo_dir: &Path) -> String {
    let base = git_out(repo_dir, &["rev-parse", "main"]);
    let tip = git_out(repo_dir, &["rev-parse", "feature"]);
    let text = git_out(
        repo_dir,
        &["diff", "--no-color", "-U3", &base, &tip, "--", "order.rb"],
    );
    let parsed = kb_code_server::review_hunks::parse_unified_diff(&format!("{text}\n"));
    assert_eq!(parsed.hunks.len(), 1, "{text}");
    kb_code_server::review_hunks::hunk_id("order.rb", &parsed.hunks[0])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hunk_turn_join_claims_the_edit_that_produced_the_hunk() {
    let repo = fixture_repo();
    let transcripts_tmp = tempfile::tempdir().unwrap();
    let transcripts_root = transcripts_tmp.path().join("transcripts");
    std::fs::create_dir_all(&transcripts_root).unwrap();
    let canonical = std::fs::canonicalize(repo.path()).unwrap();
    seed_edit_session(&transcripts_root, "sess-cents", &canonical);

    let b = boot(
        vec![repo_entry(repo.path())],
        Some(TranscriptsSection {
            enabled: true,
            root: transcripts_root.to_string_lossy().into_owned(),
            exclude_projects: Vec::new(),
            index_thinking: true,
        }),
    )
    .await;
    wait_for_transcript_turns(&b.base, 2).await;

    let client = reqwest::Client::new();
    let id = create_review(&client, &b.base, "fixture").await;
    let hunk = hunk_id_for(&canonical);

    let body: serde_json::Value = client
        .get(format!("{}/api/reviews/{id}/hunks/{hunk}/turns", b.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["schema"], "kbc-hunk-turns/1");
    assert_eq!(body["path"], "order.rb");
    // The feature commit's own diff reproduces this hunk id, so the commit
    // is NAMED rather than guessed.
    assert_eq!(body["commit_basis"], "hunk_exact");
    assert_eq!(body["commits"].as_array().unwrap().len(), 1);

    let turns = body["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 1, "{body:#?}");
    let t = &turns[0];
    // No `Kb-Session:` trailer and no kb sibling ⇒ no commit join ⇒ the
    // tier is capped at `likely`, and the response SAYS which witness was
    // missing. That is the two-tier rule doing its job.
    assert_eq!(t["tier"], "likely");
    assert!(t["why"].as_str().unwrap().contains("no commit"));
    assert_eq!(t["session_id"], "sess-cents");
    assert_eq!(t["tool"], "Edit");
    assert_eq!(t["path"], "order.rb");
    assert_eq!(t["turn_id"], "t-111111112222");
    assert!(t["kb_read"]
        .as_str()
        .unwrap()
        .starts_with("kb sessions read sess-cents"));
    assert!(t["matched_bytes"].as_u64().unwrap() >= 24);

    // The same join, surfaced on the timeline's own `turns` lane.
    let tl: serde_json::Value = client
        .get(format!(
            "{}/api/reviews/{id}/timeline?hunk={hunk}&kind=turn",
            b.base
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tl["total"], 1);
    let e = &tl["events"][0];
    assert_eq!(e["kind"], "turn");
    assert_eq!(e["turn_id"], "t-111111112222");
    assert!(
        e["body_md"].is_null(),
        "a turn event must never carry transcript prose"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_hunk_and_a_bad_address_are_two_different_answers() {
    let repo = fixture_repo();
    let b = boot(vec![repo_entry(repo.path())], None).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &b.base, "fixture").await;

    // A well-formed address that names no hunk in this patchset: 200 with
    // an empty list and the reason.
    let body: serde_json::Value = client
        .get(format!(
            "{}/api/reviews/{id}/hunks/0123456789abcdef/turns",
            b.base
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["commit_basis"], "none");
    assert!(body["turns"].as_array().unwrap().is_empty());
    assert!(body["reason"].as_str().unwrap().contains("content address"));

    // A malformed address is a 400 — a different failure, said differently.
    let resp = client
        .get(format!(
            "{}/api/reviews/{id}/hunks/NOT-A-HUNK/turns",
            b.base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}
