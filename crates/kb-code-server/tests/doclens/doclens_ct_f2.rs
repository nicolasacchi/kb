//! CT-F2 — end-to-end HTTP tests for era-resolved citations
//! (`GET /api/doc-lens?...&at=declared`) against a real two-commit fixture
//! repo (`delta`) and a mock kb daemon.
//!
//! ## The fixture, and why it is shaped this way
//!
//! `delta.rb`'s cited line (5) is genuinely REWRITTEN between rev A and the
//! working tree (not merely shifted — `git diff --unified=0` reports it as
//! `@@ -5 +5,13 @@`, a real replacement), so `RevRemap` (W2.A) falls through
//! to the token pass for it, which then finds `unique_marker_alpha` again
//! much further down the file: the CURRENT-tree verdict is honestly
//! `drifted`. Read directly against rev A's own content, the SAME hint (line
//! 5) is exactly where the token sat — CT-F2's `when_written` therefore
//! reports `confirmed`. That contrast (`confirmed` then, `drifted` now) is
//! the entire point of this phase: a citation that was true when written and
//! rotted since is a DIFFERENT failure from one that was never true.
//!
//! `epsilon.rb` exists only in the working tree (added in commit B), so a
//! citation against it never existed at rev A at all — `when_written` reports
//! `path_state_at_rev: absent` even though the CURRENT tree confirms the
//! citation outright (the file is there now, and the token resolves clean).
//!
//! `[kb_daemon]` is pointed at the mock on every boot — this suite must never
//! risk reaching a real kb daemon on `127.0.0.1:4000`.

use crate::common::{git, init_repo};
use axum::{
    extract::Path as AxPath, http::StatusCode, response::IntoResponse, routing::get, Json, Router,
};
use kb_code_server::config::{DoclensSection, KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

// --- the delta fixture ------------------------------------------------------

fn write(dir: &Path, rel: &str, body: &str) {
    let abs = dir.join(rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, body).unwrap();
}

/// Rev A — 7 lines; `unique_marker_alpha` sits at line 5, inline with a
/// different local (`token_value = …`).
const DELTA_A: &str = r#"# frozen_string_literal: true

class Delta
  def helper
    token_value = unique_marker_alpha
  end
end
"#;

/// Working tree — 19 lines. Line 5 (the cited line) is REWRITTEN, not just
/// shifted (`compute_other_thing`, unrelated to the old content), and
/// `unique_marker_alpha` reappears alone at line 17 — outside the ±3 confirm
/// window around the hint (5) but inside the ±64 drift window.
const DELTA_B: &str = r#"# frozen_string_literal: true

class Delta
  def helper
    compute_other_thing
  end

  def filler_method_02
    nil
  end

  def filler_method_03
    nil
  end

  def legacy_helper
    result = unique_marker_alpha.dup
  end
end
"#;

/// Added ONLY in commit B — never existed at rev A.
const EPSILON_B: &str = r#"class Epsilon
  def call
    epsilon_only_token
  end
end
"#;

const DELTA_FILES: usize = 2;

/// The two-commit `delta` repo; returns the tempdir and rev A's sha.
fn fixture_delta() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init_repo(dir);
    write(dir, "delta.rb", DELTA_A);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "A"]);
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let sha_a = String::from_utf8(out.stdout).unwrap().trim().to_string();

    write(dir, "delta.rb", DELTA_B);
    write(dir, "epsilon.rb", EPSILON_B);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "B"]);
    (tmp, sha_a)
}

// --- the mock kb daemon ------------------------------------------------------

fn refs() -> Value {
    json!([
        // 0 — rewritten in place: correct when written, rotted since.
        {
            "ordinal": 0, "kind": "path_line", "raw": "delta.rb:5",
            "path_hint": "delta.rb", "line_start": 5,
            "context": "unique_marker_alpha nell'helper",
            "context_tokens": ["unique_marker_alpha"]
        },
        // 1 — the path itself is new since rev A: wrong when written.
        {
            "ordinal": 1, "kind": "path_line", "raw": "epsilon.rb:3",
            "path_hint": "epsilon.rb", "line_start": 3,
            "context": "epsilon_only_token nel nuovo file",
            "context_tokens": ["epsilon_only_token"]
        },
    ])
}

fn doc(id: &str, code_rev: Value) -> Value {
    json!({
        "schema": "coderef/1",
        "kb": "platform",
        "doc_id": id,
        "doc_path": "features/delta.html",
        "doc_hash": "deadbeef01",
        "title": "Delta — era test",
        "extracted_at": 1_754_500_000i64,
        "never_scanned": false,
        "code_rev": code_rev,
        "ref_count": 2,
        "ungrouped_count": 2,
        "truncated": false,
        "groups": [],
        "refs": refs(),
    })
}

const DOC_DECLARED: &str = "declareddoc1";
const DOC_NO_REV: &str = "norevdoc0001";

