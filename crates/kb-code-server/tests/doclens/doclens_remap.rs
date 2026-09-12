//! DCB W2.A — end-to-end tests for the `kb-code-rev` line remap against a
//! real two-commit fixture repo (`gamma`) and a mock kb daemon.
//!
//! ## Why this fixture is built the way it is
//!
//! The whole phase exists for ONE contrast, and `gamma`'s history is
//! constructed to produce it: in the commit-B tree the cited token
//! `algolia_user_token` occurs at lines **13 and 14**, while the document
//! cites line **10**. The token pass's ±3 window around 10 is `[7, 13]`, which
//! contains exactly ONE of them — so the token predicate alone answers
//! `confirmed` at line **10**: plausible, and wrong by three lines. The remap
//! answers **13**, which is where the method actually is.
//!
//! Rows 1 vs 2 of `15-w2a` §5.3 are that pair, and
//! `remap_maps_a_moved_line_where_the_token_pass_would_have_answered_the_hint`
//! is this phase's headline assertion. Everything else here is a refusal:
//! `+dirty`, a label naming another checkout, an unresolvable sha, a line
//! inside a changed hunk, a path absent at the rev, a half-mapped range —
//! each falls back to the token pass and SAYS why.
//!
//! `[kb_daemon]` is pointed at the mock on every boot — this suite must never
//! risk reaching a real kb daemon on `127.0.0.1:4000`.

use axum::{
    extract::Path as AxPath, http::StatusCode, response::IntoResponse, routing::get, Json, Router,
};
use kb_code_server::config::{DoclensSection, KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

// --- the gamma fixture -----------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git -C {} {args:?} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(dir: &Path, rel: &str, body: &str) {
    let abs = dir.join(rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, body).unwrap();
}

/// Commit A — 21 lines. `algolia_user_token` is the method at line 10 (and
/// the attribute read at 11); `stale_helper` is at 18.
const PURCHASE_A: &str = r#"class Purchase < ApplicationRecord
  has_many :purchase_items

  def total
    purchase_items.sum(&:price)
  end

  private

  def algolia_user_token
    read_attribute(:algolia_user_token)
  end

  def legacy_token
    cookies[:_ALGOLIA]
  end

  def stale_helper
    nil
  end
end
"#;

/// Commit B — exactly two edits, so `git diff --unified=0 <sha_a>` is exactly
/// `@@ -2,0 +3,3 @@` + `@@ -18 +21 @@`:
///   * three lines inserted AFTER old line 2, and
///   * old line 18 (`def stale_helper`) rewritten to `def fresh_helper`.
///
/// `algolia_user_token` now sits at 13 and 14 — the pair that makes the token
/// pass's ±3 window around the doc's hint of 10 confidently wrong.
const PURCHASE_B: &str = r#"class Purchase < ApplicationRecord
  has_many :purchase_items
  has_many :shipments

  # V2

  def total
    purchase_items.sum(&:price)
  end

  private

  def algolia_user_token
    read_attribute(:algolia_user_token)
  end

  def legacy_token
    cookies[:_ALGOLIA]
  end

  def fresh_helper
    nil
  end
end
"#;

/// Absent at commit A. `tracking_code` occurs EXACTLY once (line 3) so the
/// token pass has a decisive answer for this ref — the point of row 7 is the
/// `path_absent_at_rev` fall-through, not a second ambiguity case.
const SHIPMENT_B: &str = r#"class Shipment < ApplicationRecord
  belongs_to :purchase
  def tracking_code
    carrier.code
  end
end
"#;

/// The two-commit `gamma` repo; returns the tempdir and commit A's sha.
fn fixture_gamma() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    write(dir, "app/models/purchase.rb", PURCHASE_A);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "A"]);
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let sha_a = String::from_utf8(out.stdout).unwrap().trim().to_string();

    write(dir, "app/models/purchase.rb", PURCHASE_B);
    // Absent at sha_a — the `path_absent_at_rev` arm (rename tracking is out
    // of v1 scope by recorded refusal).
    write(dir, "app/models/shipment.rb", SHIPMENT_B);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "B"]);
    (tmp, sha_a)
}

