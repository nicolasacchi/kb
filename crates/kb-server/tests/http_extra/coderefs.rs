//! DCB W1.B — `GET /api/kb/{kb}/docs/{id}/code-refs` (`coderef/1`) and the
//! corpus cursor feed `GET /api/kb/{kb}/code-refs`. Boots the in-process
//! daemon (the `exclusions.rs`/`atlas_points_memo.rs` convention) and drives
//! it end-to-end via `reqwest` — kb-server route tests have no lighter-weight
//! harness (`kb_core::test_support` is an unrelated shuffling helper, not a
//! bare-`KbContext` constructor).
//!
//! The `workplan.html` fixture is W1.A's own synthetic golden fixture
//! (`crates/kb-core/tests/coderef_fixtures/workplan.html` — invented
//! content, kb is public). Reusing it here means the expected shape (ref
//! count, group keys, code_rev) is cross-checked against
//! `kb_core::coderefs::extract` directly rather than hand-duplicated.
use crate::common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

fn workplan_html() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../kb-core/tests/coderef_fixtures/workplan.html");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Contains a `<code>` element whose content matches neither the path nor
/// the symbol grammar — a genuine zero-ref scan. (Historical note,
/// DCB-W1.B.R: before that fix, `CodeRefHook::interested`'s cheap substring
/// pre-gate lived at the hook-admission level, so a doc with no `<code` /
/// `github.com` substring never ran the hook at all and stayed PERMANENTLY
/// `never_scanned`. The pre-gate has since moved inside `enrich()` — see
/// `prose_only_doc` below for a fixture with NO `<code`/GitHub signal at
/// all, which now also scans to zero refs rather than staying
/// never_scanned.)
fn empty_doc(title: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title></head>\
         <body><h1>{title}</h1><p><code>just some prose, no path here</code></p></body></html>"
    )
}

/// DCB-W1.B.R fixture: genuinely NO code signal at all (no `<code`, no
/// GitHub link) — the doc that used to be permanently `never_scanned`
/// before the substring pre-gate moved inside `enrich()`.
fn prose_only_doc(title: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title></head>\
         <body><h1>{title}</h1><p>Just prose about the roadmap, no code \
         signal anywhere in this document.</p></body></html>"
    )
}

fn session_doc(title: &str) -> String {
    // memory-session category — CodeRefHook::interested excludes it, so this
    // doc stays `never_scanned` forever despite containing a real `<code>`.
    format!(
        "<!doctype html><html><head><title>{title}</title>\
         <meta name=\"kb-category\" content=\"memory-session\"></head>\
         <body><pre>hi</pre><p>cites <code>app/models/order.rb:10</code></p></body></html>"
    )
}

async fn boot(files: &[(&str, String)]) -> (tempfile::TempDir, std::net::SocketAddr, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    for (name, html) in files {
        std::fs::write(source.join(name), html).unwrap();
    }

    let daemon_name = format!(
        "test-coderefs-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("smoke").unwrap(),
        KbSection {
            path: source.clone(),
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: BTreeMap::new(),
            memory_scope: None,
            default_search_category: None,
            code_url: None,
            decay_policy: None,
            versions: None,
            reading_progress: None,
            search: Default::default(),
            indexable_extensions: None,
            reconcile_secs: None,
            capture_dir: None,
            resurface: None,
            slo: None,
        },
    );
    let cfg = KbConfig {
        daemon: DaemonSection {
            name: Some(daemon_name.clone()),
        },
        server: ServerSection::default(),
        ui: UiSection::default(),
        indexer: Default::default(),
        storage: Default::default(),
        share: Default::default(),
        webhooks: None,
        defaults: DefaultsSection {
            embedding_model: None,
            disable_embedder_fallback: true,
        },
        retention: Default::default(),
        backup: Default::default(),
        identity: Default::default(),
        memory: Default::default(),
        kb: kb_map,
        projects: Default::default(),
        sessions: Default::default(),
    };
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;
    (tmp, addr, source)
}

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