async fn mock_kb(sha_a: &str) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let docs: Arc<HashMap<String, Value>> = Arc::new(
        [
            (
                DOC_DECLARED.to_string(),
                doc(
                    DOC_DECLARED,
                    json!({"label": "delta", "sha": sha_a, "dirty": false}),
                ),
            ),
            (DOC_NO_REV.to_string(), doc(DOC_NO_REV, Value::Null)),
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

// --- boot --------------------------------------------------------------------

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot(repo: &Path, kb_addr: SocketAddr) -> Boot {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "delta".to_string(),
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
                    .and_then(|r| r.iter().find(|r| r["name"] == "delta"))
                {
                    if entry["file_count"].as_u64().unwrap_or(0) as usize >= expected_files {
                        return;
                    }
                }
            }
        }
        assert!(Instant::now() < deadline, "delta never finished indexing");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn lens_at(base: &str, doc: &str, at: Option<&str>) -> Value {
    let mut url = format!("{base}/api/doc-lens?kb=platform&doc={doc}&repo=delta");
    if let Some(at) = at {
        url.push_str(&format!("&at={at}"));
    }
    let resp = reqwest::Client::new().get(url).send().await.unwrap();
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

async fn table() -> (Boot, tempfile::TempDir, tokio::task::JoinHandle<()>) {
    let (delta, sha_a) = fixture_delta();
    let (kb_addr, kb) = mock_kb(&sha_a).await;
    let boot = boot(delta.path(), kb_addr).await;
    wait_for_indexed(&boot.base, DELTA_FILES).await;
    (boot, delta, kb)
}

// --- the tests ---------------------------------------------------------------

/// The headline contrast: correct when written, rotted since.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn when_written_confirms_a_hint_that_the_current_tree_honestly_reports_as_drifted() {
    let (boot, _delta, _kb) = table().await;

    let body = lens_at(&boot.base, DOC_DECLARED, Some("declared")).await;
    assert_eq!(body["era"], "declared");

    let r = by_ordinal(&body, 0);
    // The CURRENT-tree verdict: the cited line's own content was rewritten
    // (`RevRemap` falls through to the token pass), and the token is found
    // again well outside the ±3 confirm window — an honest `drifted`, not a
    // `rev_remap`-confirmed move.
    assert_eq!(r["line_state"], "drifted", "{r}");
    assert_eq!(r["line_evidence"], "context_token");
    assert_eq!(r["remap"], "inside_change");
    assert_eq!(r["resolved_line"], 17);

    // The ADDITIVE "when written" verdict: evaluated against rev A's own
    // content, with the SAME hint (5) and no remap involved at all.
    assert_eq!(r["when_written"]["path_state_at_rev"], "present");
    assert_eq!(r["when_written"]["line_state_at_rev"], "confirmed");
}

/// The other half of the contrast: a citation that never existed at the
/// declared rev, even though the CURRENT tree confirms it outright.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn when_written_is_absent_for_a_path_that_never_existed_at_the_declared_rev() {
    let (boot, _delta, _kb) = table().await;

    let body = lens_at(&boot.base, DOC_DECLARED, Some("declared")).await;
    assert_eq!(body["era"], "declared");

    let r = by_ordinal(&body, 1);
    // The CURRENT tree has no complaint at all — `epsilon.rb` exists now and
    // the token resolves cleanly.
    assert_eq!(r["path_state"], "present");
    assert_eq!(r["line_state"], "confirmed");

    // But it never existed at the rev the doc declared.
    assert_eq!(r["when_written"]["path_state_at_rev"], "absent");
    assert_eq!(r["when_written"]["line_state_at_rev"], "absent");
}

/// A doc with no usable `kb-code-rev` has nothing to compare "when written"
/// against — honest absence, never a guess, even when the caller opted in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn era_is_none_and_when_written_is_null_without_a_usable_code_rev() {
    let (boot, _delta, _kb) = table().await;

    let body = lens_at(&boot.base, DOC_NO_REV, Some("declared")).await;
    assert_eq!(body["era"], "none");
    assert!(body["rev_remap"].is_null());
    for r in body["refs"].as_array().unwrap() {
        assert!(r["when_written"].is_null(), "{r}");
    }
}

/// Opt-in gating: without `?at=declared` the response is BYTE-IDENTICAL to
/// pre-CT-F2 — `era` defaults to `"none"` and every `when_written` is `null`
/// even on a doc that DOES carry a usable `kb-code-rev`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn when_written_and_era_stay_absent_without_the_at_declared_param() {
    let (boot, _delta, _kb) = table().await;

    let body = lens_at(&boot.base, DOC_DECLARED, None).await;
    assert_eq!(body["era"], "none");
    assert_eq!(
        body["rev_remap"]["state"], "applied",
        "the doc's rev is perfectly usable — this is an OPT-IN gate, not a rev problem"
    );
    for r in body["refs"].as_array().unwrap() {
        assert!(r["when_written"].is_null(), "{r}");
    }
}