const GAMMA_FILES: usize = 2;

// --- the mock kb daemon ----------------------------------------------------

/// Every doc below cites the SAME refs; only `code_rev` differs, which is the
/// only variable the golden table (§5.3) is about.
fn refs() -> Value {
    json!([
        // 0 — the headline: cited 10, actually 13.
        {
            "ordinal": 0, "kind": "path_line", "raw": "purchase.rb:10",
            "path_hint": "purchase.rb", "line_start": 10,
            "context": "il metodo algolia_user_token legge l'attributo",
            "context_tokens": ["algolia_user_token"]
        },
        // 1 — the cited line's own content was rewritten (E4).
        {
            "ordinal": 1, "kind": "path_line", "raw": "purchase.rb:18",
            "path_hint": "purchase.rb", "line_start": 18,
            "context": "stale_helper va rimosso",
            "context_tokens": ["stale_helper"]
        },
        // 2 — a path that did not exist at the rev.
        {
            "ordinal": 2, "kind": "path_line", "raw": "shipment.rb:3",
            "path_hint": "shipment.rb", "line_start": 3,
            "context": "tracking_code è il nuovo accessor",
            "context_tokens": ["tracking_code"]
        },
        // 3 — a range: both endpoints map (4-6 -> 7-9).
        {
            "ordinal": 3, "kind": "path_range", "raw": "purchase.rb:4-6",
            "path_hint": "purchase.rb", "line_start": 4, "line_end": 6,
            "context": "il metodo total somma i prodotti",
            "context_tokens": ["purchase_items"]
        },
        // 4 — a HALF-mapped range: 16 maps, 18 is inside the change.
        {
            "ordinal": 4, "kind": "path_range", "raw": "purchase.rb:16-18",
            "path_hint": "purchase.rb", "line_start": 16, "line_end": 18,
            "context": "legacy_token e il vecchio helper",
            "context_tokens": ["legacy_token"]
        },
        // 5 — a comma line-list: ONE ref, two spans, both remapped.
        {
            "ordinal": 5, "kind": "path_list", "raw": "purchase.rb:10,14",
            "path_hint": "purchase.rb", "line_start": 10, "line_spans": "10,14",
            "context": "algolia_user_token e legacy_token",
            "context_tokens": ["algolia_user_token", "legacy_token"]
        },
        // 6 (DCB-W2.A.R fix 2) — a hint FAR past the OLD state's own 21
        // lines. `map_line` extrapolates past every hunk it saw and still
        // answers `Mapped` (old 90 -> new 93, same +3 arithmetic as row 0),
        // but `purchase.rb` only has 24 lines in the working tree — the EOF
        // bound must catch this and fall through, not ship a confirmed
        // rev_remap past the end of the file.
        {
            "ordinal": 6, "kind": "path_line", "raw": "purchase.rb:90",
            "path_hint": "purchase.rb", "line_start": 90,
            "context": "riga fuori range, anche con un rev valido",
            "context_tokens": ["purchase_items"]
        },
    ])
}

fn doc(id: &str, code_rev: Value) -> Value {
    json!({
        "schema": "coderef/1",
        "kb": "platform",
        "doc_id": id,
        "doc_path": "features/purchases.html",
        "doc_hash": "3f1abeef0000",
        "title": "Purchases — piano",
        "extracted_at": 1_754_500_000i64,
        "never_scanned": false,
        "code_rev": code_rev,
        "ref_count": 7,
        "ungrouped_count": 7,
        "truncated": false,
        "groups": [],
        "refs": refs(),
    })
}