async fn docs(client: &reqwest::Client, addr: std::net::SocketAddr) -> Vec<serde_json::Value> {
    client
        .get(url(addr, "/api/kb/smoke/docs?limit=50"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap_or_default()
}

async fn wait_for_doc(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    common::poll_until("a matching doc in kb `smoke`", || async {
        docs(client, addr).await.into_iter().find(&pred)
    })
    .await
}

async fn wait_for_doc_count(client: &reqwest::Client, addr: std::net::SocketAddr, n: usize) {
    common::poll_until(&format!("{n} docs indexed"), || async {
        let d = docs(client, addr).await;
        (d.len() >= n).then_some(())
    })
    .await;
}

async fn code_refs(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    id: &str,
) -> reqwest::Response {
    client
        .get(url(addr, &format!("/api/kb/smoke/docs/{id}/code-refs")))
        .send()
        .await
        .unwrap()
}

/// Poll `/code-refs` until the doc's extraction is visible (the hook runs
/// async as part of indexing; a doc can appear in `/docs` slightly before
/// its `code_refs_docs` row lands).
async fn wait_for_code_refs(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    id: &str,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    common::poll_until(
        &format!("code-refs for {id} matching predicate"),
        || async {
            let r = code_refs(client, addr, id).await;
            if r.status() != 200 {
                return None;
            }
            let body: serde_json::Value = r.json().await.ok()?;
            pred(&body).then_some(body)
        },
    )
    .await
}

#[tokio::test]
async fn for_doc_serves_coderef_1_for_the_fixture() {
    let html = workplan_html();
    let expected = kb_core::coderefs::extract(&html);
    let (_tmp, addr, _source) = boot(&[("workplan.html", html)]).await;
    let client = reqwest::Client::new();

    let doc = wait_for_doc(&client, addr, |d| {
        d["source_relative"].as_str() == Some("workplan.html")
    })
    .await;
    let id = doc["id"].as_str().unwrap().to_string();

    let body = wait_for_code_refs(&client, addr, &id, |b| {
        b["never_scanned"].as_bool() == Some(false)
    })
    .await;

    assert_eq!(body["schema"], "coderef/1");
    assert_eq!(body["kb"], "smoke");
    assert_eq!(body["doc_id"], id);
    assert_eq!(body["doc_path"], "workplan.html");
    assert_eq!(body["never_scanned"], false);
    assert_eq!(body["ref_count"].as_u64(), Some(expected.refs.len() as u64));
    assert_eq!(
        body["ungrouped_count"].as_u64(),
        Some(expected.ungrouped_count as u64)
    );
    // No heading-slug collisions in this fixture, so the reconstructed
    // groups[] count matches the in-process extraction 1:1 (a corpus with
    // colliding headings would NOT get this equality — see the
    // `build_groups_and_refs` doc comment in `routes/coderefs.rs`).
    assert_eq!(
        body["groups"].as_array().unwrap().len(),
        expected.groups.len()
    );
    let rev = &body["code_rev"];
    assert_eq!(rev["label"], "shopfront");
    assert_eq!(rev["sha"], "bcd13a1d3");
    assert_eq!(rev["dirty"], true);
}

#[tokio::test]
async fn every_ref_group_is_a_declared_group_key() {
    let (_tmp, addr, _source) = boot(&[("workplan.html", workplan_html())]).await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, |d| {
        d["source_relative"].as_str() == Some("workplan.html")
    })
    .await;
    let id = doc["id"].as_str().unwrap().to_string();
    let body = wait_for_code_refs(&client, addr, &id, |b| {
        b["never_scanned"].as_bool() == Some(false)
    })
    .await;

    let keys: std::collections::HashSet<&str> = body["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["key"].as_str().unwrap())
        .collect();
    for r in body["refs"].as_array().unwrap() {
        if let Some(g) = r["group"].as_str() {
            assert!(
                keys.contains(g),
                "ref.group {g:?} must equal some groups[].key"
            );
        }
    }
}

#[tokio::test]
async fn issue_ref_carries_number_in_line_start() {
    let (_tmp, addr, _source) = boot(&[("workplan.html", workplan_html())]).await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, |d| {
        d["source_relative"].as_str() == Some("workplan.html")
    })
    .await;
    let id = doc["id"].as_str().unwrap().to_string();
    let body = wait_for_code_refs(&client, addr, &id, |b| {
        b["never_scanned"].as_bool() == Some(false)
    })
    .await;

    let issues: Vec<&serde_json::Value> = body["refs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "issue")
        .collect();
    assert!(!issues.is_empty(), "fixture has issue refs");
    for i in issues {
        assert!(
            i["line_start"].as_u64().is_some(),
            "issue ref must carry a number in line_start: {i}"
        );
        assert!(i.get("href").is_none(), "R11: no issue.href on coderef/1");
        assert!(
            i["path_hint"].as_str().unwrap().contains('/'),
            "path_hint is <owner>/<repo>"
        );
    }
}

#[tokio::test]
async fn for_doc_404s_unknown_doc() {
    let (_tmp, addr, _source) = boot(&[("a.html", empty_doc("A"))]).await;
    let client = reqwest::Client::new();
    wait_for_doc_count(&client, addr, 1).await;

    let r = code_refs(&client, addr, "000000000000").await;
    assert_eq!(r.status(), 404);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["type"], "urn:kb:errors:not-found");
}

#[tokio::test]
async fn for_doc_400s_unsafe_id() {
    let (_tmp, addr, _source) = boot(&[("a.html", empty_doc("A"))]).await;
    let client = reqwest::Client::new();
    wait_for_doc_count(&client, addr, 1).await;

    let r = code_refs(&client, addr, ".hidden").await;
    assert_eq!(r.status(), 400);
}

#[tokio::test]
async fn never_scanned_doc_reports_the_flag_not_zero_refs() {
    let (_tmp, addr, _source) = boot(&[("session.html", session_doc("Session"))]).await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, |d| {
        d["source_relative"].as_str() == Some("session.html")
    })
    .await;
    let id = doc["id"].as_str().unwrap().to_string();

    // Give the hook registry a moment to run (it will correctly skip this
    // doc); the assertion is that it STAYS never_scanned, not a race against
    // it becoming scanned.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let r = code_refs(&client, addr, &id).await;
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["never_scanned"], true);
    assert!(body["extracted_at"].is_null());
    assert!(body["doc_hash"].is_null());
    assert_eq!(body["ref_count"].as_u64(), Some(0));
    assert!(body["refs"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn scanned_but_empty_doc_reports_zero_not_never_scanned() {
    let (_tmp, addr, _source) = boot(&[("empty.html", empty_doc("Empty"))]).await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, |d| {
        d["source_relative"].as_str() == Some("empty.html")
    })
    .await;
    let id = doc["id"].as_str().unwrap().to_string();
    let body = wait_for_code_refs(&client, addr, &id, |b| {
        b["never_scanned"].as_bool() == Some(false)
    })
    .await;

    assert_eq!(body["never_scanned"], false);
    assert_eq!(body["ref_count"].as_u64(), Some(0));
    assert!(body["extracted_at"].as_i64().is_some());
    assert!(body["doc_hash"].as_str().is_some());
}

/// DCB-W1.B.R — the honest `never_scanned` fix, ON THE WIRE. A doc with NO
/// `<code`/GitHub signal at all used to get NO `code_refs_docs` row (the old
/// substring pre-gate lived at `interested()`), so `never_scanned` stayed
/// `true` forever — unfixable by `kb reindex`. It must now scan to a
/// zero-ref header, exactly like `scanned_but_empty_doc_reports_zero_not_never_scanned`.
#[tokio::test]
async fn prose_only_doc_reports_scanned_zero_refs_not_never_scanned() {
    let (_tmp, addr, _source) = boot(&[("prose.html", prose_only_doc("Prose"))]).await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, |d| {
        d["source_relative"].as_str() == Some("prose.html")
    })
    .await;
    let id = doc["id"].as_str().unwrap().to_string();
    let body = wait_for_code_refs(&client, addr, &id, |b| {
        b["never_scanned"].as_bool() == Some(false)
    })
    .await;

    assert_eq!(body["never_scanned"], false);
    assert_eq!(body["ref_count"].as_u64(), Some(0));
    assert!(body["refs"].as_array().unwrap().is_empty());
    assert!(body["extracted_at"].as_i64().is_some());
    assert!(body["doc_hash"].as_str().is_some());
}

