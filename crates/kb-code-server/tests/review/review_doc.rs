//! V73-K1 — end-to-end HTTP tests for `kbc-review/1`: the document read,
//! the compose transaction (tiers, slug rule, fingerprint reconcile,
//! manual-finding protection, one SSE, the audit row), the lint, the
//! render, and ref resolution over a repository whose targets moved,
//! changed and vanished.
//!
//! Boot pattern mirrors `review_findings.rs` — each e2e file in this crate
//! duplicates its own small helper set (see `review_routes.rs`'s own doc
//! for why).

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;

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

async fn boot_with_repo(name: &str, path: &Path) -> (tempfile::TempDir, String) {
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

/// `main` has `order.rb` with a three-line `checkout!`; `feature` inserts a
/// line ABOVE it (so the cited line MOVES), edits a second file, and deletes
/// a third. That is the whole pinned/carried/orphan matrix in one fixture.
fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(dir.join("app/models")).unwrap();
    std::fs::write(
        dir.join("app/models/order.rb"),
        "class Order\n  def checkout!\n    dedup_guard\n    save!\n  end\nend\n",
    )
    .unwrap();
    std::fs::write(dir.join("stable.rb"), "STABLE = 1\n").unwrap();
    std::fs::write(dir.join("doomed.rb"), "DOOMED = 1\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(
        dir.join("app/models/order.rb"),
        "# frozen_string_literal: true\nclass Order\n  def checkout!\n    dedup_guard\n    save!\n  end\nend\n",
    )
    .unwrap();
    std::fs::remove_file(dir.join("doomed.rb")).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature work"]);
    tmp
}

async fn create_review(client: &reqwest::Client, base: &str, repo: &str) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": repo, "head_ref": "feature", "base_ref": "main", "title": "doc",
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

async fn compose(
    client: &reqwest::Client,
    base: &str,
    id: i64,
    payload: &serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{base}/api/reviews/{id}/compose"))
        .json(payload)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap())
}

/// A `minimal`-tier document with one finding at `app/models/order.rb`.
fn minimal_doc(title: &str) -> String {
    format!(
        "---\n\
         schema: kbc-review/1\n\
         summary_md: The dedup guard is the load-bearing change.\n\
         findings:\n\
         \x20 - act: issue\n\
         \x20   severity: concern\n\
         \x20   category: correctness\n\
         \x20   title: {title}\n\
         \x20   rationale: Two submits in one second collide.\n\
         \x20   location:\n\
         \x20     path: app/models/order.rb\n\
         \x20     kind: single\n\
         \x20     lines: [4]\n\
         ---\n\n\
         # Retry\n\nThe guard is at [[code:app/models/order.rb:4]].\n"
    )
}

/// Split a `render` response's `html` at the V73-K5 machine block
/// (`review_legacy::export_block_html`'s `<script type="application/json"
/// id="kbc-review">…</script>`), returning `(everything_before_it,
/// the_json_text_inside_it)`. Every render carries exactly one.
fn split_off_machine_block(html: &str) -> (&str, String) {
    const OPEN: &str = "<script type=\"application/json\" id=\"kbc-review\">";
    let open_at = html
        .find(OPEN)
        .expect("every render embeds the kbc-review machine block");
    let body_start = open_at + OPEN.len();
    let close_at = html[body_start..]
        .find("</script>")
        .expect("the machine block is closed");
    (
        &html[..open_at],
        html[body_start..body_start + close_at].to_string(),
    )
}

// --- the read ---------------------------------------------------------------