/// The §5.3 doc ids, one per `kb-code-rev` variant.
const DOC_REMAP: &str = "remapdoc0001";
const DOC_NO_REV: &str = "norevdoc0001";
const DOC_DIRTY: &str = "dirtydoc0001";
const DOC_LABEL: &str = "labeldoc0001";
const DOC_UNKNOWN: &str = "unknowndoc01";

async fn mock_kb(sha_a: &str) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let docs: Arc<HashMap<String, Value>> = Arc::new(
        [
            (
                DOC_REMAP.to_string(),
                doc(
                    DOC_REMAP,
                    json!({"label": "gamma", "sha": sha_a, "dirty": false}),
                ),
            ),
            (DOC_NO_REV.to_string(), doc(DOC_NO_REV, Value::Null)),
            (
                DOC_DIRTY.to_string(),
                doc(
                    DOC_DIRTY,
                    json!({"label": "gamma", "sha": sha_a, "dirty": true}),
                ),
            ),
            (
                DOC_LABEL.to_string(),
                doc(
                    DOC_LABEL,
                    json!({"label": "other", "sha": sha_a, "dirty": false}),
                ),
            ),
            (
                DOC_UNKNOWN.to_string(),
                doc(
                    DOC_UNKNOWN,
                    json!({"label": "gamma", "sha": "deadbeefdeadbeef", "dirty": false}),
                ),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let router = Router::new().route(
        "/api/kb/{kb}/docs/{id}/code-refs",
        get(move |AxPath((_kb, id)): AxPath<(String, String)>| {
            let docs = docs.clone();
            async move {
                match docs.get(&id) {
                    Some(d) => Json(d.clone()).into_response(),
                    None => (StatusCode::NOT_FOUND, Json(json!({"error": "no such doc"})))
                        .into_response(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (addr, handle)
}

// --- boot ------------------------------------------------------------------

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

/// `deadline_ms` well above the 3 s production default: this suite runs
/// daemons in parallel on an IO-bound host and the point here is the
/// RESOLUTION, not the budget (which has its own test in `doclens_route.rs`).
async fn boot(repo: &Path, kb_addr: SocketAddr) -> Boot {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "gamma".to_string(),
            path: std::fs::canonicalize(repo).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: true,
            url: Some(format!("http://{kb_addr}")),
            token_file: None,
            public_url: Some("https://kb.example.com".to_string()),
        },
        doclens: DoclensSection {
            kbs: vec!["platform".to_string()],
            deadline_ms: 60_000,
            ..DoclensSection::default()
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    Boot {
        tmp,
        base: format!("http://{addr}"),
        task,
    }
}

async fn wait_for_indexed(base: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|r| r.iter().find(|r| r["name"] == "gamma"))
                {
                    if entry["file_count"].as_u64().unwrap_or(0) as usize >= expected_files {
                        return;
                    }
                }
            }
        }
        assert!(Instant::now() < deadline, "gamma never finished indexing");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn lens(base: &str, doc: &str) -> Value {
    let resp = reqwest::Client::new()
        .get(format!(
            "{base}/api/doc-lens?kb=platform&doc={doc}&repo=gamma"
        ))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "{doc}: {body}");
    body
}

fn by_ordinal(body: &Value, ordinal: u64) -> &Value {
    body["refs"]
        .as_array()
        .expect("refs[]")
        .iter()
        .find(|r| r["ordinal"] == ordinal)
        .unwrap_or_else(|| panic!("no ref with ordinal {ordinal}"))
}

/// Boot once, resolve every §5.3 variant against the same daemon — the whole
/// table is one fixture build.
async fn table() -> (Boot, tempfile::TempDir, tokio::task::JoinHandle<()>) {
    let (gamma, sha_a) = fixture_gamma();
    let (kb_addr, kb) = mock_kb(&sha_a).await;
    let boot = boot(gamma.path(), kb_addr).await;
    wait_for_indexed(&boot.base, GAMMA_FILES).await;
    (boot, gamma, kb)
}

// --- the headline ----------------------------------------------------------

/// **§5.3 rows 1 vs 2 — the reason this phase exists.** Same ref, same tree,
/// same tokens; the ONLY difference is whether the document declared the
/// commit its line numbers were counted against.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remap_maps_a_moved_line_where_the_token_pass_would_have_answered_the_hint() {
    let (boot, _gamma, _kb) = table().await;

    // Row 2 — no `kb-code-rev`: the token pass finds `algolia_user_token`
    // uniquely inside ±3 of the cited 10 (it is at 13, and 14 is outside the
    // window), so it CONFIRMS the citation as written. Plausible. Wrong.
    let token = lens(&boot.base, DOC_NO_REV).await;
    assert!(token["rev_remap"].is_null(), "no rev ⇒ no block: {token}");
    let r = by_ordinal(&token, 0);
    assert_eq!(r["line_state"], "confirmed");
    assert_eq!(r["line_evidence"], "context_token");
    assert_eq!(r["resolved_line"], 10, "D6 — the token pass never moves it");
    assert_eq!(r["line_hint_delta"], 0);
    assert_eq!(r["token_line"], 13, "the evidence it confirmed ON");
    assert!(r["remap"].is_null());

    // Row 1 — the same ref, with the rev declared. git maps old 10 to new 13.
    let remapped = lens(&boot.base, DOC_REMAP).await;
    assert_eq!(remapped["rev_remap"]["state"], "applied");
    assert!(remapped["rev_remap"]["reason"].is_null());
    assert_eq!(remapped["rev_remap"]["repo_label"], "gamma");
    assert_eq!(remapped["rev_remap"]["dirty"], false);
    assert_eq!(
        remapped["rev_remap"]["resolved_sha"]
            .as_str()
            .unwrap()
            .len(),
        40,
        "the doc's sha is reported RESOLVED, full-length"
    );
    assert_eq!(remapped["rev_remap"]["budget_exhausted"], false);

    let r = by_ordinal(&remapped, 0);
    assert_eq!(r["remap"], "applied");
    assert_eq!(r["line_state"], "confirmed");
    assert_eq!(r["line_evidence"], "rev_remap");
    assert_eq!(r["resolved_line"], 13, "where the method ACTUALLY is");
    assert_eq!(r["line_hint_delta"], 3);
    assert_eq!(r["reader"]["line"], 13, "the deep link follows the map");
    // git proved the line's identity; there is no token argument to report.
    assert!(r["confirm_token"].is_null() && r["token_line"].is_null());
    // DCB-W2.A.R fix 2 — `file_lines` is no longer 0 here: the EOF bound
    // forces ONE memoised read of `purchase.rb` even on a fully successful
    // remap, so a client can still see `resolved_line (13) <= file_lines`.
    assert_eq!(r["file_lines"], 24);

    // R18's consumer contract, asserted on the PRODUCER: a `confirmed` ref
    // with a non-zero delta always carries `rev_remap`, on every ref of every
    // variant this suite resolves.
    for body in [&token, &remapped] {
        for r in body["refs"].as_array().unwrap() {
            if r["line_state"] == "confirmed" && r["line_hint_delta"].as_i64() != Some(0) {
                assert_eq!(
                    r["line_evidence"], "rev_remap",
                    "a moved+confirmed ref git never verified: {r}"
                );
            }
        }
    }
}

// --- the refusals ----------------------------------------------------------

/// §5.3 row 3 (E2). The cited numbers were counted against an uncommitted
/// tree that no sha reproduces, so `<sha>` is NOT their basis and diffing
/// from it would mint a confidently wrong map. The honest fallback is the
/// same plausible-but-wrong 10 the token pass gives — which is exactly why
/// the dirty banner is mandatory on every consumer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dirty_doc_rev_falls_back_to_the_token_pass_and_says_why() {
    let (boot, _gamma, _kb) = table().await;
    let body = lens(&boot.base, DOC_DIRTY).await;
    assert_eq!(body["rev_remap"]["state"], "skipped");
    assert_eq!(body["rev_remap"]["reason"], "doc_rev_dirty");
    assert_eq!(body["rev_remap"]["dirty"], true);
    assert!(
        body["rev_remap"]["resolved_sha"].is_null(),
        "a +dirty rev is never even resolved"
    );
    assert_eq!(body["rev_remap"]["paths_mapped"], 0);

    let r = by_ordinal(&body, 0);
    assert!(r["remap"].is_null(), "no doc-level remap ⇒ null per ref");
    assert_eq!(r["line_state"], "confirmed");
    assert_eq!(r["line_evidence"], "context_token");
    assert_eq!(r["resolved_line"], 10);
    assert_eq!(r["line_hint_delta"], 0);
    // Byte-identical to the no-rev answer — the refusal costs exactly the
    // remap, nothing else.
    let plain = lens(&boot.base, DOC_NO_REV).await;
    assert_eq!(r["line_state"], by_ordinal(&plain, 0)["line_state"]);
    assert_eq!(r["resolved_line"], by_ordinal(&plain, 0)["resolved_line"]);
}

/// §5.3 row 4 (E3). Remapping against a checkout the author never named is
/// precisely the silently-wrong-checkout failure Decision 1 exists to
/// prevent; the reason lets a consumer say "written against `other`, you
/// selected `gamma`".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn label_mismatch_falls_back_to_the_token_pass_and_says_why() {
    let (boot, _gamma, _kb) = table().await;
    let body = lens(&boot.base, DOC_LABEL).await;
    assert_eq!(body["rev_remap"]["state"], "skipped");
    assert_eq!(body["rev_remap"]["reason"], "rev_label_mismatch");
    assert_eq!(body["rev_remap"]["repo_label"], "other");
    assert_eq!(body["repo"]["name"], "gamma");
    let r = by_ordinal(&body, 0);
    assert!(r["remap"].is_null());
    assert_eq!(r["line_evidence"], "context_token");
    assert_eq!(r["resolved_line"], 10);
}

/// §5.3 row 5. A rev this checkout simply does not contain is `unavailable`,
/// not an error — the lens still resolves, on the token pass alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_rev_is_unavailable_not_an_error() {
    let (boot, _gamma, _kb) = table().await;
    let body = lens(&boot.base, DOC_UNKNOWN).await;
    assert_eq!(body["rev_remap"]["state"], "unavailable");
    assert_eq!(body["rev_remap"]["reason"], "rev_unknown");
    assert_eq!(body["rev_remap"]["sha"], "deadbeefdeadbeef");
    assert!(body["rev_remap"]["resolved_sha"].is_null());
    let r = by_ordinal(&body, 0);
    assert!(r["remap"].is_null());
    assert_eq!(r["line_state"], "confirmed");
    assert_eq!(r["line_evidence"], "context_token");
}

/// §5.3 row 6 (E4). Old line 18 (`def stale_helper`) was rewritten, so no
/// honest mapping exists — the ref falls through, and the token pass then
/// finds nothing because `stale_helper` no longer exists in the tree at all.
/// Falling through is still strictly better than emitting `unverifiable`
/// directly: for a line whose CONTENT merely moved, the token would find it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_line_inside_a_changed_hunk_is_unverifiable_when_its_content_is_gone() {
    let (boot, _gamma, _kb) = table().await;
    let body = lens(&boot.base, DOC_REMAP).await;
    let r = by_ordinal(&body, 1);
    assert_eq!(r["remap"], "inside_change");
    assert_eq!(r["line_state"], "unverifiable");
    assert_eq!(r["line_reason"], "token_not_found");
    assert_eq!(r["line_evidence"], "none");
    assert!(r["resolved_line"].is_null());
    // The fall-through DID read the file — that is what arm 2 costs.
    assert_eq!(r["file_lines"], 24);
}