#[tokio::test]
async fn code_refs_301s_through_the_moves_chain() {
    let (_tmp, addr, _source) = boot(&[("workplan.html", workplan_html())]).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let doc = wait_for_doc(&client, addr, |d| {
        d["source_relative"].as_str() == Some("workplan.html")
    })
    .await;
    let old_id = doc["id"].as_str().unwrap().to_string();
    // Wait for the extraction so the moves-301 test isn't racing the hook.
    let before = wait_for_code_refs(&client, addr, &old_id, |b| {
        b["never_scanned"].as_bool() == Some(false)
    })
    .await;
    let ref_count_before = before["ref_count"].as_u64().unwrap();
    assert!(ref_count_before > 0, "the workplan fixture has real refs");

    let r = client
        .post(url(addr, &format!("/api/kb/smoke/docs/{old_id}/move")))
        .json(&serde_json::json!({ "to": "moved/workplan.html" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap_or_default());
    let body: serde_json::Value = r.json().await.unwrap();
    let new_id = body["new_id"].as_str().unwrap().to_string();

    let r = client
        .get(url(addr, &format!("/api/kb/smoke/docs/{old_id}/code-refs")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 301, "old id's code-refs must 301");
    let loc = r
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(loc, format!("/api/kb/smoke/docs/{new_id}/code-refs"));

    // Follow the redirect (by hand — this client disables auto-redirect so
    // the 301 assertion above stays exact) and confirm the NEW id serves the
    // identical extraction: same ref_count, never_scanned false. The moves
    // engine preserves embedding without re-extracting (invariant #27), so
    // this also pins that code-refs data survives a relocate.
    let after = code_refs(&client, addr, &new_id).await;
    assert_eq!(after.status(), 200);
    let after_body: serde_json::Value = after.json().await.unwrap();
    assert_eq!(after_body["never_scanned"], false);
    assert_eq!(after_body["ref_count"].as_u64(), Some(ref_count_before));
}

/// A minimal fixture citing exactly one path — used by the `by_target`
/// reverse-lookup tests below, where the interesting variable is WHICH docs
/// cite a given path, not the full extraction grammar (`workplan_html`
/// already covers that).
fn code_doc(title: &str, path: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title></head>\
         <body><h1>{title}</h1><p>see <code>{path}</code> for details</p></body></html>"
    )
}

// --- feed --------------------------------------------------------------

async fn feed(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    query: &str,
) -> serde_json::Value {
    client
        .get(url(addr, &format!("/api/kb/smoke/code-refs{query}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn corpus_files(n: usize) -> Vec<(&'static str, String)> {
    const NAMES: &[&str] = &["a.html", "b.html", "c.html", "d.html", "e.html"];
    NAMES[..n]
        .iter()
        .map(|&name| (name, empty_doc(name)))
        .collect()
}

/// Fixture generator for the `FEED_MAX_LIMIT` clamp test — needs MORE than
/// 100 docs, since a `?limit=100000` request can only be PROVEN to have
/// clamped by coming back with exactly 100 (never all N). `corpus_files`'s
/// static `NAMES` array tops out at 5; `Box::leak` mints the extra
/// `&'static str` names cheaply (test-only, per-process, bounded).
fn many_corpus_files(n: usize) -> Vec<(&'static str, String)> {
    (0..n)
        .map(|i| {
            let name: &'static str = Box::leak(format!("f{i:04}.html").into_boxed_str());
            (name, empty_doc(name))
        })
        .collect()
}

#[tokio::test]
async fn feed_pages_without_duplicates_or_gaps() {
    let files = corpus_files(5);
    let (_tmp, addr, _source) = boot(&files).await;
    let client = reqwest::Client::new();
    wait_for_doc_count(&client, addr, 5).await;
    // Give the code-ref hook time to write every row (each write is a sibling
    // side table with no generation bump to poll on, so the docs endpoint
    // being populated doesn't guarantee code_refs_docs is too).
    common::poll_until("5 docs on the feed", || async {
        let body = feed(&client, addr, "?limit=100").await;
        (body["docs"].as_array().unwrap().len() >= 5).then_some(())
    })
    .await;

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut cursor: Option<String> = None;
    loop {
        let q = match &cursor {
            Some(c) => format!("?limit=2&cursor={c}"),
            None => "?limit=2".to_string(),
        };
        let body = feed(&client, addr, &q).await;
        assert_eq!(body["schema"], "coderef-feed/1");
        for d in body["docs"].as_array().unwrap() {
            let id = d["doc_id"].as_str().unwrap().to_string();
            assert!(seen.insert(id.clone()), "duplicate doc {id} across pages");
        }
        cursor = body["next_cursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        seen.len(),
        5,
        "every doc must appear exactly once: {seen:?}"
    );
}

#[tokio::test]
async fn feed_omits_next_cursor_on_the_last_page() {
    let files = corpus_files(2);
    let (_tmp, addr, _source) = boot(&files).await;
    let client = reqwest::Client::new();
    wait_for_doc_count(&client, addr, 2).await;
    let body = common::poll_until("2 docs on the feed", || async {
        let body = feed(&client, addr, "?limit=100").await;
        (body["docs"].as_array().unwrap().len() >= 2).then_some(body)
    })
    .await;
    assert!(
        body.get("next_cursor").is_none(),
        "short page must omit next_cursor: {body}"
    );
}

#[tokio::test]
async fn feed_cursor_round_trips_as_opaque_string() {
    let files = corpus_files(3);
    let (_tmp, addr, _source) = boot(&files).await;
    let client = reqwest::Client::new();
    wait_for_doc_count(&client, addr, 3).await;
    let first = common::poll_until("3 docs, first page", || async {
        let body = feed(&client, addr, "?limit=2").await;
        (body["docs"].as_array().unwrap().len() == 2).then_some(body)
    })
    .await;
    let cursor = first["next_cursor"]
        .as_str()
        .expect("a next_cursor on a full page")
        .to_string();
    // "<extracted_at>:<artifact_id>" — decimal, one colon, then the id.
    let (ts_part, id_part) = cursor
        .split_once(':')
        .expect("opaque cursor has exactly one ':'");
    assert!(
        ts_part.parse::<i64>().is_ok(),
        "extracted_at half must be numeric: {cursor}"
    );
    assert_eq!(id_part.len(), 12, "artifact id half is 12-hex: {cursor}");

    let second = feed(&client, addr, &format!("?limit=2&cursor={cursor}")).await;
    let first_ids: Vec<&str> = first["docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["doc_id"].as_str().unwrap())
        .collect();
    let second_ids: Vec<&str> = second["docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["doc_id"].as_str().unwrap())
        .collect();
    for id in &second_ids {
        assert!(
            !first_ids.contains(id),
            "resuming at next_cursor must not repeat a row: {id}"
        );
    }
}

#[tokio::test]
async fn feed_rejects_malformed_cursor() {
    let (_tmp, addr, _source) = boot(&[("a.html", empty_doc("A"))]).await;
    let client = reqwest::Client::new();
    wait_for_doc_count(&client, addr, 1).await;

    for bad in ["notanumber", "", "123", "123:../etc"] {
        let r = client
            .get(url(addr, &format!("/api/kb/smoke/code-refs?cursor={bad}")))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "cursor {bad:?} must 400");
    }
}

#[tokio::test]
async fn feed_refs_zero_returns_headers_only() {
    let (_tmp, addr, _source) = boot(&[("workplan.html", workplan_html())]).await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, |d| {
        d["source_relative"].as_str() == Some("workplan.html")
    })
    .await;
    let id = doc["id"].as_str().unwrap().to_string();
    wait_for_code_refs(&client, addr, &id, |b| {
        b["never_scanned"].as_bool() == Some(false)
    })
    .await;

    let body = feed(&client, addr, "?limit=100&refs=0").await;
    let docs = body["docs"].as_array().unwrap();
    assert!(!docs.is_empty());
    for d in docs {
        assert!(
            d["refs"].as_array().unwrap().is_empty(),
            "refs=0 must return empty refs[]"
        );
        if d["doc_id"] == id {
            assert!(
                d["ref_count"].as_u64().unwrap() > 0,
                "counts must stay intact in headers-only mode"
            );
        }
    }
}

#[tokio::test]
async fn feed_clamps_limit() {
    let files = corpus_files(3);
    let (_tmp, addr, _source) = boot(&files).await;
    let client = reqwest::Client::new();
    wait_for_doc_count(&client, addr, 3).await;
    common::poll_until("3 docs on the feed", || async {
        let body = feed(&client, addr, "?limit=100").await;
        (body["docs"].as_array().unwrap().len() >= 3).then_some(())
    })
    .await;

    let body = feed(&client, addr, "?limit=0").await;
    assert_eq!(
        body["docs"].as_array().unwrap().len(),
        1,
        "limit=0 clamps to 1: {body}"
    );
}

/// A SEPARATE, >100-doc corpus — the only way to make "limit clamps to
/// FEED_MAX_LIMIT=100" falsifiable. With only 3 docs (as in
/// `feed_clamps_limit` above), `len() <= 100` is vacuously true whether or
/// not the server clamps at all.
#[tokio::test]
async fn feed_clamps_limit_above_max_to_100() {
    let files = many_corpus_files(105);
    let (_tmp, addr, _source) = boot(&files).await;
    let client = reqwest::Client::new();
    // NOT `wait_for_doc_count` — its `docs()` helper hardcodes `?limit=50`
    // (fine for every OTHER test in this file, which never seeds more than
    // 5 docs), so for 105 it would never observe `>= 105` and always time
    // out regardless of real indexing progress. Wait for the code-ref hook
    // to have written a header row for every doc (best-effort, async — same
    // caveat as `feed_pages_without_duplicates_or_gaps`) via a full
    // paginated walk at a SAFE (<=100) limit instead — readiness-polling
    // must not itself depend on the clamp behavior under test.
    common::poll_until("105 docs on the feed", || async {
        let mut total = 0usize;
        let mut cursor: Option<String> = None;
        loop {
            let q = match &cursor {
                Some(c) => format!("?limit=50&cursor={c}"),
                None => "?limit=50".to_string(),
            };
            let body = feed(&client, addr, &q).await;
            total += body["docs"].as_array().unwrap().len();
            cursor = body["next_cursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        (total >= 105).then_some(())
    })
    .await;

    let body = feed(&client, addr, "?limit=100000").await;
    let docs = body["docs"].as_array().unwrap();
    assert_eq!(
        docs.len(),
        100,
        "limit clamps to FEED_MAX_LIMIT=100 even when the corpus has more"
    );
    assert!(
        body["next_cursor"].as_str().is_some(),
        "a clamped-at-100 page out of 105 docs must carry a next_cursor"
    );
}

#[tokio::test]
async fn feed_is_not_since() {
    let files = corpus_files(2);
    let (_tmp, addr, _source) = boot(&files).await;
    let client = reqwest::Client::new();
    wait_for_doc_count(&client, addr, 2).await;
    let unfiltered = common::poll_until("2 docs on the feed", || async {
        let body = feed(&client, addr, "?limit=100").await;
        (body["docs"].as_array().unwrap().len() >= 2).then_some(body)
    })
    .await;
    // `?since=` is not a recognized param on this route — it must be
    // silently ignored (unknown-param passthrough), never applied as a
    // recency filter (that meaning belongs to `routes/docs.rs`'s `since`).
    let with_since = feed(&client, addr, "?limit=100&since=7d").await;
    assert_eq!(
        unfiltered["docs"].as_array().unwrap().len(),
        with_since["docs"].as_array().unwrap().len(),
        "?since= must not filter the feed"
    );
}

// --- by_target (CT-B3) --------------------------------------------------

#[tokio::test]
async fn by_target_resolves_every_doc_citing_the_exact_path() {
    let (_tmp, addr, _source) = boot(&[
        ("a.html", code_doc("A", "order.rb")),
        ("b.html", code_doc("B", "order.rb")),
        ("c.html", code_doc("C", "cart.rb")),
    ])
    .await;
    let client = reqwest::Client::new();
    wait_for_doc_count(&client, addr, 3).await;
    // Wait for the code-ref hook to have written every row — same caveat as
    // the cursor-feed tests above (a sibling table, no generation to poll).
    common::poll_until("3 docs on the unfiltered feed", || async {
        let body = feed(&client, addr, "?limit=100").await;
        (body["docs"].as_array().unwrap().len() >= 3).then_some(())
    })
    .await;

    let body = feed(&client, addr, "?by_target=order.rb").await;
    assert_eq!(body["schema"], "coderef-feed/1");
    assert_eq!(body["kb"], "smoke");
    assert!(
        body.get("next_cursor").is_none(),
        "by_target is a complete resolution, never paginated: {body}"
    );
    let paths: std::collections::HashSet<&str> = body["docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["doc_path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        std::collections::HashSet::from(["a.html", "b.html"]),
        "only the two docs citing order.rb must come back: {body}"
    );
}

#[tokio::test]
async fn by_target_no_match_returns_an_empty_docs_array_not_an_error() {
    let (_tmp, addr, _source) = boot(&[("a.html", code_doc("A", "order.rb"))]).await;
    let client = reqwest::Client::new();
    wait_for_doc_count(&client, addr, 1).await;
    common::poll_until("1 doc on the unfiltered feed", || async {
        let body = feed(&client, addr, "?limit=100").await;
        (!body["docs"].as_array().unwrap().is_empty()).then_some(())
    })
    .await;

    let body = feed(&client, addr, "?by_target=nope.rb").await;
    assert!(body["docs"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn by_target_refs_zero_returns_headers_only() {
    let (_tmp, addr, _source) = boot(&[("a.html", code_doc("A", "order.rb"))]).await;
    let client = reqwest::Client::new();
    wait_for_doc_count(&client, addr, 1).await;
    common::poll_until("1 doc cites order.rb", || async {
        let body = feed(&client, addr, "?by_target=order.rb").await;
        (!body["docs"].as_array().unwrap().is_empty()).then_some(())
    })
    .await;

    let body = feed(&client, addr, "?by_target=order.rb&refs=0").await;
    let docs = body["docs"].as_array().unwrap();
    assert_eq!(docs.len(), 1);
    assert!(docs[0]["refs"].as_array().unwrap().is_empty());
    assert!(
        docs[0]["ref_count"].as_u64().unwrap() > 0,
        "counts stay intact"
    );
}