#[tokio::test]
async fn compose_stores_a_document_and_the_read_reports_its_omissions() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_with_repo("acme-app", repo.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;

    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({
            "schema": "kbc-review/1",
            "doc_md": minimal_doc("Dedup key ignores the attempt"),
            "tier": "minimal",
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["revision"], 1);
    assert_eq!(body["findings"]["created"].as_array().unwrap().len(), 1);
    assert_eq!(body["report_set"], true);

    let doc: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/doc"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(doc["schema"], "kbc-review/1");
    assert_eq!(doc["tier"], "minimal");
    assert_eq!(doc["revision"], 1);
    assert_eq!(doc["revisions"], 1);
    assert!(doc["summary_md"].as_str().unwrap().contains("load-bearing"));

    // An absence is STATED, never discovered.
    let omitted: Vec<&str> = doc["omitted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for expected in ["risk", "reading_order", "flows", "questions", "author"] {
        assert!(omitted.contains(&expected), "{omitted:?} omits {expected}");
    }

    // No authored reading order ⇒ a DERIVED one, captioned as such.
    assert_eq!(doc["reading_order"]["source"], "derived");
    assert!(doc["reading_order"]["caption"]
        .as_str()
        .unwrap()
        .contains("DERIVED"));

    // The report the compose set is the document's own summary.
    let report: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/report"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(report["summary"].as_str().unwrap().contains("load-bearing"));
    assert_eq!(report["schema"], "kbc-review/1");
}

#[tokio::test]
async fn a_review_with_no_document_says_so_rather_than_returning_an_empty_one() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_with_repo("acme-app", repo.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;
    let resp = client
        .get(format!("{base}/api/reviews/{id}/doc"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("no kbc-review/1"),
        "{body}"
    );
}

// --- ref resolution ---------------------------------------------------------

#[tokio::test]
async fn refs_resolve_pinned_carried_and_orphan_and_never_guess_a_line() {
    let repo = fixture_repo();
    let dir = repo.path();
    // The blob of order.rb on `main` — the bytes an author reading the BASE
    // would have pinned. The feature branch inserted a line above the cited
    // one, so this must CARRY.
    let old_blob = git_out(dir, &["rev-parse", "main:app/models/order.rb"]);
    let stable_blob = git_out(dir, &["rev-parse", "feature:stable.rb"]);

    let (_tmp, base) = boot_with_repo("acme-app", dir).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;

    let doc_md = format!(
        "---\n\
         summary_md: refs\n\
         findings: []\n\
         ---\n\
         pinned: [[code:stable.rb:1@{stable_blob}]]\n\
         carried: [[code:app/models/order.rb:3@{old_blob}]]\n\
         gone: [[code:doomed.rb:1]]\n\
         nosha: [[code:stable.rb:1]]\n\
         inert: [[gh:comment/1]] and [[kb:research/abc]]\n\
         wikilink: [[Order]]\n\
         badps: [[hunk:stable.rb@9#0]]\n"
    );
    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": doc_md, "tier": "minimal" }),
    )
    .await;
    // The `doomed.rb` and bad-patchset refs are lint ERRORS, so the compose
    // must refuse with the whole lint and write nothing.
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{body}");
    let rules: Vec<&str> = body["lint"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rule"].as_str().unwrap())
        .collect();
    assert!(rules.contains(&"ref_orphan"), "{rules:?}");
    assert!(rules.contains(&"ref_wrong_patchset"), "{rules:?}");
    assert!(rules.contains(&"bare_wikilink"), "{rules:?}");
    assert!(rules.contains(&"stale_sha"), "{rules:?}");

    // Nothing was written.
    let resp = client
        .get(format!("{base}/api/reviews/{id}/doc"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    // Now the same refs minus the two errors — and check every card's state.
    let doc_md = format!(
        "---\n\
         summary_md: refs\n\
         findings: []\n\
         ---\n\
         pinned: [[code:stable.rb:1@{stable_blob}]]\n\
         carried: [[code:app/models/order.rb:3@{old_blob}]]\n\
         nosha: [[code:app/models/order.rb:2]]\n\
         inert: [[gh:comment/1]] and [[kb:research/abc]]\n"
    );
    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": doc_md, "tier": "minimal" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");

    let doc: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/doc?resolve=true"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cards = doc["cards"].as_array().unwrap();
    let by_ref = |needle: &str| -> serde_json::Value {
        cards
            .iter()
            .find(|c| c["ref"].as_str().unwrap().contains(needle))
            .unwrap_or_else(|| panic!("no card for {needle}: {cards:#?}"))
            .clone()
    };

    let pinned = by_ref("stable.rb:1@");
    assert_eq!(pinned["state"], "pinned");
    assert_eq!(pinned["trust"], "exact", "byte equality is the only exact");
    assert_eq!(pinned["line"], 1);
    assert!(pinned["snippet"].as_str().unwrap().contains("STABLE"));

    let carried = by_ref("order.rb:3@");
    assert_eq!(carried["state"], "carried");
    assert_eq!(
        carried["trust"], "likely",
        "a carry is never exact, however good the text match"
    );
    assert_eq!(
        carried["line"], 4,
        "the cited line moved down one; the card reports where it IS"
    );
    assert!(carried["caption"]
        .as_str()
        .unwrap()
        .contains("carried from"));

    let nosha = by_ref("order.rb:2");
    assert_eq!(nosha["state"], "pinned");
    assert_eq!(
        nosha["trust"], "likely",
        "a ref that pins no blob proves nothing about the bytes"
    );

    for inert in ["gh:comment/1", "kb:research/abc"] {
        let c = by_ref(inert);
        assert_eq!(c["state"], "inert");
        assert!(c.get("trust").is_none(), "an inert link makes no claim");
    }
}

#[tokio::test]
async fn an_ambiguous_symbol_ref_is_an_orphan_naming_the_count_never_a_guess() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_with_repo("acme-app", repo.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;

    let doc_md = "---\nsummary_md: s\nfindings: []\n---\n[[sym:NoSuchSymbolAnywhere]]\n";
    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": doc_md, "tier": "minimal" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{body}");
    let row = body["lint"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["rule"] == "ref_orphan")
        .expect("an unresolvable symbol is an orphan");
    assert!(
        row["message"]
            .as_str()
            .unwrap()
            .contains("no indexed symbol"),
        "{row}"
    );
}

// --- tiers ------------------------------------------------------------------

#[tokio::test]
async fn a_tier_the_document_does_not_meet_is_refused_naming_every_field_at_once() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_with_repo("acme-app", repo.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;

    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({
            "doc_md": "---\nsummary_md: thin\nfindings: []\n---\n",
            "tier": "full",
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{body}");
    let msgs: Vec<&str> = body["lint"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["rule"] == "tier_unmet")
        .map(|r| r["message"].as_str().unwrap())
        .collect();
    assert_eq!(
        msgs.len(),
        3,
        "risk + author + blocks, all at once: {msgs:?}"
    );
}

#[tokio::test]
async fn findings_absent_entirely_is_a_tier_failure_but_an_empty_list_is_not() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_with_repo("acme-app", repo.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;

    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": "---\nsummary_md: s\n---\n", "tier": "minimal" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["lint"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["rule"] == "tier_unmet"
                && r["message"].as_str().unwrap().contains("findings")),
        "{body}"
    );

    let (status, _) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": "---\nsummary_md: s\nfindings: []\n---\n", "tier": "minimal" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
}

// --- the slug rule ----------------------------------------------------------

#[tokio::test]
async fn a_slug_is_minted_once_kept_across_a_rewording_and_never_reused() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_with_repo("acme-app", repo.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;

    // 1. First compose mints f-1.
    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": minimal_doc("Dedup key ignores the attempt"), "tier": "minimal" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(
        body["findings"]["created"].as_array().unwrap(),
        &vec![serde_json::json!("f-1")]
    );

    // 2. A pure re-word of the RATIONALE keeps the fingerprint (act,
    //    category, title and path are unchanged) and therefore the slug.
    let reworded = minimal_doc("Dedup key ignores the attempt")
        .replace("Two submits in one second collide.", "Two submits collide.");
    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": reworded, "tier": "minimal" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(
        body["findings"]["updated"].as_array().unwrap(),
        &vec![serde_json::json!("f-1")],
        "a rewording is the same finding, keeping its slug"
    );
    assert!(body["findings"]["created"].as_array().unwrap().is_empty());

    // 3. A DIFFERENT title is a different finding: f-1 is tombstoned (never
    //    renumbered) and the new one gets f-2, never f-1 again.
    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": minimal_doc("Refund path double-charges"), "tier": "minimal" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(
        body["findings"]["created"].as_array().unwrap(),
        &vec![serde_json::json!("f-2")],
        "a minted slug is never reused, not even after a tombstone"
    );
    assert_eq!(
        body["findings"]["superseded"].as_array().unwrap(),
        &vec![serde_json::json!("f-1")]
    );

    // 4. Bringing the FIRST finding back revives f-1 rather than minting f-3.
    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": minimal_doc("Dedup key ignores the attempt"), "tier": "minimal" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(
        body["findings"]["updated"].as_array().unwrap(),
        &vec![serde_json::json!("f-1")],
        "the fingerprint found the tombstoned row and revived it"
    );

    let findings: serde_json::Value = client
        .get(format!(
            "{base}/api/reviews/{id}/findings?include_superseded=true"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let slugs: Vec<&str> = findings["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs.len(), 2, "{slugs:?}");
    assert!(slugs.contains(&"f-1") && slugs.contains(&"f-2"));
}

#[tokio::test]
async fn a_compose_never_overwrites_a_human_authored_finding() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_with_repo("acme-app", repo.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;

    let manual: serde_json::Value = client
        .post(format!("{base}/api/reviews/{id}/findings"))
        .json(&serde_json::json!({
            "slug": "f-human",
            "severity": "blocker",
            "category": "correctness",
            "title": "A human said this",
            "rationale": "and a compose may not overwrite it",
            "location": { "path": "app/models/order.rb", "kind": "single", "lines": [4] },
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(manual["slug"], "f-human");

    let doc = minimal_doc("Agent finding").replace(
        "\x20 - act: issue",
        "\x20 - slug: f-human\n\x20   act: issue",
    );
    let resp = client
        .post(format!("{base}/api/reviews/{id}/compose"))
        .json(&serde_json::json!({ "doc_md": doc, "tier": "minimal" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("human-authored findings"),
        "{body}"
    );

    // And a compose that does NOT name it leaves it entirely alone.
    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": minimal_doc("Agent finding"), "tier": "minimal" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert!(
        !body["findings"]["superseded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "f-human"),
        "a manual finding is never superseded by a compose: {body}"
    );
}

// --- dry run, caps, audit, SSE ----------------------------------------------

#[tokio::test]
async fn a_dry_run_writes_nothing_emits_nothing_and_still_resolves_the_cards() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_with_repo("acme-app", repo.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;

    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({
            "doc_md": minimal_doc("Dry"),
            "tier": "minimal",
            "dry_run": true,
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["dry_run"], true);
    assert_eq!(body["would_write"]["findings"], 1);
    assert!(body["cards"].as_array().unwrap().len() == 1, "{body}");

    let resp = client
        .get(format!("{base}/api/reviews/{id}/doc"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "a dry run must write nothing"
    );
}

#[tokio::test]
async fn an_oversized_document_is_refused_with_its_own_byte_count() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_with_repo("acme-app", repo.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;

    let padding = "x".repeat(kb_code_server::review_doc::MAX_DOC_BYTES);
    let doc = format!("---\nsummary_md: s\nfindings: []\n---\n{padding}\n");
    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": doc, "tier": "minimal" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    let row = body["lint"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["rule"] == "size_cap")
        .expect("a cap REFUSES");
    assert!(
        row["message"]
            .as_str()
            .unwrap()
            .contains(&format!("{}", kb_code_server::review_doc::MAX_DOC_BYTES)),
        "the refusal names the numbers: {row}"
    );
}

#[tokio::test]
async fn one_compose_emits_exactly_one_review_changed_and_lands_one_audit_row() {
    use futures::StreamExt;
    let repo = fixture_repo();
    let (_tmp, base) = boot_with_repo("acme-app", repo.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;

    let sse = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    let mut stream = sse.bytes_stream();
    // Drain the connect-time replay before counting.
    let _ = tokio::time::timeout(std::time::Duration::from_millis(300), stream.next()).await;

    let (status, _) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": minimal_doc("SSE"), "tier": "minimal" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);

    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut hits = 0usize;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(idx) = buf.find("\"reason\":\"compose\"") {
                    hits += 1;
                    buf = buf[idx + "\"reason\":\"compose\"".len()..].to_string();
                }
                if hits > 0 {
                    break;
                }
            }
            _ => break,
        }
    }
    assert_eq!(
        hits, 1,
        "one compose is ONE caller-visible event, never three"
    );

    let audit: serde_json::Value = client
        .get(format!("{base}/api/audit"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = audit["entries"].as_array().unwrap();
    assert!(
        rows.iter()
            .any(|r| r["route"].as_str().unwrap_or("").ends_with("/compose")
                && r["method"] == "POST"),
        "the mutation ledger records the compose: {audit}"
    );
}

// --- lint + render routes ---------------------------------------------------

#[tokio::test]
async fn the_stored_document_lints_through_its_own_route() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_with_repo("acme-app", repo.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;
    let (status, _) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": minimal_doc("Lint"), "tier": "minimal" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);

    let lint: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/doc/lint"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(lint["schema"], "review-lint/1");
    assert_eq!(lint["errors"], 0, "{lint}");
}

#[tokio::test]
async fn render_escapes_everything_and_reports_unknown_placeholders() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_with_repo("acme-app", repo.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;

    let hostile = "<script>alert(1)</script>";
    let doc = minimal_doc(hostile);
    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "doc_md": doc, "tier": "minimal" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");

    // Built-in template, bearer GET.
    let out: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/doc/render"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(out["schema"], "review-render/1");
    let html = out["html"].as_str().unwrap();
    // V73-K5 (gap 4) — every render also embeds a re-importable machine
    // block, and that block's `title` field carries `hostile` VERBATIM (as
    // inert JSON text, `</` escaped so it can never terminate the block
    // early — see `review_legacy::export_block_html`). A raw-text
    // `<script type="application/json">` element's content is never
    // tokenized as nested HTML by a browser, so this is not a second XSS
    // surface, but it DOES mean the substring check below must scope to
    // the rendered TEMPLATE, not the whole page.
    let (template_html, machine_block) = split_off_machine_block(html);
    assert!(
        !template_html.contains("<script>alert"),
        "a finding title escaped its cell: {template_html}"
    );
    assert!(
        template_html.contains("&lt;script&gt;"),
        "and it is still VISIBLE, escaped"
    );
    assert!(out["unknown_placeholders"].as_array().unwrap().is_empty());
    // The machine block itself must still be well-formed JSON — proof its
    // OWN `</` escaping did not corrupt the payload it was meant to protect.
    let block_json: serde_json::Value =
        serde_json::from_str(&machine_block).expect("the embedded machine block is valid JSON");
    assert_eq!(block_json["schema"], "kbc-review/1");
    assert!(
        block_json["findings"][0]["title"]
            .as_str()
            .unwrap()
            .contains("<script>alert"),
        "the machine block carries the finding's title VERBATIM (it is data, never executed): {block_json}"
    );

    // Operator template, loopback POST — one renderer, two entry points.
    let out: serde_json::Value = client
        .post(format!("{base}/api/reviews/{id}/doc/render"))
        .json(&serde_json::json!({
            "template": "<main>{{summary}}{{findings}}</main>{{nope}}<style>.a{color:red}</style>",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let html = out["html"].as_str().unwrap();
    assert!(html.starts_with("<main>"), "{html}");
    assert!(
        html.contains("{{nope}}"),
        "an unknown placeholder is left verbatim"
    );
    assert!(
        html.contains(".a{color:red}"),
        "template CSS braces are never mangled"
    );
    assert_eq!(
        out["unknown_placeholders"].as_array().unwrap(),
        &vec![serde_json::json!("nope")]
    );

    let resp = client
        .get(format!(
            "{base}/api/reviews/{id}/doc/render?template=nosuch"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("default"),
        "the refusal lists the registered names: {body}"
    );
}

// --- the low-level twin is untouched ----------------------------------------

#[tokio::test]
async fn the_v0_compose_form_still_works_and_still_requires_its_own_fields() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_with_repo("acme-app", repo.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "acme-app").await;

    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({
            "summary": "the v0 shape",
            "findings": {
                "schema": "kbc-findings/1",
                "findings": [{
                    "slug": "f-v0",
                    "severity": "concern",
                    "category": "correctness",
                    "location": { "path": "app/models/order.rb", "kind": "single", "lines": [4] },
                    "title": "v0 finding",
                    "rationale": "unchanged",
                }],
            },
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(
        body["findings"]["created"].as_array().unwrap(),
        &vec![serde_json::json!("f-v0")],
        "an author-supplied slug is still identity on the v0 path"
    );
    assert!(
        body.get("revision").is_none(),
        "the v0 path stores no document: {body}"
    );

    let (status, body) = compose(
        &client,
        &base,
        id,
        &serde_json::json!({ "summary": "no findings" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap().contains("findings"),
        "{body}"
    );
}