/// §5.3 row 7. Rename tracking is out of v1 scope by recorded refusal: a file
/// absent at the rev falls through with its own reason rather than being
/// chased through a rename oracle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_path_absent_at_the_rev_falls_through_to_the_token_pass() {
    let (boot, _gamma, _kb) = table().await;
    let body = lens(&boot.base, DOC_REMAP).await;
    let r = by_ordinal(&body, 2);
    assert_eq!(r["remap"], "path_absent_at_rev");
    assert_eq!(r["path_state"], "present", "it IS in the working tree");
    // The token pass still answers — `tracking_code` is uniquely near line 3.
    assert_eq!(r["line_state"], "confirmed");
    assert_eq!(r["line_evidence"], "context_token");
    assert_eq!(r["resolved_line"], 3);
    // …and the path never entered the doc-level map count.
    assert_eq!(body["rev_remap"]["paths_mapped"], 1, "only purchase.rb");
}

/// §5.3 row 8 (E6) — both endpoints map, so the span is honest end-to-end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_range_remaps_both_endpoints() {
    let (boot, _gamma, _kb) = table().await;
    let body = lens(&boot.base, DOC_REMAP).await;
    let r = by_ordinal(&body, 3);
    assert_eq!(r["remap"], "applied");
    assert_eq!(r["line_state"], "confirmed");
    assert_eq!(r["line_evidence"], "rev_remap");
    assert_eq!(r["resolved_line"], 7);
    assert_eq!(r["resolved_line_end"], 9);
    assert_eq!(r["line_hint_delta"], 3);
    let spans = r["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0]["resolved_line"], 7);
    assert_eq!(spans[0]["resolved_line_end"], 9);
}

/// E6's refusal half: `16-18` has a start that maps (16 → 19) and an end
/// INSIDE the rewritten hunk. Reporting `19-21` would be a lie about the
/// span, so the whole ref falls through to the token pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_half_mapped_range_is_rejected_and_falls_through() {
    let (boot, _gamma, _kb) = table().await;
    let body = lens(&boot.base, DOC_REMAP).await;
    let r = by_ordinal(&body, 4);
    // DCB-W2.A.R fix 3 — the per-ref `remap` wire string is the FINAL
    // decision, not the start endpoint's alone: the END is what actually
    // blocked this span (its own content was rewritten), so `inside_change`
    // — the END's own outcome — is the truthful value here, not the
    // start's bare `applied` (which is true of the start ALONE but claims
    // more than the span as a whole earned).
    assert_eq!(r["remap"], "inside_change");
    assert_ne!(
        r["line_evidence"], "rev_remap",
        "a half-mapped range must never be reported as git-verified: {r}"
    );
    assert!(
        r["resolved_line_end"].is_null(),
        "no end is better than a wrong one: {r}"
    );
    // `legacy_token` is at 17 in the new tree, uniquely inside ±3 of 16.
    assert_eq!(r["line_state"], "confirmed");
    assert_eq!(r["line_evidence"], "context_token");
    assert_eq!(r["resolved_line"], 16);
}

/// §9b.1 — the remap runs PER SPAN, and the per-path diff memo makes every
/// span after the first free. One ref, two spans, both git-mapped (+3), and
/// still exactly one path in `paths_mapped`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remap_maps_every_span_of_a_path_list_with_one_diff_call() {
    let (boot, _gamma, _kb) = table().await;
    let body = lens(&boot.base, DOC_REMAP).await;
    let r = by_ordinal(&body, 5);
    assert_eq!(r["remap"], "applied", "the FIRST span's outcome");
    let spans = r["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 2, "ONE ref, many spans — never N refs");
    assert_eq!(spans[0]["resolved_line"], 13);
    assert_eq!(spans[1]["resolved_line"], 17);
    assert!(spans.iter().all(|s| s["line_evidence"] == "rev_remap"));
    assert!(spans.iter().all(|s| s["line_state"] == "confirmed"));
    assert_eq!(r["resolved_line"], 13, "the FIRST span drives the reader");
    assert_eq!(
        body["rev_remap"]["paths_mapped"], 1,
        "seven refs over two paths, ONE of which existed at the rev"
    );
    // DCB-W2.A.R fix 2 — the EOF bound reads `purchase.rb` once for the bound
    // check, memoised: both spans remapped, but `file_lines` is no longer 0.
    assert_eq!(r["file_lines"], 24);
}

/// DCB-W2.A.R fix 2 — `map_line` extrapolates past the diff's own hunks once
/// the hint sits beyond every hunk it saw: old line 90 lands at new line 93
/// by the same `+3` arithmetic as row 0's headline case, except `purchase.rb`
/// only has 24 lines in the working tree. Shipping `confirmed`+`rev_remap`
/// here would be exactly the regression W1.C's honest out-of-range golden
/// existed to prevent — the bound-check must catch it and fall through to
/// the token pass, which honestly finds nothing that far outside its
/// windows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_remapped_line_past_the_working_trees_eof_falls_through_honestly() {
    let (boot, _gamma, _kb) = table().await;
    let body = lens(&boot.base, DOC_REMAP).await;
    let r = by_ordinal(&body, 6);
    // git's own diff math still computed a number (93) — that half of the
    // wire string stays honest; what the bound-check changes is that the
    // daemon refuses to SHIP a confirmation past what the tree contains.
    assert_eq!(r["remap"], "applied");
    assert_ne!(
        r["line_evidence"], "rev_remap",
        "must never confirm a line past EOF: {r}"
    );
    assert_eq!(r["line_state"], "unverifiable");
    assert_eq!(r["line_reason"], "token_not_found");
    assert!(r["resolved_line"].is_null());
    assert_eq!(
        r["file_lines"], 24,
        "the EOF bound forces exactly the one read the token pass needed anyway"
    );
}

/// The whole table in one pass, so a fixture edit that silently changes a
/// verdict shows up as a table diff rather than as one puzzling assertion.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_golden_table_holds_for_every_kb_code_rev_variant() {
    let (boot, _gamma, _kb) = table().await;
    // (doc, rev_remap.state, rev_remap.reason, ref-0 remap, ref-0 evidence,
    //  ref-0 resolved_line)
    let rows: &[(&str, &str, &str, &str, &str, u64)] = &[
        (DOC_REMAP, "applied", "", "applied", "rev_remap", 13),
        (
            DOC_DIRTY,
            "skipped",
            "doc_rev_dirty",
            "",
            "context_token",
            10,
        ),
        (
            DOC_LABEL,
            "skipped",
            "rev_label_mismatch",
            "",
            "context_token",
            10,
        ),
        (
            DOC_UNKNOWN,
            "unavailable",
            "rev_unknown",
            "",
            "context_token",
            10,
        ),
    ];
    for (id, state, reason, remap, evidence, line) in rows {
        let body = lens(&boot.base, id).await;
        assert_eq!(body["rev_remap"]["state"], *state, "{id}");
        assert_eq!(
            body["rev_remap"]["reason"].as_str().unwrap_or(""),
            *reason,
            "{id}"
        );
        let r = by_ordinal(&body, 0);
        assert_eq!(r["remap"].as_str().unwrap_or(""), *remap, "{id}");
        assert_eq!(r["line_evidence"], *evidence, "{id}");
        assert_eq!(r["resolved_line"], *line, "{id}");
        // Whatever the arm, the ref resolved: a refusal degrades the EVIDENCE,
        // never the availability of the lens.
        assert_eq!(r["line_state"], "confirmed", "{id}");
    }
    // …and a doc with no rev at all carries neither block.
    let bare = lens(&boot.base, DOC_NO_REV).await;
    assert!(bare["rev_remap"].is_null() && bare["doc_code_rev"].is_null());
}
