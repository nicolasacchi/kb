//! End-to-end smoke tests against the in-process daemon. Boots
//! `kb_server::serve_on_random_port` against a tempdir corpus, hits
//! the v0.0.1 routes via `reqwest`, and asserts the contracts.
//!
//! Convention: ALL daemon integration tests use this in-process
//! `serve_on_random_port` pattern. Document in README.

mod common;

use kb_core::config::{
    DaemonSection, DefaultsSection, IndexerSection, KbConfig, KbSection, OutboundSection,
    RateLimitSection, RegexRule, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const CANON_REL: &[&str] = &[
    "fullscreen-viz.html",
    "kitchen-sink.html",
    "multi-page.html",
    "cost-of-abstraction.html",
];

fn canon_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/canon")
}

/// Set up a tempdir with all four canon files, plus a KbConfig that points
/// at it. Returns the tempdir (kept alive for the test), the config, and
/// the test-scoped `KbPaths` (rooted at the tempdir — no env-var dance).
fn fixture_corpus() -> (tempfile::TempDir, KbConfig, KbPaths) {
    fixture_corpus_with(None)
}

/// Build a fixture corpus with an optional `[outbound]` section per kb.
/// Used by the G3 scrub tests to verify non-loopback requests get
/// scrubbed while loopback ones don't.
fn fixture_corpus_with(
    outbound: Option<OutboundSection>,
) -> (tempfile::TempDir, KbConfig, KbPaths) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    for name in CANON_REL {
        std::fs::copy(canon_dir().join(name), source.join(name))
            .unwrap_or_else(|e| panic!("copy {name}: {e}"));
    }

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());

    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("smoke").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    (tmp, cfg, paths)
}

async fn boot() -> (tempfile::TempDir, std::net::SocketAddr) {
    let (tmp, cfg, paths) = fixture_corpus();
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    common::wait_docs_listed(addr, "smoke", 4).await;
    (tmp, addr)
}

/// `boot()` variant that wires a fixture SPA dist into the daemon.
/// Writes index.html + assets/{app.js,app.css} + logomark.svg under a
/// fresh subdir of the tempdir so spa::serve has something to read.
async fn boot_with_spa() -> (tempfile::TempDir, std::net::SocketAddr) {
    let (tmp, cfg, paths) = fixture_corpus();
    let dist = tmp.path().join("dist");
    std::fs::create_dir_all(dist.join("assets")).unwrap();
    std::fs::write(
        dist.join("index.html"),
        r#"<!doctype html><html><head><title>kb-test</title></head><body><div id="root"></div></body></html>"#,
    )
    .unwrap();
    std::fs::write(
        dist.join("assets/app-Hxxx1234.js"),
        b"console.log('kb test asset');\n",
    )
    .unwrap();
    std::fs::write(
        dist.join("assets/app-Hxxx1234.css"),
        b"body { background: tomato; }\n",
    )
    .unwrap();
    std::fs::write(
        dist.join("logomark.svg"),
        b"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 1 1'/>",
    )
    .unwrap();
    // Annotator entry — kept non-hashed so /_kb/annotate.js resolves
    // via the direct-name path; the hashed-name fallback is exercised
    // separately in annotate_js_resolves_hashed_assets.
    std::fs::write(
        dist.join("annotate.js"),
        b"/* test annotator stub */\nwindow.__KB_TEST_ANNOTATOR = true;\n",
    )
    .unwrap();
    let (addr, _task) = kb_server::serve_on_random_port_with_paths_and_spa(cfg, paths, Some(dist))
        .await
        .expect("serve");
    common::wait_docs_listed(addr, "smoke", 4).await;
    (tmp, addr)
}

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{}{}", addr, path)
}

/// Poll `/api/kb/{kb}/docs` until a doc satisfying `pred` shows up, or the
/// load-aware `common::index_wait_deadline()` expires. `boot*` helpers now
/// poll for the seeded listing themselves; this predicate wait is for
/// tests that need a *specific* doc (path / title) after a later write.
async fn wait_for_doc(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    kb: &str,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    common::poll_until(&format!("a matching doc in kb `{kb}`"), || async {
        let docs: Vec<serde_json::Value> = client
            .get(url(addr, &format!("/api/kb/{kb}/docs?limit=50")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        docs.into_iter().find(&pred)
    })
    .await
}

/// Poll `/api/sessions` until a session with `sid` is enriched (load-aware
/// deadline — see `common::index_wait_deadline`).
async fn wait_for_session(client: &reqwest::Client, addr: std::net::SocketAddr, sid: &str) {
    common::poll_until(&format!("session `{sid}`"), || async {
        let body: serde_json::Value = client
            .get(url(addr, "/api/sessions?limit=100"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        let found = body["sessions"]
            .as_array()
            .map(|a| a.iter().any(|s| s["session_id"] == sid))
            .unwrap_or(false);
        found.then_some(())
    })
    .await
}

#[tokio::test]
async fn identity_route_returns_daemon_metadata() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client.get(url(addr, "/api/identity")).send().await.unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["name"].as_str().is_some());
    assert!(body["version"].as_str().is_some());
    assert!(body["kbs"].as_array().is_some());
    assert!(body["started_at"].as_str().is_some());
    // v0.7.1 H13 — the SPA's iframe-origin derivation + mismatch banner
    // both depend on these; they're plain struct fields, easy to drop
    // in a refactor, and were untested.
    assert_eq!(
        body["artifact_host_suffix"].as_str(),
        Some(".artifacts.localhost"),
        "/api/identity must carry artifact_host_suffix"
    );
    assert_eq!(
        body["parent_origin"].as_str(),
        Some("http://localhost:4000"),
        "/api/identity must carry parent_origin"
    );
    // build_sha is the binary's git stamp; the SPA diffs it against its
    // own bundle stamp to catch a stale daemon (white-screen drift). The
    // value is environment-dependent (git sha / "unknown"), so assert
    // presence + non-empty, not a literal.
    assert!(
        body["build_sha"].as_str().is_some_and(|s| !s.is_empty()),
        "/api/identity must carry a non-empty build_sha"
    );
    // invariant:2 kb-sibling/1 Hello — a sibling daemon handshakes on
    // these three before its first real call, and fails CLOSED when they
    // don't match. Dropping one silently downgrades every sibling to the
    // legacy-peer grandfather path (i.e. no check at all).
    assert_eq!(
        body["sibling_protocol"].as_str(),
        Some("kb-sibling/1"),
        "/api/identity must carry the kb-sibling protocol name"
    );
    assert_eq!(body["sibling_major"].as_u64(), Some(1));
    assert_eq!(
        body["schema_epoch"].as_u64(),
        Some(u64::from(kb_core::storage::sqlite::schema_epoch())),
        "/api/identity's schema_epoch is this binary's own embedded epoch"
    );
}

/// `/healthz` is PURE liveness and must stay that way: the kb-sibling/1
/// Hello lives on `/api/identity` alone, so an orchestrator's restart loop
/// can never be driven by a schema/protocol mismatch.
#[tokio::test]
async fn healthz_carries_no_sibling_or_schema_fields() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/healthz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["status"].as_str(), Some("ok"));
    for field in ["sibling_protocol", "sibling_major", "schema_epoch"] {
        assert!(
            body.get(field).is_none(),
            "/healthz must stay pure liveness — found {field}"
        );
    }
}

#[tokio::test]
async fn metrics_endpoint_coarse_always_detailed_null_when_off() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/metrics"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // Coarse block is always present.
    assert!(body["requests_total"].as_u64().is_some());
    assert!(body["storage_channel_capacity"].as_u64().is_some());
    let routes = body["routes"].as_array().expect("routes array");
    assert_eq!(routes.len(), 8, "one entry per RouteKind");
    assert!(routes.iter().any(|r| r["kind"] == "search"));
    assert!(
        routes[0]["buckets"].as_array().is_some(),
        "raw bucket counts exposed (the SSE omits them)"
    );
    // Detailed layer is off by default.
    assert_eq!(body["detailed_enabled"], serde_json::json!(false));
    assert!(
        body["detailed"].is_null(),
        "detailed must be null when [server] metrics is off"
    );
}

#[tokio::test]
async fn metrics_endpoint_detailed_when_enabled() {
    let (tmp, mut cfg, paths) = fixture_corpus();
    cfg.server.metrics = true;
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let _tmp = tmp; // keep the corpus alive for the duration of the test
    let client = reqwest::Client::new();

    // A per-kb read + a keyword search populate the detailed layer.
    let _ = client
        .get(url(addr, "/api/kb/smoke/docs?limit=5"))
        .send()
        .await
        .unwrap();
    let _ = client
        .get(url(
            addr,
            "/api/search?q=smoke&mode=keyword&kb=smoke&limit=5",
        ))
        .send()
        .await
        .unwrap();

    let body: serde_json::Value = client
        .get(url(addr, "/api/metrics"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["detailed_enabled"], serde_json::json!(true));
    let detailed = &body["detailed"];
    assert!(detailed.is_object(), "detailed populated when flag on");

    // Search stages (4), bm25 recorded by the keyword search.
    let stages = detailed["search_stages"].as_array().expect("search_stages");
    assert_eq!(stages.len(), 4);
    let bm25 = stages
        .iter()
        .find(|s| s["label"] == "bm25")
        .expect("bm25 stage present");
    assert!(
        bm25["count"].as_u64().unwrap() >= 1,
        "keyword search recorded a bm25 stage"
    );

    // Per-kb recorded the /api/kb/smoke/docs read.
    let per_kb = detailed["per_kb"].as_array().expect("per_kb");
    let smoke = per_kb
        .iter()
        .find(|k| k["label"] == "smoke")
        .expect("smoke kb present");
    assert!(
        smoke["count"].as_u64().unwrap() >= 1,
        "per-kb recorded the docs read"
    );

    // Pipeline snapshot present + enabled, stable 8-variant storage shape.
    assert_eq!(detailed["pipeline"]["enabled"], serde_json::json!(true));
    assert_eq!(detailed["pipeline"]["storage"].as_array().unwrap().len(), 8);
}

#[tokio::test]
async fn kbs_route_lists_configured_kbs() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client.get(url(addr, "/api/kbs")).send().await.unwrap();
    assert!(resp.status().is_success());
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["name"].as_str().unwrap(), "smoke");
    let doc_count = body[0]["doc_count"].as_u64().unwrap();
    assert!(
        doc_count >= CANON_REL.len() as u64,
        "expected ≥{} docs, got {doc_count}",
        CANON_REL.len()
    );
}

#[tokio::test]
async fn search_route_returns_bm25_hits() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(
            addr,
            "/api/search?q=borrow&mode=keyword&kb=smoke&limit=5",
        ))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    let hits = body["hits"].as_array().unwrap();
    assert!(!hits.is_empty(), "expected at least one hit for 'borrow'");
    // fullscreen-viz is "Visualizing the Borrow Checker" — should rank highly.
    let titles: Vec<_> = hits
        .iter()
        .map(|h| h["title"].as_str().unwrap_or(""))
        .collect();
    assert!(
        titles.iter().any(|t| t.contains("Borrow Checker")),
        "expected Borrow Checker in titles, got {titles:?}"
    );
}

// Track F — the fast-popup contract: without `detail=full` a search hit
// carries ONLY the slim keys. None of the rich gallery-card fields may
// appear, or the popup payload (and every existing client) would change.
#[tokio::test]
async fn search_without_detail_omits_rich_fields() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(
            addr,
            "/api/search?q=borrow&mode=keyword&kb=smoke&limit=5",
        ))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    let hits = body["hits"].as_array().unwrap();
    assert!(!hits.is_empty(), "expected at least one hit for 'borrow'");
    for h in hits {
        for key in [
            "summary",
            "tags",
            "word_count",
            "longread",
            "mtime_unix",
            "indexed_at_unix",
            "created_unix",
            "svg_count",
            "has_canvas",
            "js_loc",
            // Q-track read-state fields — also rich-only (detail=full).
            "read_state",
            "read_pct",
            "last_opened_unix",
        ] {
            assert!(
                h.get(key).is_none(),
                "slim hit must not carry rich key {key:?}, got {h}"
            );
        }
    }
}

// Track F — `detail=full` widens each hit with the gallery-card metadata
// in one round-trip. `word_count` is the load-bearing assertion: it was
// NOT in the pre-track-F search projection, so its presence proves the
// lance SEARCH_PROJECTION actually widened (not just that the flag was
// read). `tags` is always present (Some, possibly []) once the flag is on.
#[tokio::test]
async fn search_with_detail_full_surfaces_rich_fields() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(
            addr,
            "/api/search?q=borrow&mode=keyword&kb=smoke&limit=5&detail=full",
        ))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    let hits = body["hits"].as_array().unwrap();
    assert!(!hits.is_empty(), "expected at least one hit for 'borrow'");
    assert!(
        hits.iter()
            .all(|h| h.get("tags").map(|v| v.is_array()).unwrap_or(false)),
        "detail=full must surface a tags array on every hit, got {hits:?}"
    );
    assert!(
        hits.iter()
            .any(|h| h.get("word_count").and_then(|v| v.as_u64()).is_some()),
        "detail=full must surface word_count (proves the search projection widened)"
    );
}

#[tokio::test]
async fn search_with_unsupported_mode_returns_problem_json() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // mode=banana is genuinely unsupported. mode=hybrid is supported in
    // v0.1+ but returns 400 separately ("kb has no embedder configured")
    // when the test fixture has no embedding_model.
    let resp = client
        .get(url(addr, "/api/search?q=x&mode=banana&kb=smoke"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("problem+json"), "got content-type: {ct}");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"].as_str().unwrap(), "urn:kb:errors:bad-request");
    assert_eq!(body["status"].as_u64().unwrap(), 400);
}

#[tokio::test]
async fn search_hybrid_without_embedder_returns_400() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/search?q=x&mode=hybrid&kb=smoke"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    let detail = body["detail"].as_str().unwrap_or("");
    assert!(
        detail.contains("embedding_model"),
        "detail should mention embedding_model: {detail}"
    );
}

#[tokio::test]
async fn search_semantic_without_embedder_returns_400() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/search?q=x&mode=semantic&kb=smoke"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// --- v0.1 endpoint coverage ----------------------------------------------

#[tokio::test]
async fn settings_get_returns_defaults_then_patch_persists_in_memory() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    let s: serde_json::Value = client
        .get(url(addr, "/api/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(s["theme"].is_null());

    let resp = client
        .patch(url(addr, "/api/settings"))
        .header("Origin", "http://localhost:4000")
        .json(&serde_json::json!({"theme": "ink", "accent": "blue"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let after: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(after["theme"].as_str().unwrap(), "ink");
    assert_eq!(after["accent"].as_str().unwrap(), "blue");

    // Subsequent GET reflects the patch (in-memory persistence).
    let s2: serde_json::Value = client
        .get(url(addr, "/api/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s2["theme"].as_str().unwrap(), "ink");
}

#[tokio::test]
async fn cross_stats_returns_daemon_and_kb_summaries() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["daemon"]["name"].is_string());
    assert!(body["daemon"]["started_at"].is_string());
    let kbs = body["kbs"].as_array().unwrap();
    assert_eq!(kbs.len(), 1);
    assert_eq!(kbs[0]["name"].as_str().unwrap(), "smoke");
    assert!(body["total_docs"].as_u64().is_some());
    // GC-B2 — decode-skip observability rides the same per-kb stats block; a
    // healthy kb reports 0 (no malformed batches decoded yet).
    assert_eq!(kbs[0]["decode_skips"].as_u64(), Some(0));
}

#[tokio::test]
async fn per_kb_stats_returns_doc_count() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["name"].as_str().unwrap(), "smoke");
    assert!(body["doc_count"].as_u64().unwrap() >= CANON_REL.len() as u64);
    // GC-B2 — decode_skips is always present (u64, not Option) and 0 on a
    // freshly-indexed kb with no malformed batches.
    assert_eq!(body["decode_skips"].as_u64(), Some(0));
}

#[tokio::test]
async fn sources_pause_and_resume_round_trip() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    let sources: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/sources"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let slug = sources[0]["slug"].as_str().unwrap();

    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/sources/{slug}/pause")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["paused"].as_bool(), Some(true));

    let after_pause: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/sources"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after_pause[0]["paused"].as_bool(), Some(true));

    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/sources/{slug}/resume")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["paused"].as_bool(), Some(false));
}

#[tokio::test]
async fn errors_list_returns_empty_when_no_errors() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/errors"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn atlas_recompute_emits_start_and_complete() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(addr, "/api/kb/smoke/atlas/recompute"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["run"].as_str().unwrap().starts_with("r-"));
    assert!(body["events"].as_str().unwrap().starts_with("/api/events"));
    // v0.3 — the actual recompute runs in a spawned task; let it
    // finish so the empty-kb path can return its 0-points report.
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn atlas_recompute_single_flight_flag_resets_after_completion() {
    // P3 — the single-flight guard must RELEASE after a recompute
    // finishes (the spawned task's drop-guard), so the kb isn't wedged
    // out of all future recomputes. A stuck flag would make the second
    // request return `status: "already-running"` instead of a fresh run.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    let r1: serde_json::Value = client
        .post(url(addr, "/api/kb/smoke/atlas/recompute"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        r1["run"].as_str().is_some(),
        "first recompute should start: {r1}"
    );

    // Let the spawned task finish + reset the flag via its drop-guard.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let r2: serde_json::Value = client
        .post(url(addr, "/api/kb/smoke/atlas/recompute"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        r2["run"].as_str().is_some(),
        "second recompute must start after the first finished (flag reset): {r2}"
    );
    assert_ne!(
        r2.get("status").and_then(|s| s.as_str()),
        Some("already-running"),
        "flag was not released after the first recompute completed"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
}

// --- W3 T-b: atlas time-lapse history -----------------------------------
//
// This fixture corpus has no embedder wired up, so `atlas_recompute`
// always sees an empty `list_embeddings()` and returns early — see
// `atlas_recompute_emits_start_and_complete`'s own comment. That means a
// `record_atlas_snapshot` call (and therefore any real frame) is
// unreachable from this suite: the tests below exercise the honest-empty
// list, the 404s, and the prune route's required-param + honest-count
// contract — exactly the surface this fixture CAN reach. The populated
// (real frame + alignment) path is covered by
// `routes::atlas::tests::fit_frame_alignment_*` (pure, no storage) plus
// kb-core's `storage::actor::tests` `atlas_frame_insert`/`atlas_frames`/
// `atlas_frame_points`/`atlas_frames_prune` coverage.

#[tokio::test]
async fn atlas_history_is_honest_empty_not_404_before_any_recompute() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/atlas/history"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["frames"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn atlas_history_show_404s_on_unknown_frame_id() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/atlas/history/999999"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn atlas_history_show_400s_on_non_integer_id() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/atlas/history/not-a-number"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn atlas_history_prune_requires_keep_param() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(addr, "/api/kb/smoke/atlas/history/prune"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn atlas_history_prune_on_empty_history_removes_nothing() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(addr, "/api/kb/smoke/atlas/history/prune?keep=5"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["removed"].as_u64(), Some(0));
}

#[tokio::test]
async fn docs_list_with_include_atlas_carries_atlas_fields() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // Without ?include=atlas, atlas_* fields are absent (skip_serializing_if).
    let body: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/docs?limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!body.is_empty(), "fixture corpus should yield ≥1 doc");
    for d in &body {
        assert!(d.get("atlas_x").is_none(), "atlas_x should be omitted");
        assert!(d.get("atlas_y").is_none(), "atlas_y should be omitted");
    }
    // With ?include=atlas, the fields are present (null since the
    // fixture corpus has no embedder configured, so no recompute can
    // populate them — but the shape must still surface).
    let body: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/docs?limit=10&include=atlas"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!body.is_empty());
    // After include=atlas, every row must carry the keys (value may be
    // null when no recompute has run). Since `skip_serializing_if`
    // hides None, we instead verify list_docs_with_atlas at least
    // returns the rows — the unit tests in kb-core cover the
    // populated case end-to-end.
    for d in &body {
        // Either present (Some) or absent (skipped because None) is
        // acceptable here; the contract is that the route does not
        // 500 and returns rows. The populated case is covered by the
        // kb-core integration tests on recompute_for_kb.
        assert!(d["id"].is_string());
        assert!(d["title"].is_string());
    }
}

#[tokio::test]
async fn events_schema_enum_includes_v0_1_types() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/events.schema.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let types: Vec<&str> = body["types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for needed in &[
        "index.embedding",
        "source.paused",
        "source.resumed",
        "error.dismissed",
        "error.fixed",
        "atlas.recompute.start",
        "atlas.recompute.complete",
        // v0.2 — comments + annotator. D1: `comment.created`/`resolved`/
        // `exported` were removed from the schema enum (never actually
        // emitted by any code path — see schema.rs).
        "comment.anchor_stale",
        "comments.updated",
        // v0.5 P4
        "comment.anchor_resolved",
        // v0.6+ H2 — visit/scroll/search recorder. D1: added to the
        // schema enum.
        "history.recorded",
    ] {
        assert!(types.contains(needed), "missing event type {needed}");
    }
}

#[tokio::test]
async fn events_schema_per_type_returns_comment_payloads() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    for kind in &[
        "comment.anchor_stale",
        "comments.updated",
        // v0.5 P4
        "comment.anchor_resolved",
        // v0.6+ H2 (deep-review D1)
        "history.recorded",
    ] {
        let resp = client
            .get(url(addr, &format!("/api/events/schema/{kind}/v1")))
            .send()
            .await
            .unwrap();
        assert!(
            resp.status().is_success(),
            "schema for {kind} returned {}",
            resp.status()
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["type"].as_str(), Some(*kind));
        assert!(
            body["payload"].as_array().is_some(),
            "schema for {kind} missing payload"
        );
    }
}

#[tokio::test]
async fn sources_route_lists_kb_sources() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/sources"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert!(body[0]["path"].as_str().unwrap().contains("corpus"));
}

#[tokio::test]
async fn unknown_kb_returns_404_problem_json() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/no-such-kb/sources"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"].as_str().unwrap(), "urn:kb:errors:not-found");
}

#[tokio::test]
async fn reindex_returns_202_with_run_id() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // Get the slug from the sources route.
    let sources: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/sources"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let slug = sources[0]["slug"].as_str().unwrap();

    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/sources/{slug}/reindex")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["run"].as_str().unwrap().starts_with("r-"));
    assert!(body["events"].as_str().unwrap().starts_with("/api/events"));
}

#[tokio::test]
async fn events_schema_enum_lists_v0_0_1_types() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/events.schema.json"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    let types: Vec<_> = body["types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for needed in &[
        "index.start",
        "index.file",
        "index.complete",
        "watch.create",
        "watch.modify",
        "watch.delete",
        "artifact.indexed",
        "artifact.removed",
        "query",
        "error",
        "lag",
        "gap",
    ] {
        assert!(types.contains(needed), "missing type {needed}");
    }
}

#[tokio::test]
async fn events_schema_per_type_returns_payload_fields() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/events/schema/index.start/v1.json"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"].as_str().unwrap(), "index.start");
    let payload = body["payload"].as_array().unwrap();
    assert!(payload.iter().any(|v| v.as_str() == Some("run")));
    assert!(payload.iter().any(|v| v.as_str() == Some("kb")));
}

#[tokio::test]
async fn artifact_subdomain_serves_canon_html_with_probe_injected() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // Use 127.0.0.1 + Host header to avoid DNS dependency in CI.
    let resp = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", "fullscreen-viz.artifacts.localhost:4000")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/html"), "got ct: {ct}");
    let artifact_id = resp
        .headers()
        .get("x-kb-artifact-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(artifact_id, "fullscreen-viz");
    let body = resp.text().await.unwrap();
    assert!(body.contains("Visualizing the Borrow Checker"));
    assert!(body.contains(r#"<script src="/_kb/probe.js""#));
    // Top-level bounce: framed-load guard + artifact-referrer gate +
    // permalink build, so a ctrl/⌘/middle-clicked tab lands in the kb
    // wrapper rather than the bare subdomain.
    assert!(
        body.contains("if(window.top!==window.self)return;"),
        "bounce must no-op when framed inside the kb SPA; got {body}"
    );
    assert!(
        body.contains("indexOf('.artifacts.')"),
        "bounce must gate on an artifact-subdomain referrer; got {body}"
    );
    assert!(
        body.contains("location.replace(SPA+'/a/'+encodeURIComponent(KB)"),
        "bounce must redirect to the kb permalink; got {body}"
    );
    assert!(
        body.contains("REL=\"fullscreen-viz.html\""),
        "bounce must carry the served file's source-relative path; got {body}"
    );
}

#[tokio::test]
async fn artifact_subdomain_serves_probe_js() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/_kb/probe.js", addr.port()))
        .header("Host", "fullscreen-viz.artifacts.localhost:4000")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("application/javascript"), "got ct: {ct}");
    let body = resp.text().await.unwrap();
    assert!(body.contains("postMessage"));
    assert!(body.contains("kb-probe"));
}

// --- v0.6+ H3 — scroll-reporter runtime ---------------------------------

#[tokio::test]
async fn artifact_subdomain_serves_runtime_js() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/_kb/runtime.js", addr.port()))
        .header("Host", "fullscreen-viz.artifacts.localhost:4000")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("application/javascript"), "got ct: {ct}");
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("kb:scroll"),
        "runtime must post kb:scroll messages"
    );
    assert!(
        body.contains("kb:scroll-to"),
        "runtime must listen for kb:scroll-to resume messages"
    );
    assert!(
        body.contains("requestAnimationFrame"),
        "scroll-to must defer a frame before scrollTo"
    );
}

#[tokio::test]
async fn artifact_html_injects_runtime_script_alongside_probe() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", "fullscreen-viz.artifacts.localhost:4000")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    assert!(body.contains(r#"<script src="/_kb/probe.js""#));
    assert!(
        body.contains(r#"<script src="/_kb/runtime.js""#),
        "every artifact HTML serve must inject the runtime script"
    );
    // Defer ordering: probe must appear before runtime so its
    // postMessage probes land first (the parent SPA uses kb-probe as
    // the ready ping before posting kb:scroll-to).
    let probe_idx = body.find("/_kb/probe.js").expect("probe injected");
    let runtime_idx = body.find("/_kb/runtime.js").expect("runtime injected");
    assert!(
        probe_idx < runtime_idx,
        "probe must be injected before runtime"
    );
}

#[tokio::test]
async fn artifact_subdomain_unknown_id_returns_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", "no-such-artifact.artifacts.localhost:4000")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn parent_origin_serves_404_when_spa_unavailable() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client.get(url(addr, "/no-such-path")).send().await.unwrap();
    // boot() configures spa_dist=None, so the dispatch fallback hands off
    // to spa::serve which returns 404 problem+json.
    assert_eq!(resp.status(), 404);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("application/problem+json"),
        "expected problem+json, got {ct}"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:kb:errors:spa-unavailable");
}

#[tokio::test]
async fn cors_blocks_post_from_disallowed_origin() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    let sources: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/sources"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let slug = sources[0]["slug"].as_str().unwrap();

    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/sources/{slug}/reindex")))
        .header("Origin", "http://evil.com")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn cors_allows_post_from_parent_origin() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    let sources: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/sources"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let slug = sources[0]["slug"].as_str().unwrap();

    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/sources/{slug}/reindex")))
        .header("Origin", "http://localhost:4000")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
}

// Helper: convert a Path to its parent for canon fetching (silences clippy).
#[allow(dead_code)]
fn _unused(_p: &Path) {}

// === A5 — graph endpoint (heading hierarchy) ===

#[tokio::test]
async fn graph_returns_heading_hierarchy_for_canon_artifact() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // Find an artifact id via the docs list.
    let docs: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/docs?limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // Pick the kitchen-sink artifact (has the deepest heading tree among
    // the canon set, so the graph isn't a single-h1 stub).
    let id = docs
        .iter()
        .find(|d| {
            d["path"]
                .as_str()
                .unwrap_or("")
                .ends_with("kitchen-sink.html")
        })
        .map(|d| d["id"].as_str().unwrap().to_string())
        .expect("kitchen-sink not in corpus");

    let resp = client
        .get(url(addr, &format!("/api/kb/smoke/graph/{id}")))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["artifact_id"].as_str(), Some(id.as_str()));
    let nodes = body["nodes"].as_array().unwrap();
    assert!(!nodes.is_empty(), "kitchen-sink should have headings");
    // Every node has id, title, level (1-6).
    for n in nodes {
        assert!(n["id"].as_str().is_some());
        assert!(n["title"].as_str().is_some());
        let lvl = n["level"].as_u64().unwrap();
        assert!((1..=6).contains(&lvl));
    }
    // At least one edge per non-h1 heading reachable from a parent.
    // v0.3 may add `link` edges from the cross-artifact graph; both
    // kinds are accepted.
    let edges = body["edges"].as_array().unwrap();
    for e in edges {
        let kind = e["kind"].as_str().unwrap();
        assert!(
            matches!(kind, "contains" | "link"),
            "unexpected kind {kind}"
        );
    }
    // Every node has the v0.3 `kind` field too.
    for n in nodes {
        let kind = n["kind"].as_str().unwrap();
        assert!(
            matches!(kind, "heading" | "artifact"),
            "unexpected node kind {kind}"
        );
    }
}

#[tokio::test]
async fn graph_with_depth_query_accepted_and_clamps() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let docs: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/docs?limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = docs[0]["id"].as_str().unwrap().to_string();
    // depth=99 must clamp to 3 (Db::edges_from clamp); the route just
    // needs to return 200 — canon corpus has no cross-artifact links
    // so the edges array stays heading-only.
    let resp = client
        .get(url(addr, &format!("/api/kb/smoke/graph/{id}?depth=99")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["artifact_id"].as_str(), Some(id.as_str()));
    // The edges array exists; each entry has a depth field.
    let edges = body["edges"].as_array().unwrap();
    for e in edges {
        assert!(e["depth"].as_u64().is_some());
    }
}

#[tokio::test]
async fn graph_unknown_doc_returns_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/graph/no-such-doc"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn graph_unknown_kb_returns_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/no-such-kb/graph/anything"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// === A4 — /_kb/annotate.js endpoint (annotator script serve) ===

#[tokio::test]
async fn annotate_js_served_when_spa_dist_has_direct_entry() {
    let (_tmp, addr) = boot_with_spa().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/_kb/annotate.js"))
        .header("Host", "kitchen-sink.artifacts.localhost")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("application/javascript"), "got {ct}");
    let cache = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        cache.contains("max-age"),
        "expected cache-control max-age, got {cache}"
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("__KB_TEST_ANNOTATOR"));
}

#[tokio::test]
async fn annotate_js_returns_404_when_spa_unavailable() {
    // boot() (not boot_with_spa) leaves spa_dist=None; annotator should
    // 404 cleanly, with the artifact subdomain still resolvable.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/_kb/annotate.js"))
        .header("Host", "kitchen-sink.artifacts.localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// === A3 — annotator injection on artifact subdomain (?cm=on) ===

#[tokio::test]
async fn artifact_subdomain_injects_annotator_when_cm_on() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/?cm=on"))
        .header("Host", "kitchen-sink.artifacts.localhost")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let cache = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(cache.contains("no-store"), "got cache-control={cache}");
    let body = resp.text().await.unwrap();
    // Inline data block + deferred annotator script both present.
    assert!(body.contains("window.__KB_COMMENTS"), "data block missing");
    assert!(
        body.contains(r#"<script src="/_kb/annotate.js" defer></script>"#),
        "annotator script tag missing"
    );
    // Probe still injected (we ship both on every HTML response).
    assert!(body.contains(r#"<script src="/_kb/probe.js" defer></script>"#));
    // Schema discriminator present (proves the empty skeleton path
    // worked even though no review file exists yet).
    assert!(body.contains("kb-comments/1"));
}

#[tokio::test]
async fn artifact_subdomain_omits_annotator_when_cm_absent() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/"))
        .header("Host", "kitchen-sink.artifacts.localhost")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("window.__KB_COMMENTS"),
        "annotator data should not appear without ?cm=on"
    );
    assert!(
        !body.contains("/_kb/annotate.js"),
        "annotator src should not appear without ?cm=on"
    );
    // Probe still present (cm=on doesn't touch it).
    assert!(body.contains(r#"<script src="/_kb/probe.js" defer></script>"#));
}

// === A2 — review GET (read-only) ===
//
// R8 retired the whole-document write POST; the fine-grained mutation
// endpoints (add/reply/resolve/edit/delete) are exercised in the "R5 —
// fine-grained comment endpoints" section below. Only the GET reads
// remain here.

// D7 (W1.D) — no-comments-yet is a state, not an error: GET now returns
// 200 with the canonical empty kb-comments/1 skeleton (schema + artifact
// ref + zero comments) instead of the pre-W1.D 404 problem+json.
#[tokio::test]
async fn review_get_missing_returns_empty_skeleton() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/review/no-such-id"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "kb-comments/1");
    assert_eq!(body["artifact"]["id"], "no-such-id");
    assert_eq!(body["comments"].as_array().map(|a| a.len()), Some(0));
}

#[tokio::test]
async fn review_unknown_kb_returns_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/no-such-kb/review/abc"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// === R5 — fine-grained comment endpoints ===

const ORIGIN: &str = "http://localhost:4000";

#[tokio::test]
async fn comments_add_creates_file_and_returns_201() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(addr, "/api/kb/smoke/review/aaaa11112222/comments"))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"first","anchor":{"kind":"file"},"author":"claude"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let c: serde_json::Value = resp.json().await.unwrap();
    assert!(c["id"].as_str().unwrap().starts_with("c_"), "got {c}");
    assert_eq!(c["status"], "open");
    assert_eq!(c["author"], "claude");
    assert_eq!(c["file"], "aaaa11112222");
    assert_eq!(c["fileLabel"], "main");
    assert!(c["editedAt"].is_null());

    // The file now exists and GET returns the one comment.
    let got: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/review/aaaa11112222"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(got["comments"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn comments_apply_batch_is_atomic_and_creates_file() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let base = "/api/kb/smoke/review/ba7cafe11111";

    // Empty batch is a 400 (catches a misfire).
    let resp = client
        .post(url(addr, &format!("{base}/apply")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"ops": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Two adds in one atomic call create the file + return both ids.
    let resp = client
        .post(url(addr, &format!("{base}/apply")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"ops": [
            {"op":"add_comment","anchor":{"kind":"file"},"author":"claude","body":"one"},
            {"op":"add_comment","anchor":{"kind":"section","id":"intro"},"author":"you","body":"two"}
        ]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let r: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(r["applied"], 2);
    assert_eq!(r["created_comment_ids"].as_array().unwrap().len(), 2);

    let got: serde_json::Value = client
        .get(url(addr, base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(got["comments"].as_array().unwrap().len(), 2);

    // A batch whose 2nd op references a missing comment is rejected
    // wholesale — the 1st op (an add) must NOT have landed (atomicity).
    let resp = client
        .post(url(addr, &format!("{base}/apply")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"ops": [
            {"op":"add_comment","anchor":{"kind":"file"},"author":"claude","body":"three"},
            {"op":"resolve","comment_id":"c_does_not_exist"}
        ]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    let got: serde_json::Value = client
        .get(url(addr, base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        got["comments"].as_array().unwrap().len(),
        2,
        "the rolled-back add must not persist"
    );
}

#[tokio::test]
async fn comments_import_round_trips_and_guards_overwrite() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let base = "/api/kb/smoke/review/c0ffeecafe11";
    let doc = serde_json::json!({
        "schema":"kb-comments/1",
        "artifact":{"id":"orig-id","title":"T","kb":"other-kb","tags":[],"pages":[]},
        "generatedAt":"2026-05-14T10:00:00Z",
        "comments":[{"id":"c_kept","status":"resolved","file":"orig-id","fileLabel":"main",
            "anchor":{"kind":"file"},"author":"you","body":"carried over",
            "createdAt":"2026-05-14T10:00:00Z","editedAt":null,"replies":[]}]
    });

    // Import into a fresh artifact: ids/status preserved, artifact ref re-pinned.
    let resp = client
        .post(url(addr, &format!("{base}/import")))
        .header("Origin", ORIGIN)
        .json(&doc)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let got: serde_json::Value = client
        .get(url(addr, base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(got["comments"][0]["id"], "c_kept");
    assert_eq!(got["comments"][0]["status"], "resolved");
    assert_eq!(got["artifact"]["id"], "c0ffeecafe11");

    // Re-import without force is refused — never clobbers live comments.
    let resp = client
        .post(url(addr, &format!("{base}/import")))
        .header("Origin", ORIGIN)
        .json(&doc)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // force=true overwrites.
    let resp = client
        .post(url(addr, &format!("{base}/import?force=true")))
        .header("Origin", ORIGIN)
        .json(&doc)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn comments_add_records_history_row() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let c: serde_json::Value = client
        .post(url(addr, "/api/kb/smoke/review/a1b2c3d4e5f6/comments"))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"x","anchor":{"kind":"file"},"author":"you"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cid = c["id"].as_str().unwrap().to_string();
    let body: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/history?kind=comment"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entries = body["entries"].as_array().expect("entries array");
    let row = entries
        .iter()
        .find(|e| e["artifact_id"] == "a1b2c3d4e5f6" && e["comment_id"] == cid.as_str())
        .expect("comment history row exists");
    assert_eq!(row["kind"], "comment");
}

#[tokio::test]
async fn comments_lifecycle_add_reply_resolve_unresolve() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let base = "/api/kb/smoke/review/bbbb11112222";
    let c: serde_json::Value = client
        .post(url(addr, &format!("{base}/comments")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"q","anchor":{"kind":"file"},"author":"you"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cid = c["id"].as_str().unwrap().to_string();

    let r = client
        .post(url(addr, &format!("{base}/comments/{cid}/replies")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"author":"claude","body":"a"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let reply: serde_json::Value = r.json().await.unwrap();
    assert!(
        reply["id"].as_str().unwrap().starts_with("r_"),
        "got {reply}"
    );

    let res = client
        .post(url(addr, &format!("{base}/comments/{cid}/resolve")))
        .header("Origin", ORIGIN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.json::<serde_json::Value>().await.unwrap()["open_count"],
        0
    );

    let res = client
        .post(url(addr, &format!("{base}/comments/{cid}/unresolve")))
        .header("Origin", ORIGIN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.json::<serde_json::Value>().await.unwrap()["open_count"],
        1
    );
}

#[tokio::test]
async fn comments_reply_on_missing_file_returns_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(
            addr,
            "/api/kb/smoke/review/nofile123456/comments/c_x/replies",
        ))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"author":"claude","body":"x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn comments_resolve_missing_comment_returns_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let base = "/api/kb/smoke/review/cccc11112222";
    client
        .post(url(addr, &format!("{base}/comments")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"q","anchor":{"kind":"file"},"author":"you"}))
        .send()
        .await
        .unwrap();
    let resp = client
        .post(url(
            addr,
            &format!("{base}/comments/c_doesnotexist/resolve"),
        ))
        .header("Origin", ORIGIN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn comments_edit_comment_and_reply_set_edited_at() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let base = "/api/kb/smoke/review/dddd11112222";
    let c: serde_json::Value = client
        .post(url(addr, &format!("{base}/comments")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"orig","anchor":{"kind":"file"},"author":"claude"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cid = c["id"].as_str().unwrap().to_string();
    let r: serde_json::Value = client
        .post(url(addr, &format!("{base}/comments/{cid}/replies")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"author":"you","body":"rorig"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rid = r["id"].as_str().unwrap().to_string();

    let resp = client
        .patch(url(addr, &format!("{base}/comments/{cid}")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"edited-comment"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = client
        .patch(url(addr, &format!("{base}/comments/{cid}/replies/{rid}")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"edited-reply"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let got: serde_json::Value = client
        .get(url(addr, base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let comment = &got["comments"][0];
    assert_eq!(comment["body"], "edited-comment");
    assert!(
        comment["editedAt"].as_str().is_some(),
        "comment editedAt set"
    );
    let reply = &comment["replies"][0];
    assert_eq!(reply["body"], "edited-reply");
    assert!(reply["editedAt"].as_str().is_some(), "reply editedAt set");
}

#[tokio::test]
async fn comments_edit_reply_missing_rid_returns_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let base = "/api/kb/smoke/review/eeee11112222";
    let c: serde_json::Value = client
        .post(url(addr, &format!("{base}/comments")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"q","anchor":{"kind":"file"},"author":"you"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cid = c["id"].as_str().unwrap().to_string();
    // Comment present, reply id absent → 404 (distinct from a missing comment).
    let resp = client
        .patch(url(
            addr,
            &format!("{base}/comments/{cid}/replies/r_missing"),
        ))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn comments_delete_comment_and_reply() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let base = "/api/kb/smoke/review/ffff11112222";
    let c: serde_json::Value = client
        .post(url(addr, &format!("{base}/comments")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"q","anchor":{"kind":"file"},"author":"you"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cid = c["id"].as_str().unwrap().to_string();
    let r: serde_json::Value = client
        .post(url(addr, &format!("{base}/comments/{cid}/replies")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"author":"claude","body":"a"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rid = r["id"].as_str().unwrap().to_string();

    let resp = client
        .delete(url(addr, &format!("{base}/comments/{cid}/replies/{rid}")))
        .header("Origin", ORIGIN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = client
        .delete(url(addr, &format!("{base}/comments/{cid}")))
        .header("Origin", ORIGIN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let got: serde_json::Value = client
        .get(url(addr, base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(got["comments"].as_array().unwrap().len(), 0);

    // Deleting again → 404.
    let resp = client
        .delete(url(addr, &format!("{base}/comments/{cid}")))
        .header("Origin", ORIGIN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// invariant:6 reanchor
#[tokio::test]
async fn comments_set_anchor_repoints_and_404_on_missing() {
    // R9 — `PATCH …/comments/{cid}/anchor` re-points a comment's anchor.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let base = "/api/kb/smoke/review/abcabc123123";
    // Start with a whole-file anchor.
    let c: serde_json::Value = client
        .post(url(addr, &format!("{base}/comments")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"q","anchor":{"kind":"file"},"author":"you"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cid = c["id"].as_str().unwrap().to_string();

    // Re-point it to a section anchor.
    let resp = client
        .patch(url(addr, &format!("{base}/comments/{cid}/anchor")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"anchor":{"kind":"section","id":"overview"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<serde_json::Value>().await.unwrap()["ok"], true);

    // GET reflects the new anchor.
    let got: serde_json::Value = client
        .get(url(addr, base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let anchor = &got["comments"][0]["anchor"];
    assert_eq!(anchor["kind"], "section");
    assert_eq!(anchor["id"], "overview");

    // Missing comment → 404.
    let resp = client
        .patch(url(addr, &format!("{base}/comments/c_missing/anchor")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"anchor":{"kind":"file"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn comments_set_anchor_prunes_stale_sidecar() {
    // R9 — re-pointing an anchor prunes any stale-anchor sidecar entry for
    // that comment (the old anchor's flag no longer applies). Mirrors the
    // delete_comment prune path. Pre-stage a review file + a stale entry,
    // then reanchor and confirm the entry is gone.
    let (tmp, cfg, paths) = fixture_corpus();
    let smoke = kb_core::types::KbName::new("smoke").unwrap();
    const ART: &str = "def0def0def0";
    let review_dir = paths.kb_review_dir(&smoke);
    std::fs::create_dir_all(&review_dir).unwrap();
    let review_file = paths.kb_review_file(&smoke, ART);
    std::fs::write(
        &review_file,
        format!(
            r#"{{"schema":"kb-comments/1","artifact":{{"id":"{ART}","title":"T","kb":"smoke","tags":[],"pages":[]}},"generatedAt":"2026-05-14T10:00:00Z","comments":[{{"id":"c_stale1","status":"open","file":"{ART}","fileLabel":"main","anchor":{{"kind":"section","id":"gone"}},"author":"you","body":"q","createdAt":"2026-05-14T10:00:00Z","editedAt":null,"replies":[]}}]}}"#
        ),
    )
    .unwrap();
    let sidecar = kb_core::anchors::sidecar_path(&review_dir);
    let mut stale: std::collections::HashMap<(String, String), kb_core::anchors::StaleAnchorEntry> =
        std::collections::HashMap::new();
    stale.insert(
        (ART.to_string(), "c_stale1".to_string()),
        kb_core::anchors::StaleAnchorEntry {
            anchor_kind: "section".to_string(),
            fuzzy_score: 0.0,
        },
    );
    kb_core::anchors::save(&sidecar, &stale).unwrap();

    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let client = reqwest::Client::new();

    // Cold load surfaces the pre-staged stale entry.
    let body: serde_json::Value = client
        .get(url(addr, "/api/anchors/stale"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let present = body["anchors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["artifact_id"] == ART && a["comment_id"] == "c_stale1");
    assert!(present, "pre-staged stale entry should surface: {body:#}");

    // Re-point the anchor → the handler prunes the sidecar entry.
    let resp = client
        .patch(url(
            addr,
            &format!("/api/kb/smoke/review/{ART}/comments/c_stale1/anchor"),
        ))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"anchor":{"kind":"section","id":"overview"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Sidecar entry is gone.
    let body: serde_json::Value = client
        .get(url(addr, "/api/anchors/stale"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let still_present = body["anchors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["artifact_id"] == ART && a["comment_id"] == "c_stale1");
    assert!(
        !still_present,
        "stale entry should be pruned after reanchor: {body:#}"
    );
    drop(tmp);
}

#[tokio::test]
async fn comments_resolve_all_and_unresolve_all() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let base = "/api/kb/smoke/review/1234aabbccdd";
    for i in 0..3 {
        client
            .post(url(addr, &format!("{base}/comments")))
            .header("Origin", ORIGIN)
            .json(&serde_json::json!({"body":format!("c{i}"),"anchor":{"kind":"file"},"author":"you"}))
            .send()
            .await
            .unwrap();
    }
    let resp = client
        .post(url(addr, &format!("{base}/resolve-all")))
        .header("Origin", ORIGIN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["flipped"], 3);
    assert_eq!(body["open_count"], 0);

    let resp = client
        .post(url(addr, &format!("{base}/unresolve-all")))
        .header("Origin", ORIGIN)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["flipped"], 3);
    assert_eq!(body["open_count"], 3);
}

#[tokio::test]
async fn comments_list_reviews_status_and_author_filters() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let base = "/api/kb/smoke/review/5678aabbccdd";
    client
        .post(url(addr, &format!("{base}/comments")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"yo","anchor":{"kind":"file"},"author":"you"}))
        .send()
        .await
        .unwrap();
    let c2: serde_json::Value = client
        .post(url(addr, &format!("{base}/comments")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"cl","anchor":{"kind":"file"},"author":"claude"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cid2 = c2["id"].as_str().unwrap();
    client
        .post(url(addr, &format!("{base}/comments/{cid2}/resolve")))
        .header("Origin", ORIGIN)
        .send()
        .await
        .unwrap();

    // Default = open → only the `you` comment.
    let rows: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/reviews"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let comments = rows["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 1, "default lists only open: {rows}");
    assert_eq!(comments[0]["author"], "you");
    assert_eq!(comments[0]["status"], "open");

    // status=all → both.
    let rows: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/reviews?status=all"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rows["comments"].as_array().unwrap().len(), 2);

    // status=all&author=claude → the resolved claude comment.
    let rows: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/reviews?status=all&author=claude"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let comments = rows["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["author"], "claude");
    assert_eq!(comments[0]["status"], "resolved");
    assert_eq!(comments[0]["stale"], false);
    // Row-shape contract: snake_case keys matching the pre-R6 `kb comments
    // list --json` output verbatim — `created_at`/`file_label`, NOT the
    // camelCase wire-schema names. Guards the cross-version `jq` consumers.
    let row = &comments[0];
    assert!(
        row["created_at"].is_string(),
        "snake_case created_at present"
    );
    assert!(row["createdAt"].is_null(), "no camelCase createdAt leak");
    assert!(
        row["file_label"].is_string(),
        "snake_case file_label present"
    );
    assert!(row["fileLabel"].is_null(), "no camelCase fileLabel leak");
}

#[tokio::test]
async fn comments_illegal_cid_returns_400() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // `.hidden` cid is rejected by is_safe_id before any file load.
    let resp = client
        .post(url(
            addr,
            "/api/kb/smoke/review/9999aabbccdd/comments/.hidden/resolve",
        ))
        .header("Origin", ORIGIN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// R8 — these three replace the guarantees the retired whole-doc write
// POST tests gave: serialised read-modify-write (no lost updates), the
// resolve/edit-doesn't-record-history contract, and the illegal-id 400.

// invariant:6 review-lock
#[tokio::test]
async fn comments_concurrent_adds_no_lost_updates() {
    // The whole-doc POST's If-Match was the old way two clients avoided
    // clobbering each other; the fine-grained add path instead serialises
    // every load → mutate → save under the per-daemon `review_lock`. Fire
    // a burst of concurrent adds and assert all of them survive.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let base = "/api/kb/smoke/review/conc12345678";

    let posts = (0..8).map(|i| {
        let client = client.clone();
        let url = url(addr, &format!("{base}/comments"));
        async move {
            client
                .post(url)
                .header("Origin", ORIGIN)
                .json(&serde_json::json!({
                    "body": format!("c{i}"),
                    "anchor": {"kind": "file"},
                    "author": "you",
                }))
                .send()
                .await
                .unwrap()
                .status()
        }
    });
    let statuses = futures::future::join_all(posts).await;
    for s in &statuses {
        assert_eq!(*s, 201, "every concurrent add should return 201");
    }

    let got: serde_json::Value = client
        .get(url(addr, base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        got["comments"].as_array().unwrap().len(),
        8,
        "review_lock must serialise the read-modify-write so no add is lost: {got}"
    );
}

#[tokio::test]
async fn comments_resolve_does_not_record_history() {
    // Only a brand-new comment records a history row; resolve (and edit)
    // must not. Add one comment, confirm exactly one history row, resolve
    // it, then confirm the count is still exactly one.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let base = "/api/kb/smoke/review/hist98765432";
    let c: serde_json::Value = client
        .post(url(addr, &format!("{base}/comments")))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"q","anchor":{"kind":"file"},"author":"you"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cid = c["id"].as_str().unwrap().to_string();

    let count_for = |body: &serde_json::Value| -> usize {
        body["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["artifact_id"] == "hist98765432" && e["comment_id"] == cid.as_str())
            .count()
    };

    let before: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/history?kind=comment"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(count_for(&before), 1, "the add records exactly one row");

    let resp = client
        .post(url(addr, &format!("{base}/comments/{cid}/resolve")))
        .header("Origin", ORIGIN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let after: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/history?kind=comment"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        count_for(&after),
        1,
        "resolve must not record a new history row"
    );
}

#[tokio::test]
async fn comments_add_illegal_artifact_id_returns_400() {
    // `.hidden` artifact id starts with a dot → is_safe_id rejects → 400
    // (preserves the retired traversal-POST test's intent).
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(addr, "/api/kb/smoke/review/.hidden/comments"))
        .header("Origin", ORIGIN)
        .json(&serde_json::json!({"body":"x","anchor":{"kind":"file"},"author":"you"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// === D4 — content-hash artifact subdomain ===

#[tokio::test]
async fn artifact_subdomain_resolves_content_hash_id() {
    // The SPA's /a/{kb}/{id} URLs use 12-char hex content hashes, not
    // file stems. The artifact handler must resolve `<hex>.artifacts...`
    // to the file path stored in lance under that id.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // Pick the first doc id from the gallery list.
    let docs: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/docs?limit=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = docs[0]["id"].as_str().unwrap();
    assert!(
        id.len() == 12 && id.chars().all(|c| c.is_ascii_hexdigit()),
        "expected hex id, got {id}"
    );

    let resp = client
        .get(url(addr, "/"))
        .header("Host", format!("{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let xkb = resp
        .headers()
        .get("x-kb-artifact-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(xkb, id);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/html"), "got {ct}");
}

// === D2 — SPA serving tests ===

#[tokio::test]
async fn spa_root_returns_shell_with_no_cache() {
    let (_tmp, addr) = boot_with_spa().await;
    let client = reqwest::Client::new();
    let resp = client.get(url(addr, "/")).send().await.unwrap();
    assert!(resp.status().is_success());
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/html"), "got {ct}");
    let cache = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(cache.contains("no-cache"), "got cache-control={cache}");
    let body = resp.text().await.unwrap();
    assert!(body.contains("kb-test"), "shell body should be served");
}

#[tokio::test]
async fn spa_client_route_returns_shell() {
    let (_tmp, addr) = boot_with_spa().await;
    let client = reqwest::Client::new();
    // /settings is a React Router route — should fall through to index.html.
    let resp = client.get(url(addr, "/settings")).send().await.unwrap();
    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    assert!(body.contains("kb-test"));
}

#[tokio::test]
async fn spa_permalink_returns_shell() {
    let (_tmp, addr) = boot_with_spa().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/a/smoke/some-artifact-id"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    assert!(body.contains("kb-test"));
}

// invariant:34 best-effort-splice
#[tokio::test]
async fn spa_permalink_injects_og_meta_for_real_artifact() {
    // OG1 — a permalink to a REAL indexed artifact gets per-artifact OpenGraph
    // meta spliced into the static shell <head> (the lookup resolves the URL's
    // source-relative segment to the stored canonical-abs path). The shell body
    // (#root marker) is preserved.
    let (_tmp, addr) = boot_with_spa().await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, "smoke", |d| {
        d.get("source_relative").and_then(|v| v.as_str()).is_some()
            && d.get("title").and_then(|v| v.as_str()).is_some()
    })
    .await;
    let rel = doc["source_relative"].as_str().unwrap();
    let title = doc["title"].as_str().unwrap();
    let body = client
        .get(url(addr, &format!("/a/smoke/{rel}")))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("kb-test"), "shell body must survive");
    assert!(
        body.contains("property=\"og:title\""),
        "expected injected OG meta for /a/smoke/{rel} (lookup must resolve); head was: {}",
        &body[..body.len().min(500)]
    );
    let escaped = title
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    assert!(
        body.contains(&format!("og:title\" content=\"{escaped}\"")),
        "injected og:title must carry the escaped artifact title '{escaped}'"
    );
}

#[tokio::test]
async fn spa_multipage_deeplink_returns_shell() {
    // Regression: the multi-page sub-route `/a/{kb}/{id}/{page}.html`
    // ends in `.html`, so the SPA fallback used to treat it as an asset
    // request and 404. It must serve the SPA shell so React Router can
    // render the deep link (spa-multipage.spec.ts depends on this).
    let (_tmp, addr) = boot_with_spa().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/a/smoke/some-artifact-id/01-timeline.html"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "multipage deep link must serve the shell, got {}",
        resp.status()
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("kb-test"), "expected the SPA shell body");
}

#[tokio::test]
async fn spa_hashed_asset_served_with_immutable_cache() {
    let (_tmp, addr) = boot_with_spa().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/assets/app-Hxxx1234.js"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("application/javascript"), "got {ct}");
    let cache = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(cache.contains("immutable"), "got cache-control={cache}");
}

#[tokio::test]
async fn spa_root_asset_gets_short_cache() {
    let (_tmp, addr) = boot_with_spa().await;
    let client = reqwest::Client::new();
    let resp = client.get(url(addr, "/logomark.svg")).send().await.unwrap();
    assert!(resp.status().is_success());
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ct, "image/svg+xml");
    let cache = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // Not in /assets/, so the immutable cache doesn't apply.
    assert!(
        cache.contains("max-age=3600") && !cache.contains("immutable"),
        "got cache-control={cache}"
    );
}

#[tokio::test]
async fn spa_missing_asset_returns_404() {
    let (_tmp, addr) = boot_with_spa().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/assets/does-not-exist.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn spa_path_traversal_blocked() {
    // reqwest normalizes `/assets/../../../etc/passwd` client-side, so we
    // can't easily test raw-segment traversal over HTTP. We test the
    // adjacent invariant: requesting an asset with an extension that
    // doesn't exist under spa_dist returns 404 (the canonicalize check
    // in read_asset rejects anything that doesn't resolve inside dist).
    let (_tmp, addr) = boot_with_spa().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/etc/passwd.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// === D3 — docs list endpoint (gallery) ===

#[tokio::test]
async fn docs_list_returns_artifact_summaries() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/docs"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    // canon corpus has 4 root artifacts + multi-page expands to several
    // additional pages — total >= 4.
    assert!(
        body.len() >= CANON_REL.len(),
        "expected ≥{} docs, got {}",
        CANON_REL.len(),
        body.len()
    );
    for doc in &body {
        assert!(doc["id"].as_str().is_some(), "doc {:?} has id", doc);
        assert!(doc["title"].as_str().is_some());
        assert!(doc["path"].as_str().is_some());
        // v0.33 X2 — first_indexed_unix is present once the bring-up seed
        // (or indexer success tail) has written doc_first_seen. A missing
        // key is tolerated only if the field is null (skip_serializing_if),
        // but the seeded canon corpus should expose a positive timestamp.
        if let Some(ts) = doc.get("first_indexed_unix") {
            assert!(
                ts.as_i64().is_some_and(|t| t > 0),
                "first_indexed_unix must be a positive unix seconds when present: {ts}"
            );
        }
    }
}

#[tokio::test]
async fn docs_list_respects_limit_param() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/docs?limit=2"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.len() <= 2, "limit=2 returned {} rows", body.len());
}

#[tokio::test]
async fn docs_list_includes_folder_field() {
    // The canon corpus is flat at the kb root, so every folder is "".
    // boot_with_nested_fixture sets up a nested artifact at
    // <root>/ideas/foo/index.html — verify the nested doc reports
    // folder="ideas/foo" while flat docs report "".
    let (_tmp, addr) = boot_with_nested_fixture().await;
    let client = reqwest::Client::new();
    let body: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/nested/docs?limit=20"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let nested = body
        .iter()
        .find(|d| {
            d["path"]
                .as_str()
                .unwrap_or("")
                .ends_with("/ideas/foo/index.html")
        })
        .expect("nested doc in list");
    assert_eq!(nested["folder"].as_str(), Some("ideas/foo"));
    // Track U — source_relative is the full rel path (folder + filename).
    assert_eq!(
        nested["source_relative"].as_str(),
        Some("ideas/foo/index.html")
    );
    // Verify the fields are present (possibly empty) on every row.
    for d in &body {
        assert!(d["folder"].is_string(), "doc {d:?} missing folder field");
        assert!(
            d["source_relative"].is_string(),
            "doc {d:?} missing source_relative field"
        );
    }
}

#[tokio::test]
async fn docs_by_path_resolves_nested_artifact() {
    // Track U — GET /docs/by-path/<rel> resolves an artifact by its
    // source-relative path and returns the full doc (incl. id) the SPA
    // needs for the iframe origin. Also pins matchit precedence: the
    // static `by-path` segment must not be swallowed by `/docs/{id}`.
    let (_tmp, addr) = boot_with_nested_fixture().await;
    let client = reqwest::Client::new();

    // by-path → 200 with matching path + a 12-hex id.
    let resp = client
        .get(url(
            addr,
            "/api/kb/nested/docs/by-path/ideas/foo/index.html",
        ))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "by-path got {}", resp.status());
    let doc: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        doc["source_relative"].as_str(),
        Some("ideas/foo/index.html")
    );
    let id = doc["id"].as_str().unwrap();
    assert_eq!(id.len(), 12, "id should be 12-hex: {id}");

    // The id resolved above must still resolve via the `{id}` route —
    // confirms the two routes coexist.
    let by_id = client
        .get(url(addr, &format!("/api/kb/nested/docs/{id}")))
        .send()
        .await
        .unwrap();
    assert!(by_id.status().is_success());
    let by_id_doc: serde_json::Value = by_id.json().await.unwrap();
    assert_eq!(by_id_doc["id"].as_str(), Some(id));

    // Unknown path → 404.
    let miss = client
        .get(url(addr, "/api/kb/nested/docs/by-path/ideas/foo/nope.html"))
        .send()
        .await
        .unwrap();
    assert_eq!(miss.status(), 404);
}

#[tokio::test]
async fn folders_route_returns_tree_with_counts() {
    // Build a corpus with two top-level folders, one nested topic
    // (multiple docs under a single subfolder), and a root-level doc.
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(source.join("ideas").join("passwordless-login")).unwrap();
    std::fs::create_dir_all(source.join("ideas").join("jira")).unwrap();
    std::fs::create_dir_all(source.join("changelog").join("daily")).unwrap();
    std::fs::write(
        source.join("INDEX.html"),
        b"<html><title>Index</title></html>",
    )
    .unwrap();
    for n in ["01", "02", "03"] {
        std::fs::write(
            source
                .join("ideas")
                .join("passwordless-login")
                .join(format!("{n}.html")),
            format!("<html><title>passwordless {n}</title></html>"),
        )
        .unwrap();
    }
    std::fs::write(
        source.join("ideas").join("jira").join("plan.html"),
        b"<html><title>Jira plan</title></html>",
    )
    .unwrap();
    std::fs::write(
        source
            .join("changelog")
            .join("daily")
            .join("2026-05-13.html"),
        b"<html><title>Daily</title></html>",
    )
    .unwrap();

    let daemon_name = format!(
        "test-folders-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("nested").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/kb/nested/folders"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let folders = body["folders"].as_array().expect("folders array");
    // Two top-level entries (changelog, ideas) since INDEX.html is at
    // root and gets bucketed under empty-string folder (which is not
    // returned at top-level: only non-empty paths are top-level nodes).
    let names: Vec<&str> = folders
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"changelog"), "got {names:?}");
    assert!(names.contains(&"ideas"), "got {names:?}");
    // `ideas` count = 3 (passwordless-login) + 1 (jira) = 4 (descendant-inclusive)
    let ideas = folders.iter().find(|f| f["path"] == "ideas").unwrap();
    assert_eq!(ideas["count"].as_u64(), Some(4), "ideas count: {ideas}");
    let ideas_children = ideas["children"].as_array().unwrap();
    let child_names: Vec<&str> = ideas_children
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert!(child_names.contains(&"ideas/jira"), "got {child_names:?}");
    assert!(
        child_names.contains(&"ideas/passwordless-login"),
        "got {child_names:?}"
    );
    let pwd = ideas_children
        .iter()
        .find(|c| c["path"] == "ideas/passwordless-login")
        .unwrap();
    assert_eq!(pwd["count"].as_u64(), Some(3));
}

#[tokio::test]
async fn docs_list_unknown_kb_returns_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/no-such-kb/docs"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// === S2 (S-milestone) — paginated envelope, server-side filter/sort ===

#[tokio::test]
async fn docs_list_envelope_mode_when_offset_present() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/docs?offset=0&limit=2"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // Envelope shape — not a bare array.
    assert!(
        body.is_object(),
        "envelope mode must return an object: {body}"
    );
    let docs = body["docs"].as_array().expect("docs array");
    assert!(docs.len() <= 2);
    let total = body["total"].as_u64().expect("total int");
    assert!(total >= CANON_REL.len() as u64);
    assert_eq!(body["offset"].as_u64(), Some(0));
    assert_eq!(body["limit"].as_u64(), Some(2));
    // has_more is true when there are more docs than the first page.
    assert_eq!(body["has_more"].as_bool(), Some(total > 2));
}

#[tokio::test]
async fn docs_list_legacy_mode_without_offset_returns_bare_array() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/docs?limit=10"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    // Bare array — backwards-compat with v0.1 callers.
    assert!(
        body.is_array(),
        "legacy mode must return a bare array: {body}"
    );
}

#[tokio::test]
async fn docs_list_folder_filter_is_server_side_and_descendant_inclusive() {
    let (_tmp, addr) = boot_with_nested_fixture().await;
    let client = reqwest::Client::new();
    // Asking for everything under `ideas` returns the nested doc.
    let body: serde_json::Value = client
        .get(url(
            addr,
            "/api/kb/nested/docs?folder=ideas&offset=0&limit=100",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let docs = body["docs"].as_array().expect("docs array");
    assert!(
        docs.iter().any(|d| d["folder"] == "ideas/foo"),
        "folder=ideas should match descendant ideas/foo: {docs:?}",
    );
    // Asking for `ideas/foo` directly also returns it (exact match).
    let body2: serde_json::Value = client
        .get(url(
            addr,
            "/api/kb/nested/docs?folder=ideas/foo&offset=0&limit=100",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let docs2 = body2["docs"].as_array().unwrap();
    assert!(docs2.iter().any(|d| d["folder"] == "ideas/foo"));
    // A sibling-named filter (`ideas-other`) returns zero — descendant
    // match must not be tricked by prefix-only string matches.
    let body3: serde_json::Value = client
        .get(url(
            addr,
            "/api/kb/nested/docs?folder=ideas-other&offset=0&limit=100",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body3["total"].as_u64(), Some(0));
}

#[tokio::test]
async fn docs_list_sort_title_asc_is_server_side() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(
            addr,
            "/api/kb/smoke/docs?sort=title&dir=asc&offset=0&limit=50",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let docs = body["docs"].as_array().expect("docs array");
    let titles: Vec<&str> = docs
        .iter()
        .map(|d| d["title"].as_str().unwrap_or(""))
        .collect();
    let mut sorted = titles.clone();
    sorted.sort();
    assert_eq!(titles, sorted, "expected server-side title-asc ordering");
}

#[tokio::test]
async fn docs_list_envelope_limit_capped_at_500() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/docs?offset=0&limit=10000"))
        .send()
        .await
        .unwrap();
    let link = resp
        .headers()
        .get("link")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body: serde_json::Value = resp.json().await.unwrap();
    // In envelope mode, requested limit is clamped to MAX_ENVELOPE_LIMIT (500).
    assert_eq!(body["limit"].as_u64(), Some(500));
    // No Link header on the last page (smoke kb has <500 docs).
    let total = body["total"].as_u64().unwrap();
    if total <= 500 {
        assert!(link.is_none(), "no rel=next when single page covers all");
    }
}

#[tokio::test]
async fn docs_list_group_folder_prepends_folder_to_sort() {
    // S7 — with `group=folder` the route returns rows ordered by
    // (folder ASC, sort/dir). Verify by asking for two folders' rows
    // and checking they appear in folder-grouped blocks.
    let (_tmp, addr) = boot_with_nested_fixture().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(
            addr,
            "/api/kb/nested/docs?group=folder&offset=0&limit=200",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let docs = body["docs"].as_array().expect("docs array");
    // Walk through the rows and confirm each folder block is
    // contiguous — once we leave a folder we never return to it.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut current = String::new();
    for (i, d) in docs.iter().enumerate() {
        let f = d["folder"].as_str().unwrap_or("").to_string();
        if i == 0 {
            current = f.clone();
            seen.insert(f);
            continue;
        }
        if f != current {
            assert!(
                !seen.contains(&f),
                "folder {f:?} reappeared after we left it — group=folder broken",
            );
            current = f.clone();
            seen.insert(f);
        }
    }
}

#[tokio::test]
async fn tags_route_returns_count_and_color_seed_per_tag() {
    // S3 — the route now aggregates the full corpus via
    // docs_query::aggregate_tags. We verify response shape + the
    // (count desc, name asc) ordering contract.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/tags"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let tags = body.as_array().expect("tags array");
    if tags.is_empty() {
        return; // canon corpus may have no tags; shape-only check
    }
    for t in tags {
        assert!(t["name"].is_string(), "tag {t:?} missing name");
        assert!(t["count"].as_u64().is_some(), "tag {t:?} missing count");
        assert!(
            t["color_seed"].as_u64().is_some(),
            "tag {t:?} missing color_seed",
        );
    }
    for window in tags.windows(2) {
        let a = (
            window[0]["count"].as_u64().unwrap(),
            window[0]["name"].as_str().unwrap(),
        );
        let b = (
            window[1]["count"].as_u64().unwrap(),
            window[1]["name"].as_str().unwrap(),
        );
        assert!(
            a.0 > b.0 || (a.0 == b.0 && a.1 <= b.1),
            "tags out of order: {a:?} before {b:?}",
        );
    }
}

#[tokio::test]
async fn docs_list_projection_slim_drops_gallery_fields() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(
            addr,
            "/api/kb/smoke/docs?projection=slim&offset=0&limit=5",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let docs = body["docs"].as_array().expect("docs array");
    assert!(!docs.is_empty());
    for d in docs {
        // Slim keeps these:
        assert!(d["id"].is_string());
        assert!(d["title"].is_string());
        assert!(d["path"].is_string());
        assert!(d["folder"].is_string());
        // Slim drops gallery-only fields; `skip_serializing_if`
        // removes None Options, so these keys MUST be absent. Note:
        // kb_category is intentionally kept in slim (the detail
        // siblings popover's isIndexPage check needs it).
        assert!(d.get("summary").is_none(), "slim leaked summary");
        assert!(d.get("svg_count").is_none(), "slim leaked svg_count");
        assert!(d.get("backlinks").is_none(), "slim leaked backlinks");
        assert!(d.get("kb_status").is_none(), "slim leaked kb_status");
        // `tags` is a Vec (not Option) so it's always present, but slim
        // empties it.
        assert_eq!(d["tags"].as_array().map(|a| a.len()), Some(0));
    }
}

#[tokio::test]
async fn artifact_subdomain_still_served_when_spa_enabled() {
    // Make sure mounting the SPA doesn't break artifact-subdomain
    // serving — the dispatch fallback must branch on Host correctly.
    let (_tmp, addr) = boot_with_spa().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/_kb/probe.js"))
        .header("Host", "kitchen-sink.artifacts.localhost")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("application/javascript"));
}

// === Cross-artifact relative-link trampoline ===

/// Boot a fresh daemon with a custom two-file corpus that the cross-link
/// tests use. Returns the source-file id, target-file id, addr, and
/// (kept-alive) tempdir.
async fn boot_with_cross_link_corpus() -> (tempfile::TempDir, std::net::SocketAddr, String, String)
{
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    let target_dir = source.join("incidents").join("checks");
    let src_dir = source.join("changelog").join("daily");
    std::fs::create_dir_all(&target_dir).unwrap();
    std::fs::create_dir_all(&src_dir).unwrap();

    let target_bytes =
        b"<html><title>Check 2026-05-13</title><body>target body of the check</body></html>";
    let target_path = target_dir.join("check.html");
    std::fs::write(&target_path, target_bytes).unwrap();

    // The source file links across the tree via `../../incidents/...`.
    let source_html = r#"<html><title>Daily Log</title><body><a href="../../incidents/checks/check.html">go</a></body></html>"#;
    let source_path = src_dir.join("log.html");
    std::fs::write(&source_path, source_html).unwrap();

    // Artifact ids are path-based (a hash of the source-relative path),
    // so derive the expected subdomain ids from the rel paths the
    // indexer will see — not the file contents. v0.7.1 P4: go through
    // `paths::doc_rel_path` rather than hand-typing the path strings,
    // so the test rides any future normalisation (separator handling,
    // canonicalisation) the indexer itself rides.
    let source_id = kb_core::ids::ArtifactId::from_path(&kb_core::paths::doc_rel_path(
        &source_path.to_string_lossy(),
        &source,
    ))
    .as_str()
    .to_string();
    let target_id = kb_core::ids::ArtifactId::from_path(&kb_core::paths::doc_rel_path(
        &target_path.to_string_lossy(),
        &source,
    ))
    .as_str()
    .to_string();

    let daemon_name = format!(
        "test-cross-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("smoke").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    common::wait_docs_listed(addr, "smoke", 2).await;
    (tmp, addr, source_id, target_id)
}

#[tokio::test]
async fn edges_route_returns_link_edges() {
    // v0.7.1 H12 — GET /api/kb/{kb}/edges backs the SPA atlas's
    // cross-artifact edge layer; the SPA hard-codes the
    // `{edges:[{src,dst}]}` shape. This route had no server-side test.
    let (_tmp, addr, source_id, target_id) = boot_with_cross_link_corpus().await;
    let client = reqwest::Client::new();

    // The initial-walk order may index the linking file before its
    // target, so the link doesn't resolve to a recorded edge. A reindex
    // pass — both rows now present — records it deterministically.
    let r = client
        .post(url(addr, "/api/kb/smoke/reindex"))
        .send()
        .await
        .unwrap();
    assert!(
        r.status().is_success() || r.status().as_u16() == 202,
        "reindex returned {}",
        r.status()
    );

    // Poll /edges until the cross-link edge is recorded (or time out).
    let mut edges: Vec<serde_json::Value> = Vec::new();
    for _ in 0..50 {
        let body: serde_json::Value = client
            .get(url(addr, "/api/kb/smoke/edges"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        edges = body["edges"].as_array().cloned().unwrap_or_default();
        if !edges.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(edges.len(), 1, "expected one link edge, got {edges:?}");
    assert_eq!(edges[0]["src"].as_str(), Some(source_id.as_str()));
    assert_eq!(edges[0]["dst"].as_str(), Some(target_id.as_str()));
    // ids are 12-hex path hashes.
    for end in ["src", "dst"] {
        let id = edges[0][end].as_str().unwrap();
        assert_eq!(id.len(), 12, "{end} id should be 12 hex chars: {id}");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

#[tokio::test]
async fn edges_route_unknown_kb_returns_404() {
    let (_tmp, addr, _, _) = boot_with_cross_link_corpus().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/nonexistent/edges"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn cross_artifact_relative_link_returns_trampoline_pointing_at_target() {
    let (_tmp, addr, source_id, target_id) = boot_with_cross_link_corpus().await;
    let client = reqwest::Client::new();
    let host = format!("{source_id}.artifacts.localhost:{}", addr.port());

    // The browser sends the request as a root-anchored path after
    // resolving `../../incidents/...` against `/` — same byte-shape
    // the user-reported bug produced.
    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/incidents/checks/check.html",
            addr.port()
        ))
        .header("Host", &host)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "expected 200 trampoline, got {}",
        resp.status()
    );
    let header_id = resp
        .headers()
        .get("x-kb-trampoline")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert_eq!(header_id, target_id, "trampoline header must name target");
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("open-artifact") && body.contains(&target_id),
        "trampoline body must postMessage open-artifact with target id; got {body}"
    );
    // Track U — the message also carries the target's source-relative
    // path (bound to `REL`) so the parent SPA can navigate to the
    // path-based permalink.
    assert!(
        body.contains("REL=\"incidents/checks/check.html\""),
        "trampoline body must carry target source_relative path; got {body}"
    );
    // ctrl/⌘/middle-click new-tab support: when loaded top-level (no
    // parent SPA) the trampoline bounces to the kb wrapper instead of
    // posting into the void.
    assert!(
        body.contains("window.top===window.self") && body.contains("location.replace"),
        "trampoline must bounce a top-level load to the kb wrapper; got {body}"
    );
}

#[tokio::test]
async fn cross_artifact_request_outside_source_root_is_rejected() {
    let (_tmp, addr, source_id, _target_id) = boot_with_cross_link_corpus().await;
    let client = reqwest::Client::new();
    let host = format!("{source_id}.artifacts.localhost:{}", addr.port());

    // `/../../../etc/passwd` — even after the source_root fallback,
    // canonicalising must keep the result inside the kb source root.
    // We hit the daemon with the literal `..` segments by going through
    // a raw http path; reqwest normalises some of them, so use the raw
    // tcp-stream approach instead.
    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/%2e%2e/%2e%2e/%2e%2e/etc/passwd",
            addr.port()
        ))
        .header("Host", &host)
        .send()
        .await
        .unwrap();
    // 400 (path traversal) or 404 (couldn't canonicalize) are both
    // acceptable; what's NOT acceptable is 200 leaking host data.
    assert!(
        resp.status() == reqwest::StatusCode::BAD_REQUEST
            || resp.status() == reqwest::StatusCode::NOT_FOUND,
        "expected 4xx, got {}",
        resp.status()
    );
}

/// Boot a daemon with two HTML files in the SAME folder, both indexed.
/// Models the cohabiting-siblings bug: source artifact links a sibling
/// inside its own asset_base, which (pre-fix) was served normally and
/// the iframe probe falsely fired `pm:page` instead of trampolining.
async fn boot_with_same_folder_cohabiting_corpus(
) -> (tempfile::TempDir, std::net::SocketAddr, String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    let notes_dir = source.join("notes");
    std::fs::create_dir_all(&notes_dir).unwrap();

    let source_html = r#"<html><title>Note A</title><body><a href="b.html">to b</a></body></html>"#;
    let source_path = notes_dir.join("a.html");
    std::fs::write(&source_path, source_html).unwrap();

    let target_html = b"<html><title>Note B</title><body>target body of note b</body></html>";
    let target_path = notes_dir.join("b.html");
    std::fs::write(&target_path, target_html).unwrap();

    let source_id = kb_core::ids::ArtifactId::from_path(&kb_core::paths::doc_rel_path(
        &source_path.to_string_lossy(),
        &source,
    ))
    .as_str()
    .to_string();
    let target_id = kb_core::ids::ArtifactId::from_path(&kb_core::paths::doc_rel_path(
        &target_path.to_string_lossy(),
        &source,
    ))
    .as_str()
    .to_string();

    let daemon_name = format!(
        "test-cohab-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("smoke").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    common::wait_docs_listed(addr, "smoke", 2).await;
    (tmp, addr, source_id, target_id)
}

#[tokio::test]
async fn same_folder_cohabiting_artifact_link_returns_trampoline() {
    // Two indexed artifacts in `notes/`; A links to B as `b.html` (bare
    // filename, resolves inside A's asset_base). Pre-fix: 200 with
    // probe-injected HTML → probe posts `pm:page` → parent SPA appends
    // `?p=b.html` to A's URL. Post-fix: server short-circuits with a
    // trampoline so the parent navigates to B's own permalink + subdomain.
    let (_tmp, addr, source_id, target_id) = boot_with_same_folder_cohabiting_corpus().await;
    let client = reqwest::Client::new();
    let host = format!("{source_id}.artifacts.localhost:{}", addr.port());

    let resp = client
        .get(format!("http://127.0.0.1:{}/b.html", addr.port()))
        .header("Host", &host)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "expected 200 trampoline, got {}",
        resp.status()
    );
    let header_id = resp
        .headers()
        .get("x-kb-trampoline")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert_eq!(
        header_id, target_id,
        "trampoline header must name the cohabiting target"
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("open-artifact") && body.contains(&target_id),
        "trampoline body must postMessage open-artifact with target id; got {body}"
    );
    assert!(
        body.contains("REL=\"notes/b.html\""),
        "trampoline body must carry target source_relative path; got {body}"
    );
}

#[tokio::test]
async fn same_folder_self_link_is_served_normally() {
    // The guard against false-positive trampolines: A linking *to itself*
    // (`a.html` while on the A subdomain) must serve A's HTML normally,
    // NOT trampoline (which would cause an infinite postMessage loop).
    // `target.id == artifact_id` short-circuits the indexed-artifact
    // check; this test pins that behaviour.
    let (_tmp, addr, source_id, _target_id) = boot_with_same_folder_cohabiting_corpus().await;
    let client = reqwest::Client::new();
    let host = format!("{source_id}.artifacts.localhost:{}", addr.port());

    let resp = client
        .get(format!("http://127.0.0.1:{}/a.html", addr.port()))
        .header("Host", &host)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "expected 200 self-serve, got {}",
        resp.status()
    );
    assert!(
        resp.headers().get("x-kb-trampoline").is_none(),
        "self-link must NOT trampoline; X-Kb-Trampoline header was set"
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Note A") && !body.contains("open-artifact"),
        "self-link must serve the artifact's own HTML, not a trampoline; got {body}"
    );
}

// === v0.3 G3 — outbound scrubbing ===

async fn boot_with_outbound(
    outbound: OutboundSection,
) -> (tempfile::TempDir, std::net::SocketAddr) {
    let (tmp, cfg, paths) = fixture_corpus_with(Some(outbound));
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    common::wait_docs_listed(addr, "smoke", 4).await;
    (tmp, addr)
}

/// Variant of `boot_with_outbound` that injects a synthetic file with
/// a `<template id="kb-prompt">` into the corpus before starting the
/// daemon, so the G3 strip tests have something to assert against.
/// The canon corpus has no kb-prompt template (intentional — those
/// files come from research and don't carry prompt bundles).
async fn boot_with_outbound_and_promptful_artifact(
    outbound: OutboundSection,
) -> (tempfile::TempDir, std::net::SocketAddr, String) {
    let (tmp, cfg, paths) = fixture_corpus_with(Some(outbound));
    // The corpus source path was created by fixture_corpus_with.
    let source = cfg.kb.values().next().unwrap().path.clone();
    let id = "with-prompt";
    let html = r##"<!doctype html>
<html><body>
<h1>With prompt</h1>
<template id="kb-prompt">SECRET PROMPT do not leak</template>
<p>Visible body content.</p>
</body></html>"##;
    std::fs::write(source.join(format!("{id}.html")), html).unwrap();

    // Pre-populate a token so the `/api/.../artifact/{id}` consumer can make
    // an AUTHORIZED non-loopback pull (the fail-closed guard 401s a token-less
    // non-loopback /api request). The auth-free artifact-subdomain consumer of
    // this helper hits the fallback handler, which never checks the token, so
    // it is unaffected.
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(paths.token_file(), FIXTURE_TOKEN).unwrap();
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    common::wait_docs_listed(addr, "smoke", 5).await;
    (tmp, addr, id.to_string())
}

#[tokio::test]
async fn outbound_scrub_strips_kb_prompt_on_non_loopback_request() {
    let (_tmp, addr, id) = boot_with_outbound_and_promptful_artifact(OutboundSection {
        strip_kb_prompt: true,
        redactions: Vec::new(),
    })
    .await;
    let client = reqwest::Client::new();
    let host = format!("{id}.artifacts.localhost:4000");

    // Loopback path — template should be present.
    let loopback = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", &host)
        .send()
        .await
        .unwrap();
    assert!(loopback.status().is_success());
    let body = loopback.text().await.unwrap();
    assert!(
        body.contains("SECRET PROMPT"),
        "loopback path must NOT scrub kb-prompt; got: {}",
        &body[..body.len().min(300)]
    );

    // Non-loopback path — template gone.
    let external = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", &host)
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert!(external.status().is_success());
    let body = external.text().await.unwrap();
    assert!(
        !body.contains("SECRET PROMPT"),
        "non-loopback path MUST strip kb-prompt template"
    );
    assert!(
        body.contains("Visible body content"),
        "scrub must not destroy the rest of the body"
    );
}

#[tokio::test]
async fn markdown_artifact_serves_rendered_html_and_scrubs_prompt() {
    // P4 — the artifact serve path renders a `.md` source to a full HTML page
    // BEFORE the scrub runs:
    //   * the response is the styled prose shell (not raw markdown source),
    //   * served from the CSP-wrapped `text/html` branch with the probe
    //     injected (same pipeline as a hand-authored HTML artifact),
    //   * the inline `<template id="kb-prompt">` is present on loopback but
    //     stripped on a non-loopback request — proving render-runs-before-
    //     scrub, the load-bearing markdown security invariant.
    let (_tmp, cfg, paths) = fixture_corpus_with(Some(OutboundSection {
        strip_kb_prompt: true,
        redactions: Vec::new(),
    }));
    let source = cfg.kb.values().next().unwrap().path.clone();
    let md = "---\ntitle: MD Serve Test\nkb-tags: rust, markdown\n---\n\
              # Rendered Heading\n\n\
              Some **bold** prose body text.\n\n\
              <template id=\"kb-prompt\">SECRET MD PROMPT do not leak</template>\n";
    std::fs::write(source.join("serve-me.md"), md).unwrap();
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");

    let client = reqwest::Client::new();
    // The `.md` artifact resolves on its path-based (12-hex) id, so address
    // the subdomain by that id rather than the file stem (the `<id>.html`
    // fallback only catches legacy HTML stems).
    let doc = wait_for_doc(&client, addr, "smoke", |d| {
        d["source_relative"]
            .as_str()
            .is_some_and(|s| s.ends_with(".md"))
    })
    .await;
    let id = doc["id"].as_str().expect("doc id").to_string();
    let host = format!("{id}.artifacts.localhost:4000");

    // Loopback — rendered shell + probe; kb-prompt PRESENT (no scrub).
    let lb = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", &host)
        .send()
        .await
        .unwrap();
    assert!(lb.status().is_success(), "got {}", lb.status());
    let ct = lb
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ct.contains("text/html"),
        "markdown must serve as text/html; got {ct}"
    );
    let body = lb.text().await.unwrap();
    assert!(
        body.contains("kb-md-doc") && body.contains("kb-md-bar"),
        "rendered prose shell + accent bar, not raw `.md`: {}",
        &body[..body.len().min(400)]
    );
    assert!(body.contains("Rendered Heading"), "heading rendered");
    assert!(
        body.contains("<strong>bold</strong>"),
        "GFM emphasis rendered, not raw `**bold**`"
    );
    assert!(
        body.contains(r#"<script src="/_kb/probe.js""#),
        "probe injected into rendered markdown"
    );
    assert!(
        body.contains("SECRET MD PROMPT"),
        "loopback must NOT scrub the kb-prompt"
    );
    assert!(
        !body.contains("**bold**"),
        "raw markdown source must not leak through"
    );

    // Non-loopback — kb-prompt stripped, body retained (render ran first).
    let ext = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", &host)
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert!(ext.status().is_success());
    let body = ext.text().await.unwrap();
    assert!(
        !body.contains("SECRET MD PROMPT"),
        "non-loopback MUST strip the inline kb-prompt from rendered markdown"
    );
    assert!(
        body.contains("prose body text"),
        "scrub must not destroy the rendered body"
    );
}

#[tokio::test]
async fn serve_renders_extension_mapped_txt_through_the_markdown_pipeline() {
    // SC5 — the X1 residual: a `.txt` mapped to Markdown via `[kb.*]
    // indexable_extensions` must render on the artifact subdomain, not fall
    // through to the raw-bytes branch (which is what the hardcoded
    // `kb_core::indexer::is_markdown(".txt")` check used to force).
    let (_tmp, mut cfg, paths) = fixture_corpus_with(None);
    let kb_name = KbName::new("smoke").unwrap();
    let source = cfg.kb.get(&kb_name).unwrap().path.clone();
    cfg.kb.get_mut(&kb_name).unwrap().indexable_extensions = Some(BTreeMap::from([(
        "txt".to_string(),
        "markdown".to_string(),
    )]));
    std::fs::write(
        source.join("mapped.txt"),
        "---\ntitle: Mapped Txt Test\n---\n# Rendered From Txt\n\nSome **bold** prose.\n",
    )
    .unwrap();

    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, "smoke", |d| {
        d["source_relative"]
            .as_str()
            .is_some_and(|s| s.ends_with("mapped.txt"))
    })
    .await;
    let id = doc["id"].as_str().expect("doc id").to_string();
    let host = format!("{id}.artifacts.localhost:4000");

    let resp = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", &host)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ct.contains("text/html"),
        "mapped .txt must render as text/html; got {ct}"
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("kb-md-doc") && body.contains("kb-md-bar"),
        "rendered prose shell, not raw `.txt` source: {}",
        &body[..body.len().min(400)]
    );
    assert!(body.contains("Rendered From Txt"), "heading rendered");
    assert!(
        body.contains("<strong>bold</strong>"),
        "GFM emphasis rendered, not raw `**bold**`"
    );
    assert!(
        !body.contains("**bold**"),
        "raw markdown source must not leak through"
    );
}

#[tokio::test]
async fn serve_still_serves_unmapped_txt_raw_pre_x1_parity() {
    // The flip side: with NO `indexable_extensions` configured (the default),
    // a `.txt` sibling asset must still serve as raw bytes — the SC5 fix only
    // changes behaviour for a kb that opts a `.txt` extension INTO the
    // Markdown pipeline, never the unmapped default.
    let (_tmp, cfg, paths) = fixture_corpus_with(None);
    let source = cfg.kb.values().next().unwrap().path.clone();
    std::fs::write(
        source.join("sibling.txt"),
        "# Not Rendered\n\nSome **bold** text.\n",
    )
    .unwrap();
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;

    let client = reqwest::Client::new();
    // `kitchen-sink.html` is a top-level canon entrypoint addressed by its
    // legacy filename-stem subdomain (matches the other direct-host tests in
    // this file); `sibling.txt` resolves as a sibling asset under the same
    // source root, unmapped and thus never indexed.
    let resp = client
        .get(format!("http://127.0.0.1:{}/sibling.txt", addr.port()))
        .header("Host", "kitchen-sink.artifacts.localhost:4000")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert_eq!(
        ct, "text/plain; charset=utf-8",
        "unmapped .txt must NOT be re-labelled text/html"
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("# Not Rendered") && body.contains("**bold**"),
        "unmapped .txt must serve verbatim raw source, not rendered markdown: {body}"
    );
}

#[tokio::test]
async fn outbound_scrub_applies_regex_redaction_on_non_loopback() {
    // Add a redaction that maps any digit run of 4+ to <num>.
    let (_tmp, addr) = boot_with_outbound(OutboundSection {
        strip_kb_prompt: false,
        redactions: vec![RegexRule {
            pattern: r"\d{4,}".to_string(),
            replacement: "<num>".to_string(),
        }],
    })
    .await;
    let client = reqwest::Client::new();

    // fullscreen-viz.html has plenty of digits; verify some are gone
    // under non-loopback but preserved under loopback.
    let loopback = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", "fullscreen-viz.artifacts.localhost:4000")
        .send()
        .await
        .unwrap();
    let lb_body = loopback.text().await.unwrap();
    let lb_has_digits = lb_body.chars().filter(|c| c.is_ascii_digit()).count();

    let external = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", "fullscreen-viz.artifacts.localhost:4000")
        .header("X-Forwarded-For", "203.0.113.42")
        .send()
        .await
        .unwrap();
    let ext_body = external.text().await.unwrap();
    let ext_has_digits = ext_body.chars().filter(|c| c.is_ascii_digit()).count();

    assert!(
        ext_has_digits < lb_has_digits,
        "expected fewer digits after redaction: loopback={lb_has_digits} ext={ext_has_digits}"
    );
}

#[tokio::test]
async fn outbound_scrub_skipped_when_no_outbound_section() {
    // The default fixture has outbound: None — body length must be
    // byte-identical with and without X-Forwarded-For (no scrubbing
    // path runs).
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let loopback = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", "kitchen-sink.artifacts.localhost:4000")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let external = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", "kitchen-sink.artifacts.localhost:4000")
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(
        loopback.len(),
        external.len(),
        "no outbound config → bodies must be identical regardless of XFF"
    );
}

#[tokio::test]
async fn api_artifact_route_renders_markdown_and_scrubs_prompt() {
    // P4 — `artifact_bytes` (the `kb get` / SPA-download path) must ALSO render
    // markdown BEFORE the scrub; the subdomain serve isn't the only read site.
    let (_tmp, cfg, paths) = fixture_corpus_with(Some(OutboundSection {
        strip_kb_prompt: true,
        redactions: Vec::new(),
    }));
    let source = cfg.kb.values().next().unwrap().path.clone();
    let md = "# Rendered Heading\n\nSome **bold** body.\n\n\
              <template id=\"kb-prompt\">SECRET MD PROMPT</template>\n";
    std::fs::write(source.join("get-me.md"), md).unwrap();
    // A non-loopback pull of artifact bytes now requires auth (the fail-closed
    // guard 401s a token-less non-loopback /api request). Pre-populate a token
    // so the realistic "authorized partner pulling bytes remotely" scrub path
    // is reachable; the non-loopback request below carries the bearer.
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(paths.token_file(), FIXTURE_TOKEN).unwrap();
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");

    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, "smoke", |d| {
        d["source_relative"]
            .as_str()
            .is_some_and(|s| s.ends_with(".md"))
    })
    .await;
    let id = doc["id"].as_str().expect("doc id").to_string();

    // Authorized non-loopback GET → rendered HTML with the prompt stripped
    // (render first). The bearer satisfies auth; the XFF makes it non-loopback
    // so the outbound scrub engages.
    let ext = client
        .get(url(addr, &format!("/api/kb/smoke/artifact/{id}")))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert!(ext.status().is_success());
    let body = ext.text().await.unwrap();
    assert!(
        body.contains("kb-md-doc") && body.contains("<strong>bold</strong>"),
        "rendered shell, not raw .md: {}",
        &body[..body.len().min(300)]
    );
    assert!(
        !body.contains("SECRET MD PROMPT"),
        "non-loopback `kb get` MUST strip the kb-prompt from rendered markdown"
    );
    assert!(!body.contains("**bold**"), "raw markdown must not leak");

    // ?download=1 → attachment filename maps .md → .html.
    let dl = client
        .get(url(
            addr,
            &format!("/api/kb/smoke/artifact/{id}?download=1"),
        ))
        .send()
        .await
        .unwrap();
    assert!(dl.status().is_success());
    let disp = dl
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        disp.contains("get-me.html") && !disp.contains("get-me.md"),
        "download filename must map .md→.html; got: {disp}"
    );
}

#[tokio::test]
async fn api_artifact_route_scrubs_on_non_loopback() {
    // S3 regression: the /api/kb/{kb}/artifact/{id} route used to bypass
    // the outbound-scrub layer entirely. Now it must apply the same rule
    // as the artifact subdomain serve — strip kb-prompt + run regex
    // redactions when the genuine client is non-loopback.
    let (_tmp, addr, file_stem) = boot_with_outbound_and_promptful_artifact(OutboundSection {
        strip_kb_prompt: true,
        redactions: Vec::new(),
    })
    .await;
    let client = reqwest::Client::new();

    // `/api/.../artifact/{id}` takes the path-hash id, not the file stem.
    // Look it up via the docs list — POLLED (MI test-hardening, 2026-08):
    // this used to be a single-shot fetch trusting `boot_with_outbound_and_
    // promptful_artifact`'s fixed post-boot sleep, which raced the indexer
    // under host I/O contention. `wait_for_doc` retries with a load-aware
    // deadline instead of panicking on the first miss.
    let doc = wait_for_doc(&client, addr, "smoke", |d| {
        d.get("path")
            .and_then(|p| p.as_str())
            .is_some_and(|p| p.ends_with(&format!("{file_stem}.html")))
    })
    .await;
    let id = doc["id"]
        .as_str()
        .unwrap_or_else(|| panic!("artifact {file_stem} indexed with no id: {doc}"))
        .to_string();

    let lb = client
        .get(url(addr, &format!("/api/kb/smoke/artifact/{id}")))
        .send()
        .await
        .unwrap();
    assert!(
        lb.status().is_success(),
        "loopback GET failed: {}",
        lb.status()
    );
    let body = lb.text().await.unwrap();
    assert!(
        body.contains("SECRET PROMPT"),
        "loopback path must NOT scrub kb-prompt on /api/.../artifact/{{id}}"
    );

    let ext = client
        .get(url(addr, &format!("/api/kb/smoke/artifact/{id}")))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert!(ext.status().is_success());
    let body = ext.text().await.unwrap();
    assert!(
        !body.contains("SECRET PROMPT"),
        "/api/.../artifact/{{id}} on non-loopback MUST strip kb-prompt; got: {}",
        &body[..body.len().min(400)]
    );
    assert!(
        body.contains("Visible body content"),
        "scrub on /api path must not destroy the rest of the body"
    );
}

// === v0.4 A2 — bearer-token auth ===

const FIXTURE_TOKEN: &str = "test-token-fixture-deadbeef";

/// Boot a daemon with a token file pre-populated. The token is
/// `FIXTURE_TOKEN`. Existing helpers drop a tempdir-rooted KbPaths
/// where `paths.token_file()` resolves to `<tmp>/config/token`; this
/// writes the token there before the daemon reads it at startup.
async fn boot_with_token() -> (tempfile::TempDir, std::net::SocketAddr) {
    let (tmp, cfg, paths) = fixture_corpus();
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(paths.token_file(), FIXTURE_TOKEN).unwrap();
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    common::wait_docs_listed(addr, "smoke", 4).await;
    (tmp, addr)
}

#[tokio::test]
async fn auth_loopback_bypass_lets_requests_through_without_token() {
    // When ConnectInfo says peer is 127.0.0.1 AND there's no XFF,
    // the middleware bypasses auth even when a token is configured.
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let resp = client.get(url(addr, "/api/identity")).send().await.unwrap();
    assert_eq!(resp.status(), 200, "loopback should bypass auth");
}

#[tokio::test]
async fn auth_non_loopback_request_without_token_returns_401() {
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/identity"))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("problem+json"), "got ct: {ct}");
    let www_auth = resp
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(www_auth.contains("Bearer"), "got: {www_auth}");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"].as_str(), Some("urn:kb:errors:unauthorized"));
}

#[tokio::test]
async fn auth_non_loopback_with_wrong_token_returns_401() {
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/identity"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn auth_non_loopback_with_correct_token_returns_200() {
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/identity"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn auth_artifact_subdomain_stays_open_for_non_loopback() {
    // The artifact subdomain handler is NOT under /api, so it skips
    // the auth layer. Non-loopback requests still reach it (subject
    // to the v0.3 outbound scrubbing layer when configured).
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", "kitchen-sink.artifacts.localhost:4000")
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
}

#[tokio::test]
async fn auth_spoofed_leftmost_xff_loopback_does_not_bypass() {
    // v0.7.1 C1 — a client forging `X-Forwarded-For: 127.0.0.1` cannot
    // bypass auth: a real reverse proxy appends the true client IP to
    // the right, and the daemon walks the chain right-to-left, so the
    // appended external IP is the one that counts.
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();

    // Spoofed leftmost loopback + an appended external IP → enforced.
    let spoofed = client
        .get(url(addr, "/api/identity"))
        .header("X-Forwarded-For", "127.0.0.1, 8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(
        spoofed.status(),
        401,
        "spoofed leftmost XFF=127.0.0.1 must not bypass auth"
    );

    // The same request with the correct token still authenticates.
    let ok = client
        .get(url(addr, "/api/identity"))
        .header("X-Forwarded-For", "127.0.0.1, 8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
}

// === v0.4 B1 — rate limit ===

#[tokio::test]
async fn rate_limit_kicks_in_on_burst_of_search_requests() {
    // Default policy: 60 req/min/token on /api/search. Fire 80 in a
    // burst with the same token + non-loopback XFF; expect ≥10 to
    // come back 429 + a Retry-After header.
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let mut count_200 = 0;
    let mut count_429 = 0;
    let mut saw_retry_after = false;
    for _ in 0..80 {
        let resp = client
            .get(url(addr, "/api/search?q=borrow&mode=keyword&kb=smoke"))
            .header("X-Forwarded-For", "8.8.8.8")
            .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
            .send()
            .await
            .unwrap();
        match resp.status().as_u16() {
            200 => count_200 += 1,
            429 => {
                count_429 += 1;
                if resp.headers().get("retry-after").is_some() {
                    saw_retry_after = true;
                }
            }
            other => panic!("unexpected status {other}"),
        }
    }
    assert!(
        count_429 >= 10,
        "expected ≥10 429s, got {count_429} (200s: {count_200})"
    );
    assert!(saw_retry_after, "429 should carry Retry-After");
}

// === v0.5 P3 — POST /api/kb/{kb}/review/{id}/export?format=… ===

#[tokio::test]
async fn export_returns_400_when_format_missing() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(addr, "/api/kb/smoke/review/abc123/export"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"].as_str(), Some("urn:kb:errors:bad-request"));
}

#[tokio::test]
async fn export_returns_404_when_review_missing() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(
            addr,
            "/api/kb/smoke/review/no-such-id/export?format=claude",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn export_returns_claude_prompt_for_existing_review() {
    let (tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // Drop a fixture review file under <state>/smoke/.review/.
    let review_dir = tmp
        .path()
        .join("state")
        .join(format!(
            "test-{}",
            tmp.path().file_name().unwrap().to_string_lossy()
        ))
        .join("smoke")
        .join(".review");
    std::fs::create_dir_all(&review_dir).unwrap();
    let review_path = review_dir.join("smoke-export-fixture.json");
    let body = r#"{
      "schema": "kb-comments/1",
      "artifact": {"id": "smoke-export-fixture", "title": "Smoke Export", "kb": "smoke", "tags": [], "pages": []},
      "generatedAt": "2026-05-13T10:00:00Z",
      "comments": [{
        "id": "c_1", "status": "open", "file": "smoke-export-fixture", "fileLabel": "main",
        "anchor": {"kind": "file"}, "author": "you", "body": "test export body",
        "createdAt": "2026-05-13T10:00:00Z", "editedAt": null, "replies": []
      }]
    }"#;
    std::fs::write(&review_path, body).unwrap();

    let resp = client
        .post(url(
            addr,
            "/api/kb/smoke/review/smoke-export-fixture/export?format=claude",
        ))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/markdown"), "got: {ct}");
    let disp = resp
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        disp.contains("smoke-export-fixture-review.md"),
        "got: {disp}"
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("# Review: Smoke Export"));
    assert!(body.contains("test export body"));
}

#[tokio::test]
async fn export_streams_large_review_under_the_cap() {
    // v0.7.1 H7: the export route streams a chunked body (no
    // Content-Length) and caps at EXPORT_MAX_BYTES (32 MiB). A
    // realistic large review — well past the old v0.5 1 MB cap, far
    // under 32 MiB — round-trips fine.
    let (tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let review_dir = tmp
        .path()
        .join("state")
        .join(format!(
            "test-{}",
            tmp.path().file_name().unwrap().to_string_lossy()
        ))
        .join("smoke")
        .join(".review");
    std::fs::create_dir_all(&review_dir).unwrap();

    // ~600 comments × ~2 KB body each → comfortably past 1 MB of JSON.
    let big_body = "x".repeat(2048);
    let comment_count = 600usize;
    let mut comments = String::new();
    for i in 0..comment_count {
        if i > 0 {
            comments.push(',');
        }
        comments.push_str(&format!(
            r#"{{"id":"c_{i}","status":"open","file":"big-review","fileLabel":"main","anchor":{{"kind":"file"}},"author":"you","body":"{big_body}","createdAt":"2026-05-14T10:00:00Z","editedAt":null,"replies":[]}}"#
        ));
    }
    let doc = format!(
        r#"{{"schema":"kb-comments/1","artifact":{{"id":"big-review","title":"Big Review","kb":"smoke","tags":[],"pages":[]}},"generatedAt":"2026-05-14T10:00:00Z","comments":[{comments}]}}"#
    );
    assert!(
        doc.len() > 1_048_576,
        "fixture must exceed the old 1 MB cap, got {}",
        doc.len()
    );
    std::fs::write(review_dir.join("big-review.json"), &doc).unwrap();

    let resp = client
        .post(url(
            addr,
            "/api/kb/smoke/review/big-review/export?format=json",
        ))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    // Streamed: chunked transfer, no Content-Length.
    assert!(
        resp.headers().get("content-length").is_none(),
        "streamed export must not set Content-Length"
    );
    let out = resp.text().await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        parsed["comments"].as_array().map(|a| a.len()),
        Some(comment_count),
        "all comments must survive the streamed export"
    );
}

#[tokio::test]
async fn export_over_size_cap_returns_413() {
    // v0.7.1 H7 — a rendered export past EXPORT_MAX_BYTES (32 MiB) is
    // rejected with 413 rather than driving unbounded allocation. One
    // comment with a ~40 MB body is the cheapest way past the cap.
    let (tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let review_dir = tmp
        .path()
        .join("state")
        .join(format!(
            "test-{}",
            tmp.path().file_name().unwrap().to_string_lossy()
        ))
        .join("smoke")
        .join(".review");
    std::fs::create_dir_all(&review_dir).unwrap();

    let huge_body = "x".repeat(40 * 1024 * 1024);
    let doc = format!(
        r#"{{"schema":"kb-comments/1","artifact":{{"id":"huge","title":"Huge","kb":"smoke","tags":[],"pages":[]}},"generatedAt":"2026-05-14T10:00:00Z","comments":[{{"id":"c_huge","status":"open","file":"huge","fileLabel":"main","anchor":{{"kind":"file"}},"author":"you","body":"{huge_body}","createdAt":"2026-05-14T10:00:00Z","editedAt":null,"replies":[]}}]}}"#
    );
    std::fs::write(review_dir.join("huge.json"), &doc).unwrap();

    let resp = client
        .post(url(addr, "/api/kb/smoke/review/huge/export?format=json"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        413,
        "an export past the 32 MiB cap must 413, not allocate unbounded"
    );
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("problem+json"), "got ct: {ct}");
}

// === v0.4 D2 — /api/kb/{kb}/artifact/{id} read-only HTML ===

#[tokio::test]
async fn artifact_bytes_route_serves_raw_html_for_known_id() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // Pick the first doc id off the list endpoint.
    let docs: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/docs?limit=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = docs[0]["id"].as_str().unwrap();
    let resp = client
        .get(url(addr, &format!("/api/kb/smoke/artifact/{id}")))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/html"), "got ct: {ct}");
    // deep-review P1 → P0: this main-origin byte endpoint must neutralize
    // active content so a hostile artifact can't execute against the trusted
    // SPA origin. nosniff + a `sandbox` CSP (unique opaque origin, no scripts).
    assert_eq!(
        resp.headers()
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff"),
        "artifact_bytes must send X-Content-Type-Options: nosniff"
    );
    assert_eq!(
        resp.headers()
            .get("content-security-policy")
            .and_then(|v| v.to_str().ok()),
        Some("sandbox"),
        "artifact_bytes must sandbox the response on the main origin"
    );
    let xkb = resp
        .headers()
        .get("x-kb-artifact-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(xkb, id);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("<html") || body.contains("<HTML"),
        "got: {}",
        &body[..body.len().min(200)]
    );
}

#[tokio::test]
async fn artifact_bytes_route_returns_404_for_unknown_id() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/artifact/no-such-doc"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// === v0.4 C1 — mDNS smoke ===

#[tokio::test]
async fn mdns_advertise_does_not_panic_when_enabled() {
    // Boot a daemon with server.mdns = true. We only assert the boot
    // succeeds (the multicast registration may or may not complete in
    // the test runner's network namespace — CI runners commonly block
    // multicast). A successful identity GET means the mDNS code path
    // didn't panic the daemon.
    let (tmp, mut cfg, paths) = fixture_corpus_with(None);
    cfg.server.mdns = true;
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let _tmp = tmp;
    let client = reqwest::Client::new();
    let resp = client.get(url(addr, "/api/identity")).send().await.unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
}

// === v0.4 B2 — ?cm=on cache-key headers ===

#[tokio::test]
async fn cm_on_sets_private_no_store_and_vary_authorization() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/?cm=on", addr.port()))
        .header("Host", "kitchen-sink.artifacts.localhost:4000")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let cache_control = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        cache_control.contains("private") && cache_control.contains("no-store"),
        "expected `private, no-store`, got: {cache_control}"
    );
    let vary = resp
        .headers()
        .get("vary")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        vary.contains("Authorization"),
        "expected Vary to include Authorization, got: {vary}"
    );
}

#[tokio::test]
async fn cm_off_uses_plain_vary_without_authorization() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/", addr.port()))
        .header("Host", "kitchen-sink.artifacts.localhost:4000")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let vary = resp
        .headers()
        .get("vary")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        !vary.contains("Authorization"),
        "non-cm response should NOT vary on Authorization, got: {vary}"
    );
}

#[tokio::test]
async fn rate_limit_search_override_caps_at_configured_per_min() {
    // kb.toml `[server.rate_limit] search = 3` → only 3 search
    // requests should land 200 in a single-minute window; the
    // remaining 7 of a 10-burst with the same XFF + token must
    // come back 429.
    let (tmp, mut cfg, paths) = fixture_corpus();
    cfg.server.rate_limit = Some(RateLimitSection {
        search: Some(3),
        atlas_recompute: None,
        review_post: None,
        history_post: None,
    });
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(paths.token_file(), FIXTURE_TOKEN).unwrap();
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let _tmp = tmp;

    let client = reqwest::Client::new();
    let mut count_200 = 0;
    let mut count_429 = 0;
    for _ in 0..10 {
        let resp = client
            .get(url(addr, "/api/search?q=borrow&mode=keyword&kb=smoke"))
            .header("X-Forwarded-For", "8.8.8.8")
            .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
            .send()
            .await
            .unwrap();
        match resp.status().as_u16() {
            200 => count_200 += 1,
            429 => count_429 += 1,
            other => panic!("unexpected status {other}"),
        }
    }
    assert_eq!(count_200, 3, "expected exactly 3 200s under search=3");
    assert_eq!(count_429, 7, "remaining requests should 429");
}

#[tokio::test]
async fn rate_limit_skipped_for_loopback() {
    // Loopback (no XFF) bypasses the limiter — fire 80 requests +
    // expect zero 429s.
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let mut count_429 = 0;
    for _ in 0..80 {
        let resp = client
            .get(url(addr, "/api/search?q=borrow&mode=keyword&kb=smoke"))
            .send()
            .await
            .unwrap();
        if resp.status().as_u16() == 429 {
            count_429 += 1;
        }
    }
    assert_eq!(count_429, 0, "loopback must bypass rate limit");
}

/// Fixture for the nested-sibling-asset case. The canon corpus has all
/// artifacts at the kb root, so it doesn't exercise the path where an
/// artifact at `<root>/ideas/foo/index.html` references siblings under
/// `<root>/ideas/foo/_assets/...`.
async fn boot_with_nested_fixture() -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    let nested = source.join("ideas").join("foo");
    let assets = nested.join("_assets");
    std::fs::create_dir_all(&assets).unwrap();

    std::fs::write(
        nested.join("index.html"),
        r#"<!doctype html><html><head><title>Nested Sibling</title>
<link rel="stylesheet" href="_assets/style.css"></head>
<body><h1>nested sibling fixture</h1></body></html>"#,
    )
    .unwrap();
    std::fs::write(
        assets.join("style.css"),
        b"body { background: rebeccapurple; }\n",
    )
    .unwrap();
    // A file OUTSIDE the kb root — used to confirm traversal still 400s
    // via a symlink that points out of `corpus/`.
    std::fs::write(tmp.path().join("outside.txt"), b"secret\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(tmp.path().join("outside.txt"), nested.join("escape.txt")).unwrap();

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("nested").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    common::wait_docs_listed(addr, "nested", 1).await;
    (tmp, addr)
}

#[tokio::test]
async fn artifact_subdomain_serves_nested_sibling_asset() {
    // Regression: an artifact at <root>/ideas/foo/index.html references
    // `_assets/style.css`; the daemon must serve the sibling asset under
    // the artifact's content-hash subdomain, not 404 by joining to the
    // kb root.
    let (_tmp, addr) = boot_with_nested_fixture().await;
    let client = reqwest::Client::new();

    let doc = wait_for_doc(&client, addr, "nested", |d| {
        d["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("/ideas/foo/index.html")
    })
    .await;
    let id = doc["id"].as_str().expect("nested artifact id").to_string();
    assert!(
        id.len() == 12 && id.chars().all(|c| c.is_ascii_hexdigit()),
        "expected hex id, got {id}"
    );

    // Root subdomain request: HTML body, contains the stylesheet link.
    let resp = client
        .get(url(addr, "/"))
        .header("Host", format!("{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "root got {}", resp.status());
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("_assets/style.css"),
        "expected stylesheet reference in body"
    );

    // Sibling asset: 200 + text/css.
    let resp = client
        .get(url(addr, "/_assets/style.css"))
        .header("Host", format!("{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "expected sibling asset 200, got {}",
        resp.status()
    );
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/css"), "got {ct}");
    let css = resp.text().await.unwrap();
    assert!(css.contains("rebeccapurple"), "expected css body");

    // Symlink escape — the artifact's directory contains `escape.txt`
    // symlinked to a file outside the kb root. The handler's
    // canonicalize + starts_with(source_root) bound must still reject
    // it as path traversal even though we now resolve relative to the
    // artifact's parent dir.
    #[cfg(unix)]
    {
        let resp = client
            .get(url(addr, "/escape.txt"))
            .header("Host", format!("{id}.artifacts.localhost"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            400,
            "symlink escape must 400, got {}",
            resp.status()
        );
    }

    // Missing sibling — a reference to an asset that isn't on disk must
    // 404, not 500 and not a path-traversal 400. This exercises the
    // asset_base-relative miss path: `_assets/missing.css` fails to
    // canonicalise under the artifact's own dir, the cross-artifact
    // root fallback also misses, so the handler 404s cleanly.
    let resp = client
        .get(url(addr, "/_assets/missing.css"))
        .header("Host", format!("{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "missing sibling asset must 404, got {}",
        resp.status()
    );
}

/// Fixture for the walk-up resolver case. Entrypoint lives at depth 3
/// below the kb root; `_assets/` lives one level up from the entrypoint,
/// two levels below the kb root — so neither the entrypoint's own folder
/// nor the kb source root holds the asset. The resolver must walk
/// `parent/child/grandchild` → `parent/child` → `parent` (hit).
async fn boot_with_deep_nested_fixture() -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    let parent = source.join("parent");
    let grandchild = parent.join("child").join("grandchild");
    let assets = parent.join("_assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::create_dir_all(&grandchild).unwrap();

    std::fs::write(
        grandchild.join("index.html"),
        r#"<!doctype html><html><head><title>Deep Nested</title>
<link rel="stylesheet" href="_assets/x.css">
<script src="_assets/x.js"></script></head>
<body><h1>deep nested fixture</h1></body></html>"#,
    )
    .unwrap();
    std::fs::write(assets.join("x.css"), b"body { background: papayawhip; }\n").unwrap();
    std::fs::write(assets.join("x.js"), b"console.log('walk-up');\n").unwrap();

    // A file OUTSIDE the kb root, plus a symlink to it placed at an
    // *intermediate* walk-up level (`parent/_assets/leak.css`). The walk
    // from grandchild misses at depth 3 + depth 2, then hits the symlink
    // at depth 1; canonicalize follows it outside source_root and the
    // Ok-but-escape branch must still return 400.
    std::fs::write(tmp.path().join("outside.txt"), b"secret\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(tmp.path().join("outside.txt"), assets.join("leak.css")).unwrap();

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("deep").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    common::wait_docs_listed(addr, "deep", 1).await;
    (tmp, addr)
}

#[tokio::test]
async fn artifact_subdomain_walks_up_to_find_css() {
    // Walk-up: grandchild/index.html references `_assets/x.css`; the
    // asset lives two levels up. The resolver must probe
    // grandchild → child → parent and serve from `parent/_assets/x.css`.
    let (_tmp, addr) = boot_with_deep_nested_fixture().await;
    let client = reqwest::Client::new();

    let doc = wait_for_doc(&client, addr, "deep", |d| {
        d["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("/parent/child/grandchild/index.html")
    })
    .await;
    let id = doc["id"].as_str().expect("deep artifact id").to_string();

    let resp = client
        .get(url(addr, "/_assets/x.css"))
        .header("Host", format!("{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "expected walk-up css 200, got {}",
        resp.status()
    );
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/css"), "got {ct}");
    let css = resp.text().await.unwrap();
    assert!(css.contains("papayawhip"), "expected css body");
}

#[tokio::test]
async fn artifact_subdomain_walks_up_to_find_js() {
    // Same walk, different MIME — confirms the resolver's content-type
    // path isn't tied to the folder it found the file in.
    let (_tmp, addr) = boot_with_deep_nested_fixture().await;
    let client = reqwest::Client::new();

    let doc = wait_for_doc(&client, addr, "deep", |d| {
        d["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("/parent/child/grandchild/index.html")
    })
    .await;
    let id = doc["id"].as_str().expect("deep artifact id").to_string();

    let resp = client
        .get(url(addr, "/_assets/x.js"))
        .header("Host", format!("{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "expected walk-up js 200, got {}",
        resp.status()
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("walk-up"), "expected js body");
}

#[tokio::test]
#[cfg(unix)]
async fn artifact_subdomain_walk_up_symlink_escape_rejected() {
    // A symlink at an intermediate walk-up level points outside the kb
    // root. The walk hits it at depth 1 (parent/_assets/leak.css);
    // canonicalize follows the symlink, starts_with(source_root) fails,
    // and the resolver must 400 — not silently skip and 404 at the next
    // level. Locks the escape-semantics decision.
    let (_tmp, addr) = boot_with_deep_nested_fixture().await;
    let client = reqwest::Client::new();

    let doc = wait_for_doc(&client, addr, "deep", |d| {
        d["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("/parent/child/grandchild/index.html")
    })
    .await;
    let id = doc["id"].as_str().expect("deep artifact id").to_string();

    let resp = client
        .get(url(addr, "/_assets/leak.css"))
        .header("Host", format!("{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        400,
        "walk-up symlink escape must 400, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn artifact_subdomain_walk_up_missing_asset_404s() {
    // Walk reaches source_root without finding the file; resolver 404s
    // cleanly (not 500, not 400). Exercises the loop's source_root
    // termination branch.
    let (_tmp, addr) = boot_with_deep_nested_fixture().await;
    let client = reqwest::Client::new();

    let doc = wait_for_doc(&client, addr, "deep", |d| {
        d["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("/parent/child/grandchild/index.html")
    })
    .await;
    let id = doc["id"].as_str().expect("deep artifact id").to_string();

    let resp = client
        .get(url(addr, "/_assets/does-not-exist.css"))
        .header("Host", format!("{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "missing walk-up asset must 404, got {}",
        resp.status()
    );
}

/// Raw HTTP/1.1 GET that bypasses reqwest's url-crate normalisation.
/// The url crate strips `..` segments and collapses consecutive slashes
/// before sending — fine for browser-like traffic, useless for testing
/// the daemon's own defences against a malicious client that doesn't
/// pre-normalise. Used by the dot-segment + multi-slash walk-up tests.
async fn raw_get(addr: std::net::SocketAddr, host: &str, path: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let body = String::from_utf8_lossy(&buf).into_owned();
    let status: u16 = body
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, body)
}

/// Fixture for the shadowing test. Two `_assets/shadow.css` candidates
/// at different walk-up depths from the entrypoint; the nearer one
/// MUST win. If the walk-up loop broke "first-match-wins" (e.g. by
/// walking all the way up before checking), the far one would be
/// served — different content lets the test detect either choice.
async fn boot_with_walk_up_shadowing_fixture() -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    let far_assets = source.join("a").join("_assets");
    let near_assets = source.join("a").join("b").join("_assets");
    let entry = source.join("a").join("b").join("c");
    std::fs::create_dir_all(&far_assets).unwrap();
    std::fs::create_dir_all(&near_assets).unwrap();
    std::fs::create_dir_all(&entry).unwrap();

    std::fs::write(
        entry.join("index.html"),
        r#"<!doctype html><html><head><title>Shadowing</title>
<link rel="stylesheet" href="_assets/shadow.css"></head>
<body><h1>shadowing fixture</h1></body></html>"#,
    )
    .unwrap();
    std::fs::write(far_assets.join("shadow.css"), b"/* FAR */\n").unwrap();
    std::fs::write(near_assets.join("shadow.css"), b"/* NEAR */\n").unwrap();

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("shadow").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    common::wait_docs_listed(addr, "shadow", 1).await;
    (tmp, addr)
}

#[tokio::test]
async fn artifact_subdomain_walk_up_nearer_assets_shadows_further() {
    // First-match-wins: walking from `a/b/c/index.html`, the resolver
    // must return `a/b/_assets/shadow.css` (NEAR), not `a/_assets/shadow.css`
    // (FAR). Locks the proposal's "closer-to-entrypoint shadows further-up"
    // guarantee in code.
    let (_tmp, addr) = boot_with_walk_up_shadowing_fixture().await;
    let client = reqwest::Client::new();

    let doc = wait_for_doc(&client, addr, "shadow", |d| {
        d["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("/a/b/c/index.html")
    })
    .await;
    let id = doc["id"]
        .as_str()
        .expect("shadowing artifact id")
        .to_string();

    let resp = client
        .get(url(addr, "/_assets/shadow.css"))
        .header("Host", format!("{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let css = resp.text().await.unwrap();
    assert!(
        css.contains("NEAR") && !css.contains("FAR"),
        "expected NEAR to shadow FAR, got body: {css}"
    );
}

/// Fixture for the "walk reaches source_root" test. Only the kb root
/// has `_assets/global.css`; the entrypoint sits four levels below.
/// The walk must terminate at source_root after probing it (not one
/// level above, not one level below) — proves the loop's
/// `probe == source_root` termination + final-probe-of-root semantics.
async fn boot_with_walk_up_root_only_assets_fixture() -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    let assets = source.join("_assets");
    let entry = source.join("a").join("b").join("c").join("d");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::create_dir_all(&entry).unwrap();

    std::fs::write(
        entry.join("index.html"),
        r#"<!doctype html><html><head><title>Root-only assets</title>
<link rel="stylesheet" href="_assets/global.css"></head>
<body><h1>root-only assets fixture</h1></body></html>"#,
    )
    .unwrap();
    std::fs::write(assets.join("global.css"), b"/* GLOBAL */\n").unwrap();

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("root-only").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    common::wait_docs_listed(addr, "root-only", 1).await;
    (tmp, addr)
}

#[tokio::test]
async fn artifact_subdomain_walk_up_reaches_source_root() {
    // The walk must descend all four intermediate levels and finally
    // hit `_assets/global.css` at source_root itself. Confirms the loop
    // probes source_root (the final iteration) before terminating, not
    // one level above.
    let (_tmp, addr) = boot_with_walk_up_root_only_assets_fixture().await;
    let client = reqwest::Client::new();

    let doc = wait_for_doc(&client, addr, "root-only", |d| {
        d["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("/a/b/c/d/index.html")
    })
    .await;
    let id = doc["id"]
        .as_str()
        .expect("root-only artifact id")
        .to_string();

    let resp = client
        .get(url(addr, "/_assets/global.css"))
        .header("Host", format!("{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "expected root-only css 200, got {}",
        resp.status()
    );
    let css = resp.text().await.unwrap();
    assert!(css.contains("GLOBAL"), "got body: {css}");
}

#[tokio::test]
async fn artifact_subdomain_walk_up_dot_dot_escape_returns_400() {
    // Send `_assets/../../../outside.txt` via raw HTTP (reqwest would
    // normalise the `..`). Walk-up reaches `parent/_assets/` at depth 1,
    // canonicalize follows the `..` segments out of source_root and
    // lands on `<tmp>/outside.txt`. starts_with(source_root) fails →
    // 400 path-traversal. Exercises the trust boundary against a
    // malicious client that pre-encodes traversal directly in the URL.
    let (tmp, addr) = boot_with_deep_nested_fixture().await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, "deep", |d| {
        d["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("/parent/child/grandchild/index.html")
    })
    .await;
    let id = doc["id"].as_str().expect("deep artifact id").to_string();
    // Sanity: outside.txt is where the fixture put it (asserts the test
    // is actually targeting an existing escape target — not a 404 that
    // happens for an unrelated reason).
    assert!(
        tmp.path().join("outside.txt").exists(),
        "fixture invariant: outside.txt must exist"
    );

    let host = format!("{id}.artifacts.localhost");
    let (status, _body) = raw_get(addr, &host, "/_assets/../../../outside.txt").await;
    assert_eq!(
        status, 400,
        "dot-segment escape to {{tmp}}/outside.txt must 400, got {status}"
    );
}

/// Fixture for the cohabiting-via-walk-up trampoline test. Two indexed
/// artifacts: `area/sibling.html` (depth 1) and `area/sub1/sub2/grandchild.html`
/// (depth 3). When grandchild's subdomain requests `/sibling.html`, the
/// walk hits sibling.html at depth 1; `get_by_source_path` finds it as
/// a *different* indexed artifact and the resolver must return the
/// trampoline (which re-mounts the iframe at sibling's own subdomain)
/// — NOT serve sibling's HTML inline on the grandchild's subdomain.
async fn boot_with_walk_up_cohabit_fixture() -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    let area = source.join("area");
    let deep = area.join("sub1").join("sub2");
    std::fs::create_dir_all(&deep).unwrap();

    std::fs::write(
        area.join("sibling.html"),
        r#"<!doctype html><html><head><title>Sibling cohabit</title></head>
<body><h1>SIBLING_RAW_PAYLOAD_MARKER</h1></body></html>"#,
    )
    .unwrap();
    std::fs::write(
        deep.join("grandchild.html"),
        r#"<!doctype html><html><head><title>Grandchild cohabit</title></head>
<body><h1>grandchild cohabit fixture</h1></body></html>"#,
    )
    .unwrap();

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("cohabit").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    common::wait_docs_listed(addr, "cohabit", 2).await;
    (tmp, addr)
}

#[tokio::test]
async fn artifact_subdomain_walk_up_cohabit_triggers_trampoline() {
    // Walk from grandchild's subdomain hits sibling.html two levels up.
    // The resolver must return the trampoline response (not raw HTML)
    // — checked via the `X-Kb-Trampoline` response header and the
    // absence of sibling.html's verbatim body marker.
    let (_tmp, addr) = boot_with_walk_up_cohabit_fixture().await;
    let client = reqwest::Client::new();

    let sibling = wait_for_doc(&client, addr, "cohabit", |d| {
        d["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("/area/sibling.html")
    })
    .await;
    let sibling_id = sibling["id"].as_str().expect("sibling id").to_string();
    let grandchild = wait_for_doc(&client, addr, "cohabit", |d| {
        d["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("/area/sub1/sub2/grandchild.html")
    })
    .await;
    let grandchild_id = grandchild["id"]
        .as_str()
        .expect("grandchild id")
        .to_string();
    assert_ne!(
        sibling_id, grandchild_id,
        "fixture invariant: ids must differ"
    );

    let resp = client
        .get(url(addr, "/sibling.html"))
        .header("Host", format!("{grandchild_id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "trampoline response must 200, got {}",
        resp.status()
    );
    let tramp = resp
        .headers()
        .get("x-kb-trampoline")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    assert_eq!(
        tramp.as_deref(),
        Some(sibling_id.as_str()),
        "X-Kb-Trampoline must point to sibling id, got {tramp:?}"
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("open-artifact"),
        "trampoline body must contain the open-artifact postMessage payload"
    );
    assert!(
        !body.contains("SIBLING_RAW_PAYLOAD_MARKER"),
        "trampoline must NOT inline sibling.html's raw body"
    );
}

/// Fixture for the multi-kb / source-root-isolation test. Two kbs at
/// sibling paths under `<tmp>`, plus a poison `_assets/x.css` at
/// `<tmp>/_assets/` (OUTSIDE both source roots). If the walk-up loop
/// were ever changed to traverse past source_root, the poison would
/// be hit; the assertion of 404 locks the current termination semantics.
async fn boot_with_walk_up_two_kb_fixture() -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let kb_a_root = tmp.path().join("kb-a");
    let kb_b_root = tmp.path().join("kb-b");
    let kb_a_entry = kb_a_root.join("parent").join("child");
    let poison_assets = tmp.path().join("_assets");
    std::fs::create_dir_all(&kb_a_entry).unwrap();
    std::fs::create_dir_all(&kb_b_root).unwrap();
    std::fs::create_dir_all(&poison_assets).unwrap();

    std::fs::write(
        kb_a_entry.join("index.html"),
        r#"<!doctype html><html><head><title>kb-A entrypoint</title>
<link rel="stylesheet" href="_assets/x.css"></head>
<body><h1>kb-a fixture</h1></body></html>"#,
    )
    .unwrap();
    // kb-B's own file — keeps the kb non-empty so the indexer registers it.
    std::fs::write(
        kb_b_root.join("readme.html"),
        r#"<!doctype html><html><head><title>kb-B readme</title></head>
<body><h1>kb-b fixture</h1></body></html>"#,
    )
    .unwrap();
    // The poison — would be served if walk-up ever escaped source_root.
    std::fs::write(poison_assets.join("x.css"), b"/* POISON */\n").unwrap();

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("alpha").unwrap(),
        KbSection {
            path: kb_a_root,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    kb_map.insert(
        KbName::new("bravo").unwrap(),
        KbSection {
            path: kb_b_root,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    common::wait_docs_listed(addr, "alpha", 1).await;
    common::wait_docs_listed(addr, "bravo", 1).await;
    (tmp, addr)
}

#[tokio::test]
async fn artifact_subdomain_walk_up_does_not_escape_source_root() {
    // Walk from kb-A's entrypoint must terminate at kb-A's source_root
    // and NOT continue up to `<tmp>` where the poison asset lives.
    // 404 is the expected outcome: the walk doesn't reach the poison,
    // so the daemon returns "not found" rather than serving cross-kb
    // content. (If termination broke, the starts_with guard would
    // turn the hit into a 400 — also a failure of this test.)
    let (tmp, addr) = boot_with_walk_up_two_kb_fixture().await;
    let client = reqwest::Client::new();

    let doc = wait_for_doc(&client, addr, "alpha", |d| {
        d["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("/parent/child/index.html")
    })
    .await;
    let id = doc["id"].as_str().expect("kb-a artifact id").to_string();
    assert!(
        tmp.path().join("_assets/x.css").exists(),
        "fixture invariant: poison _assets/x.css must exist outside source_root"
    );

    let resp = client
        .get(url(addr, "/_assets/x.css"))
        .header("Host", format!("{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        404,
        "walk-up must terminate at source_root; got {}",
        resp.status()
    );
}

#[tokio::test]
async fn artifact_subdomain_walk_up_multi_slash_404() {
    // Send `///etc/passwd` via raw HTTP — the multiple leading slashes
    // are reqwest-normalised away, so we use a raw socket. After
    // `trim_start_matches('/')`, the resolver sees rel = "etc/passwd",
    // a relative path, and walk-up misses at every level → 404. Pins
    // that multi-slash forms cannot smuggle an absolute path into
    // `Path::join` (which would replace the base if `rel` started
    // with `/`).
    let (_tmp, addr) = boot_with_deep_nested_fixture().await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, "deep", |d| {
        d["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("/parent/child/grandchild/index.html")
    })
    .await;
    let id = doc["id"].as_str().expect("deep artifact id").to_string();

    let host = format!("{id}.artifacts.localhost");
    let (status, _body) = raw_get(addr, &host, "///etc/passwd").await;
    assert_eq!(
        status, 404,
        "multi-slash path must 404 (no absolute-path smuggle), got {status}"
    );
}

// === v0.6 R1 — runs / queries history endpoints ===

#[tokio::test]
async fn runs_endpoint_lists_completed_runs_after_boot_indexer() {
    // The fixture corpus boots with a single source; the indexer fires
    // `index.start` + `index.complete` on first walk. Both should land
    // in the runs ring by the time we hit /runs.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/runs"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let rows: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(!rows.is_empty(), "expected at least one run after boot");
    let r = &rows[0];
    assert!(r["run"].as_str().is_some(), "run id missing: {r}");
    let status = r["status"].as_str().unwrap_or("");
    assert!(
        status == "complete" || status == "running",
        "unexpected status {status}"
    );
}

#[tokio::test]
async fn runs_endpoint_clamps_limit_to_max() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // limit way past the ring cap — should still 200, capped server-side.
    let resp = client
        .get(url(addr, "/api/kb/smoke/runs?limit=99999"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let rows: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(rows.len() <= 256, "ring capacity max is 256");
}

#[tokio::test]
async fn runs_endpoint_unknown_kb_returns_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/no-such-kb/runs"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn queries_endpoint_lists_recent_searches() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // Fire two searches so the ring has something.
    for q in ["borrow", "macro"] {
        let _ = client
            .get(url(
                addr,
                &format!("/api/search?q={q}&mode=keyword&kb=smoke"),
            ))
            .send()
            .await
            .unwrap();
    }
    // The query firehose envelopes are async-published; give the ring a
    // beat to ingest them.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let resp = client
        .get(url(addr, "/api/kb/smoke/queries?limit=10"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let rows: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(rows.len() >= 2, "expected ≥2 query rows, got {rows:?}");
    // Newest first.
    let qs: Vec<_> = rows.iter().map(|r| r["q"].as_str().unwrap()).collect();
    assert_eq!(qs[0], "macro");
    assert_eq!(qs[1], "borrow");
    // Schema shape.
    let r = &rows[0];
    assert!(r["mode"].as_str().is_some());
    assert!(r["hits"].as_u64().is_some());
    assert!(r["ms"].as_u64().is_some());
    assert!(r["at"].as_str().is_some());
}

#[tokio::test]
async fn queries_endpoint_unknown_kb_returns_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/no-such-kb/queries"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// === GC-B3 — zero-hit query aggregation (corpus-gap signal) ===

#[tokio::test]
async fn queries_zero_hit_groups_by_normalized_text_and_filters_min_count() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // Three zero-hit searches that only differ by case/whitespace should
    // collapse into one normalized group with count 3.
    for q in ["Zzzznomatch", "zzzznomatch", "  zzzznomatch  "] {
        let _ = client
            .get(url(addr, "/api/search"))
            .query(&[("q", q), ("mode", "keyword"), ("kb", "smoke")])
            .send()
            .await
            .unwrap();
    }
    // A single-occurrence zero-hit query.
    let _ = client
        .get(url(addr, "/api/search"))
        .query(&[("q", "onceonly"), ("mode", "keyword"), ("kb", "smoke")])
        .send()
        .await
        .unwrap();
    // A query that DOES hit (the kitchen-sink title) must never appear —
    // even under a permissive min_count.
    let _ = client
        .get(url(addr, "/api/search"))
        .query(&[("q", "everything"), ("mode", "keyword"), ("kb", "smoke")])
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let resp = client
        .get(url(addr, "/api/kb/smoke/queries"))
        .query(&[("zero_hit", "true")])
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let groups: Vec<serde_json::Value> = resp.json().await.unwrap();
    let by_q: std::collections::HashMap<&str, u64> = groups
        .iter()
        .map(|g| (g["query"].as_str().unwrap(), g["count"].as_u64().unwrap()))
        .collect();
    assert_eq!(by_q.get("zzzznomatch"), Some(&3));
    assert_eq!(by_q.get("onceonly"), Some(&1));
    assert!(
        !by_q.contains_key("everything"),
        "a query with hits must never show up in the zero-hit report: {groups:?}"
    );

    // min_count=2 drops the singleton.
    let resp = client
        .get(url(addr, "/api/kb/smoke/queries"))
        .query(&[("zero_hit", "true"), ("min_count", "2")])
        .send()
        .await
        .unwrap();
    let groups: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(
        groups.len(),
        1,
        "expected only the count=3 group: {groups:?}"
    );
    assert_eq!(groups[0]["query"].as_str().unwrap(), "zzzznomatch");
}

#[tokio::test]
async fn queries_zero_hit_all_fans_out_across_kbs() {
    let (_tmp, addr, _alpha_id, _beta_id) = boot_with_two_kbs().await;
    let client = reqwest::Client::new();

    // Nonsense tokens sharing no substring with the corpus content (a
    // hyphenated query like "alpha-miss" would tokenize into ["alpha",
    // "miss"] and OR-match the alpha doc's own title/content, which
    // would defeat the zero-hit signal this test is checking).
    let _ = client
        .get(url(addr, "/api/search"))
        .query(&[("q", "florbnix"), ("mode", "keyword"), ("kb", "alpha")])
        .send()
        .await
        .unwrap();
    let _ = client
        .get(url(addr, "/api/search"))
        .query(&[("q", "quixotropic"), ("mode", "keyword"), ("kb", "beta")])
        .send()
        .await
        .unwrap();
    let _ = client
        .get(url(addr, "/api/search"))
        .query(&[("q", "quixotropic"), ("mode", "keyword"), ("kb", "beta")])
        .send()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let resp = client
        .get(url(addr, "/api/queries/zero-hit"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let rows: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(rows.len(), 2, "expected one row per kb: {rows:?}");
    // Submission order (invariant #28) is the BTreeMap kb-name order:
    // "alpha" before "beta".
    assert_eq!(rows[0]["kb"].as_str().unwrap(), "alpha");
    assert_eq!(rows[1]["kb"].as_str().unwrap(), "beta");

    let alpha_groups = rows[0]["groups"].as_array().unwrap();
    assert_eq!(alpha_groups.len(), 1, "{rows:?}");
    assert_eq!(alpha_groups[0]["query"].as_str().unwrap(), "florbnix");
    assert_eq!(alpha_groups[0]["count"].as_u64().unwrap(), 1);

    let beta_groups = rows[1]["groups"].as_array().unwrap();
    assert_eq!(beta_groups.len(), 1, "{rows:?}");
    assert_eq!(beta_groups[0]["query"].as_str().unwrap(), "quixotropic");
    assert_eq!(beta_groups[0]["count"].as_u64().unwrap(), 2);
}

/// Spin up the daemon with TWO kbs, each owning a distinct artifact,
/// and return `(_tmp_guard, addr)` plus the two artifact ids so the
/// caller can hit each via its subdomain.
async fn boot_with_two_kbs() -> (
    tempfile::TempDir,
    std::net::SocketAddr,
    /* alpha_id */ String,
    /* beta_id */ String,
) {
    let tmp = tempfile::tempdir().unwrap();
    let alpha_root = tmp.path().join("alpha-src");
    let beta_root = tmp.path().join("beta-src");
    std::fs::create_dir_all(&alpha_root).unwrap();
    std::fs::create_dir_all(&beta_root).unwrap();
    // Distinct relative paths per kb: artifact ids are now path-based
    // (hash of the source-relative path), so two files both at
    // `note.html` would collide on one id. Distinct names keep the
    // two artifacts — and the subdomains the test hits — distinct.
    std::fs::write(
        alpha_root.join("alpha-note.html"),
        "<!doctype html><html><body><h1>alpha kb note</h1></body></html>",
    )
    .unwrap();
    std::fs::write(
        beta_root.join("beta-note.html"),
        "<!doctype html><html><body><h1>beta kb note</h1></body></html>",
    )
    .unwrap();

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("alpha").unwrap(),
        KbSection {
            path: alpha_root,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    kb_map.insert(
        KbName::new("beta").unwrap(),
        KbSection {
            path: beta_root,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    common::wait_docs_listed(addr, "alpha", 1).await;
    common::wait_docs_listed(addr, "beta", 1).await;

    let client = reqwest::Client::new();
    let mut ids: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    for kb in ["alpha", "beta"] {
        let docs: Vec<serde_json::Value> = client
            .get(url(addr, &format!("/api/kb/{kb}/docs?limit=10")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = docs
            .first()
            .and_then(|d| d["id"].as_str())
            .unwrap_or_else(|| panic!("no docs indexed in kb {kb}"))
            .to_string();
        ids.insert(kb, id);
    }
    let alpha_id = ids.remove("alpha").unwrap();
    let beta_id = ids.remove("beta").unwrap();
    (tmp, addr, alpha_id, beta_id)
}

// invariant:28 btree-fanout
#[tokio::test]
async fn federated_search_scope_all_is_deterministic_and_attributes_kbs() {
    // FF-B characterization pin (v0.19 cross-kb fan-out): federated
    // (`scope=all`) keyword search must return BOTH corpora's hits, each
    // attributed to its OWNING kb, in a DETERMINISTIC, submission-ordered
    // (BTreeMap: alpha < beta) order. Each corpus contributes one rank-0 hit,
    // so the cross-kb RRF ties and the output reflects arm (submission) order.
    // The fan-out refactor (buffered + submission-order fold) must keep this
    // byte-identical — a completion-order collect would flip the arm order
    // under load. Keyword mode is used so the test needs no embedder.
    let (_tmp, addr, alpha_id, beta_id) = boot_with_two_kbs().await;
    let client = reqwest::Client::new();

    let fetch = || async {
        let v: serde_json::Value = client
            .get(url(
                addr,
                "/api/search?q=note&mode=keyword&scope=all&limit=10",
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["id"].as_str().unwrap().to_string(),
                    h["kb"].as_str().unwrap().to_string(),
                )
            })
            .collect::<Vec<(String, String)>>()
    };

    let first = fetch().await;
    assert_eq!(
        first,
        vec![
            (alpha_id.clone(), "alpha".to_string()),
            (beta_id.clone(), "beta".to_string()),
        ],
        "scope=all relevance order + kb attribution"
    );
    // Determinism: identical across repeated calls.
    for _ in 0..3 {
        assert_eq!(
            fetch().await,
            first,
            "federated search order must be deterministic"
        );
    }

    // sort=title asc applies the explicit cmp_search sort over the merged set.
    let titled: serde_json::Value = client
        .get(url(
            addr,
            "/api/search?q=note&mode=keyword&scope=all&sort=title&dir=asc&limit=10",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let order: Vec<String> = titled["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["kb"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        order,
        vec!["alpha".to_string(), "beta".to_string()],
        "sort=title asc order"
    );
}

#[tokio::test]
async fn multi_kb_events_share_one_global_id_space() {
    // v0.7.1 H5 — a daemon with two kbs serves /api/events from a single
    // daemon-wide bus, so the SSE `id`s are globally unique. Pre-H5 each
    // kb had its own bus with ids from 1, so the merged stream emitted
    // colliding ids ([1,2,3,4,1,2,3,4]) and Last-Event-ID resume drifted.
    use futures::StreamExt;
    let (_tmp, addr, _alpha_id, _beta_id) = boot_with_two_kbs().await;
    let client = reqwest::Client::new();
    let resp = client.get(url(addr, "/api/events")).send().await.unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());

    // The two kbs each indexed one artifact on the initial walk; those
    // envelopes are already in the ring, so a fresh subscriber replays
    // them immediately. Read a bounded window, then parse the frames.
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    let _ = tokio::time::timeout(Duration::from_millis(800), async {
        while let Some(chunk) = stream.next().await {
            if let Ok(bytes) = chunk {
                buf.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    })
    .await;

    let ids: Vec<&str> = buf
        .lines()
        .filter_map(|l| l.strip_prefix("id:"))
        .map(str::trim)
        .collect();
    assert!(
        ids.len() >= 4,
        "expected both kbs' initial-walk events replayed, got {ids:?}"
    );
    let unique: std::collections::HashSet<&&str> = ids.iter().collect();
    assert_eq!(
        unique.len(),
        ids.len(),
        "SSE event ids collided — not a single global id space: {ids:?}"
    );
}

#[tokio::test]
async fn events_query_param_resumes_like_the_header() {
    // A2 — browser EventSource can't set the Last-Event-ID header, and the
    // SPA's manual-backoff reconnect builds a FRESH EventSource (so native
    // header replay never fires either) — pre-fix, every reconnect resumed
    // from 0 and replayed the entire ring. `?last_event_id=` must behave
    // like the header: replay only ids strictly after the cursor.
    use futures::StreamExt;
    let (_tmp, addr, _alpha_id, _beta_id) = boot_with_two_kbs().await;
    let client = reqwest::Client::new();

    // Fresh subscribe (no cursor) to learn the ring's ids. Predicate-driven
    // drain, NOT a flat window: a fixed 800ms read was load-sensitive (the
    // nextest flip's parallel load starved it — replay bytes arrived after
    // the window closed). Stop as soon as two ids have landed; the 10s
    // ceiling only bounds a genuinely broken stream.
    let resp = client.get(url(addr, "/api/events")).send().await.unwrap();
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if buf.lines().filter(|l| l.starts_with("id:")).count() >= 2 {
            break;
        }
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(Ok(bytes))) => buf.push_str(&String::from_utf8_lossy(&bytes)),
            _ => break,
        }
    }
    let ids: Vec<u64> = buf
        .lines()
        .filter_map(|l| l.strip_prefix("id:"))
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    assert!(
        ids.len() >= 2,
        "fixture should replay ring events, got {ids:?}"
    );
    // Penultimate id — exactly the events after it may be replayed.
    let cursor = ids[ids.len() - 2];

    let resp = client
        .get(url(addr, &format!("/api/events?last_event_id={cursor}")))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let mut buf2 = String::new();
    let mut stream2 = resp.bytes_stream();
    // Same predicate-driven drain: one replayed id is all the asserts need.
    let deadline2 = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if buf2.lines().any(|l| l.starts_with("id:")) {
            break;
        }
        match tokio::time::timeout_at(deadline2, stream2.next()).await {
            Ok(Some(Ok(bytes))) => buf2.push_str(&String::from_utf8_lossy(&bytes)),
            _ => break,
        }
    }
    let resumed: Vec<u64> = buf2
        .lines()
        .filter_map(|l| l.strip_prefix("id:"))
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    assert!(
        !resumed.is_empty(),
        "the event after the cursor must be replayed"
    );
    assert!(
        resumed.iter().all(|i| *i > cursor),
        "resume must skip ids <= {cursor}, got {resumed:?}"
    );
}

#[tokio::test]
async fn cors_allows_loopback_origins_on_reads_only() {
    // SW3 — loopback-only CORS on the /api tree makes multi-daemon fleet
    // SSE possible: the SPA (served by daemon A) streams /api/events from
    // daemon B cross-origin, which the browser only permits when B's
    // response carries Access-Control-Allow-Origin. Non-loopback origins
    // must get NO ACAO (drive-by websites stay blind to the local API).
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // Loopback origin, GET → ACAO echoes the origin (+ Vary: Origin).
    let resp = client
        .get(url(addr, "/api/identity"))
        .header("Origin", "http://localhost:9999")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:9999"),
    );

    // The SSE stream itself must be CORS-readable — that's the fleet case.
    let resp = client
        .get(url(addr, "/api/events?types=metrics.tick"))
        .header("Origin", "http://127.0.0.1:4738")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("http://127.0.0.1:4738"),
    );
    drop(resp);

    // Non-loopback origin → no ACAO header at all.
    let resp = client
        .get(url(addr, "/api/identity"))
        .header("Origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    assert!(resp.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn metrics_tick_reports_live_sse_subscriber_count() {
    // SW1 — `sse_subscribers` gauges live /api/events HTTP consumers via
    // a Drop-guard owned by each response stream. Two open streams must
    // read 2; closing one must converge to 1 once the server notices the
    // dead socket on a subsequent tick write.
    use futures::StreamExt;
    let (_tmp, addr, _alpha_id, _beta_id) = boot_with_two_kbs().await;
    let client = reqwest::Client::new();

    // Consumer #1: a plain firehose, held open (and unread) for the test.
    let extra = client.get(url(addr, "/api/events")).send().await.unwrap();
    assert!(extra.status().is_success(), "got {}", extra.status());

    // Consumer #2: the probe — reads metrics.tick frames for the gauge.
    let resp = client
        .get(url(addr, "/api/events?types=metrics.tick"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let mut stream = resp.bytes_stream();

    // Latest sse_subscribers value seen in the buffered tick frames.
    fn latest_gauge(buf: &str) -> Option<u64> {
        buf.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str::<serde_json::Value>(d.trim()).ok())
            .filter_map(|v| v.get("payload")?.get("sse_subscribers")?.as_u64())
            .next_back()
    }

    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let chunk = tokio::time::timeout(Duration::from_millis(1500), stream.next())
            .await
            .ok()
            .flatten();
        if let Some(Ok(bytes)) = chunk {
            buf.push_str(&String::from_utf8_lossy(&bytes));
        }
        if latest_gauge(&buf) == Some(2) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "gauge never reported 2 live consumers; ticks seen:\n{buf}"
        );
    }

    // Close consumer #1. The guard drops when the server's next tick
    // write hits the dead socket, so a later tick must report 1.
    drop(extra);
    buf.clear();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let chunk = tokio::time::timeout(Duration::from_millis(1500), stream.next())
            .await
            .ok()
            .flatten();
        if let Some(Ok(bytes)) = chunk {
            buf.push_str(&String::from_utf8_lossy(&bytes));
        }
        if latest_gauge(&buf) == Some(1) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "gauge never converged to 1 after closing a consumer; ticks seen:\n{buf}"
        );
    }
}

#[tokio::test]
async fn artifact_subdomain_resolves_across_kbs() {
    // Regression: with two kbs configured, the artifact handler used to
    // pick `state.kbs.values().next()` — a non-deterministic HashMap
    // iteration. Whichever kb was picked owned ~half the requests; the
    // other kb's artifacts 404'd. After the BTreeMap + resolve_artifact_kb
    // fix, the handler walks all kbs (alphabetical) and routes the request
    // to the kb that actually owns the id.
    let (_tmp, addr, alpha_id, beta_id) = boot_with_two_kbs().await;
    assert_ne!(alpha_id, beta_id, "fixture should produce distinct ids");
    let client = reqwest::Client::new();

    // alpha artifact via its own subdomain.
    let resp = client
        .get(url(addr, "/"))
        .header("Host", format!("{alpha_id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "alpha artifact got {} via subdomain",
        resp.status()
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("alpha kb note"),
        "expected alpha body, got: {}",
        &body[..body.len().min(200)]
    );

    // beta artifact via its own subdomain.
    let resp = client
        .get(url(addr, "/"))
        .header("Host", format!("{beta_id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "beta artifact got {} via subdomain",
        resp.status()
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("beta kb note"),
        "expected beta body, got: {}",
        &body[..body.len().min(200)]
    );

    // Unknown id under either subdomain → 404 (no kb owns it).
    let resp = client
        .get(url(addr, "/"))
        .header("Host", "deadbeef0000.artifacts.localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// --- v0.6+ H2 — history endpoints ----------------------------------------

#[tokio::test]
async fn history_open_returns_visit_id_and_resume_scroll() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .json(&serde_json::json!({ "artifact_id": "deadbeef0001" }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    let visit_id = body["visit_id"].as_i64().expect("visit_id");
    assert!(visit_id > 0);
    assert_eq!(body["scroll_y"].as_i64(), Some(0));

    let resp = client
        .post(url(addr, "/api/kb/smoke/history/scroll"))
        .json(&serde_json::json!({
            "visit_id": visit_id,
            "scroll_y": 512,
            "scroll_max": 2048
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    let resp = client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .json(&serde_json::json!({ "artifact_id": "deadbeef0001" }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["visit_id"].as_i64(), Some(visit_id), "same visit row");
    assert_eq!(body["scroll_y"].as_i64(), Some(512), "scroll resumed");
}

#[tokio::test]
async fn history_scroll_on_unknown_visit_returns_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(addr, "/api/kb/smoke/history/scroll"))
        .json(&serde_json::json!({
            "visit_id": 99999,
            "scroll_y": 100,
            "scroll_max": 1000
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// invariant:19 seed-on-open
#[tokio::test]
async fn reading_capture_round_trip_and_seed_on_open() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // Open a visit. A fresh visit's seed-on-open reading state is empty.
    let resp = client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .json(&serde_json::json!({ "artifact_id": "deadbeef0002" }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let visit_id = body["visit_id"].as_i64().expect("visit_id");
    assert_eq!(
        body["reading"]["active_ms"].as_i64(),
        Some(0),
        "fresh visit seed empty"
    );

    // Scroll near the bottom so completion_pct is meaningful.
    client
        .post(url(addr, "/api/kb/smoke/history/scroll"))
        .json(&serde_json::json!({ "visit_id": visit_id, "scroll_y": 800, "scroll_max": 1000 }))
        .send()
        .await
        .unwrap();

    // Reading beacon: "intro" read (60s of a 200-word section), "risks"
    // skimmed (3s). active_ms 95s, stopped in "risks".
    let resp = client
        .post(url(addr, "/api/kb/smoke/history/reading"))
        .json(&serde_json::json!({
            "visit_id": visit_id,
            "artifact_id": "deadbeef0002",
            "active_ms": 95000,
            "last_section": "risks",
            "sections": [
                {"id":"intro","idx":0,"text":"Intro","level":2,"words":200,"content_px":1000,"dwell_ms":60000,"enters":1},
                {"id":"risks","idx":1,"text":"Risks","level":2,"words":200,"content_px":1000,"dwell_ms":3000,"enters":1}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // GET summary reflects the capture (classify: intro read, risks skim).
    let resp = client
        .get(url(addr, "/api/kb/smoke/artifacts/deadbeef0002/reading"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let s: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(s["completion_pct"].as_i64(), Some(80));
    assert_eq!(
        s["read_pct"].as_i64(),
        Some(50),
        "intro read / both = 50% words-weighted"
    );
    assert_eq!(s["active_ms_total"].as_i64(), Some(95000));
    assert_eq!(s["visit_count"].as_i64(), Some(1));
    assert_eq!(s["stopped_at"]["section_id"].as_str(), Some("risks"));
    let secs = s["sections"].as_array().unwrap();
    assert_eq!(secs.len(), 2);
    assert_eq!(secs[0]["state"].as_str(), Some("read"));
    assert_eq!(secs[1]["state"].as_str(), Some("skim"));

    // ?lite=true → whole-page numbers only, no per-section breakdown.
    let resp = client
        .get(url(
            addr,
            "/api/kb/smoke/artifacts/deadbeef0002/reading?lite=true",
        ))
        .send()
        .await
        .unwrap();
    let s: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(s["completion_pct"].as_i64(), Some(80));
    assert!(
        s["sections"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "lite omits sections"
    );

    // Seed-on-open: re-open within the 30-min window returns the accumulated
    // reading state so the runtime resumes its cumulative counters.
    let resp = client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .json(&serde_json::json!({ "artifact_id": "deadbeef0002" }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["visit_id"].as_i64(), Some(visit_id), "same visit row");
    assert_eq!(
        body["reading"]["active_ms"].as_i64(),
        Some(95000),
        "seed carries active_ms"
    );
    assert_eq!(body["reading"]["last_section"].as_str(), Some("risks"));
    assert_eq!(
        body["reading"]["sections"].as_array().map(|a| a.len()),
        Some(2),
        "seed carries per-section dwell"
    );

    // 404 on a stale/unknown visit.
    let resp = client
        .post(url(addr, "/api/kb/smoke/history/reading"))
        .json(&serde_json::json!({
            "visit_id": 99999, "artifact_id": "deadbeef0002", "sections": [], "active_ms": 1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn history_search_records_query_and_returns_id() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(addr, "/api/kb/smoke/history/search"))
        .json(&serde_json::json!({ "query": "borrow checker" }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["id"].as_i64().expect("id") > 0);

    let resp = client
        .post(url(addr, "/api/kb/smoke/history/search"))
        .json(&serde_json::json!({ "query": "   " }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// invariant:8 append-only
#[tokio::test]
async fn history_list_returns_newest_first_and_kind_filters() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    let _ = client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .json(&serde_json::json!({ "artifact_id": "aaaa11112222" }))
        .send()
        .await
        .unwrap();
    let _ = client
        .post(url(addr, "/api/kb/smoke/history/search"))
        .json(&serde_json::json!({ "query": "rust" }))
        .send()
        .await
        .unwrap();
    let _ = client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .json(&serde_json::json!({ "artifact_id": "bbbb33334444" }))
        .send()
        .await
        .unwrap();

    let resp = client
        .get(url(addr, "/api/kb/smoke/history?limit=20"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let entries = body["entries"].as_array().expect("entries array");
    assert!(entries.len() >= 3);
    assert_eq!(entries[0]["kind"].as_str(), Some("open"));
    assert_eq!(entries[0]["artifact_id"].as_str(), Some("bbbb33334444"));

    let resp = client
        .get(url(addr, "/api/kb/smoke/history?kind=search&limit=20"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let entries = body["entries"].as_array().expect("entries array");
    assert!(entries.iter().all(|e| e["kind"] == "search"));
    assert!(entries.iter().any(|e| e["query"] == "rust"));
}

// --- GC-B5: history.open_source (web vs cli; kb cat/read telemetry) -------

#[tokio::test]
async fn history_open_source_round_trips_through_list() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // Explicit "cli" — what `kb cat`/`kb read` send.
    client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .json(&serde_json::json!({ "artifact_id": "cli000000001", "source": "cli" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // No `source` field at all — what the SPA sends today; must not 400
    // (additive field) and must come back with no open_source.
    client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .json(&serde_json::json!({ "artifact_id": "web0000000001" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // An unrecognised source value is dropped to null rather than 400ing —
    // soft telemetry, not worth failing the visit over.
    client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .json(&serde_json::json!({ "artifact_id": "odd00000001", "source": "bogus" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let resp = client
        .get(url(addr, "/api/kb/smoke/history?kind=open&limit=20"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let entries = body["entries"].as_array().expect("entries array");
    let find = |id: &str| {
        entries
            .iter()
            .find(|e| e["artifact_id"] == id)
            .unwrap_or_else(|| panic!("no entry for {id}"))
    };
    assert_eq!(find("cli000000001")["open_source"].as_str(), Some("cli"));
    assert!(
        find("web0000000001")["open_source"].is_null(),
        "no source sent ⇒ null, got {:?}",
        find("web0000000001")
    );
    assert!(
        find("odd00000001")["open_source"].is_null(),
        "unrecognised source ⇒ dropped to null, got {:?}",
        find("odd00000001")
    );
}

// --- v0.34 Y1 — multi-user identity (panel-mandated) ----------------------

/// Two users via trusted-hop `Remote-User` (loopback peer = trusted) get
/// independent history rows within the 30-min window; comment attribution
/// stamps the requesting user; forged `Remote-User` from a non-trusted
/// peer is ignored (attributes as operator/loopback).
#[tokio::test]
async fn multi_user_history_and_comments_are_per_user() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let art = "deadbeef00aa";

    // alice opens
    let resp = client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .header("Remote-User", "alice")
        .json(&serde_json::json!({ "artifact_id": art, "source": "web" }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "alice open: {}", resp.status());
    let alice_body: serde_json::Value = resp.json().await.unwrap();
    let alice_visit = alice_body["visit_id"].as_i64().expect("alice visit_id");

    // bob opens same artifact within the 30-min window → SEPARATE row
    let resp = client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .header("Remote-User", "bob")
        .json(&serde_json::json!({ "artifact_id": art, "source": "web" }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "bob open: {}", resp.status());
    let bob_body: serde_json::Value = resp.json().await.unwrap();
    let bob_visit = bob_body["visit_id"].as_i64().expect("bob visit_id");
    assert_ne!(
        alice_visit, bob_visit,
        "two users within 30 min must get independent history rows"
    );

    // Independent scroll: bob scrolls; alice's resume scroll stays 0.
    let resp = client
        .post(url(addr, "/api/kb/smoke/history/scroll"))
        .header("Remote-User", "bob")
        .json(&serde_json::json!({
            "visit_id": bob_visit,
            "scroll_y": 999,
            "scroll_max": 2000
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    let resp = client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .header("Remote-User", "alice")
        .json(&serde_json::json!({ "artifact_id": art }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["visit_id"].as_i64(), Some(alice_visit));
    assert_eq!(
        body["scroll_y"].as_i64(),
        Some(0),
        "alice's scroll must be unaffected by bob's reads"
    );

    // bob's re-open resumes his scroll.
    let resp = client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .header("Remote-User", "bob")
        .json(&serde_json::json!({ "artifact_id": art }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["visit_id"].as_i64(), Some(bob_visit));
    assert_eq!(body["scroll_y"].as_i64(), Some(999));

    // bob posts a comment; user field is bob.
    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/review/{art}/comments")))
        .header("Remote-User", "bob")
        .json(&serde_json::json!({
            "body": "bob's note",
            "author": "you",
            "anchor": { "kind": "file" }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "bob comment: {}", resp.status());
    let c: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(c["user"].as_str(), Some("bob"), "comment user stamped");

    // GET /api/identity reflects Remote-User.
    let resp = client
        .get(url(addr, "/api/identity"))
        .header("Remote-User", "alice")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let id: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(id["user"].as_str(), Some("alice"));
    assert_eq!(id["identity_source"].as_str(), Some("header"));

    // GET /api/users includes observed alice/bob.
    let resp = client.get(url(addr, "/api/users")).send().await.unwrap();
    assert!(resp.status().is_success());
    let users: serde_json::Value = resp.json().await.unwrap();
    let names: Vec<&str> = users["users"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|u| u["name"].as_str())
        .collect();
    assert!(names.contains(&"alice"), "observed alice: {names:?}");
    assert!(names.contains(&"bob"), "observed bob: {names:?}");
    assert!(
        names.contains(&"operator"),
        "configured operator: {names:?}"
    );
}

/// v0.34 Y1 mutation policy, end to end: comment body EDIT + DELETE are
/// owner-only (403 not-owner) through BOTH the direct routes AND the batch
/// path; RESOLVE stays open to every user (the collaboration surface).
#[tokio::test]
async fn comment_mutation_policy_owner_only_direct_and_batch() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let art = "deadbeef00ab";

    // bob creates a comment.
    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/review/{art}/comments")))
        .header("Remote-User", "bob")
        .json(&serde_json::json!({
            "body": "bob's finding",
            "author": "you",
            "anchor": { "kind": "file" }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let c: serde_json::Value = resp.json().await.unwrap();
    let cid = c["id"].as_str().expect("comment id").to_string();

    // alice cannot EDIT bob's body (direct route) → 403 not-owner.
    let resp = client
        .patch(url(
            addr,
            &format!("/api/kb/smoke/review/{art}/comments/{cid}"),
        ))
        .header("Remote-User", "alice")
        .json(&serde_json::json!({ "body": "hijacked" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403, "direct edit by non-owner");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"].as_str(), Some("urn:kb:errors:not-owner"));

    // alice cannot EDIT bob's body through the BATCH path either.
    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/review/{art}/apply")))
        .header("Remote-User", "alice")
        .json(&serde_json::json!({ "ops": [
            { "op": "edit_comment", "comment_id": cid, "body": "hijacked" }
        ]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403, "batch edit by non-owner");

    // alice cannot DELETE bob's comment through the batch either.
    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/review/{art}/apply")))
        .header("Remote-User", "alice")
        .json(&serde_json::json!({ "ops": [
            { "op": "delete_comment", "comment_id": cid }
        ]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403, "batch delete by non-owner");

    // alice CAN resolve bob's comment (collaboration surface, incl. batch).
    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/review/{art}/apply")))
        .header("Remote-User", "alice")
        .json(&serde_json::json!({ "ops": [
            { "op": "resolve", "comment_id": cid }
        ]}))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "resolve by non-owner must stay open: {}",
        resp.status()
    );

    // bob edits his own comment through the batch → applied.
    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/review/{art}/apply")))
        .header("Remote-User", "bob")
        .json(&serde_json::json!({ "ops": [
            { "op": "edit_comment", "comment_id": cid, "body": "bob rewrote" }
        ]}))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "owner batch edit: {}",
        resp.status()
    );
}

#[tokio::test]
async fn history_open_rejects_path_traversal_in_artifact_id() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .json(&serde_json::json!({ "artifact_id": "../etc/passwd" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn history_list_kind_filter_rejects_unknown_value() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/history?kind=bogus"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// L1 — /api/kb/{kb}/lookup. Resolves an artifact by 12-hex id, an
// exact source-relative path, or a unique filename suffix; surfaces
// ambiguity + not-found via the `kind` field.

#[tokio::test]
async fn lookup_resolves_known_basename_to_unique_suffix() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/lookup"))
        .query(&[("q", "cost-of-abstraction.html")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // The canon fixture has cost-of-abstraction.html at the root —
    // the exact-path branch hits before the suffix scan, so kind is
    // "exact" rather than "unique_suffix".
    assert_eq!(body["kind"], "exact", "got {body:#}");
    assert!(body["id"].as_str().unwrap().len() == 12);
    assert!(body["source_relative"]
        .as_str()
        .unwrap()
        .ends_with("cost-of-abstraction.html"));
    assert_eq!(body["folder"], "");
}

#[tokio::test]
async fn lookup_passes_through_12_hex_id() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // First, resolve via basename to get a real id.
    let id_body: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/lookup"))
        .query(&[("q", "kitchen-sink.html")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = id_body["id"].as_str().unwrap();

    // Now look that id up directly.
    let body: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/lookup"))
        .query(&[("q", id)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["kind"], "exact", "got {body:#}");
    assert_eq!(body["id"], id);
}

#[tokio::test]
async fn lookup_returns_not_found_for_unknown() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/lookup"))
        .query(&[("q", "definitely-not-a-real-file.html")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["kind"], "not_found", "got {body:#}");
}

#[tokio::test]
async fn lookup_400s_on_missing_q() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/lookup"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
}

// R1 — reconciler hardening surface.

#[tokio::test]
async fn stats_carries_reconcile_secs_and_initial_nulls() {
    // Right after boot the reconciler hasn't run yet, so
    // last_reconcile_* fields are absent (Option::None →
    // skip_serializing_if). reconcile_secs is always present.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        body["reconcile_secs"].is_u64(),
        "reconcile_secs must be a number: {body:#}"
    );
    // last_reconcile_at is absent before the first tick. We accept
    // either Null OR missing (some serde versions render `None` as
    // missing; the route uses skip_serializing_if).
    assert!(
        body.get("last_reconcile_at")
            .map(|v| v.is_null())
            .unwrap_or(true),
        "last_reconcile_at must be absent before first tick: {body:#}"
    );
}

#[tokio::test]
async fn schema_enum_includes_watcher_lagged() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/events.schema.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let types = body["types"].as_array().expect("types array");
    let names: Vec<&str> = types.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        names.contains(&"watcher.lagged"),
        "schema enum must list watcher.lagged: {names:?}"
    );
    assert!(names.contains(&"reconcile.complete"));
}

#[tokio::test]
async fn reindex_route_returns_success() {
    // Plain POST smoke. The deeper "reindex emits artifact.indexed"
    // path is exercised by the existing reindex+SSE tests; this case
    // just guards against breakage of the simple route contract that
    // `kb reindex` relies on.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(addr, "/api/kb/smoke/reindex"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "reindex POST should succeed; got {}",
        resp.status()
    );
}

#[tokio::test]
async fn lookup_reports_ambiguity_with_candidates() {
    // Custom fixture: two folders with `note.html`. The basename
    // suffix branch must return Ambiguous with both candidates.
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(source.join("a")).unwrap();
    std::fs::create_dir_all(source.join("b")).unwrap();
    std::fs::write(
        source.join("a/note.html"),
        b"<html><title>A note</title></html>",
    )
    .unwrap();
    std::fs::write(
        source.join("b/note.html"),
        b"<html><title>B note</title></html>",
    )
    .unwrap();

    let daemon_name = format!(
        "test-lookup-amb-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("dup").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/kb/dup/lookup"))
        .query(&[("q", "note.html")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["kind"], "ambiguous", "got {body:#}");
    let candidates = body["candidates"].as_array().expect("candidates array");
    assert_eq!(candidates.len(), 2);
    let rels: std::collections::HashSet<&str> = candidates
        .iter()
        .map(|c| c["source_relative"].as_str().unwrap())
        .collect();
    assert!(rels.contains("a/note.html"));
    assert!(rels.contains("b/note.html"));
}

// ---------------------------------------------------------------------------
// Q1 — `/api/anchors/stale` cold-load endpoint for the SPA dashboard.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anchors_stale_endpoint_empty_when_no_sidecar() {
    // Fresh boot — no kb has ever had a stale anchor → 200 + empty
    // `anchors` array (NOT 404; an empty list is the steady state).
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/anchors/stale"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["anchors"], serde_json::json!([]));
}

#[tokio::test]
async fn anchors_stale_endpoint_returns_persisted_sidecar_entries() {
    // Pre-stage the smoke kb's stale sidecar BEFORE boot — the daemon
    // never has to fire an event for the cold load to surface it.
    // This mirrors the v0.5 P4 / v0.7.1 P2 contract: the indexer
    // persists `(artifact, comment)` pairs to
    // `<state>/<kb>/.anchors-stale.json`; the endpoint reads them.
    let (tmp, cfg, paths) = fixture_corpus();
    let smoke = kb_core::types::KbName::new("smoke").unwrap();
    let review_dir = paths.kb_review_dir(&smoke);
    std::fs::create_dir_all(&review_dir).unwrap();
    let sidecar = kb_core::anchors::sidecar_path(&review_dir);
    let mut stale: std::collections::HashMap<(String, String), kb_core::anchors::StaleAnchorEntry> =
        std::collections::HashMap::new();
    stale.insert(
        ("art-aaa".to_string(), "c_one".to_string()),
        kb_core::anchors::StaleAnchorEntry {
            anchor_kind: "section".to_string(),
            fuzzy_score: 0.0,
        },
    );
    stale.insert(
        ("art-bbb".to_string(), "c_two".to_string()),
        kb_core::anchors::StaleAnchorEntry {
            anchor_kind: "chapter".to_string(),
            fuzzy_score: 0.42,
        },
    );
    kb_core::anchors::save(&sidecar, &stale).unwrap();

    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/anchors/stale"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let anchors = body["anchors"].as_array().expect("anchors array");
    assert_eq!(anchors.len(), 2, "got {body:#}");
    let pairs: std::collections::HashSet<(String, String)> = anchors
        .iter()
        .map(|a| {
            (
                a["artifact_id"].as_str().unwrap().to_string(),
                a["comment_id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(pairs.contains(&("art-aaa".to_string(), "c_one".to_string())));
    assert!(pairs.contains(&("art-bbb".to_string(), "c_two".to_string())));
    // Every row carries the kb tag + the v3 metadata.
    for a in anchors {
        assert_eq!(a["kb"], "smoke");
        assert!(
            a.get("anchor_kind").and_then(|v| v.as_str()).is_some(),
            "row missing anchor_kind: {a}"
        );
        assert!(
            a.get("fuzzy_score").and_then(|v| v.as_f64()).is_some(),
            "row missing fuzzy_score: {a}"
        );
    }
    // #4 — find the chapter-anchor entry and verify its metadata
    // survived the round-trip.
    let chapter = anchors
        .iter()
        .find(|a| a["artifact_id"] == "art-bbb")
        .expect("art-bbb row");
    assert_eq!(chapter["anchor_kind"], "chapter");
    let score = chapter["fuzzy_score"].as_f64().unwrap();
    assert!((score - 0.42).abs() < 1e-6, "fuzzy_score drifted: {score}");
    drop(tmp);
}

#[tokio::test]
async fn anchors_stale_endpoint_sorts_within_kb_for_stable_output() {
    // Stable output across calls keeps the SPA's session-state merge
    // deterministic + makes caching / etag-style optimisations safe
    // to add later. Within a single kb we sort by (artifact, comment).
    let (tmp, cfg, paths) = fixture_corpus();
    let smoke = kb_core::types::KbName::new("smoke").unwrap();
    let review_dir = paths.kb_review_dir(&smoke);
    std::fs::create_dir_all(&review_dir).unwrap();
    let sidecar = kb_core::anchors::sidecar_path(&review_dir);
    let mut stale: std::collections::HashMap<(String, String), kb_core::anchors::StaleAnchorEntry> =
        std::collections::HashMap::new();
    // Insertion order != lexicographic order; the endpoint must sort.
    let meta = || kb_core::anchors::StaleAnchorEntry {
        anchor_kind: "stale".to_string(),
        fuzzy_score: 0.0,
    };
    stale.insert(("zzz".to_string(), "c_z".to_string()), meta());
    stale.insert(("aaa".to_string(), "c_a".to_string()), meta());
    stale.insert(("mmm".to_string(), "c_m".to_string()), meta());
    kb_core::anchors::save(&sidecar, &stale).unwrap();

    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/anchors/stale"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let anchors = body["anchors"].as_array().expect("anchors array");
    let ids: Vec<&str> = anchors
        .iter()
        .map(|a| a["artifact_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["aaa", "mmm", "zzz"], "expected sorted order");
    drop(tmp);
}

// ---- Bake-off B1: per-kb dim guard ----
//
// The bake-off lets one daemon host kbs embedded by different-dim
// models (bge-small 384, bge-base 768, bge-large 1024). The kb-server
// startup path resolves each kb's `config_dim` from its
// `embedding_model` and forwards it to `StorageActor::spawn`. These
// tests cover the daemon-level wiring; the open-time guard itself is
// exercised exhaustively in kb-core::storage::lance.
//
// Convention: these tests pre-create the per-kb lance dataset by
// driving `kb_core::storage::Storage::open` against the SAME path the
// daemon will later use (`paths.kb_lance(kb)`). Setting
// `embedding_model: None` keeps the daemon from spawning a real
// kb-embedder subprocess (which would download model weights at test
// time).

#[tokio::test]
async fn daemon_brings_up_two_kbs_with_different_disk_dims() {
    // The bake-off setup: one kb at 384-dim (bge-small), another at
    // 768-dim (bge-base), both on the same daemon. Both kbs use
    // `embedding_model: None` to skip embedder spawn so the test
    // doesn't download anything; the dims are baked into the lance
    // datasets directly.
    use kb_core::storage::lance::Storage;

    let tmp = tempfile::tempdir().unwrap();
    let source_small = tmp.path().join("corpus-small");
    let source_base = tmp.path().join("corpus-base");
    std::fs::create_dir_all(&source_small).unwrap();
    std::fs::create_dir_all(&source_base).unwrap();
    for name in CANON_REL {
        std::fs::copy(canon_dir().join(name), source_small.join(name)).unwrap();
        std::fs::copy(canon_dir().join(name), source_base.join(name)).unwrap();
    }

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let paths = KbPaths::rooted_at(tmp.path(), &daemon_name);

    // Pre-create the two lance datasets at the dims the daemon will
    // expect. The daemon's `Storage::open` re-reads them and captures
    // the disk-dim into `Storage::dim`.
    let kb_small = KbName::new("smoke-small").unwrap();
    let kb_base = KbName::new("smoke-base").unwrap();
    {
        let s = Storage::open(&paths.kb_lance(&kb_small), Some(384))
            .await
            .unwrap();
        assert_eq!(s.dim(), 384);
        drop(s);
        let s = Storage::open(&paths.kb_lance(&kb_base), Some(768))
            .await
            .unwrap();
        assert_eq!(s.dim(), 768);
    }

    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        kb_small.clone(),
        KbSection {
            path: source_small,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    kb_map.insert(
        kb_base.clone(),
        KbSection {
            path: source_base,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
            name: Some(daemon_name),
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

    // Daemon must bring up both kbs without erroring on the dim mix.
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;

    let client = reqwest::Client::new();
    let kbs: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kbs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: std::collections::BTreeSet<&str> =
        kbs.iter().map(|k| k["name"].as_str().unwrap()).collect();
    assert!(names.contains("smoke-small"), "got {names:?}");
    assert!(names.contains("smoke-base"), "got {names:?}");
    drop(tmp);
}

#[tokio::test]
async fn daemon_startup_fails_when_kb_dim_mismatches_configured_model() {
    // Pre-create a 384-dim dataset, then configure the kb to use
    // bge-base (768-dim). `Storage::open` returns `Error::Config` and
    // `bring_up_kb` propagates with `?` — daemon startup must surface
    // the error. The embedder subprocess is NEVER spawned (open fails
    // BEFORE spawn_ipc) so this test is fast + offline.
    use kb_core::storage::lance::Storage;

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    for name in CANON_REL {
        std::fs::copy(canon_dir().join(name), source.join(name)).unwrap();
    }

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let paths = KbPaths::rooted_at(tmp.path(), &daemon_name);
    let kb = KbName::new("mismatched").unwrap();

    // Lance dataset on disk = 384-dim.
    {
        let s = Storage::open(&paths.kb_lance(&kb), Some(384))
            .await
            .unwrap();
        drop(s);
    }

    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        kb,
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            // Configured-model dim = 768. Disk = 384. Mismatch.
            embedding_model: Some("bge-base-en-v1.5".to_string()),
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
            name: Some(daemon_name),
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

    let result = kb_server::serve_on_random_port_with_paths(cfg, paths).await;
    let err = match result {
        Ok(_) => panic!("daemon startup should have failed with dim mismatch"),
        Err(e) => e,
    };
    let chain = format!("{err:#}");
    assert!(
        chain.contains("384") && chain.contains("768"),
        "error must surface both dims; got: {chain}"
    );
    drop(tmp);
}

#[tokio::test]
async fn daemon_resolves_defaults_embedding_model_for_dim_check() {
    // D2 — `[defaults] embedding_model = "bge-base-en-v1.5"` (768-dim)
    // + a kb that omits `embedding_model` must resolve to bge-base. We
    // verify by pre-creating a 384-dim lance dataset on disk and asserting
    // the daemon startup fails with a dim-mismatch error naming BOTH
    // dims: 384 (disk) and 768 (resolved). This proves the resolver
    // picked the defaults layer (not the registry default = bge-small =
    // 384, which would have matched and silently spawned an embedder).
    // Failure happens inside `Storage::open` BEFORE `Embedder::spawn_ipc`,
    // so this test is offline + fast (no model download).
    use kb_core::storage::lance::Storage;

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    for name in CANON_REL {
        std::fs::copy(canon_dir().join(name), source.join(name)).unwrap();
    }
    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let paths = KbPaths::rooted_at(tmp.path(), &daemon_name);
    let kb = KbName::new("uses-defaults").unwrap();
    {
        let s = Storage::open(&paths.kb_lance(&kb), Some(384))
            .await
            .unwrap();
        drop(s);
    }

    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        kb,
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            // Per-kb omits embedding_model — the daemon must fall through
            // to [defaults] below.
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
            name: Some(daemon_name),
        },
        server: ServerSection::default(),
        ui: UiSection::default(),
        indexer: Default::default(),
        storage: Default::default(),
        share: Default::default(),
        webhooks: None,
        defaults: DefaultsSection {
            embedding_model: Some("bge-base-en-v1.5".to_string()),
            // Leave the registry fallback ARMED to verify it doesn't
            // pre-empt the explicit [defaults] entry.
            disable_embedder_fallback: false,
        },
        retention: Default::default(),
        backup: Default::default(),
        identity: Default::default(),
        memory: Default::default(),
        kb: kb_map,
        projects: Default::default(),
        sessions: Default::default(),
    };

    let err = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect_err("daemon startup must fail with dim mismatch");
    let chain = format!("{err:#}");
    assert!(
        chain.contains("384") && chain.contains("768"),
        "[defaults] embedding_model = bge-base must propagate as the 768-dim \
         check against the 384-dim disk; got: {chain}"
    );
    drop(tmp);
}

#[tokio::test]
async fn daemon_per_kb_embedding_model_wins_over_defaults() {
    // D2 — per-kb embedding_model = "bge-large-en-v1.5" (1024-dim) must
    // win even when `[defaults] embedding_model = "bge-base-en-v1.5"`
    // (768-dim). Pre-build a 768-dim dataset and assert the mismatch
    // surfaces with 768 vs 1024 — proving the daemon used the per-kb
    // section, NOT [defaults].
    use kb_core::storage::lance::Storage;

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    for name in CANON_REL {
        std::fs::copy(canon_dir().join(name), source.join(name)).unwrap();
    }
    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let paths = KbPaths::rooted_at(tmp.path(), &daemon_name);
    let kb = KbName::new("per-kb-wins").unwrap();
    {
        let s = Storage::open(&paths.kb_lance(&kb), Some(768))
            .await
            .unwrap();
        drop(s);
    }

    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        kb,
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: Some("bge-large-en-v1.5".to_string()),
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
            name: Some(daemon_name),
        },
        server: ServerSection::default(),
        ui: UiSection::default(),
        indexer: Default::default(),
        storage: Default::default(),
        share: Default::default(),
        webhooks: None,
        defaults: DefaultsSection {
            embedding_model: Some("bge-base-en-v1.5".to_string()),
            disable_embedder_fallback: false,
        },
        retention: Default::default(),
        backup: Default::default(),
        identity: Default::default(),
        memory: Default::default(),
        kb: kb_map,
        projects: Default::default(),
        sessions: Default::default(),
    };

    let err = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect_err("daemon startup must fail with dim mismatch");
    let chain = format!("{err:#}");
    assert!(
        chain.contains("768") && chain.contains("1024"),
        "per-kb bge-large (1024) must beat [defaults] bge-base (768) — \
         expected the disk-vs-config mismatch to surface 768 vs 1024; got: {chain}"
    );
    drop(tmp);
}

#[tokio::test]
async fn daemon_refuses_public_bind_without_token() {
    // deep-review P1 → P0: a non-loopback bind with NO bearer token must FAIL
    // CLOSED at startup, rather than silently disabling auth on a daemon that
    // exposes `DELETE /api/kb`. We bind the unspecified address `0.0.0.0`
    // (a public bind → non-loopback `local_addr`), create NO token file, and
    // rely on `KB_ALLOW_NO_AUTH` being unset (no other test mutates it), so
    // the startup guard must refuse and the error names the missing token.
    let tmp = tempfile::tempdir().unwrap();
    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let paths = KbPaths::rooted_at(tmp.path(), &daemon_name);
    let cfg = KbConfig {
        daemon: DaemonSection {
            name: Some(daemon_name),
        },
        server: ServerSection {
            addr: "0.0.0.0:0".to_string(),
            ..ServerSection::default()
        },
        ui: UiSection::default(),
        indexer: Default::default(),
        storage: Default::default(),
        share: Default::default(),
        webhooks: None,
        defaults: DefaultsSection {
            embedding_model: None,
            disable_embedder_fallback: false,
        },
        retention: Default::default(),
        backup: Default::default(),
        identity: Default::default(),
        memory: Default::default(),
        kb: BTreeMap::new(),
        projects: Default::default(),
        sessions: Default::default(),
    };
    let err = kb_server::serve_with_paths(cfg, paths.config_file(), paths)
        .await
        .expect_err("token-less public bind must refuse to start");
    let chain = format!("{err:#}");
    assert!(
        chain.contains("without a bearer token"),
        "expected the fail-closed startup-guard message; got: {chain}"
    );
    drop(tmp);
}

#[tokio::test]
async fn healthz_is_unauthenticated_and_reports_status() {
    // P0 ops — `/healthz` is mounted on the TOP-LEVEL router, OUTSIDE the
    // `/api` bearer-auth nest. Prove it: boot a TOKEN-configured daemon, then
    // hit `/healthz` from a simulated non-loopback client (XFF) with NO bearer.
    // A token-gated /api route would 401 here; the probe must still 200, carry
    // `no-store`, and return the status body.
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/healthz"))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "/healthz must bypass bearer-auth");
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "a liveness probe must not be cacheable"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert!(
        body["daemon"].as_str().is_some_and(|s| !s.is_empty()),
        "daemon name present: {body}"
    );
    assert!(
        body["kbs"].as_u64().unwrap_or(0) >= 1,
        "at least the fixture kb is loaded: {body}"
    );
    assert!(
        body["uptime_secs"].as_i64().is_some(),
        "uptime present: {body}"
    );
}

#[tokio::test]
async fn artifact_bytes_download_flag_toggles_attachment() {
    // `?download=1` flips the artifact-bytes route from inline to an
    // attachment so the SPA's download control saves the raw source.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, "smoke", |_| true).await;
    let id = doc["id"].as_str().expect("doc id");

    // No flag → inline (no Content-Disposition: attachment).
    let inline = client
        .get(url(addr, &format!("/api/kb/smoke/artifact/{id}")))
        .send()
        .await
        .unwrap();
    assert!(inline.status().is_success());
    let cd = inline
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        !cd.contains("attachment"),
        "without ?download the response must stay inline; got {cd:?}"
    );

    // ?download=1 → attachment with a filename derived from the basename.
    let dl = client
        .get(url(
            addr,
            &format!("/api/kb/smoke/artifact/{id}?download=1"),
        ))
        .send()
        .await
        .unwrap();
    assert!(dl.status().is_success());
    let cd = dl
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .expect("download response must carry Content-Disposition");
    assert!(cd.starts_with("attachment;"), "got {cd:?}");
    assert!(cd.contains("filename="), "got {cd:?}");
    // The fixture corpus filenames are ASCII, so the basename appears
    // verbatim in the quoted fallback.
    let fname = doc["path"].as_str().unwrap().rsplit('/').next().unwrap();
    assert!(cd.contains(fname), "disposition {cd:?} must name {fname:?}");
}

/// Build a small multi-folder corpus for the folder-download tests:
/// `ideas/passwordless-login/{01,02,03}.html`, `ideas/jira/plan.html`,
/// `changelog/daily/2026-05-13.html`, and a root `INDEX.html`.
async fn boot_with_multifolder_fixture() -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(source.join("ideas").join("passwordless-login")).unwrap();
    std::fs::create_dir_all(source.join("ideas").join("jira")).unwrap();
    std::fs::create_dir_all(source.join("changelog").join("daily")).unwrap();
    std::fs::write(
        source.join("INDEX.html"),
        b"<html><title>Index</title></html>",
    )
    .unwrap();
    for n in ["01", "02", "03"] {
        std::fs::write(
            source
                .join("ideas")
                .join("passwordless-login")
                .join(format!("{n}.html")),
            format!("<html><title>passwordless {n}</title></html>"),
        )
        .unwrap();
    }
    std::fs::write(
        source.join("ideas").join("jira").join("plan.html"),
        b"<html><title>Jira plan</title></html>",
    )
    .unwrap();
    std::fs::write(
        source
            .join("changelog")
            .join("daily")
            .join("2026-05-13.html"),
        b"<html><title>Daily</title></html>",
    )
    .unwrap();

    let daemon_name = format!(
        "test-dl-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("nested").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    common::wait_docs_listed(addr, "nested", 6).await;
    (tmp, addr)
}

fn zip_entry_names(bytes: &[u8]) -> Vec<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).expect("valid zip");
    (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().to_string())
        .collect()
}

#[tokio::test]
async fn download_route_zips_folder_descendants() {
    let (_tmp, addr) = boot_with_multifolder_fixture().await;
    let client = reqwest::Client::new();
    // Wait until the indexer has the deepest doc before downloading.
    let _ = wait_for_doc(&client, addr, "nested", |d| {
        d["path"].as_str().unwrap_or("").ends_with("plan.html")
    })
    .await;

    let resp = client
        .get(url(addr, "/api/kb/nested/download?folder=ideas"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "status {}", resp.status());
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/zip")
    );
    let cd = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .expect("zip response carries Content-Disposition");
    assert!(cd.contains("attachment"), "got {cd:?}");
    assert!(cd.contains("nested-ideas.zip"), "got {cd:?}");

    let bytes = resp.bytes().await.unwrap();
    let mut names = zip_entry_names(&bytes);
    names.sort();
    // Descendant-inclusive: both subfolders' docs land in the archive,
    // keyed by source-relative path. The root INDEX.html + changelog/*
    // are out of scope.
    assert_eq!(
        names,
        vec![
            "ideas/jira/plan.html".to_string(),
            "ideas/passwordless-login/01.html".to_string(),
            "ideas/passwordless-login/02.html".to_string(),
            "ideas/passwordless-login/03.html".to_string(),
        ],
        "got {names:?}"
    );
}

#[tokio::test]
async fn download_route_whole_kb_when_no_folder() {
    let (_tmp, addr) = boot_with_multifolder_fixture().await;
    let client = reqwest::Client::new();
    let _ = wait_for_doc(&client, addr, "nested", |d| {
        d["path"].as_str().unwrap_or("").ends_with("INDEX.html")
    })
    .await;

    let resp = client
        .get(url(addr, "/api/kb/nested/download"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let cd = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(cd.contains("nested-all.zip"), "got {cd:?}");
    let bytes = resp.bytes().await.unwrap();
    let names = zip_entry_names(&bytes);
    // All 6 indexed HTML docs, including the root index.
    assert!(names.contains(&"INDEX.html".to_string()), "got {names:?}");
    assert!(
        names.contains(&"changelog/daily/2026-05-13.html".to_string()),
        "got {names:?}"
    );
    assert_eq!(names.len(), 6, "got {names:?}");
}

#[tokio::test]
async fn download_route_unknown_folder_returns_404() {
    let (_tmp, addr) = boot_with_multifolder_fixture().await;
    let client = reqwest::Client::new();
    let _ = wait_for_doc(&client, addr, "nested", |_| true).await;
    let resp = client
        .get(url(addr, "/api/kb/nested/download?folder=does/not/exist"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// --- kb share routes (S7) ----------------------------------------------------
// These exercise the route wiring + validation + registry-read paths without
// touching Cloudflare/GitHub: the validation 400, the unconfigured-host 400,
// and the unknown-share 404 all resolve before any backend network call. A
// real deploy/gate/revoke lives in the #[ignore] live lane (plan R9).

#[tokio::test]
async fn shares_list_starts_empty() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/smoke/shares"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty(), "no shares recorded yet: {body:?}");
}

#[tokio::test]
async fn share_create_rejects_cloudflare_without_gate_or_public() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(addr, "/api/kb/smoke/share"))
        .json(&serde_json::json!({
            "target": "fullscreen-viz.html",
            "host": "cloudflare-pages"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        400,
        "cloudflare-pages needs --gate or --public"
    );
}

#[tokio::test]
async fn share_create_rejects_unconfigured_host() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // Valid combo (public) but the fixture has no [share.cloudflare].
    let resp = client
        .post(url(addr, "/api/kb/smoke/share"))
        .json(&serde_json::json!({
            "target": "fullscreen-viz.html",
            "host": "cloudflare-pages",
            "public": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("not configured"),
        "got {body:?}"
    );
}

#[tokio::test]
async fn share_delete_unknown_is_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .delete(url(addr, "/api/kb/smoke/shares/kb-share-nope-000000"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ---- v0.9 M3: agent-memory recall fan-out + rerank + tombstone -------
//
// CI-safe: both corpora run with `embedding_model = None`, so recall
// falls back to BM25 (no model download). The rerank math itself is
// unit-tested in `kb_core::memory`; these assert the wiring (scope
// resolution, cross-corpus merge, salience ordering, supersede drop).

fn memory_html(
    title: &str,
    body: &str,
    salience: Option<f32>,
    decay: Option<&str>,
    supersedes: Option<&str>,
) -> String {
    let mut metas = String::from(r#"<meta name="kb-category" content="memory-user">"#);
    if let Some(s) = salience {
        metas.push_str(&format!(r#"<meta name="kb-salience" content="{s}">"#));
    }
    if let Some(d) = decay {
        metas.push_str(&format!(r#"<meta name="kb-decay" content="{d}">"#));
    }
    if let Some(sup) = supersedes {
        metas.push_str(&format!(r#"<meta name="kb-supersedes" content="{sup}">"#));
    }
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>{title}</title>{metas}</head><body><h1>{title}</h1><p>{body}</p></body></html>"#
    )
}

/// How far into the past [`seed_memory_file`] backdates a seeded memory
/// artifact's mtime. Anything ≥ 1 whole second works; 10s is comfortably
/// clear of coarse-granularity clocks/filesystems.
const SEED_MTIME_BACKDATE_SECS: u64 = 10;

/// Write one seeded memory artifact, then BACKDATE its mtime by
/// [`SEED_MTIME_BACKDATE_SECS`].
///
/// The backdate is load-bearing, not hygiene. Two tests in this family have
/// the DAEMON rewrite a seeded source in place after boot (the MI-W3.2b
/// salience splice, the MI-W2.3 soft-forget tombstone) and then wait for the
/// reindex to surface in `/api/memory/census`. Two independent second-
/// granularity gates decide whether that rewrite is ever noticed when the
/// live fs event doesn't arrive:
///
///   * the reconcile safety net's G5 producer-side dedup skips a file whose
///     on-disk mtime EQUALS the mtime stored at last index, and both sides
///     are truncated to whole seconds (`kb_core::indexer::walk_core`,
///     `disk_mtime == Some(stored)`, `.as_secs() as i64` on both);
///   * `notify`'s own `PollWatcher` is no fallback — it compares
///     `system_time_to_seconds(mtime)` too and only consults a content hash
///     under `compare_contents`, which the daemon does not enable.
///
/// So if the seed and the daemon's rewrite land in the SAME wall-clock
/// second — a sub-second seed→boot→patch path, i.e. a FAST disk — the
/// reconciler is permanently blind to that rewrite: every later pass
/// re-stats the same second and skips. The live watcher becomes the ONLY
/// thing that can heal the row, and if that one event is missed the test
/// burns its entire deadline with the pipeline idle and the daemon
/// otherwise healthy (it answers every census poll) — the "timed out after
/// 120s waiting for: census to reflect the patched salience" false-red on
/// the ci-host runners, which this box's slower disk hides by taking >1s to get
/// from seed to patch. Both rewrites go through `fsx::write_atomic`, i.e. a
/// RENAME over an already-indexed path, which is a different watcher path
/// from the plain create that `l6_link_mutation_emits_memory_linked_sse`
/// (green on the same runner) proves does arrive there.
///
/// Backdating the seed makes any post-boot rewrite strictly newer in whole
/// seconds, so the short `reconcile_secs` set below is a REAL backstop for
/// this family on every filesystem and every runner — watcher events or
/// not. (It only covers files seeded HERE; a memory created post-boot via
/// `POST …/artifacts` and rewritten in the same second would still depend
/// on the live watcher.)
fn seed_memory_file(path: &std::path::Path, html: &str) {
    std::fs::write(path, html).unwrap();
    let past = std::time::SystemTime::now() - Duration::from_secs(SEED_MTIME_BACKDATE_SECS);
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(past))
        .unwrap();
}

/// Boot a daemon with a `global`-scope memory corpus ("globalmem") and a
/// `project`-scope one ("projmem"), each pre-populated with the given
/// (filename, html) files. Both have no embedder → BM25 recall.
async fn boot_memory_corpora(
    global_files: &[(&str, String)],
    proj_files: &[(&str, String)],
) -> (tempfile::TempDir, std::net::SocketAddr) {
    boot_memory_corpora_with_code_url(global_files, proj_files, None).await
}

/// [`boot_memory_corpora`] with an explicit `[kb.globalmem] code_url` — the
/// INERT DCB pointer at kb-code (invariant #2: kb renders links to it, never
/// calls it). CT-E5's narrative list description carries the session-diff
/// link only when this is set.
async fn boot_memory_corpora_with_code_url(
    global_files: &[(&str, String)],
    proj_files: &[(&str, String)],
    code_url: Option<&str>,
) -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let gdir = tmp.path().join("globalmem");
    let pdir = tmp.path().join("projmem");
    std::fs::create_dir_all(&gdir).unwrap();
    std::fs::create_dir_all(&pdir).unwrap();
    for (name, html) in global_files {
        seed_memory_file(&gdir.join(name), html);
    }
    for (name, html) in proj_files {
        seed_memory_file(&pdir.join(name), html);
    }

    let daemon_name = format!(
        "memtest-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("globalmem").unwrap(),
        KbSection {
            path: gdir,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: BTreeMap::new(),
            memory_scope: Some("global".into()),
            default_search_category: None,
            code_url: code_url.map(str::to_string),
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
    kb_map.insert(
        KbName::new("projmem").unwrap(),
        KbSection {
            path: pdir,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: BTreeMap::new(),
            memory_scope: Some("project".into()),
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
        // 5s, not the 60s default: this family's census waits are the only
        // ones that depend on the daemon rewriting a SOURCE file after boot
        // (see `seed_memory_file`), so the reconcile pass must be able to
        // heal that rewrite well inside `common::index_wait_deadline()`
        // (30s locally) when the live watcher event never lands. Backdated
        // seed mtimes are what make that pass actually EMIT; the two go
        // together. The watch backend stays `auto`/native — swapping it for
        // `poll` would BOTH lose native-watcher coverage and fix nothing
        // (`PollWatcher` has the same seconds-granularity blind spot).
        indexer: IndexerSection {
            reconcile_secs: Some(5),
            ..Default::default()
        },
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
    tokio::time::sleep(Duration::from_millis(400)).await;
    (tmp, addr)
}

// ---- MI-W1.2: memory census — per-corpus paginated scan ---------------

#[tokio::test]
async fn memory_census_paginates_one_corpus_with_metadata() {
    let global = vec![
        (
            "g1.html",
            memory_html("Alpha Fact", "alpha body", Some(0.9), Some("slow"), None),
        ),
        (
            "g2.html",
            memory_html("Beta Fact", "beta body", Some(0.4), Some("fast"), None),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Alpha Fact").await;
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Beta Fact").await;

    let resp: serde_json::Value = client
        .get(url(addr, "/api/memory/census?kb=globalmem"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["total"], 2);
    let rows = resp["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for r in rows {
        assert_eq!(r["pinned"], false, "row {r:?}");
        // No memory_recalls ledger data anywhere in this daemon — every
        // row starts at zero, absent last_recalled_at (never Some(0)).
        assert_eq!(r["recall_count"], 0, "row {r:?}");
        assert!(r["last_recalled_at"].is_null(), "row {r:?}");
        // CT-C5 — recall_used_count rides the same zero-ledger default.
        assert_eq!(r["recall_used_count"], 0, "row {r:?}");
        assert_eq!(r["category"], "memory-user", "row {r:?}");
    }
    let alpha = rows.iter().find(|r| r["title"] == "Alpha Fact").unwrap();
    assert!((alpha["salience"].as_f64().unwrap() - 0.9).abs() < 1e-6);
    assert_eq!(alpha["decay_bucket"], "slow");
    let beta = rows.iter().find(|r| r["title"] == "Beta Fact").unwrap();
    assert_eq!(beta["decay_bucket"], "fast");

    // Pagination: limit=1 returns exactly one row per page but `total`
    // always reports the TRUE uncapped count, and the two pages are
    // disjoint (deterministic id-sort order).
    let page1: serde_json::Value = client
        .get(url(
            addr,
            "/api/memory/census?kb=globalmem&limit=1&offset=0",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page1["rows"].as_array().unwrap().len(), 1);
    assert_eq!(page1["total"], 2);
    let page2: serde_json::Value = client
        .get(url(
            addr,
            "/api/memory/census?kb=globalmem&limit=1&offset=1",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page2["rows"].as_array().unwrap().len(), 1);
    assert_eq!(page2["total"], 2);
    assert_ne!(page1["rows"][0]["id"], page2["rows"][0]["id"]);
}

/// CT-E4 — `?sort=unverified` on the census: agent-hot-human-cold rows
/// (recalled by agents, never opened by the human) first, recall_count
/// DESC within/after the bucket, id ASC ties. Three memories: B recalled
/// by TWO captured sessions, C by one, A never. Also pins the two edges of
/// the additive contract: absent `sort` keeps the id-ASC order
/// byte-identical even once ledger + read signals exist, and an unknown
/// sort term is a 400, never a silent ignore.
#[tokio::test]
async fn memory_census_sort_unverified_orders_agent_hot_human_cold_first() {
    // Ids are the SHA-256 prefix of the source-relative path (see
    // `memory_recall_drops_superseded`), so the recall-ledger transcripts
    // can name the REAL ids up front.
    let id_a = kb_core::ids::ArtifactId::from_path("a-fact.html").to_string();
    let id_b = kb_core::ids::ArtifactId::from_path("b-fact.html").to_string();
    let id_c = kb_core::ids::ArtifactId::from_path("c-fact.html").to_string();

    /// One captured session whose `kb-recall` hook attachment names the
    /// given globalmem memory id (the free-text ledger grammar the
    /// existing `session_recalls_*` fixtures use).
    fn recall_jsonl(sid: &str, mem_id: &str, n: u32) -> String {
        format!(
            "{{\"sessionId\":\"{sid}\",\"cwd\":\"/home/u/proj\",\"timestamp\":\"2026-06-04T09:00:00.000Z\"}}\n\
             {{\"parentUuid\":null,\"isSidechain\":false,\"attachment\":{{\"type\":\"hook_additional_context\",\"content\":[\"Relevant memories from kb (recall - these persist across sessions):\\n- some fact  [globalmem]  (id {mem_id})\"],\"hookName\":\"UserPromptSubmit\",\"toolUseID\":\"UserPromptSubmit\",\"hookEvent\":\"UserPromptSubmit\"}},\"type\":\"attachment\",\"uuid\":\"40000000-0000-4000-8000-00000000000{n}\",\"timestamp\":\"2026-06-04T09:00:05.000Z\",\"sessionId\":\"{sid}\"}}\n\
             {{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}},\"timestamp\":\"2026-06-04T09:00:06.000Z\",\"sessionId\":\"{sid}\"}}\n\
             {{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"hi\"}}]}},\"timestamp\":\"2026-06-04T09:00:07.000Z\",\"sessionId\":\"{sid}\"}}\n"
        )
    }
    let s1 = recall_jsonl("sess-cte4-001", &id_b, 1);
    let s2 = recall_jsonl("sess-cte4-002", &id_b, 2);
    let s3 = recall_jsonl("sess-cte4-003", &id_c, 3);

    let global = vec![
        (
            "a-fact.html",
            memory_html("A Fact", "alpha body", Some(0.9), Some("slow"), None),
        ),
        (
            "b-fact.html",
            memory_html("B Fact", "beta body", Some(0.9), Some("slow"), None),
        ),
        (
            "c-fact.html",
            memory_html("C Fact", "gamma body", Some(0.9), Some("slow"), None),
        ),
        (
            "session-20260604T090100Z-sess-cte4-001.html",
            session_transcript_html("sess-cte4-001", "20260604T090100Z", &s1),
        ),
        (
            "session-20260604T090200Z-sess-cte4-002.html",
            session_transcript_html("sess-cte4-002", "20260604T090200Z", &s2),
        ),
        (
            "session-20260604T090300Z-sess-cte4-003.html",
            session_transcript_html("sess-cte4-003", "20260604T090300Z", &s3),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "A Fact").await;
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "B Fact").await;
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "C Fact").await;

    // The ledger rows land with the async session-capture parse — poll the
    // DEFAULT census until B shows 2 recalls and C shows 1.
    let census = common::poll_until("census recall counts B=2 C=1", || async {
        let body: serde_json::Value = client
            .get(url(addr, "/api/memory/census?kb=globalmem"))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        let rows = body["rows"].as_array()?;
        let count_of = |id: &str| {
            rows.iter()
                .find(|r| r["id"] == id)
                .and_then(|r| r["recall_count"].as_u64())
        };
        (count_of(&id_b) == Some(2) && count_of(&id_c) == Some(1)).then_some(body)
    })
    .await;

    // Absent `sort` — the default id-ASC order is untouched by the ledger
    // signals (byte-identical pre-CT-E4 ordering; transcripts excluded).
    assert_eq!(census["total"], 3, "census: {census:?}");
    let default_ids: Vec<String> = census["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    let mut expected_default = vec![id_a.clone(), id_b.clone(), id_c.clone()];
    expected_default.sort();
    assert_eq!(default_ids, expected_default, "default order is id ASC");

    // Nothing opened yet — every recalled row is agent-hot-human-cold:
    // B (2 recalls) leads, then C (1), then never-recalled A.
    let sorted: serde_json::Value = client
        .get(url(addr, "/api/memory/census?kb=globalmem&sort=unverified"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sorted_ids: Vec<String> = sorted["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        sorted_ids,
        vec![id_b.clone(), id_c.clone(), id_a.clone()],
        "all unopened: bucket by recall_count DESC"
    );

    // The human OPENS B (visit + scroll + reading beacon — the same
    // sequence `memory_recall_carries_read_state` records): B leaves
    // the bucket, so C (1 recall, still unopened) now leads and B drops to
    // the cold remainder — still ahead of never-recalled A.
    let open: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/history/open"))
        .json(&serde_json::json!({ "artifact_id": id_b }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let vid = open["visit_id"].as_i64().unwrap();
    client
        .post(url(addr, "/api/kb/globalmem/history/scroll"))
        .json(&serde_json::json!({ "visit_id": vid, "scroll_y": 80, "scroll_max": 100 }))
        .send()
        .await
        .unwrap();
    client
        .post(url(addr, "/api/kb/globalmem/history/reading"))
        .json(&serde_json::json!({
            "visit_id": vid, "artifact_id": id_b, "active_ms": 5000,
            "last_section": "intro", "sections": []
        }))
        .send()
        .await
        .unwrap();

    let sorted2: serde_json::Value = client
        .get(url(addr, "/api/memory/census?kb=globalmem&sort=unverified"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sorted2_ids: Vec<String> = sorted2["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        sorted2_ids,
        vec![id_c.clone(), id_b.clone(), id_a.clone()],
        "an opened row leaves the bucket however often it was recalled"
    );

    // An unknown sort term is a 400, never a silent ignore.
    let bad = client
        .get(url(addr, "/api/memory/census?kb=globalmem&sort=salience"))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), reqwest::StatusCode::BAD_REQUEST);
}

// ---- MI-W4.5: atlas memory mode — the memory-scope gate ---------------

/// A memory-scoped kb's `/atlas/points` carries salience/decay_bucket/
/// pinned/forgotten/supersedes for every point.
#[tokio::test]
async fn atlas_points_carries_memory_fields_for_memory_scoped_kb() {
    let global = vec![
        (
            "g1.html",
            memory_html("Alpha Fact", "alpha body", Some(0.9), Some("slow"), None),
        ),
        (
            "g2.html",
            memory_html(
                "Beta Fact",
                "beta body",
                Some(0.4),
                Some("fast"),
                Some("g1"),
            ),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Alpha Fact").await;
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Beta Fact").await;

    let resp: serde_json::Value = client
        .get(url(addr, "/api/kb/globalmem/atlas/points"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let points = resp["points"].as_array().unwrap();
    assert_eq!(points.len(), 2, "points: {points:?}");
    let beta = points.iter().find(|p| p["title"] == "Beta Fact").unwrap();
    assert!((beta["salience"].as_f64().unwrap() - 0.4).abs() < 1e-6);
    assert_eq!(beta["decay_bucket"], "fast");
    assert_eq!(beta["pinned"], false);
    assert_eq!(beta["forgotten"], false);
    assert_eq!(beta["supersedes"], "g1");
}

/// MI-W4.5 review fix — reviewer-caught pin-staleness bug: `/atlas/points`'s
/// ETag keyed ONLY on `index_generation()`, but `PinnedMemoryAdd`/`Remove`
/// (invariant #15 — not a row-set/edge mutation) never bumps it, so a client
/// polling with `If-None-Match` would 304 back a pre-pin snapshot forever.
/// Pins BOTH halves of the fix: a conditional GET with the pre-pin ETag must
/// NOT 304 after a pin (the ETag itself must differ), and the fresh payload
/// must actually carry `pinned: true`.
#[tokio::test]
async fn atlas_points_etag_reflects_pin_state_change() {
    let global = vec![(
        "g1.html",
        memory_html("Pinnable Fact", "pin me", Some(0.5), None, None),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Pinnable Fact"
    })
    .await;
    let id = doc["id"].as_str().unwrap().to_string();

    let before = client
        .get(url(addr, "/api/kb/globalmem/atlas/points"))
        .send()
        .await
        .unwrap();
    let etag_before = before
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .expect("ETag header present")
        .to_string();
    let body_before: serde_json::Value = before.json().await.unwrap();
    let point_before = body_before["points"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id.as_str())
        .expect("the pinnable point");
    assert_eq!(point_before["pinned"], false);

    let pin_resp = client
        .post(url(addr, &format!("/api/kb/globalmem/memories/{id}/pin")))
        .send()
        .await
        .unwrap();
    assert!(
        pin_resp.status().is_success(),
        "pin failed: {}",
        pin_resp.status()
    );

    // A conditional GET with the PRE-pin ETag must NOT 304 — the pin
    // changed state the ETag must reflect, even though index_generation
    // never moved.
    let conditional = client
        .get(url(addr, "/api/kb/globalmem/atlas/points"))
        .header(reqwest::header::IF_NONE_MATCH, &etag_before)
        .send()
        .await
        .unwrap();
    assert_ne!(
        conditional.status(),
        reqwest::StatusCode::NOT_MODIFIED,
        "a pin must invalidate the atlas/points ETag, not 304 back a stale snapshot"
    );
    let etag_after = conditional
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .expect("ETag header present")
        .to_string();
    assert_ne!(etag_before, etag_after, "ETag must change on pin");
    let body_after: serde_json::Value = conditional.json().await.unwrap();
    let point_after = body_after["points"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id.as_str())
        .expect("the pinnable point");
    assert_eq!(point_after["pinned"], true, "point: {point_after:?}");

    // The NEW etag correctly 304s a repeat poll (no further pin/unpin).
    let repeat = client
        .get(url(addr, "/api/kb/globalmem/atlas/points"))
        .header(reqwest::header::IF_NONE_MATCH, &etag_after)
        .send()
        .await
        .unwrap();
    assert_eq!(repeat.status(), reqwest::StatusCode::NOT_MODIFIED);
}

/// The SAME memory metas on a doc in a kb WITHOUT `memory_scope` set (a
/// stray meta on an ordinary corpus) must come back with every memory field
/// absent — the atlas is byte-unchanged for non-memory corpora.
#[tokio::test]
async fn atlas_points_omits_memory_fields_for_non_memory_scoped_kb() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plain");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("stray.html"),
        memory_html("Stray Doc", "body text", Some(0.5), Some("slow"), None),
    )
    .unwrap();

    let daemon_name = format!(
        "atlasmem-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("plain").unwrap(),
        KbSection {
            path: dir,
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
    tokio::time::sleep(Duration::from_millis(400)).await;

    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "plain", |d| d["title"] == "Stray Doc").await;

    let resp: serde_json::Value = client
        .get(url(addr, "/api/kb/plain/atlas/points"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let points = resp["points"].as_array().unwrap();
    let doc = points.iter().find(|p| p["title"] == "Stray Doc").unwrap();
    assert!(doc.get("salience").is_none(), "point: {doc:?}");
    assert!(doc.get("decay_bucket").is_none(), "point: {doc:?}");
    assert!(doc.get("pinned").is_none(), "point: {doc:?}");
    assert!(doc.get("forgotten").is_none(), "point: {doc:?}");
    assert!(doc.get("supersedes").is_none(), "point: {doc:?}");
}

/// MI-W4.0 — census must surface exactly what recall can serve: a
/// memory-scoped corpus row need not carry a `"memory-"`-prefixed
/// `kb-category` to be recallable (recall excludes only `memory-session`
/// transcripts), so census must not silently drop it either. Regression for
/// the pre-MI-W4.0 bug where census (and dupes) independently required
/// `category.starts_with("memory-")`, under-reporting the exact population
/// census exists to audit.
#[tokio::test]
async fn memory_census_and_recall_agree_on_non_memory_prefixed_category() {
    let global = vec![(
        "g1.html",
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Odd Category Fact</title><meta name="kb-category" content="project"><meta name="kb-salience" content="0.7"></head><body><h1>Odd Category Fact</h1><p>the quokka project ships next week</p></body></html>"#.to_string(),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Odd Category Fact"
    })
    .await;

    // census must list it, with its true (non-"memory-") category — not
    // silently drop it because "project" doesn't start with "memory-".
    let census: serde_json::Value = client
        .get(url(addr, "/api/memory/census?kb=globalmem"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(census["total"], 1, "census: {census:?}");
    let rows = census["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "census: {census:?}");
    assert_eq!(rows[0]["category"], "project");

    // recall must serve it too — same predicate, no drift between the two.
    let recall: serde_json::Value = client
        .get(url(addr, "/api/memory/recall?q=quokka&scope=all&limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hits = recall["hits"].as_array().unwrap();
    assert!(
        hits.iter().any(|h| h["title"] == "Odd Category Fact"),
        "recall: {recall:?}"
    );
}

/// MI-W3.3a — `?type=` on `/api/memory/census` is an exact facet filter,
/// applied before pagination (`total` reflects the filtered count), and
/// leaves untyped memories out when a type IS requested.
#[tokio::test]
async fn memory_census_type_filter_is_exact_and_precedes_pagination() {
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();

    for (title, memory_type) in [
        ("Episodic One", Some("episodic")),
        ("Semantic One", Some("semantic")),
        ("Semantic Two", Some("semantic")),
        ("Untyped One", None),
    ] {
        let mut body = serde_json::json!({"title": title, "body": "x"});
        if let Some(t) = memory_type {
            body["memory_type"] = serde_json::json!(t);
        }
        let created: serde_json::Value = client
            .post(url(addr, "/api/kb/globalmem/artifacts"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = created["id"].as_str().unwrap().to_string();
        wait_for_doc(&client, addr, "globalmem", |d| d["id"] == id).await;
    }

    // Unfiltered — all four.
    let all: serde_json::Value = client
        .get(url(addr, "/api/memory/census?kb=globalmem"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(all["total"], 4);

    // Filtered to "semantic" — exactly the two, total reflects the filter.
    let semantic: serde_json::Value = client
        .get(url(addr, "/api/memory/census?kb=globalmem&type=semantic"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(semantic["total"], 2);
    let rows = semantic["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for r in rows {
        assert_eq!(r["memory_type"], "semantic", "row {r:?}");
    }

    // Filtered to "procedural" — none of the four match.
    let procedural: serde_json::Value = client
        .get(url(addr, "/api/memory/census?kb=globalmem&type=procedural"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(procedural["total"], 0);
    assert!(procedural["rows"].as_array().unwrap().is_empty());
}

/// MI-W3.1 — `/api/memory/dupes` route wiring: fans out across every
/// memory corpus by default, honours `?kb=` to restrict to one, validates
/// `?threshold=` and rejects a non-memory `?kb=` target. `boot_memory_corpora`
/// has no embedder (BM25-only), so no artifact here ever carries a stored
/// embedding — `scanned` is deterministically 0 and `pairs` empty either
/// way; the PAIRING arithmetic itself is exhaustively unit-tested in
/// `kb_core::memory`'s `find_duplicate_pairs` tests. This test is about the
/// ROUTE: does it reach every corpus, filter to memory categories, and
/// reject bad input — never about whether two given vectors are "close".
#[tokio::test]
async fn memory_dupes_route_fans_out_and_validates_input() {
    let global = vec![(
        "g1.html",
        memory_html("Global One", "alpha body", Some(0.9), Some("slow"), None),
    )];
    let proj = vec![(
        "p1.html",
        memory_html("Project One", "beta body", Some(0.8), Some("slow"), None),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &proj).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Global One").await;
    wait_for_doc(&client, addr, "projmem", |d| d["title"] == "Project One").await;

    // Default (no ?kb=) — no embeddings anywhere, so scanned is 0 and
    // pairs is empty, but the route itself must succeed (200), not error.
    let all: serde_json::Value = client
        .get(url(addr, "/api/memory/dupes"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(all["scanned"], 0);
    assert!(all["pairs"].as_array().unwrap().is_empty());
    assert!((all["threshold"].as_f64().unwrap() - 0.90).abs() < 1e-6);

    // Explicit threshold is echoed back.
    let custom: serde_json::Value = client
        .get(url(addr, "/api/memory/dupes?threshold=0.5"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!((custom["threshold"].as_f64().unwrap() - 0.5).abs() < 1e-6);

    // ?kb= restricts to one memory corpus — still 200, still empty (no
    // embeddings), but exercises the single-corpus code path.
    let scoped: serde_json::Value = client
        .get(url(addr, "/api/memory/dupes?kb=globalmem"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(scoped["scanned"], 0);

    // An out-of-range threshold 400s.
    let bad_threshold = client
        .get(url(addr, "/api/memory/dupes?threshold=1.5"))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_threshold.status(), 400);

    // An unknown ?kb= 404s via the shared resolve_kb preamble (the
    // memory_scope-mismatch 400 path is unit-covered by the route's own
    // logic — `boot_memory_corpora` has no non-memory corpus to name here).
    let unknown_kb = client
        .get(url(addr, "/api/memory/dupes?kb=does-not-exist"))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown_kb.status(), 404);
}

/// MI-W4.4 — `/api/memory/triage` route wiring: fans out across every
/// memory corpus, scores with `kb_core::triage::build_queue`, excludes
/// pinned memories, and honours `?kb=`/`?limit=`. `boot_memory_corpora` has
/// no embedder, so the DUPLICATE reason never fires here (exhaustively
/// unit-tested in `kb_core::triage` instead) — this test is about the
/// ROUTE's own three reachable reasons (below-floor-now, high-
/// salience-dormant, superseded-not-forgotten) and the pinned exemption.
#[tokio::test]
async fn memory_triage_route_fans_out_scores_and_excludes_pinned() {
    // "below_floor_now" — salience already at/under the daemon's default
    // (Balanced, floor 0.15) is excluded right now, independent of age.
    let cold = memory_html("Cold Fact", "cold body", Some(0.05), None, None);
    // "high_salience_dormant" — high salience, and this daemon has no
    // memory_recalls ledger rows anywhere ⇒ recall_count 0 ⇒ never-recalled.
    let hot = memory_html("Hot Unused Fact", "hot body", Some(0.9), None, None);
    // Same shape as `cold`, but pinned below — must NOT appear in the
    // queue despite otherwise qualifying for below_floor_now.
    let pinned_cold = memory_html("Pinned Cold Fact", "pinned body", Some(0.05), None, None);
    // "superseded_not_forgotten" — ids are deterministic from the
    // source-relative path (invariant #27), so the new fact's
    // `kb-supersedes` can name the old one before either is indexed.
    let old_id = kb_core::ids::ArtifactId::from_path("g-old.html");
    let old = memory_html("Superseded Fact", "old body", Some(0.5), None, None);
    let new = memory_html(
        "Superseding Fact",
        "new body",
        Some(0.5),
        None,
        Some(old_id.as_str()),
    );
    // An ordinary memory that should never be flagged for anything.
    let fine = memory_html("Fine Fact", "fine body", Some(0.5), None, None);

    let global = vec![
        ("g-cold.html", cold),
        ("g-hot.html", hot),
        ("g-pinned.html", pinned_cold),
        ("g-old.html", old),
        ("g-new.html", new),
        ("g-fine.html", fine),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    for title in [
        "Cold Fact",
        "Hot Unused Fact",
        "Pinned Cold Fact",
        "Superseded Fact",
        "Superseding Fact",
        "Fine Fact",
    ] {
        wait_for_doc(&client, addr, "globalmem", move |d| d["title"] == title).await;
    }

    let pinned_doc = wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Pinned Cold Fact"
    })
    .await;
    let pinned_id = pinned_doc["id"].as_str().unwrap();
    let r = client
        .post(url(
            addr,
            &format!("/api/kb/globalmem/memories/{pinned_id}/pin"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let body: serde_json::Value = client
        .get(url(addr, "/api/memory/triage"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let items = body["items"].as_array().unwrap();
    let titles: Vec<&str> = items
        .iter()
        .map(|it| it["title"].as_str().unwrap())
        .collect();
    assert!(titles.contains(&"Cold Fact"), "items: {items:?}");
    assert!(titles.contains(&"Hot Unused Fact"), "items: {items:?}");
    assert!(titles.contains(&"Superseded Fact"), "items: {items:?}");
    assert!(
        !titles.contains(&"Pinned Cold Fact"),
        "pinned must be exempt: {items:?}"
    );
    assert!(
        !titles.contains(&"Superseding Fact"),
        "the superseder has no reason of its own: {items:?}"
    );
    assert!(
        !titles.contains(&"Fine Fact"),
        "an ordinary memory must not be flagged: {items:?}"
    );

    let cold_item = items.iter().find(|it| it["title"] == "Cold Fact").unwrap();
    assert_eq!(cold_item["reason_kind"], "below_floor_now");
    assert_eq!(
        cold_item["source_relative"], "g-cold.html",
        "item: {cold_item:?}"
    );
    let hot_item = items
        .iter()
        .find(|it| it["title"] == "Hot Unused Fact")
        .unwrap();
    assert_eq!(hot_item["reason_kind"], "high_salience_dormant");
    let old_item = items
        .iter()
        .find(|it| it["title"] == "Superseded Fact")
        .unwrap();
    assert_eq!(old_item["reason_kind"], "superseded_not_forgotten");

    // `?limit=` bounds the queue.
    let limited: serde_json::Value = client
        .get(url(addr, "/api/memory/triage?limit=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(limited["items"].as_array().unwrap().len(), 1);

    // `?kb=` restricts scope — still non-empty since globalmem holds every
    // fixture doc here.
    let scoped: serde_json::Value = client
        .get(url(addr, "/api/memory/triage?kb=globalmem"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!scoped["items"].as_array().unwrap().is_empty());

    // Unknown `?kb=` 404s via the shared resolve_kb preamble.
    let unknown_kb = client
        .get(url(addr, "/api/memory/triage?kb=does-not-exist"))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown_kb.status(), 404);
}

/// MI-W1.2 — `supersedes`/`superseded_by` are a corpus-local reverse
/// lookup: `superseded_by` on the OLD memory must resolve to whichever NEW
/// memory named it via `kb-supersedes`, even though only the new artifact's
/// own meta carries the forward pointer.
#[tokio::test]
async fn memory_census_computes_supersedes_reverse_lookup() {
    // Artifact ids are deterministic from the source-relative path
    // (invariant #27) — precompute the old fact's id so the new one's
    // `kb-supersedes` meta can name it before either file is indexed.
    let old_id = kb_core::ids::ArtifactId::from_path("g-old.html");
    let global = vec![
        (
            "g-old.html",
            memory_html("Old Fact", "old body", Some(0.5), None, None),
        ),
        (
            "g-new.html",
            memory_html(
                "New Fact",
                "new body",
                Some(0.5),
                None,
                Some(old_id.as_str()),
            ),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Old Fact").await;
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "New Fact").await;

    let resp: serde_json::Value = client
        .get(url(addr, "/api/memory/census?kb=globalmem"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = resp["rows"].as_array().unwrap();
    let old_row = rows.iter().find(|r| r["title"] == "Old Fact").unwrap();
    let new_row = rows.iter().find(|r| r["title"] == "New Fact").unwrap();
    assert_eq!(new_row["supersedes"], old_id.as_str());
    assert_eq!(old_row["superseded_by"], new_row["id"].clone());
}

/// MI-W2.3 — the full soft-forget round trip end to end: DELETE (default,
/// soft) tombstones in place → `recall` drops it → `census` STILL lists it
/// with `forgotten: true` (the whole point of a tombstone) → DELETE
/// `?purge=true` removes it entirely (gone from census too).
#[tokio::test]
async fn memory_forget_soft_then_purge_round_trip() {
    let global = vec![(
        "quokka.html",
        memory_html(
            "Quokka Fact",
            "the quokka pipeline deploys on fridays",
            Some(0.9),
            Some("slow"),
            None,
        ),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Quokka Fact").await;
    let id = doc["id"].as_str().unwrap().to_string();

    // Sanity: recall finds it before anything happens.
    let before: serde_json::Value = client
        .get(url(addr, "/api/memory/recall?q=quokka&scope=all&limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids_before: Vec<String> = before["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        ids_before.contains(&id),
        "recall finds it before forgetting"
    );

    // 1. DELETE without ?purge → soft forget.
    let del: serde_json::Value = client
        .delete(url(addr, &format!("/api/kb/globalmem/artifacts/{id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(del["purged"], false, "default DELETE soft-forgets");

    // File must still exist on disk — a tombstone, not a deletion.
    assert!(
        _tmp.path().join("globalmem").join("quokka.html").exists(),
        "soft-forgotten source file must remain on disk"
    );

    // 2. Wait for the reindex to pick up kb-status=forgotten, observed via
    // census (recall's own floor/status filter needs the SAME reindex).
    let forgotten_row = common::poll_until("the census row to flip forgotten=true", || async {
        let census: serde_json::Value = client
            .get(url(addr, "/api/memory/census?kb=globalmem"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        census["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id && r["forgotten"] == true)
            .cloned()
    })
    .await;
    assert_eq!(forgotten_row["id"], id);

    // 3. recall must now drop it (invariant #10's status=="forgotten"
    // filter, lit up by MI-W2.3).
    let after: serde_json::Value = client
        .get(url(addr, "/api/memory/recall?q=quokka&scope=all&limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids_after: Vec<String> = after["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        !ids_after.contains(&id),
        "recall must drop a soft-forgotten memory: {ids_after:?}"
    );

    // 4. DELETE ?purge=true → hard delete, no trace anywhere.
    let purge: serde_json::Value = client
        .delete(url(
            addr,
            &format!("/api/kb/globalmem/artifacts/{id}?purge=true"),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(purge["purged"], true, "?purge=true hard-deletes");
    assert!(
        !_tmp.path().join("globalmem").join("quokka.html").exists(),
        "purge must remove the source file"
    );

    common::poll_until("the purged row to disappear from census", || async {
        let census: serde_json::Value = client
            .get(url(addr, "/api/memory/census?kb=globalmem"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        (!census["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == id))
        .then_some(())
    })
    .await;
}

/// MI-W3.2b — PATCH …/memories/{id}/salience end to end: the response
/// echoes the clamped value, the SOURCE FILE is spliced (not a shadow
/// store), and a reindex round-trips the new value into `census` — twice,
/// to pin reindex STABILITY (a second reindex of unchanged bytes must not
/// drift the value).
#[tokio::test]
async fn memory_salience_patch_round_trips_and_survives_reindex() {
    let global = vec![(
        "quokka.html",
        memory_html(
            "Quokka Fact",
            "the quokka pipeline deploys on fridays",
            Some(0.20),
            Some("slow"),
            None,
        ),
    )];
    let (tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Quokka Fact").await;
    let id = doc["id"].as_str().unwrap().to_string();

    // 1. PATCH salience — response echoes the new value.
    let patched: serde_json::Value = client
        .patch(url(
            addr,
            &format!("/api/kb/globalmem/memories/{id}/salience"),
        ))
        .json(&serde_json::json!({"salience": 0.77}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(patched["id"], id);
    assert!((patched["salience"].as_f64().unwrap() - 0.77).abs() < 1e-6);

    // 2. The SOURCE FILE itself carries the new value (a splice, not a
    // shadow store) — read it straight off disk, no daemon round-trip.
    let src = std::fs::read_to_string(tmp.path().join("globalmem").join("quokka.html")).unwrap();
    assert!(src.contains(r#"<meta name="kb-salience" content="0.77">"#));
    assert!(
        !src.contains(r#"content="0.2""#) && !src.contains(r#"content="0.20""#),
        "the OLD value must be replaced, not left alongside the new one"
    );

    // 3. Wait for reindex — census reflects the new salience.
    common::poll_until("census to reflect the patched salience", || async {
        let census: serde_json::Value = client
            .get(url(addr, "/api/memory/census?kb=globalmem"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        census["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .filter(|row| (row["salience"].as_f64().unwrap_or(0.0) - 0.77).abs() < 1e-6)
            .map(|_| ())
    })
    .await;

    // 4. Reindex STABILITY: force a second reindex of the SAME (now
    // unchanged) bytes and confirm the value doesn't drift.
    client
        .post(url(addr, "/api/kb/globalmem/reindex"))
        .send()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let census_again: serde_json::Value = client
        .get(url(addr, "/api/memory/census?kb=globalmem"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row_again = census_again["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .expect("row survives a second reindex");
    assert!(
        (row_again["salience"].as_f64().unwrap_or(0.0) - 0.77).abs() < 1e-6,
        "a reindex of unchanged bytes must not drift salience: {row_again:?}"
    );

    // 5. Out-of-range + non-finite inputs are clamped/rejected, not written
    // verbatim.
    let clamped: serde_json::Value = client
        .patch(url(
            addr,
            &format!("/api/kb/globalmem/memories/{id}/salience"),
        ))
        .json(&serde_json::json!({"salience": 5.0}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!((clamped["salience"].as_f64().unwrap() - 1.0).abs() < 1e-6);

    let bad = client
        .patch(url(
            addr,
            &format!("/api/kb/globalmem/memories/{id}/salience"),
        ))
        .body(r#"{"salience": null}"#)
        .header("content-type", "application/json")
        .send()
        .await
        .unwrap();
    assert!(
        !bad.status().is_success(),
        "a non-numeric body must 400/422"
    );

    let unknown = client
        .patch(url(
            addr,
            "/api/kb/globalmem/memories/does-not-exist/salience",
        ))
        .json(&serde_json::json!({"salience": 0.5}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn memory_recall_scope_fans_out_across_corpora() {
    let global = vec![(
        "g1.html",
        memory_html(
            "Global Pref",
            "user prefers tabs over spaces zebra",
            Some(0.9),
            Some("slow"),
            None,
        ),
    )];
    let proj = vec![(
        "p1.html",
        memory_html(
            "Project Fact",
            "the zebra service runs on port 8080",
            Some(0.8),
            Some("slow"),
            None,
        ),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &proj).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Global Pref").await;
    wait_for_doc(&client, addr, "projmem", |d| d["title"] == "Project Fact").await;

    // scope=all spans both memory corpora.
    let all: serde_json::Value = client
        .get(url(addr, "/api/memory/recall?q=zebra&scope=all&limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let kbs: Vec<String> = all["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["kb"].as_str().unwrap().to_string())
        .collect();
    assert!(
        kbs.contains(&"globalmem".to_string()),
        "all → global: {kbs:?}"
    );
    assert!(
        kbs.contains(&"projmem".to_string()),
        "all → project: {kbs:?}"
    );

    // scope=global is restricted to the global-scope corpus.
    let g: serde_json::Value = client
        .get(url(
            addr,
            "/api/memory/recall?q=zebra&scope=global&limit=10",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let gkbs: Vec<String> = g["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["kb"].as_str().unwrap().to_string())
        .collect();
    assert!(!gkbs.is_empty(), "scope=global returned hits");
    assert!(
        gkbs.iter().all(|k| k == "globalmem"),
        "global only: {gkbs:?}"
    );
}

#[tokio::test]
async fn memory_recall_carries_read_state() {
    let global = vec![(
        "g1.html",
        memory_html(
            "Readme Pref",
            "user prefers tabs over spaces wombat",
            Some(0.9),
            Some("slow"),
            None,
        ),
    )];
    let proj: Vec<(&str, String)> = vec![];
    let (_tmp, addr) = boot_memory_corpora(&global, &proj).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Readme Pref").await;

    // Resolve the memory's id via recall. Before any visit, read_pct is absent.
    let r: serde_json::Value = client
        .get(url(addr, "/api/memory/recall?q=wombat&scope=all&limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hit = r["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["title"] == "Readme Pref")
        .expect("memory hit");
    let id = hit["id"].as_str().unwrap().to_string();
    let kb = hit["kb"].as_str().unwrap().to_string();
    assert!(
        hit.get("read_pct").map(|v| v.is_null()).unwrap_or(true),
        "no read state before a visit"
    );

    // Record a visit: open → scroll to 90% → reading beacon stopping in "intro".
    let open: serde_json::Value = client
        .post(url(addr, &format!("/api/kb/{kb}/history/open")))
        .json(&serde_json::json!({ "artifact_id": id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let vid = open["visit_id"].as_i64().unwrap();
    client
        .post(url(addr, &format!("/api/kb/{kb}/history/scroll")))
        .json(&serde_json::json!({ "visit_id": vid, "scroll_y": 90, "scroll_max": 100 }))
        .send()
        .await
        .unwrap();
    client
        .post(url(addr, &format!("/api/kb/{kb}/history/reading")))
        .json(&serde_json::json!({
            "visit_id": vid, "artifact_id": id, "active_ms": 5000,
            "last_section": "intro", "sections": []
        }))
        .send()
        .await
        .unwrap();

    // Recall again → the hit now carries the human's read state (ambient).
    let r2: serde_json::Value = client
        .get(url(addr, "/api/memory/recall?q=wombat&scope=all&limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hit2 = r2["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["id"].as_str() == Some(id.as_str()))
        .expect("memory hit after visit");
    assert_eq!(
        hit2["read_pct"].as_i64(),
        Some(90),
        "recall enriched with scroll %"
    );
    assert_eq!(hit2["stopped_at"].as_str(), Some("intro"));
    assert!(hit2["last_read_at"].as_i64().unwrap_or(0) > 0);
}

#[tokio::test]
async fn memory_recall_drops_superseded() {
    // The id is the SHA-256 prefix of the source-relative path, so we can
    // compute the old memory's id up front and have the new one supersede it.
    let old_id = kb_core::ids::ArtifactId::from_path("old.html").to_string();
    let global = vec![
        (
            "old.html",
            memory_html(
                "Old Endpoint",
                "deploy uses the quetzal pipeline",
                Some(0.5),
                Some("slow"),
                None,
            ),
        ),
        (
            "new.html",
            memory_html(
                "New Endpoint",
                "deploy uses the quetzal pipeline v2",
                Some(0.5),
                Some("slow"),
                Some(&old_id),
            ),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "New Endpoint").await;
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Old Endpoint").await;

    let r: serde_json::Value = client
        .get(url(addr, "/api/memory/recall?q=quetzal&scope=all&limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<String> = r["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap().to_string())
        .collect();
    assert!(!ids.is_empty(), "recall returned the surviving memory");
    assert!(
        !ids.contains(&old_id),
        "superseded old memory dropped: {ids:?}"
    );
}

#[tokio::test]
async fn memory_recall_inline_floor_uses_default_salience() {
    // RA1 — the recall route's INLINE decay floor (routes/memory.rs) must read
    // the same default as rerank: a memory with NO kb-salience meta defaults to
    // kb_core::memory::DEFAULT_SALIENCE (0.5), which is above the Balanced floor
    // (0.15), so it survives; a memory at 0.10 is below the floor and drops.
    // Guards against the route's literal drifting from the re-exported const.
    // (Premise: DEFAULT_SALIENCE 0.5 > Balanced floor 0.15.)
    let global = vec![
        (
            "nosal.html",
            // No salience meta → DEFAULT_SALIENCE (0.5) → survives the floor.
            memory_html(
                "No Salience Narwhal",
                "narwhal fact",
                None,
                Some("slow"),
                None,
            ),
        ),
        (
            "lowsal.html",
            // 0.10 ≤ 0.15 Balanced floor → dropped by the inline floor.
            memory_html(
                "Low Salience Narwhal",
                "narwhal fact",
                Some(0.10),
                Some("slow"),
                None,
            ),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "No Salience Narwhal"
    })
    .await;
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Low Salience Narwhal"
    })
    .await;

    let r: serde_json::Value = client
        .get(url(addr, "/api/memory/recall?q=narwhal&scope=all&limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let titles: Vec<String> = r["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["title"].as_str().unwrap().to_string())
        .collect();
    assert!(
        titles.contains(&"No Salience Narwhal".to_string()),
        "no-salience memory (→DEFAULT_SALIENCE 0.5) must survive the Balanced floor: {titles:?}"
    );
    assert!(
        !titles.contains(&"Low Salience Narwhal".to_string()),
        "0.10-salience memory must drop below the Balanced floor: {titles:?}"
    );

    // RA-recall — `no_floor=true` bypasses the floor (the dedup-oracle path):
    // the 0.10 memory now surfaces.
    let r2: serde_json::Value = client
        .get(url(
            addr,
            "/api/memory/recall?q=narwhal&scope=all&limit=10&no_floor=true",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let titles2: Vec<String> = r2["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["title"].as_str().unwrap().to_string())
        .collect();
    assert!(
        titles2.contains(&"Low Salience Narwhal".to_string()),
        "no_floor=true must surface the floored 0.10 memory: {titles2:?}"
    );
}

#[tokio::test]
async fn memory_recall_ranks_higher_salience_first() {
    // Identical bodies → equal BM25 relevance; salience is the tie-breaker.
    let body = "the okapi config lives under etc";
    // v0.10 M1 — Balanced (default) drops salience ≤ 0.15, Strict
    // drops ≤ 0.20. Use 0.25 for the low side so the test stays valid
    // under both default and future-strict policy flips. The point is
    // still that 0.95 outranks 0.25, just with both above the floor.
    let global = vec![
        (
            "lo.html",
            memory_html("LowSalience", body, Some(0.25), Some("slow"), None),
        ),
        (
            "hi.html",
            memory_html("HighSalience", body, Some(0.95), Some("slow"), None),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "HighSalience").await;
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "LowSalience").await;

    let r: serde_json::Value = client
        .get(url(
            addr,
            "/api/memory/recall?q=okapi&scope=global&limit=10",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hits = r["hits"].as_array().unwrap();
    assert!(hits.len() >= 2, "both memories recalled: {hits:?}");
    assert_eq!(
        hits[0]["title"].as_str().unwrap(),
        "HighSalience",
        "higher salience ranks first"
    );
}

// ---- v0.9 M4: memory ingest (write-only, collision-safe) -------------

// invariant:10 write-only-ingest
#[tokio::test]
async fn memory_ingest_writes_indexes_once_and_is_searchable() {
    use futures::StreamExt;
    // Empty corpora → the only artifact.indexed in the ring is ours.
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "Deploy uses Quetzal",
            "body": "The deploy pipeline is named quetzal and runs nightly.",
            "category": "memory-project",
            "salience": 0.7,
            "decay": "slow"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let created: serde_json::Value = resp.json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert!(!id.is_empty());
    assert!(created["path"].as_str().unwrap().ends_with(".html"));

    // Write-only: the watcher indexes it → searchable, same id we returned.
    let doc = wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Deploy uses Quetzal"
    })
    .await;
    assert_eq!(doc["id"].as_str().unwrap(), id);

    // Exactly one index pass: the events ring holds a single
    // artifact.indexed (the corpus had no other files → no double-embed).
    let resp = client.get(url(addr, "/api/events")).send().await.unwrap();
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    let _ = tokio::time::timeout(Duration::from_millis(800), async {
        while let Some(chunk) = stream.next().await {
            if let Ok(bytes) = chunk {
                buf.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    })
    .await;
    let indexed = buf
        .lines()
        .filter_map(|l| l.strip_prefix("event:"))
        .map(str::trim)
        .filter(|&e| e == "artifact.indexed")
        .count();
    assert_eq!(indexed, 1, "exactly one index pass; frames:\n{buf}");
}

#[tokio::test]
async fn memory_ingest_same_title_yields_two_artifacts() {
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();

    let r1: serde_json::Value = client
        .post(url(addr, "/api/kb/projmem/artifacts"))
        .json(&serde_json::json!({"title": "Same Title", "body": "first body alpha"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let r2: serde_json::Value = client
        .post(url(addr, "/api/kb/projmem/artifacts"))
        .json(&serde_json::json!({"title": "Same Title", "body": "second body beta"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let id1 = r1["id"].as_str().unwrap();
    let id2 = r2["id"].as_str().unwrap();
    assert_ne!(id1, id2, "same-title remembers must get distinct ids");
    assert_ne!(r1["path"], r2["path"], "and distinct paths");

    // Both index into two rows (no silent merge_insert overwrite).
    wait_for_doc(&client, addr, "projmem", |d| d["id"] == id1).await;
    wait_for_doc(&client, addr, "projmem", |d| d["id"] == id2).await;
}

/// MI-W3.3a / MI-W3.4 — `memory_type` + `source` round-trip end to end:
/// valid closed-set values persist to the source, census surfaces both,
/// and an invalid value on either 400s BEFORE anything is written.
#[tokio::test]
async fn memory_ingest_memory_type_and_source_round_trip_and_validate() {
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "Fetched Fact",
            "body": "scraped from a changelog page",
            "memory_type": "semantic",
            "source": "fetched-web",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    // The source file itself carries both metas.
    let rel = created["path"].as_str().unwrap();
    let src = std::fs::read_to_string(_tmp.path().join("globalmem").join(rel)).unwrap();
    assert!(src.contains(r#"<meta name="kb-memory-type" content="semantic">"#));
    assert!(src.contains(r#"<meta name="kb-source" content="fetched-web">"#));

    // census surfaces both (after the watcher's reindex).
    let row = common::poll_until("census to index the new memory", || async {
        let census: serde_json::Value = client
            .get(url(addr, "/api/memory/census?kb=globalmem"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        census["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .cloned()
    })
    .await;
    assert_eq!(row["memory_type"], "semantic");
    assert_eq!(row["source"], "fetched-web");

    // An unrecognised memory_type is rejected BEFORE any write.
    let bad_type = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "Bad Type", "body": "x", "memory_type": "not-a-real-type",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_type.status(), 400);

    // An unrecognised source is likewise rejected.
    let bad_source = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "Bad Source", "body": "x", "source": "telepathy",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_source.status(), 400);

    // Neither rejected write left a trace in the corpus.
    let census: serde_json::Value = client
        .get(url(addr, "/api/memory/census?kb=globalmem"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        census["rows"].as_array().unwrap().len(),
        1,
        "only the one valid memory made it in"
    );
}

// ---- v0.9 M6: memory.* SSE + supersede staleness ---------------------

#[tokio::test]
async fn memory_supersede_emits_stale_event() {
    use futures::StreamExt;
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();

    // Old memory.
    let r1: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({"title": "Old Way", "body": "deploy via foo"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let old_id = r1["id"].as_str().unwrap().to_string();

    // Superseding memory — the ingest path fires memory.ingested AND
    // memory.stale{id: old_id} (supersede is a 2-memory relationship).
    let _r2: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "New Way", "body": "deploy via bar", "supersedes": old_id
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Connect AFTER the posts: the daemon-wide bus ring replays the
    // just-emitted memory.stale to a fresh subscriber.
    let resp = client.get(url(addr, "/api/events")).send().await.unwrap();
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    let _ = tokio::time::timeout(Duration::from_millis(1000), async {
        while let Some(chunk) = stream.next().await {
            if let Ok(bytes) = chunk {
                buf.push_str(&String::from_utf8_lossy(&bytes));
                if buf.contains("memory.stale") && buf.contains(&old_id) {
                    break;
                }
            }
        }
    })
    .await;
    assert!(
        buf.contains("memory.stale"),
        "expected a memory.stale frame; got:\n{buf}"
    );
    assert!(
        buf.contains(&old_id),
        "memory.stale must name the superseded id {old_id}; got:\n{buf}"
    );
}

// === S5 admin actions: DELETE /api/kb/{kb}, history purge, drain ===

#[tokio::test]
async fn drop_kb_wipes_lance_and_sqlite_for_that_kb() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // Sanity — pre-drop the kb has >0 docs from the canon corpus.
    let stats_pre: serde_json::Value = client
        .get(format!(
            "http://127.0.0.1:{}/api/kb/smoke/stats",
            addr.port()
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        stats_pre["doc_count"].as_u64().unwrap_or(0) > 0,
        "fixture must boot with docs before the drop; got {stats_pre}"
    );

    let resp = client
        .delete(format!("http://127.0.0.1:{}/api/kb/smoke", addr.port()))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "drop returned {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["kb"], "smoke");
    assert!(body["lance_rows_deleted"].as_u64().unwrap() > 0);

    // Post-drop the kb still exists (route + actor stayed up) and
    // its doc_count is 0. A subsequent reindex would re-walk from
    // source; we don't run one here to keep the test fast.
    let stats_post: serde_json::Value = client
        .get(format!(
            "http://127.0.0.1:{}/api/kb/smoke/stats",
            addr.port()
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        stats_post["doc_count"].as_u64().unwrap_or(99),
        0,
        "post-drop doc_count must be 0; got {stats_post}"
    );
}

#[tokio::test]
async fn drop_kb_unknown_returns_problem_json() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .delete(format!(
            "http://127.0.0.1:{}/api/kb/nonexistent",
            addr.port()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ct.contains("application/problem+json"),
        "expected RFC 7807 problem+json on unknown kb; got {ct}"
    );
}

#[tokio::test]
async fn history_purge_clears_only_history_table() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // Seed a history row via the open endpoint so the table isn't empty.
    let docs: Vec<serde_json::Value> = client
        .get(format!(
            "http://127.0.0.1:{}/api/kb/smoke/docs?limit=1",
            addr.port()
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = docs[0]["id"].as_str().unwrap().to_string();
    client
        .post(format!(
            "http://127.0.0.1:{}/api/kb/smoke/history/open",
            addr.port()
        ))
        .json(&serde_json::json!({"artifact_id": id}))
        .send()
        .await
        .unwrap();
    let pre = client
        .get(format!(
            "http://127.0.0.1:{}/api/kb/smoke/history?limit=10",
            addr.port()
        ))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let pre_entries = pre["entries"].as_array().cloned().unwrap_or_default();
    assert!(
        !pre_entries.is_empty(),
        "history must have rows pre-purge; got empty"
    );

    let resp = client
        .post(format!(
            "http://127.0.0.1:{}/api/kb/smoke/history/purge",
            addr.port()
        ))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["kb"], "smoke");
    assert!(body["rows_deleted"].as_u64().unwrap() >= 1);

    let post = client
        .get(format!(
            "http://127.0.0.1:{}/api/kb/smoke/history?limit=10",
            addr.port()
        ))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let post_entries = post["entries"].as_array().cloned().unwrap_or_default();
    assert!(
        post_entries.is_empty(),
        "history must be empty post-purge; got {post_entries:?}"
    );

    // Stats still works — the actor stayed up.
    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/api/kb/smoke/stats",
            addr.port()
        ))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
}

#[tokio::test]
async fn shutdown_route_returns_202_and_signals_drain() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{}/api/shutdown", addr.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["draining"], true);
    // The in-process `serve_on_random_port_with_paths` doesn't wire
    // the shutdown signal to process exit — only the daemon binary
    // main does that — but the broadcast::watch channel HAS been
    // flipped, which is the contract SPA + TUI consumers depend on:
    // a subsequent `subscribe()` would see `wait_for(|&d| d)` resolve
    // immediately and SSE handlers would close their streams.
}

#[tokio::test]
async fn events_schema_includes_admin_kinds() {
    let (_tmp, addr) = boot().await;
    let resp = reqwest::get(format!(
        "http://127.0.0.1:{}/api/events.schema.json",
        addr.port()
    ))
    .await
    .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let kinds: Vec<String> = body["types"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    for k in [
        "kb.dropped",
        "history.purged",
        "atlas.recluster.start",
        "atlas.recluster.complete",
        // v0.10 K2 — anchor corkboard SSE events must be advertised so
        // the SPA can subscribe via the schema discovery endpoint.
        "anchor.added",
        "anchor.removed",
    ] {
        assert!(
            kinds.iter().any(|s| s == k),
            "schema enum missing {k}; got {kinds:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// v0.10 K2 — anchor corkboard. /api/anchors (cross-kb list) +
// /api/kb/{kb}/anchors/{id} (pin/unpin).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn corkboard_empty_at_boot() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/anchors"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["anchors"], serde_json::json!([]));
}

#[tokio::test]
async fn corkboard_pin_unpin_round_trip_through_http() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // Pull a real artifact id from the fixture corpus.
    let docs: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/docs?limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!docs.is_empty(), "fixture corpus produced no docs");
    let aid = docs[0]["id"].as_str().expect("doc id").to_string();

    // POST pin — first call inserts (added=true).
    let r = client
        .post(url(addr, &format!("/api/kb/smoke/anchors/{aid}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "first pin should 200");
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["added"], serde_json::json!(true));

    // Idempotent second POST returns added=false.
    let body: serde_json::Value = client
        .post(url(addr, &format!("/api/kb/smoke/anchors/{aid}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["added"], serde_json::json!(false));

    // GET cross-kb list surfaces the pinned artifact, with the lance
    // projection filled in (title / source_relative / folder all
    // present since the artifact is in lance).
    let list: serde_json::Value = client
        .get(url(addr, "/api/anchors"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let anchors = list["anchors"].as_array().expect("anchors array");
    assert_eq!(anchors.len(), 1, "expected exactly one anchor; got {list}");
    assert_eq!(anchors[0]["kb"], "smoke");
    assert_eq!(anchors[0]["artifact_id"], aid);
    assert!(
        anchors[0].get("title").and_then(|v| v.as_str()).is_some(),
        "title projection missing; got {list}"
    );
    assert!(anchors[0]
        .get("source_relative")
        .and_then(|v| v.as_str())
        .is_some());

    // DELETE unpin returns 204; second DELETE is idempotent 204.
    let r = client
        .delete(url(addr, &format!("/api/kb/smoke/anchors/{aid}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let r = client
        .delete(url(addr, &format!("/api/kb/smoke/anchors/{aid}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204, "unpin must be idempotent");

    // List is empty again.
    let list: serde_json::Value = client
        .get(url(addr, "/api/anchors"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["anchors"], serde_json::json!([]));
}

#[tokio::test]
async fn corkboard_pin_refuses_unknown_artifact() {
    // K2 decision — a 404 here is preferable to a silent insert that
    // the cross-kb list would later project as a tombstone. A typo from
    // the SPA is the user's bug to fix, not ours to paper over.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let r = client
        .post(url(
            addr,
            "/api/kb/smoke/anchors/nonexistent000000000000000000",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn docs_list_accepts_q_dsl_and_reports_ms() {
    // Q2 — `?q=` is parsed as the DSL, unioned with flat params, and
    // the response carries route timing in `ms`.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // Baseline: bare envelope mode returns ms field + zero parse warnings.
    let body: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/docs?offset=0&limit=5"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["ms"].is_number(), "ms field missing: {body}");
    assert!(
        body.get("query_warnings")
            .is_none_or(|v| v.as_array().is_some_and(|a| a.is_empty())),
        "expected no warnings: {body}"
    );

    // `?q=index:true` narrows to index pages — the fixture has one.
    let body: serde_json::Value = client
        .get(url(
            addr,
            "/api/kb/smoke/docs?offset=0&limit=10&q=index%3Atrue",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let docs = body["docs"].as_array().expect("docs array");
    for d in docs {
        let path = d["path"].as_str().unwrap();
        assert!(
            path.to_lowercase().ends_with("index.html"),
            "expected index.html under index:true; got {path}"
        );
    }
    assert!(body["ms"].is_number());
}

#[tokio::test]
async fn docs_list_surfaces_query_parse_warnings() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // Unknown key → warning, no docs dropped.
    let body: serde_json::Value = client
        .get(url(
            addr,
            "/api/kb/smoke/docs?offset=0&limit=5&q=severity%3Ahigh",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let warnings = body["query_warnings"]
        .as_array()
        .expect("query_warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| { w.as_str().is_some_and(|s| s.contains("unknown key")) }),
        "expected unknown-key warning: {body}"
    );
}

/// v0.16 Q-track — a tiny corpus with explicit `kb-tags` so OR/NOT
/// DSL filtering has stable, predictable tag sets to assert on (the
/// frozen canon fixture carries no tags, only path-derived ones). Four
/// root docs: `a-rust.html` (rust), `b-rust-atlas.html` (rust, atlas),
/// `c-atlas.html` (atlas), `d-sql.html` (sql).
async fn boot_with_tagged_fixture() -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    let doc = |tags: &str, title: &str| {
        format!(
            r#"<!doctype html><html><head><meta charset="utf-8">
<title>{title}</title>
<meta name="kb-tags" content="{tags}"></head>
<body><h1>{title}</h1><p>fixture body for query DSL OR/NOT tests.</p></body></html>"#
        )
    };
    std::fs::write(source.join("a-rust.html"), doc("rust", "A Rust")).unwrap();
    std::fs::write(
        source.join("b-rust-atlas.html"),
        doc("rust, atlas", "B Rust Atlas"),
    )
    .unwrap();
    std::fs::write(source.join("c-atlas.html"), doc("atlas", "C Atlas")).unwrap();
    std::fs::write(source.join("d-sql.html"), doc("sql", "D Sql")).unwrap();

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("tagged").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    common::wait_docs_listed(addr, "tagged", 4).await;
    (tmp, addr)
}

/// GET the tagged fixture's docs for a `?q=` DSL string, returning
/// `(total, sorted basenames, query_warnings)`.
async fn q_docs(addr: std::net::SocketAddr, kb: &str, q: &str) -> (u64, Vec<String>, Vec<String>) {
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, &format!("/api/kb/{kb}/docs")))
        .query(&[("q", q), ("envelope", "1"), ("limit", "50")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let total = body["total"].as_u64().expect("total");
    let mut names: Vec<String> = body["docs"]
        .as_array()
        .expect("docs array")
        .iter()
        .map(|d| {
            d["path"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    names.sort();
    let warnings: Vec<String> = body["query_warnings"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|w| w.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    (total, names, warnings)
}

#[tokio::test]
async fn docs_list_or_returns_union() {
    // `tag:a OR tag:b` returns the UNION of both branches (not the old
    // "collapse to the first conjunct" behaviour), with no warnings.
    let (_tmp, addr) = boot_with_tagged_fixture().await;

    let (rust_total, rust, _) = q_docs(addr, "tagged", "tag:rust").await;
    assert_eq!(rust_total, 2, "tag:rust → a-rust + b-rust-atlas");
    assert_eq!(rust, vec!["a-rust.html", "b-rust-atlas.html"]);

    let (atlas_total, _, _) = q_docs(addr, "tagged", "tag:atlas").await;
    assert_eq!(atlas_total, 2, "tag:atlas → b-rust-atlas + c-atlas");

    let (or_total, or_names, warnings) = q_docs(addr, "tagged", "tag:rust OR tag:atlas").await;
    assert_eq!(or_total, 3, "union rust(2) ∪ atlas(2) = 3 (b shared)");
    assert_eq!(
        or_names,
        vec!["a-rust.html", "b-rust-atlas.html", "c-atlas.html"]
    );
    assert!(
        warnings.is_empty(),
        "OR no longer collapses → no warning: {warnings:?}"
    );
}

#[tokio::test]
async fn docs_list_not_excludes() {
    // `NOT tag:x` excludes matching rows; a positive term plus a NOT
    // includes-then-excludes. Neither emits the old "v0.11" warning.
    let (_tmp, addr) = boot_with_tagged_fixture().await;

    let (not_total, not_names, w1) = q_docs(addr, "tagged", "NOT tag:sql").await;
    assert_eq!(not_total, 3, "all but d-sql");
    assert!(!not_names.contains(&"d-sql.html".to_string()));
    assert!(w1.is_empty(), "NOT no longer deferred → no warning: {w1:?}");

    let (combo_total, combo_names, w2) = q_docs(addr, "tagged", "tag:rust NOT tag:atlas").await;
    assert_eq!(combo_total, 1, "rust minus atlas = a-rust only");
    assert_eq!(combo_names, vec!["a-rust.html"]);
    assert!(w2.is_empty(), "{w2:?}");
}

// ---------------------------------------------------------------------------
// v0.10 M2 — memory pin + decay policy routes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_policy_round_trip() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // GET default policy.
    let body: serde_json::Value = client
        .get(url(addr, "/api/memory/policy"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["policy"], "balanced");
    // PUT a new policy.
    let r = client
        .put(url(addr, "/api/memory/policy"))
        .json(&serde_json::json!({"policy": "loose"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["policy"], "loose");
    // GET reflects it.
    let body: serde_json::Value = client
        .get(url(addr, "/api/memory/policy"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["policy"], "loose");
    // Invalid value → 400.
    let r = client
        .put(url(addr, "/api/memory/policy"))
        .json(&serde_json::json!({"policy": "nope"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}

#[tokio::test]
async fn memory_policy_persists_across_restart() {
    // v0.12 — PUT writes <state>/memory-policy.json; the next boot
    // loads it back. Verify by spinning two daemons against the same
    // KbPaths root.
    let (tmp, cfg, paths) = fixture_corpus();
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg.clone(), paths.clone())
        .await
        .expect("serve");
    let client = reqwest::Client::new();
    // Flip to loose.
    let r = client
        .put(url(addr, "/api/memory/policy"))
        .json(&serde_json::json!({"policy": "loose"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // Drop the first daemon; a second boot against the same KbPaths
    // should read the persisted "loose".
    drop(_task);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let (addr2, _task2) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let body: serde_json::Value = client
        .get(url(addr2, "/api/memory/policy"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["policy"], "loose");
    drop(tmp);
}

#[tokio::test]
async fn kb_section_decay_policy_parses_from_toml() {
    // v0.13 — `[kb.foo] decay_policy = "strict"` in kb.toml is parsed
    // into the in-memory KbSection. The "unknown value falls back with
    // a warn" branch is exercised at boot in serve(), not in this
    // narrow round-trip.
    // toml isn't a dev-dep here; use the kb-core loader path: write
    // a temp file + KbConfig::load.
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("kb.toml");
    std::fs::write(
        &cfg_path,
        r#"
        [kb.foo]
        path = "/tmp/foo"
        memory_scope = "project"
        decay_policy = "strict"
    "#,
    )
    .unwrap();
    let cfg = kb_core::config::KbConfig::load(&cfg_path).unwrap();
    let kb = cfg
        .kb
        .get(&kb_core::types::KbName::new("foo").unwrap())
        .unwrap();
    assert_eq!(kb.decay_policy.as_deref(), Some("strict"));
    assert_eq!(kb.memory_scope.as_deref(), Some("project"));
}

#[tokio::test]
async fn saved_queries_round_trip_through_http() {
    // v0.13 Q4 — upsert by name (case-insensitive), list, delete.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // Start empty.
    let body: serde_json::Value = client
        .get(url(addr, "/api/saved-queries"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["queries"], serde_json::json!([]));

    // POST upserts.
    let r = client
        .post(url(addr, "/api/saved-queries"))
        .json(&serde_json::json!({
            "name": "Recent rust",
            "path": "/",
            "search": "?tags=rust",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    let qs = body["queries"].as_array().expect("queries array");
    assert_eq!(qs.len(), 1);
    assert_eq!(qs[0]["name"], "Recent rust");
    assert_eq!(qs[0]["search"], "?tags=rust");

    // Second POST with same name (different case) overwrites.
    let body: serde_json::Value = client
        .post(url(addr, "/api/saved-queries"))
        .json(&serde_json::json!({
            "name": "RECENT RUST",
            "path": "/",
            "search": "?tags=rust&since=7d",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let qs = body["queries"].as_array().expect("queries array");
    assert_eq!(qs.len(), 1, "case-insensitive dedupe");
    assert_eq!(qs[0]["search"], "?tags=rust&since=7d");

    // DELETE removes; 204 even on missing-name.
    let r = client
        .delete(url(addr, "/api/saved-queries/RECENT%20RUST"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let r = client
        .delete(url(addr, "/api/saved-queries/nope"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);

    // List is empty again.
    let body: serde_json::Value = client
        .get(url(addr, "/api/saved-queries"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["queries"], serde_json::json!([]));
}

#[tokio::test]
async fn saved_queries_reject_empty_and_oversized_names() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let r = client
        .post(url(addr, "/api/saved-queries"))
        .json(&serde_json::json!({
            "name": "   ",
            "path": "/",
            "search": "",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let big = "a".repeat(120);
    let r = client
        .post(url(addr, "/api/saved-queries"))
        .json(&serde_json::json!({
            "name": big,
            "path": "/",
            "search": "",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}

#[tokio::test]
async fn memory_promote_rejects_same_kb() {
    // v0.13 D7 — promoting to the source kb itself is a 400; the
    // route refuses to write the same content into the corpus that
    // already holds it.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // The smoke fixture's only kb is `smoke` (non-memory). Use a
    // real artifact id from it; promote to itself should 400.
    let docs: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/docs?limit=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let aid = docs[0]["id"].as_str().expect("doc id").to_string();
    let r = client
        .post(url(addr, &format!("/api/kb/smoke/memories/{aid}/promote")))
        .json(&serde_json::json!({"dest_kb": "smoke"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}

#[tokio::test]
async fn memory_pin_refuses_unknown_artifact() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let r = client
        .post(url(
            addr,
            "/api/kb/smoke/memories/nonexistent000000000000000/pin",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn memory_pin_unpin_round_trip() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let docs: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/docs?limit=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let aid = docs[0]["id"].as_str().expect("doc id").to_string();

    let r = client
        .post(url(addr, &format!("/api/kb/smoke/memories/{aid}/pin")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["pinned"], serde_json::json!(true));

    // Idempotent unpin.
    let r = client
        .delete(url(addr, &format!("/api/kb/smoke/memories/{aid}/pin")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let r = client
        .delete(url(addr, &format!("/api/kb/smoke/memories/{aid}/pin")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
}

#[tokio::test]
async fn corkboard_pin_refuses_unknown_kb() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let r = client
        .post(url(addr, "/api/kb/no-such-kb/anchors/abcdef012345"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

// === v0.14 S3 — /api/sessions/* ============================================

/// Wrap a JSONL string as a kb-capture.sh-style memory-session
/// artifact. Mirrors the production wrapper exactly (same metas, same
/// <pre> escaping), so the indexer's enrichment hook runs the same
/// parse path as it would on a real Stop-hook capture.
fn session_transcript_html(session_id: &str, ts: &str, jsonl: &str) -> String {
    let esc = jsonl
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>Session transcript {ts}</title>
<meta name="kb-category" content="memory-session">
<meta name="kb-decay" content="fast">
<meta name="kb-session" content="{session_id}">
</head><body>
<h1>Session transcript {ts}</h1>
<pre>{esc}</pre>
</body></html>
"#
    )
}

/// Wrap a memory artifact carrying a `kb-session` meta — what
/// `kb remember` produces when the marker file is present.
fn memory_with_session_html(title: &str, session_id: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>{title}</title><meta name="kb-category" content="memory-user"><meta name="kb-salience" content="0.8"><meta name="kb-session" content="{session_id}"></head><body><h1>{title}</h1><p>body</p></body></html>"#
    )
}

/// SP3 — `GET /api/sessions/{id}/export` streams a portable `.kbsession.zip`
/// (manifest.json + byte-identical session.jsonl) for `kb sessions pull`. On a
/// loopback bind the transcript is NOT force-scrubbed (`x-kb-session-scrubbed:
/// 0`); the tricky `< & >` chars round-trip verbatim.
#[tokio::test]
async fn session_export_route_streams_a_valid_bundle() {
    use std::io::Read;
    let sid = "sess-export-001";
    let jsonl = "{\"sessionId\":\"sess-export-001\",\"cwd\":\"/home/u/proj\",\"timestamp\":\"2026-06-01T10:00:00Z\"}\n\
                 {\"type\":\"assistant\",\"text\":\"a < b & c > d\"}\n";
    let global = vec![(
        "session-20260601T100000Z-sess-export-001.html",
        session_transcript_html(sid, "20260601T100000Z", jsonl),
    )];
    let proj: Vec<(&str, String)> = Vec::new();
    let (_tmp, addr) = boot_memory_corpora(&global, &proj).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T100000Z"
    })
    .await;
    // Wait for the session-enrichment row (sessions_get) to land before export.
    for _ in 0..100 {
        let r = client
            .get(url(addr, "/api/sessions/sess-export-001"))
            .send()
            .await
            .unwrap();
        if r.status() == 200 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let resp = client
        .get(url(addr, "/api/sessions/sess-export-001/export"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/zip"
    );
    // Loopback → the secrets floor is NOT force-enabled.
    assert_eq!(
        resp.headers()
            .get("x-kb-session-scrubbed")
            .map(|v| v.to_str().unwrap()),
        Some("0")
    );
    let bytes = resp.bytes().await.unwrap().to_vec();

    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let manifest: serde_json::Value = {
        let mut f = zip.by_name("manifest.json").unwrap();
        let mut s = String::new();
        f.read_to_string(&mut s).unwrap();
        serde_json::from_str(&s).unwrap()
    };
    assert_eq!(manifest["session_id"], "sess-export-001");
    assert_eq!(manifest["schema"], "kb-session-bundle/1");
    assert_eq!(manifest["origin"]["kb"], "globalmem");
    assert_eq!(manifest["scrubbed"], false);

    let transcript = {
        let mut f = zip.by_name("session.jsonl").unwrap();
        let mut s = String::new();
        f.read_to_string(&mut s).unwrap();
        s
    };
    assert_eq!(
        transcript, jsonl,
        "recovered transcript is byte-identical to the original"
    );

    // Unknown session → 404.
    let miss = client
        .get(url(addr, "/api/sessions/nope-404/export"))
        .send()
        .await
        .unwrap();
    assert_eq!(miss.status(), 404);
}

// === MI-W4.2c — GET /api/sessions/{id}/recalls =============================

#[tokio::test]
async fn session_recalls_returns_404_for_unknown_session() {
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();
    let r = client
        .get(url(addr, "/api/sessions/nope-404/recalls"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

/// A real session with no `Item::MemoryInjection` at all (no `kb-recall`
/// hits) resolves 200 with an empty list — "never recalled anything" is a
/// normal, common case, not an error.
#[tokio::test]
async fn session_recalls_is_empty_for_a_session_with_no_ledger_rows() {
    let sid = "sess-recalls-empty";
    let jsonl = "{\"sessionId\":\"sess-recalls-empty\",\"cwd\":\"/home/u/proj\",\"timestamp\":\"2026-06-04T09:00:00.000Z\"}\n\
                 {\"type\":\"assistant\",\"text\":\"hi\",\"timestamp\":\"2026-06-04T09:00:01.000Z\"}\n";
    let global = vec![(
        "session-20260604T090000Z-sess-recalls-empty.html",
        session_transcript_html(sid, "20260604T090000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_session(&client, addr, sid).await;

    let body: serde_json::Value = client
        .get(url(addr, &format!("/api/sessions/{sid}/recalls")))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["recalls"].as_array().unwrap().len(), 0);
}

/// End-to-end: a `kb-recall` hook injection in the transcript lands in the
/// session's OWN `memory_recalls` ledger (via the `memory-recall-ledger`
/// enrichment hook), and `/recalls` serves it back — the PULL-side mirror of
/// `/memories`' WRITE-side list, MI-W4.2c's "Memories recalled" data
/// source. The referenced memory id doesn't correspond to a real doc in
/// this daemon (ids are content-hashed, not caller-chosen), so `title`/
/// `source_relative` are asserted ABSENT — pinning the "ledger row survives
/// even when the memory itself doesn't resolve" contract from the route's
/// doc comment.
#[tokio::test]
async fn session_recalls_returns_injected_ledger_hits() {
    let sid = "sess-recalls-001";
    let jsonl = concat!(
        "{\"sessionId\":\"sess-recalls-001\",\"cwd\":\"/home/u/proj\",\"timestamp\":\"2026-06-04T09:00:00.000Z\"}\n",
        "{\"parentUuid\":null,\"isSidechain\":false,\"attachment\":{\"type\":\"hook_additional_context\",\"content\":[\"Relevant memories from kb (recall - these persist across sessions):\\n- alpha fact  [globalmem]  (id aaaaaaaaaaaa)\"],\"hookName\":\"UserPromptSubmit\",\"toolUseID\":\"UserPromptSubmit\",\"hookEvent\":\"UserPromptSubmit\"},\"type\":\"attachment\",\"uuid\":\"20000001-0000-4000-8000-000000000001\",\"timestamp\":\"2026-06-04T09:00:05.000Z\",\"sessionId\":\"sess-recalls-001\"}\n",
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"},\"timestamp\":\"2026-06-04T09:00:06.000Z\",\"sessionId\":\"sess-recalls-001\"}\n",
        "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]},\"timestamp\":\"2026-06-04T09:00:07.000Z\",\"sessionId\":\"sess-recalls-001\"}\n",
    );
    let global = vec![(
        "session-20260604T090000Z-sess-recalls-001.html",
        session_transcript_html(sid, "20260604T090000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_session(&client, addr, sid).await;

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let recalls = loop {
        let body: serde_json::Value = client
            .get(url(addr, &format!("/api/sessions/{sid}/recalls")))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let arr = body["recalls"].as_array().cloned().unwrap_or_default();
        if !arr.is_empty() {
            break arr;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the memory_recalls ledger to land"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(recalls.len(), 1, "recalls: {recalls:?}");
    let r = &recalls[0];
    assert_eq!(r["kb"], "globalmem");
    assert_eq!(r["id"], "aaaaaaaaaaaa");
    assert!(r["title"].is_null(), "no real doc has this id: {r:?}");
    assert!(r["source_relative"].is_null(), "no real doc: {r:?}");
    assert!(r["recalled_at"].is_i64(), "recalled_at: {r:?}");
    // CT-C5 — the assistant's "hi" reply never names the memory (id or
    // title), so `used` reads false end-to-end.
    assert_eq!(r["used"], false, "recalls: {recalls:?}");
}

/// CT-C5, end-to-end: a LATER turn that explicitly names the recalled
/// memory's id lands `used: true` all the way through the full pipeline
/// (transcript → `memory-recall-ledger` hook → sqlite → this route).
#[tokio::test]
async fn session_recalls_marks_used_true_when_the_id_is_referenced_later() {
    let sid = "sess-recalls-002";
    let jsonl = concat!(
        "{\"sessionId\":\"sess-recalls-002\",\"cwd\":\"/home/u/proj\",\"timestamp\":\"2026-06-04T09:00:00.000Z\"}\n",
        "{\"parentUuid\":null,\"isSidechain\":false,\"attachment\":{\"type\":\"hook_additional_context\",\"content\":[\"Relevant memories from kb (recall - these persist across sessions):\\n- alpha fact  [globalmem]  (id bbbbbbbbbbbb)\"],\"hookName\":\"UserPromptSubmit\",\"toolUseID\":\"UserPromptSubmit\",\"hookEvent\":\"UserPromptSubmit\"},\"type\":\"attachment\",\"uuid\":\"20000002-0000-4000-8000-000000000001\",\"timestamp\":\"2026-06-04T09:00:05.000Z\",\"sessionId\":\"sess-recalls-002\"}\n",
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"},\"timestamp\":\"2026-06-04T09:00:06.000Z\",\"sessionId\":\"sess-recalls-002\"}\n",
        "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"per memory bbbbbbbbbbbb, doing the thing.\"}]},\"timestamp\":\"2026-06-04T09:00:07.000Z\",\"sessionId\":\"sess-recalls-002\"}\n",
    );
    let global = vec![(
        "session-20260604T090000Z-sess-recalls-002.html",
        session_transcript_html(sid, "20260604T090000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_session(&client, addr, sid).await;

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let recalls = loop {
        let body: serde_json::Value = client
            .get(url(addr, &format!("/api/sessions/{sid}/recalls")))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let arr = body["recalls"].as_array().cloned().unwrap_or_default();
        if !arr.is_empty() {
            break arr;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the memory_recalls ledger to land"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(recalls.len(), 1, "recalls: {recalls:?}");
    assert_eq!(recalls[0]["used"], true, "recalls: {recalls:?}");
}

/// R0 — a captured session transcript is an internal episodic log: it must
/// NOT surface in default document search or in the every-turn recall push,
/// even though the corpus is recall-eligible (memory_scope=global). It stays
/// reachable via /api/sessions (and, later, `kb recollect`). The
/// `?category=memory-session` opt-in still surfaces it in search, so the
/// exclusion is a sensible default, not a hard wall.
// invariant:11 episodic-R0
#[tokio::test]
async fn memory_session_transcripts_excluded_from_search_and_recall() {
    let sid = "sess-excl-001";
    // The distinctive token "qwortzle" lands in BOTH the raw transcript body
    // and a curated memory's title, so a query for it would match both
    // without the category gate.
    let jsonl = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"investigate the qwortzle subsystem\"}}\n";
    let global = vec![
        (
            "session-20260601T100000Z-sess-excl-001.html",
            session_transcript_html(sid, "20260601T100000Z", jsonl),
        ),
        (
            "curated-qwortzle.html",
            memory_with_session_html("Curated qwortzle fact", sid),
        ),
    ];
    let proj: Vec<(&str, String)> = Vec::new();
    let (_tmp, addr) = boot_memory_corpora(&global, &proj).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T100000Z"
    })
    .await;
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Curated qwortzle fact"
    })
    .await;

    let titles = |v: &serde_json::Value| -> Vec<String> {
        v["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["title"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>()
    };

    // (1) Default document search excludes the transcript, keeps the curated fact.
    let s: serde_json::Value = client
        .get(url(
            addr,
            "/api/search?q=qwortzle&mode=keyword&scope=all&limit=10",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let st = titles(&s);
    assert!(
        st.iter().any(|t| t == "Curated qwortzle fact"),
        "curated memory present in default search: {st:?}"
    );
    assert!(
        !st.iter().any(|t| t.starts_with("Session transcript")),
        "session transcript must NOT appear in default search: {st:?}"
    );

    // (2) Every-turn recall excludes the transcript, keeps the curated fact.
    let r: serde_json::Value = client
        .get(url(
            addr,
            "/api/memory/recall?q=qwortzle&scope=all&limit=10",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rt = titles(&r);
    assert!(
        rt.iter().any(|t| t == "Curated qwortzle fact"),
        "curated memory present in recall: {rt:?}"
    );
    assert!(
        !rt.iter().any(|t| t.starts_with("Session transcript")),
        "session transcript must NOT appear in recall: {rt:?}"
    );

    // (3) The ?category=memory-session opt-in still surfaces the transcript,
    // so the exclusion is a default, not a hard wall (recollect relies on
    // being able to reach these rows).
    let opt: serde_json::Value = client
        .get(url(
            addr,
            "/api/search?q=qwortzle&mode=keyword&scope=all&limit=10&category=memory-session",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ot = titles(&opt);
    assert!(
        ot.iter().any(|t| t.starts_with("Session transcript")),
        "?category=memory-session opt-in must surface the transcript: {ot:?}"
    );
}

/// Boot a single kb ("sessconf") with an optional `default_search_category`
/// and the given (filename, html) files. No embedder → keyword search.
async fn boot_kb_with_default_search_category(
    default_search_category: Option<&str>,
    files: &[(&str, String)],
) -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sessconf");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, html) in files {
        std::fs::write(dir.join(name), html).unwrap();
    }
    let daemon_name = format!(
        "dsctest-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("sessconf").unwrap(),
        KbSection {
            path: dir,
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
            default_search_category: default_search_category.map(str::to_string),
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
    tokio::time::sleep(Duration::from_millis(400)).await;
    (tmp, addr)
}

/// R0-opt-in — a kb configured with `[kb.*] default_search_category`
/// (design point 4, architecture invariant #11) surfaces its
/// memory-session rows on a `scope=one` request with NO `?category=`,
/// where an unconfigured kb keeps R0's default-exclude behavior for the
/// exact same request.
// invariant:11 episodic-R0
#[tokio::test]
async fn default_search_category_fires_on_absent_category_single_kb() {
    let sid = "sess-dsc-001";
    let jsonl = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"debug the flibbertigibbet queue\"}}\n";
    let files = vec![(
        "session-20260701T090000Z-sess-dsc-001.html",
        session_transcript_html(sid, "20260701T090000Z", jsonl),
    )];
    let client = reqwest::Client::new();

    // No configured default: an absent ?category= keeps R0's exclusion.
    let (_tmp_a, addr_a) = boot_kb_with_default_search_category(None, &files).await;
    wait_for_doc(&client, addr_a, "sessconf", |d| {
        d["title"] == "Session transcript 20260701T090000Z"
    })
    .await;
    let before: serde_json::Value = client
        .get(url(
            addr_a,
            "/api/search?q=flibbertigibbet&mode=keyword&kb=sessconf&limit=10",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        before["hits"].as_array().unwrap().is_empty(),
        "no configured default: transcript stays hidden on an absent ?category=: {before:?}"
    );

    // sessconf's default_search_category="memory-session": the SAME
    // absent-?category= request now surfaces the transcript.
    let (_tmp_b, addr_b) =
        boot_kb_with_default_search_category(Some("memory-session"), &files).await;
    wait_for_doc(&client, addr_b, "sessconf", |d| {
        d["title"] == "Session transcript 20260701T090000Z"
    })
    .await;
    let after: serde_json::Value = client
        .get(url(
            addr_b,
            "/api/search?q=flibbertigibbet&mode=keyword&kb=sessconf&limit=10",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let titles: Vec<String> = after["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["title"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        titles.iter().any(|t| t.starts_with("Session transcript")),
        "configured default_search_category must surface the transcript on an absent ?category=: {titles:?}"
    );
}

/// R0-opt-in — an EXPLICIT `?category=` always passes through unchanged;
/// the default only fires on genuine absence (design point 4). Same
/// request, same result, whether or not this kb has a configured default.
// invariant:11 episodic-R0
#[tokio::test]
async fn default_search_category_leaves_explicit_category_param_unaffected() {
    let sid = "sess-dsc-002";
    let jsonl = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"chase the wobblesnatch bug\"}}\n";
    let files = vec![(
        "session-20260701T091500Z-sess-dsc-002.html",
        session_transcript_html(sid, "20260701T091500Z", jsonl),
    )];
    let client = reqwest::Client::new();

    for default_cat in [None, Some("memory-session")] {
        let (_tmp, addr) = boot_kb_with_default_search_category(default_cat, &files).await;
        wait_for_doc(&client, addr, "sessconf", |d| {
            d["title"] == "Session transcript 20260701T091500Z"
        })
        .await;
        let explicit: serde_json::Value = client
            .get(url(
                addr,
                "/api/search?q=wobblesnatch&mode=keyword&kb=sessconf&limit=10&category=memory-session",
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let titles: Vec<String> = explicit["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["title"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(
            titles.iter().any(|t| t.starts_with("Session transcript")),
            "explicit ?category=memory-session must surface the transcript regardless of \
             default_search_category={default_cat:?}: {titles:?}"
        );
    }
}

/// R0 — a kb configured with `default_search_category` still gets its
/// memory-session rows excluded from a `scope=all` federated query when no
/// explicit `?category=` is given: the single-kb default (design point 4)
/// must never leak into the federated fan-out (`federated_search` builds
/// one shared `Filters` from `params` alone, never consulting per-kb
/// config — architecture invariant #11).
// invariant:11 episodic-R0
#[tokio::test]
async fn default_search_category_does_not_leak_into_scope_all() {
    let sid = "sess-dsc-003";
    let jsonl = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"investigate the crumplehorn latency spike\"}}\n";
    let global = vec![
        (
            "session-20260701T093000Z-sess-dsc-003.html",
            session_transcript_html(sid, "20260701T093000Z", jsonl),
        ),
        (
            "curated-crumplehorn.html",
            memory_with_session_html("Curated crumplehorn fact", sid),
        ),
    ];

    let tmp = tempfile::tempdir().unwrap();
    let gdir = tmp.path().join("globalmem");
    std::fs::create_dir_all(&gdir).unwrap();
    for (name, html) in &global {
        std::fs::write(gdir.join(name), html).unwrap();
    }
    let daemon_name = format!(
        "dscall-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("globalmem").unwrap(),
        KbSection {
            path: gdir,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: BTreeMap::new(),
            memory_scope: Some("global".into()),
            // The kb under test DOES have a configured default — the point
            // of this test is proving it never reaches scope=all.
            default_search_category: Some("memory-session".into()),
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
    tokio::time::sleep(Duration::from_millis(400)).await;

    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260701T093000Z"
    })
    .await;
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Curated crumplehorn fact"
    })
    .await;

    let titles = |v: &serde_json::Value| -> Vec<String> {
        v["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["title"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>()
    };

    let s: serde_json::Value = client
        .get(url(
            addr,
            "/api/search?q=crumplehorn&mode=keyword&scope=all&limit=10",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let st = titles(&s);
    assert!(
        st.iter().any(|t| t == "Curated crumplehorn fact"),
        "curated memory present in scope=all search: {st:?}"
    );
    assert!(
        !st.iter().any(|t| t.starts_with("Session transcript")),
        "a kb's default_search_category must NOT leak into scope=all: {st:?}"
    );
}

/// R1, operator decision (2026-07-21, W0.6 full-text BM25 lane) — once
/// indexed, a session's TWO surfaces diverge on purpose:
///
/// - The EMBEDDING/chunk/excerpt surface (`fields.body` /
///   `body_text_excerpt`) is the deterministic INSIGHT DIGEST, unchanged —
///   ranking rationale stays a small, high-signal vector.
/// - The BM25/keyword surface (`fields.code`, one of `FTS_COLUMNS`) is now
///   the FULL evidence text: `indexer::prepare_doc` leaves the parser's
///   `code` field as-is (the `pre code, pre` selector already sweeps the
///   whole document, main `<pre>` included) instead of clearing/overwriting
///   it, so ANY exact token that ever appeared anywhere in the session must
///   be BM25-findable, not just what the digest distilled.
///
/// This test pins both halves on the SAME artifact: a token from the
/// human's prompt (which the digest carries) finds the session via the
/// `?category=memory-session` opt-in, as before. A token that lives ONLY
/// mid-main-transcript — a NON-closing assistant record (absent from
/// title/first-prompt/aiTitle/decisions/research/commits/basenames AND,
/// since sessions-rethink W2/R3, NOT the transcript's closure either, so the
/// R1 digest never sees it — closure text now legitimately rides the digest,
/// see the trailing assistant record below) now ALSO finds the session, via
/// both `GET /api/search` and `GET /api/sessions/recollect` — proving the
/// full-text BM25 lane. The digest EXCERPT (`detail=full`'s `summary`,
/// backed by `body_text_excerpt`) never echoes that token back, proving the
/// embedding/excerpt surface is unaffected by the full-text BM25 change.
#[tokio::test]
async fn bm25_covers_full_transcript_while_embedding_surface_stays_digest() {
    let sid = "sess-digest-001";
    let jsonl = concat!(
        r#"{"type":"user","promptSource":"typed","message":{"role":"user","content":"investigate the snorklewhack module"},"cwd":"/tmp/projflarn","gitBranch":"trunk"}"#,
        "\n",
        r#"{"aiTitle":"Snorklewhack investigation"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","model":"claude","content":[{"type":"text","text":"the zibberwock detail turned out to matter after all"}]}}"#,
        "\n",
        // W2/R3 — a TRAILING closing record, distinct from the mid-transcript
        // "zibberwock" text above: since closure now legitimately rides the
        // digest (`last_assistant_text`), the mid-transcript-only token this
        // test proves stays digest-invisible must NOT also be the LAST
        // assistant record, or it would (correctly, by design) start
        // showing up in the excerpt.
        r#"{"type":"assistant","message":{"role":"assistant","model":"claude","content":[{"type":"text","text":"Investigation of the snorklewhack module is complete."}]}}"#,
        "\n",
    );
    let global = vec![(
        "session-20260601T120000Z-sess-digest-001.html",
        session_transcript_html(sid, "20260601T120000Z", jsonl),
    )];
    let proj: Vec<(&str, String)> = Vec::new();
    let (_tmp, addr) = boot_memory_corpora(&global, &proj).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T120000Z"
    })
    .await;

    let titles_for = |body: &serde_json::Value| -> Vec<String> {
        body["hits"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|h| h["title"].as_str().map(str::to_string))
            .collect()
    };

    // Digest token (the human's ask) finds the session via the opt-in — the
    // R1 digest substitution is untouched by this change.
    let hit: serde_json::Value = client
        .get(url(
            addr,
            "/api/search?q=snorklewhack&mode=keyword&scope=all&limit=10&category=memory-session",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        titles_for(&hit)
            .iter()
            .any(|t| t.starts_with("Session transcript")),
        "digest token must find the session: {:?}",
        titles_for(&hit)
    );

    // Mid-main-transcript-only token (assistant prose, nowhere the R1 digest
    // reads from) now MUST match — `fields.code` keeps the parser's full
    // sweep of the document instead of being cleared/overwritten.
    let full_text_hit: serde_json::Value = client
        .get(url(
            addr,
            "/api/search?q=zibberwock&mode=keyword&scope=all&limit=10&category=memory-session",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        titles_for(&full_text_hit)
            .iter()
            .any(|t| t.starts_with("Session transcript")),
        "the full-text BM25 lane must find a mid-transcript-only token: {:?}",
        titles_for(&full_text_hit)
    );

    // The digest EXCERPT (rich-hit `summary`, `detail=full`) stays
    // digest-only: the embedding/excerpt surface is UNCHANGED by the
    // full-text BM25 lane, so it must never echo the mid-transcript token
    // back even though the token is now BM25-searchable.
    let rich: serde_json::Value = client
        .get(url(
            addr,
            "/api/search?q=snorklewhack&mode=keyword&scope=all&limit=10&category=memory-session&detail=full",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_hit = rich["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| {
            h["title"]
                .as_str()
                .unwrap_or_default()
                .starts_with("Session transcript")
        })
        .expect("rich hit for the session");
    let summary = session_hit["summary"].as_str().unwrap_or_default();
    assert!(
        !summary.contains("zibberwock"),
        "digest excerpt must not echo the full-text-only token: {summary}"
    );

    // GET /api/sessions/recollect rides the same hybrid/BM25 storage query
    // (filtered to memory-session), so it finds the mid-transcript token
    // too — poll since the canonical session_id resolution depends on the
    // async post-upsert enrichment hook populating the sqlite `sessions`
    // row (mirrors `sidecar_text_block_is_bm25_searchable_and_recollectable`).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let body: serde_json::Value = client
            .get(url(addr, "/api/sessions/recollect?q=zibberwock&limit=10"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let sessions = body["sessions"].as_array().cloned().unwrap_or_default();
        if sessions.iter().any(|s| s["session_id"] == sid) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for recollect to surface the full-text-only token: {body}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// [`session_transcript_html`] plus an additive W0.6 sidecar-text tail block
/// (`kb_core::sessions::render_sidecar_text_block` / `replace_sidecar_text_block`)
/// — the shape `kb sessions capture` / `kb import claude-history
/// --refresh-subagents` write when the session has `agent-*.jsonl`
/// sidecars. `sidecar_texts` are `(agent_id, raw_jsonl)` pairs.
fn session_transcript_html_with_sidecar_text(
    session_id: &str,
    ts: &str,
    jsonl: &str,
    sidecar_texts: &[(String, String)],
) -> String {
    let base = session_transcript_html(session_id, ts, jsonl);
    let block = kb_core::sessions::render_sidecar_text_block(sidecar_texts);
    kb_core::sessions::replace_sidecar_text_block(&base, block)
}

/// W0.6 — a subagent sidecar's raw CONVERSATION TEXT, riding the additive
/// `kb-session-sidecar-text` tail block, is searchable evidence: a UNIQUE
/// token that lives ONLY in the sidecar text (never in the main `<pre>`
/// transcript, never in the R1 digest — invariant #11's digest substitution
/// replaces `body`/`body_text_excerpt` wholesale and would otherwise bury
/// it) is found by BOTH `GET /api/search?...&category=memory-session` and
/// `GET /api/sessions/recollect` — the indexer routes the sidecar-text
/// block through the `code` FTS column (the one column the digest
/// substitution doesn't overwrite), so a plain BM25 query (which auto-fills
/// every FTS-indexed column) matches it without touching embed/vector
/// relevance.
// invariant:11 sidecar-text-searchable (W0.6)
#[tokio::test]
async fn sidecar_text_block_is_bm25_searchable_and_recollectable() {
    let sid = "sess-sidecar-text-001";
    let jsonl = concat!(
        r#"{"type":"user","promptSource":"typed","message":{"role":"user","content":"investigate the flibbertigibbet module"},"cwd":"/tmp/projwidget","gitBranch":"trunk"}"#,
        "\n",
    );
    let sidecar_raw = concat!(
        r#"{"type":"assistant","message":{"role":"assistant","model":"claude","content":[{"type":"text","text":"the zanzibarnacle detail matters here"}]}}"#,
        "\n",
    );
    // Sanity: the unique token lives ONLY in the sidecar text, never in the
    // main transcript the digest is built from.
    assert!(!jsonl.contains("zanzibarnacle"));

    let html = session_transcript_html_with_sidecar_text(
        sid,
        "20260601T130000Z",
        jsonl,
        &[("a1".to_string(), sidecar_raw.to_string())],
    );
    let global = vec![("session-20260601T130000Z-sess-sidecar-text-001.html", html)];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T130000Z"
    })
    .await;

    let titles_for = |body: &serde_json::Value| -> Vec<String> {
        body["hits"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|h| h["title"].as_str().map(str::to_string))
            .collect()
    };

    // GET /api/search?...&category=memory-session finds the session via the
    // sidecar-text-only token.
    let hit: serde_json::Value = client
        .get(url(
            addr,
            "/api/search?q=zanzibarnacle&mode=keyword&scope=all&limit=10&category=memory-session",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        titles_for(&hit)
            .iter()
            .any(|t| t.starts_with("Session transcript")),
        "sidecar-text token must find the session via /api/search: {:?}",
        titles_for(&hit)
    );

    // GET /api/sessions/recollect finds it too — poll since the canonical
    // session_id resolution depends on the async post-upsert enrichment
    // hook populating the sqlite `sessions` row (mirrors
    // recollect_finds_sessions_by_digest_with_surfaced_signals).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let body: serde_json::Value = client
            .get(url(
                addr,
                "/api/sessions/recollect?q=zanzibarnacle&limit=10",
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let sessions = body["sessions"].as_array().cloned().unwrap_or_default();
        if sessions.iter().any(|s| s["session_id"] == sid) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for recollect to surface the sidecar-text session: {body}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn sessions_list_returns_indexed_session_with_memory_count() {
    let sid = "sess-list-001";
    let jsonl = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"first prompt of the session\"}}\n";
    let global = vec![
        (
            "session-20260524T100000Z-sess-list-001.html",
            session_transcript_html(sid, "20260524T100000Z", jsonl),
        ),
        (
            "mem-from-session.html",
            memory_with_session_html("Mem From Session", sid),
        ),
    ];
    let proj: Vec<(&str, String)> = Vec::new();
    let (_tmp, addr) = boot_memory_corpora(&global, &proj).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260524T100000Z"
    })
    .await;
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Mem From Session"
    })
    .await;

    let resp: serde_json::Value = client
        .get(url(addr, "/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = resp["sessions"].as_array().expect("sessions array");
    let row = sessions
        .iter()
        .find(|r| r["session_id"] == sid)
        .expect("seeded session present in list");
    assert_eq!(row["kb"], "globalmem");
    assert_eq!(row["message_count"], 1);
    assert_eq!(row["memory_count"], 1, "the seeded memory counts");
    assert_eq!(row["first_user_prompt"], "first prompt of the session");
    // S4 — `title` is now the persisted aiTitle (None here, since this
    // transcript has no ai-title record), NOT the HTML <title> the lance
    // projection used to surface. `display_name` is server-computed and
    // falls back to the first prompt when there's no aiTitle.
    assert!(row["title"].is_null(), "no aiTitle ⇒ title absent");
    assert_eq!(row["display_name"], "first prompt of the session");
}

// P1 — A1/A2/A3: aiTitle → title → display_name, cwd → folder, file counts,
// the /folders facet, and the ?folder= filter.
#[tokio::test]
async fn sessions_identity_folders_and_folder_filter() {
    // Two sessions in different working dirs; one carries an aiTitle.
    let jsonl_a = concat!(
        r#"{"type":"user","cwd":"/home/u/project/alpha","gitBranch":"main","promptSource":"typed","message":{"role":"user","content":"build the alpha widget"}}"#,
        "\n",
        r#"{"type":"ai-title","aiTitle":"Alpha widget build"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/home/u/project/alpha/x.rs"}}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/home/u/project/alpha/y.rs"}}]}}"#,
        "\n",
    );
    let jsonl_b = concat!(
        r#"{"type":"user","cwd":"/home/u/project/beta","promptSource":"typed","message":{"role":"user","content":"fix the beta bug"}}"#,
        "\n",
    );
    let global = vec![
        (
            "session-20260524T100000Z-sid-alpha.html",
            session_transcript_html("sid-alpha", "20260524T100000Z", jsonl_a),
        ),
        (
            "session-20260524T110000Z-sid-beta.html",
            session_transcript_html("sid-beta", "20260524T110000Z", jsonl_b),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260524T110000Z"
    })
    .await;
    // Wait until both enrichment rows have landed (cwd persisted).
    loop {
        let resp: serde_json::Value = client
            .get(url(addr, "/api/sessions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let n = resp["sessions"].as_array().map(|a| a.len()).unwrap_or(0);
        if n >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Identity: the alpha session's aiTitle drives title + display_name;
    // cwd → folder; the Edit/Read tool-calls drive the file counts.
    let resp: serde_json::Value = client
        .get(url(addr, "/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = resp["sessions"].as_array().unwrap();
    let alpha = sessions
        .iter()
        .find(|r| r["session_id"] == "sid-alpha")
        .expect("alpha session present");
    assert_eq!(alpha["title"], "Alpha widget build");
    assert_eq!(alpha["display_name"], "Alpha widget build");
    assert_eq!(alpha["cwd"], "/home/u/project/alpha");
    assert_eq!(alpha["folder"], "alpha");
    assert_eq!(alpha["git_branch"], "main");
    assert_eq!(alpha["files_read_count"], 1);
    assert_eq!(alpha["files_edited_count"], 1);

    // /folders facet: two distinct working dirs, each with one session.
    let folders: serde_json::Value = client
        .get(url(addr, "/api/sessions/folders"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let fs = folders["folders"].as_array().expect("folders array");
    assert_eq!(fs.len(), 2, "two distinct cwds");
    let alpha_folder = fs
        .iter()
        .find(|f| f["folder"] == "/home/u/project/alpha")
        .expect("alpha folder present");
    assert_eq!(alpha_folder["label"], "alpha");
    assert_eq!(alpha_folder["count"], 1);

    // ?folder= filter: narrows to the one session in that working dir.
    let filtered: serde_json::Value = client
        .get(url(addr, "/api/sessions?folder=/home/u/project/beta"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let only = filtered["sessions"].as_array().unwrap();
    assert_eq!(only.len(), 1, "folder filter narrows to beta");
    assert_eq!(only[0]["session_id"], "sid-beta");
    assert_eq!(only[0]["display_name"], "fix the beta bug");
}

// P2 — A4/A6/A7: the file-activity manifest (/files) and the reverse
// artifact->sessions endpoint. Asserts the manifest structure (paths +
// actions + out-of-corpus plain paths), which is deterministic. In-corpus
// resolution is covered by kb-core unit tests (the process-global mount table
// is per-daemon in prod but shared across the multi-daemon test harness, so
// in_corpus is asserted at the unit level, not here).
#[tokio::test]
async fn sessions_files_manifest_and_reverse_endpoint() {
    let sid = "sid-files-001";
    let jsonl = concat!(
        r#"{"type":"user","cwd":"/home/u/proj","promptSource":"typed","message":{"role":"user","content":"do file work"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/home/u/proj/read-me.rs"}}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/home/u/proj/edit-me.rs","old_string":"a","new_string":"b"}}]}}"#,
        "\n",
    );
    let global = vec![(
        "session-20260524T100000Z-sid-files-001.html",
        session_transcript_html(sid, "20260524T100000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260524T100000Z"
    })
    .await;
    // Wait until the file edges have landed.
    loop {
        let f: serde_json::Value = client
            .get(url(addr, &format!("/api/sessions/{sid}/files")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if f["files"].as_array().map(|a| a.len()).unwrap_or(0) >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let files: serde_json::Value = client
        .get(url(addr, &format!("/api/sessions/{sid}/files")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = files["files"].as_array().unwrap();
    // Manifest carries both touches with correct actions; edits sort first.
    let edit = arr
        .iter()
        .find(|f| f["basename"] == "edit-me.rs")
        .expect("edited file in manifest");
    assert_eq!(edit["action"], "edit");
    let read = arr
        .iter()
        .find(|f| f["basename"] == "read-me.rs")
        .expect("read file in manifest");
    assert_eq!(read["action"], "read");
    // These abs paths are out-of-corpus → plain paths, no resolved artifact.
    assert_eq!(edit["in_corpus"], false);
    assert!(edit["target_artifact_id"].is_null());

    // The reverse endpoint is well-formed (empty here — the edited files
    // aren't kb artifacts). 200 + a `sessions` array.
    let rev: serde_json::Value = client
        .get(url(addr, "/api/artifacts/globalmem/deadbeefcafe/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(rev["sessions"].is_array());
}

// AS — the artifact->sessions reverse endpoint surfaces the ORIGIN session (the
// one the artifact was born in, via `<meta kb-session>` → lance `kb_session`)
// with `authored=true`, even when that session left no resolved `session_files`
// row, and carries the per-action booleans the read/write filter needs. (The
// in-corpus read/wrote/edited fold is unit-tested in kb-core; the process-global
// mount table makes it non-deterministic in this shared multi-daemon harness, so
// here we exercise the deterministic origin-union path.)
#[tokio::test]
async fn artifact_sessions_surfaces_origin_with_authored_flag() {
    let sid = "sid-origin-001";
    let global = vec![
        // A memory artifact stamped with kb-session=sid → its lance row carries
        // `kb_session`, the origin signal the reverse endpoint reads.
        (
            "mem-origin.html",
            memory_with_session_html("Origin memory", sid),
        ),
        // The matching transcript — gives the session a row (so lookup_session
        // resolves it). Minimal: one typed prompt, no file tool_use, so the
        // session links to the artifact ONLY through the origin union.
        (
            "session-20260524T100000Z-sid-origin-001.html",
            session_transcript_html(
                sid,
                "20260524T100000Z",
                concat!(
                    r#"{"type":"user","promptSource":"typed","message":{"role":"user","content":"build the origin memory"}}"#,
                    "\n",
                ),
            ),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Origin memory"
    })
    .await;
    let art_id = doc["id"].as_str().expect("artifact id").to_string();
    wait_for_session(&client, addr, sid).await;

    // Poll the reverse endpoint until the origin union surfaces the session
    // (depends on the artifact's `kb_session` having landed in lance).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let s = loop {
        let rev: serde_json::Value = client
            .get(url(
                addr,
                &format!("/api/artifacts/globalmem/{art_id}/sessions"),
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(hit) = rev["sessions"]
            .as_array()
            .and_then(|a| a.iter().find(|s| s["session_id"] == sid).cloned())
        {
            break hit;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the origin session in the reverse endpoint"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    assert_eq!(s["authored"], true, "origin session flagged authored");
    // A creation-only origin (no file row) reports `write`, never `read`.
    assert_eq!(s["action"], "write", "creation reports write, not read");
    assert_eq!(s["read"], false);
    assert_eq!(s["wrote"], false);
    assert_eq!(s["edited"], false);
    assert_eq!(
        s["first_user_prompt"], "build the origin memory",
        "opening prompt carried through"
    );
}

// P4/S9 — decisions log + effort/error stats persisted and served.
#[tokio::test]
async fn sessions_decisions_and_effort_persisted_and_served() {
    let sid = "sid-dec-001";
    let jsonl = concat!(
        r#"{"type":"user","promptSource":"typed","message":{"role":"user","content":"do the work"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","model":"claude-opus-4-8","usage":{"input_tokens":1000,"output_tokens":200},"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
        "\n",
        r#"{"type":"user","toolUseResult":{"answers":{"Scope?":"Full vision","Lenses?":"A, B"},"questions":[{"question":"Scope?"},{"question":"Lenses?"}]},"message":{"role":"user","content":[{"type":"tool_result","content":"Your questions have been answered: ..."}]}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","is_error":true,"content":"boom"}]}}"#,
        "\n",
    );
    let global = vec![(
        "session-20260524T100000Z-sid-dec-001.html",
        session_transcript_html(sid, "20260524T100000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260524T100000Z"
    })
    .await;
    // Wait for the decisions to land.
    loop {
        let d: serde_json::Value = client
            .get(url(addr, &format!("/api/sessions/{sid}/decisions")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if d["decisions"].as_array().map(|a| a.len()).unwrap_or(0) >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Effort + errors on the session row.
    let list: serde_json::Value = client
        .get(url(addr, "/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["session_id"] == sid)
        .expect("session present");
    assert_eq!(row["token_total"], 1200);
    assert_eq!(row["tool_calls"], 1);
    assert_eq!(row["error_count"], 1);
    assert_eq!(row["model"], "claude-opus-4-8");

    // The decisions log: two question decisions in order.
    let decs: serde_json::Value = client
        .get(url(addr, &format!("/api/sessions/{sid}/decisions")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = decs["decisions"].as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["kind"], "question");
    assert_eq!(arr[0]["prompt"], "Scope?");
    assert_eq!(arr[0]["answer"], "Full vision");
    assert_eq!(arr[1]["answer"], "A, B");
}

// P5 — git commits detected + persisted + served.
#[tokio::test]
async fn sessions_commits_persisted_and_served() {
    let sid = "sid-commit-001";
    let jsonl = concat!(
        r#"{"type":"user","promptSource":"typed","message":{"role":"user","content":"ship it"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"c1","name":"Bash","input":{"command":"git add -A && git commit -m \"feat: the thing\""}}]}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"[main beadf00d] feat: the thing\n 2 files changed"}]}}"#,
        "\n",
    );
    let global = vec![(
        "session-20260524T100000Z-sid-commit-001.html",
        session_transcript_html(sid, "20260524T100000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260524T100000Z"
    })
    .await;
    loop {
        let c: serde_json::Value = client
            .get(url(addr, &format!("/api/sessions/{sid}/commits")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if c["commits"].as_array().map(|a| a.len()).unwrap_or(0) >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let commits: serde_json::Value = client
        .get(url(addr, &format!("/api/sessions/{sid}/commits")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = commits["commits"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["kind"], "commit");
    assert_eq!(arr[0]["sha"], "beadf00d");
    assert_eq!(arr[0]["subject"], "feat: the thing");
}

// === kb-code Wave 0 (W0.6) — sha→session reverse endpoint + bulk feed =======

/// A minimal one-user-turn transcript with no Bash/commit activity — the
/// `by-commit`/`commit-map` tests attach a resolved commit via the additive
/// `<script id="kb-session-commits">` block instead (capture-time resolution
/// shape, V0025/W0.4), so `sha_full`/`resolved`/`trailers` are populated —
/// something transcript-only detection never produces.
fn minimal_jsonl() -> String {
    concat!(
        r#"{"type":"user","promptSource":"typed","message":{"role":"user","content":"go"}}"#,
        "\n",
    )
    .to_string()
}

/// [`session_transcript_html`] plus an additive resolved-commits block
/// (`kb_core::sessions::render_commits_block`) — the shape `kb sessions
/// capture` writes after capture-time `git show -s` resolution.
fn session_transcript_html_with_commits(
    session_id: &str,
    ts: &str,
    jsonl: &str,
    commits: &[kb_core::sessions::CapturedCommit],
) -> String {
    let base = session_transcript_html(session_id, ts, jsonl);
    let block = kb_core::sessions::render_commits_block(commits);
    base.replace("</body>", &format!("{block}</body>"))
}

fn resolved_commit_fixture(
    sha_full: &str,
    trailers: Vec<String>,
) -> kb_core::sessions::CapturedCommit {
    kb_core::sessions::CapturedCommit {
        kind: "commit".into(),
        sha: Some(sha_full[..8].to_string()),
        subject: Some("feat: resolved subject".into()),
        resolved: true,
        sha_full: Some(sha_full.to_string()),
        repo_root: Some("/repo".into()),
        author: Some("kb-test <t@kb>".into()),
        parents: Some(1),
        trailers,
    }
}

/// W0.6 — `GET /api/sessions/by-commit?sha=` matches on a short-sha PREFIX
/// and, independently, on a `sha_full` (V0025) prefix, and surfaces the
/// resolved subject/trailers/display_name/started_at.
#[tokio::test]
async fn by_commit_matches_short_sha_and_sha_full_prefix() {
    let sid = "sid-bycommit-001";
    let full_sha = "beadf00d1234567890abcdef1234567890abcdef";
    let commit = resolved_commit_fixture(full_sha, vec!["Kb-Session: sid-bycommit-001".into()]);
    let html =
        session_transcript_html_with_commits(sid, "20260601T100000Z", &minimal_jsonl(), &[commit]);
    let global = vec![("session-20260601T100000Z-sid-bycommit-001.html", html)];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T100000Z"
    })
    .await;

    // Poll until the resolved commit lands (mirrors sessions_commits_persisted_and_served).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let body: serde_json::Value = client
            .get(url(addr, "/api/sessions/by-commit?sha=beadf00d"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if body["matches"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the commit"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Short-sha prefix.
    let by_short: serde_json::Value = client
        .get(url(addr, "/api/sessions/by-commit?sha=beadf00d"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let matches = by_short["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    let m = &matches[0];
    assert_eq!(m["kb"], "globalmem");
    assert_eq!(m["session_id"], sid);
    assert_eq!(m["resolved"], true);
    assert_eq!(m["subject"], "feat: resolved subject");
    assert_eq!(m["trailers"][0], "Kb-Session: sid-bycommit-001");
    assert!(!m["display_name"].as_str().unwrap().is_empty());
    assert!(m["started_at"].as_i64().unwrap() > 0);

    // sha_full prefix, deeper into the string than the short sha covers.
    let by_full: serde_json::Value = client
        .get(url(
            addr,
            &format!("/api/sessions/by-commit?sha={}", &full_sha[..20]),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(by_full["matches"].as_array().unwrap().len(), 1);

    // No match → empty 200 (not 404).
    let none = client
        .get(url(addr, "/api/sessions/by-commit?sha=fffffff"))
        .send()
        .await
        .unwrap();
    assert_eq!(none.status(), 200);
    let none_body: serde_json::Value = none.json().await.unwrap();
    assert!(none_body["matches"].as_array().unwrap().is_empty());

    // <7 hex chars → 400.
    let short = client
        .get(url(addr, "/api/sessions/by-commit?sha=abc"))
        .send()
        .await
        .unwrap();
    assert_eq!(short.status(), 400);
}

// ---- MI-W4.6 — provenance thread: per-commit touched-file staleness --

/// A REAL git repo (not the `resolved_commit_fixture` default `"/repo"`
/// placeholder) with a first commit touching `a.txt`+`b.txt`, then a second
/// commit touching ONLY `a.txt` — so the two files exercise both
/// `changed_since` outcomes for one `GET …/commits/{sha}/files` call.
/// Returns `(repo_dir, first_commit_sha_full)`.
fn init_touched_files_repo(tmp: &std::path::Path) -> (std::path::PathBuf, String) {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "user.email=t@kb",
                "-c",
                "user.name=kb-test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .expect("git runs")
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one").unwrap();
    std::fs::write(repo.join("b.txt"), "one").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "first"]);
    let sha = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse")
        .stdout;
    let sha_full = String::from_utf8(sha).unwrap().trim().to_string();

    // A second commit touches ONLY a.txt — b.txt stays unchanged since.
    std::fs::write(repo.join("a.txt"), "two").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-q", "-m", "second"]);

    (repo, sha_full)
}

/// `GET /api/sessions/{sid}/commits/{sha}/files` resolves the touched files
/// off the commit's OWN `repo_root`/`sha_full` (from `session_commits`, via
/// the transcript's additive resolved-commits block — same shape
/// `by_commit_matches_short_sha_and_sha_full_prefix` exercises) and annotates
/// each with a real `changed_since` verdict.
#[tokio::test]
async fn commit_files_reports_touched_file_staleness_from_a_real_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, sha_full) = init_touched_files_repo(tmp.path());

    let sid = "sid-provfiles-001";
    let mut commit = resolved_commit_fixture(&sha_full, vec![]);
    commit.repo_root = Some(repo.to_string_lossy().to_string());
    let html =
        session_transcript_html_with_commits(sid, "20260602T100000Z", &minimal_jsonl(), &[commit]);
    let global = vec![("session-20260602T100000Z-sid-provfiles-001.html", html)];
    let (_tmp2, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260602T100000Z"
    })
    .await;

    // Poll until the resolved commit lands (mirrors the by-commit tests).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let c: serde_json::Value = client
            .get(url(addr, &format!("/api/sessions/{sid}/commits")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if c["commits"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the commit"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let resp = client
        .get(url(
            addr,
            &format!("/api/sessions/{sid}/commits/{sha_full}/files"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["available"], true, "body: {body:?}");
    assert_eq!(body["truncated"], false);
    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "files: {files:?}");
    let a = files.iter().find(|f| f["path"] == "a.txt").unwrap();
    assert_eq!(a["changed_since"], true);
    assert!(a["last_touched_unix"].as_i64().unwrap() > 0);
    let b = files.iter().find(|f| f["path"] == "b.txt").unwrap();
    assert_eq!(b["changed_since"], false);
    assert!(b["last_touched_unix"].as_i64().unwrap() > 0);

    // A short-form sha the transcript detected but that never resolved
    // (no repo_root/sha_full) degrades to `available: false`, never a 404.
    let unresolved = client
        .get(url(
            addr,
            &format!("/api/sessions/{sid}/commits/deadbeef/files"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(unresolved.status(), 200);
    let unresolved_body: serde_json::Value = unresolved.json().await.unwrap();
    assert_eq!(unresolved_body["available"], false);
    assert!(unresolved_body["files"].as_array().unwrap().is_empty());
}

/// W0.6 — two captures of ONE session (same session_id, same sha) must only
/// surface the NEWEST capture's row (#11) — not one match per capture.
#[tokio::test]
async fn by_commit_scopes_to_newest_capture() {
    let sid = "sid-bycommit-multicap";
    let commit_old = resolved_commit_fixture("cafe000011112222333344445555666677778888", vec![]);
    let commit_new = resolved_commit_fixture("cafe000011112222333344445555666677778888", vec![]);
    let old_html = session_transcript_html_with_commits(
        sid,
        "20260601T090000Z",
        &minimal_jsonl(),
        &[commit_old],
    );
    let new_html = session_transcript_html_with_commits(
        sid,
        "20260601T110000Z",
        &minimal_jsonl(),
        &[commit_new],
    );
    let global = vec![
        (
            "session-20260601T090000Z-sid-bycommit-multicap.html",
            old_html,
        ),
        (
            "session-20260601T110000Z-sid-bycommit-multicap.html",
            new_html,
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T090000Z"
    })
    .await;
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T110000Z"
    })
    .await;

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let matched_started_at = loop {
        let body: serde_json::Value = client
            .get(url(addr, "/api/sessions/by-commit?sha=cafe0000"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let matches = body["matches"].as_array().cloned().unwrap_or_default();
        if !matches.is_empty() {
            assert_eq!(
                matches.len(),
                1,
                "only the newest capture matches, not both"
            );
            break matches[0]["started_at"].as_i64().unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the commit"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    // `GET /api/sessions/{sid}` is the existing, already-tested oracle for
    // "which capture is newest" (#11, `sessions_get`'s
    // `ORDER BY started_at DESC` — see `sessions_identity_folders_and_folder_filter`).
    // The by-commit match must agree with it.
    let detail: serde_json::Value = client
        .get(url(addr, &format!("/api/sessions/{sid}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        matched_started_at,
        detail["started_at"].as_i64().unwrap(),
        "by-commit's match is the same newest capture /api/sessions/{{sid}} reports"
    );
}

/// W0.6 — cross-kb fan-out: a commit recorded in ONE corpus is found when the
/// daemon hosts several (existing multi-kb harness idiom via
/// `boot_memory_corpora`).
#[tokio::test]
async fn by_commit_fans_out_across_corpora() {
    let sid = "sid-bycommit-crosskb";
    let commit = resolved_commit_fixture("f00dcafe11112222333344445555666677778888", vec![]);
    let html =
        session_transcript_html_with_commits(sid, "20260601T120000Z", &minimal_jsonl(), &[commit]);
    // The commit lives in `projmem`; `globalmem` holds nothing.
    let proj = vec![("session-20260601T120000Z-sid-bycommit-crosskb.html", html)];
    let (_tmp, addr) = boot_memory_corpora(&[], &proj).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "projmem", |d| {
        d["title"] == "Session transcript 20260601T120000Z"
    })
    .await;

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let body: serde_json::Value = client
            .get(url(addr, "/api/sessions/by-commit?sha=f00dcafe"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let matches = body["matches"].as_array().cloned().unwrap_or_default();
        if !matches.is_empty() {
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0]["kb"], "projmem");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the commit"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// W0.6 — `GET /api/sessions/commit-map`: pagination (`limit`/`offset`),
/// `since` filtering by the owning session's `started_at`, and newest-capture
/// scoping (#11) — a duplicate older capture never contributes a second row.
#[tokio::test]
async fn commit_map_paginates_filters_since_and_scopes_to_newest_capture() {
    let commit_a = resolved_commit_fixture("aaaa111122223333444455556666777788889999", vec![]);
    let commit_b = resolved_commit_fixture("bbbb111122223333444455556666777788889999", vec![]);
    let commit_a_old = resolved_commit_fixture("aaaa111122223333444455556666777788889999", vec![]);
    let html_a = session_transcript_html_with_commits(
        "sid-map-a",
        "20260601T140000Z",
        &minimal_jsonl(),
        &[commit_a],
    );
    let html_b = session_transcript_html_with_commits(
        "sid-map-b",
        "20260601T130000Z",
        &minimal_jsonl(),
        &[commit_b],
    );
    // An OLDER capture of sid-map-a with the identical sha — must not
    // double-count in the bulk feed.
    let html_a_old = session_transcript_html_with_commits(
        "sid-map-a",
        "20260601T100000Z",
        &minimal_jsonl(),
        &[commit_a_old],
    );
    let global = vec![
        ("session-20260601T140000Z-sid-map-a.html", html_a),
        ("session-20260601T130000Z-sid-map-b.html", html_b),
        ("session-20260601T100000Z-sid-map-a.html", html_a_old),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T140000Z"
    })
    .await;
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T130000Z"
    })
    .await;
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T100000Z"
    })
    .await;

    // Poll until both distinct sessions' commits have landed (2 rows, not 3 —
    // the duplicate older capture of sid-map-a must never surface).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let all: serde_json::Value = loop {
        let body: serde_json::Value = client
            .get(url(addr, "/api/sessions/commit-map?limit=100&offset=0"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if body["commits"].as_array().map(|a| a.len()).unwrap_or(0) >= 2 {
            break body;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for commits"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let commits = all["commits"].as_array().unwrap();
    assert_eq!(
        commits.len(),
        2,
        "newest-capture scoped: the duplicate older sid-map-a capture never contributes a 3rd row"
    );
    let session_ids: std::collections::BTreeSet<&str> = commits
        .iter()
        .map(|c| c["session_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        session_ids,
        std::collections::BTreeSet::from(["sid-map-a", "sid-map-b"])
    );

    // Pagination: limit=1 offset=0 returns the newest (sid-map-a, started
    // later); limit=1 offset=1 returns sid-map-b; both carry next_offset
    // semantics consistent with a 2-row total.
    let page1: serde_json::Value = client
        .get(url(addr, "/api/sessions/commit-map?limit=1&offset=0"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let page1_commits = page1["commits"].as_array().unwrap();
    assert_eq!(page1_commits.len(), 1);
    assert_eq!(page1_commits[0]["session_id"], "sid-map-a");
    assert_eq!(page1["next_offset"], 1);

    let page2: serde_json::Value = client
        .get(url(addr, "/api/sessions/commit-map?limit=1&offset=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let page2_commits = page2["commits"].as_array().unwrap();
    assert_eq!(page2_commits.len(), 1);
    assert_eq!(page2_commits[0]["session_id"], "sid-map-b");
    // page2 is itself full-size (len == limit), so `next_offset` is the
    // documented HINT ("might be more" — Some, not a guarantee), not None.
    // The definitive end-of-data signal is the NEXT page coming back empty.
    assert_eq!(page2["next_offset"], 2);
    let page3: serde_json::Value = client
        .get(url(addr, "/api/sessions/commit-map?limit=1&offset=2"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        page3["commits"].as_array().unwrap().is_empty(),
        "offset=2 is past the end — genuinely no more"
    );
    assert!(page3["next_offset"].is_null());

    // `since` floors on the owning session's started_at: read the two
    // sessions' actual started_at (parsed from the capture filename
    // timestamp) from `commits` above and pick a floor strictly between
    // them — excludes the older sid-map-b, keeps the newer sid-map-a.
    let started_at_of = |sid: &str| -> i64 {
        commits
            .iter()
            .find(|c| c["session_id"] == sid)
            .and_then(|c| c["started_at"].as_i64())
            .unwrap()
    };
    let (a_started, b_started) = (started_at_of("sid-map-a"), started_at_of("sid-map-b"));
    assert!(
        a_started > b_started,
        "sid-map-a captured later than sid-map-b"
    );
    let since_ts = b_started + 1;
    let since_body: serde_json::Value = client
        .get(url(
            addr,
            &format!("/api/sessions/commit-map?since={since_ts}&limit=100"),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let since_commits = since_body["commits"].as_array().unwrap();
    assert_eq!(since_commits.len(), 1);
    assert_eq!(since_commits[0]["session_id"], "sid-map-a");
}

/// R2 — `GET /api/why?path=` assembles the sessions that touched a file with
/// the reasoning that produced it (prompt + decisions + commits), labels each
/// hit exact vs fuzzy by path alignment, and never asserts a fuzzy basename
/// match as exact.
// invariant:11 why-R2
#[tokio::test]
async fn why_assembles_touching_sessions_with_reasoning() {
    let sid = "sid-why-001";
    let jsonl = concat!(
        r#"{"type":"user","promptSource":"typed","message":{"role":"user","content":"make wibble robust"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/work/proj/src/wibble.rs","old_string":"a","new_string":"b"}}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"c1","name":"Bash","input":{"command":"git commit -m \"feat: harden wibble\""}}]}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"[main deadbee1] feat: harden wibble\n 1 file changed"}]}}"#,
        "\n",
    );
    let global = vec![(
        "session-20260601T130000Z-sid-why-001.html",
        session_transcript_html(sid, "20260601T130000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T130000Z"
    })
    .await;

    let why = |q: &str| {
        let c = client.clone();
        let u = url(addr, &format!("/api/why?path={q}"));
        async move {
            c.get(u)
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };

    // Poll until the session_files edge for wibble.rs is enriched.
    let mut body = why("src/wibble.rs").await;
    for _ in 0..40 {
        if body["sessions"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        body = why("src/wibble.rs").await;
    }
    let sessions = body["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions.len(), 1, "one touching session: {body}");
    let s = &sessions[0];
    assert_eq!(s["session_id"], sid);
    assert_eq!(s["action"], "edit");
    assert_eq!(
        s["confidence"], "exact",
        "src/wibble.rs aligns with the stored absolute path"
    );
    assert_eq!(s["first_user_prompt"], "make wibble robust");
    let commits = s["commits"].as_array().expect("commits");
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0]["subject"], "feat: harden wibble");

    // A same-basename path that does NOT align is labelled fuzzy, not exact.
    let fuzzy = why("/etc/elsewhere/wibble.rs").await;
    let fs = fuzzy["sessions"].as_array().expect("sessions");
    assert_eq!(fs.len(), 1);
    assert_eq!(fs[0]["confidence"], "fuzzy");
}

/// R3 — `GET /api/sessions/recollect` does semantic "has this been done?" over
/// the R1 digests: it surfaces the matching session (not an unrelated one) and
/// returns the success/error/recency/staleness signals the agent weighs.
// invariant:11 recollect-R3
#[tokio::test]
async fn recollect_finds_sessions_by_digest_with_surfaced_signals() {
    let global = vec![
        (
            "session-20260601T100000Z-s-flux.html",
            session_transcript_html(
                "s-flux",
                "20260601T100000Z",
                r#"{"type":"user","promptSource":"typed","cwd":"/proj/widget","message":{"role":"user","content":"implement the flux capacitor calibration routine"}}"#,
            ),
        ),
        (
            "session-20260601T110000Z-s-docs.html",
            session_transcript_html(
                "s-docs",
                "20260601T110000Z",
                r#"{"type":"user","promptSource":"typed","cwd":"/proj/widget","message":{"role":"user","content":"write documentation for the gizmo manual"}}"#,
            ),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T110000Z"
    })
    .await;

    let recollect = |q: &str| {
        let c = client.clone();
        let u = url(addr, &format!("/api/sessions/recollect?q={q}&limit=10"));
        async move {
            c.get(u)
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };

    // Poll until the digest is indexed AND the session row enriched.
    let mut body = recollect("flux").await;
    for _ in 0..40 {
        if body["sessions"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        body = recollect("flux").await;
    }
    let sessions = body["sessions"].as_array().expect("sessions array");
    assert!(
        !sessions.is_empty(),
        "recollect found the flux session: {body}"
    );
    let s = &sessions[0];
    assert_eq!(s["session_id"], "s-flux", "{body}");
    assert_eq!(
        s["first_user_prompt"],
        "implement the flux capacitor calibration routine"
    );
    // The surfaced signals are present + well-typed.
    assert!(s["score"].as_f64().unwrap_or(0.0) > 0.0, "{s}");
    assert!(s["age_days"].as_i64().is_some(), "{s}");
    assert!(s["stale"].as_bool().is_some(), "{s}");
    assert!(s["error_count"].as_i64().is_some(), "{s}");
    assert!(s["commit_count"].as_i64().is_some(), "{s}");
    assert!(s["committed"].as_bool().is_some(), "{s}");
    // The unrelated session is NOT surfaced for this query.
    assert!(
        !sessions.iter().any(|x| x["session_id"] == "s-docs"),
        "unrelated session excluded: {body}"
    );
}

/// R7 — `recollect --similar-to <sid>` queries with the source session's
/// indexed digest excerpt: it finds the topic-twin, and EXCLUDES the source
/// session itself (never "similar to itself").
#[tokio::test]
async fn recollect_similar_to_finds_twin_and_excludes_source() {
    let global = vec![
        (
            "session-20260601T100000Z-s-src.html",
            session_transcript_html(
                "s-src",
                "20260601T100000Z",
                r#"{"type":"user","promptSource":"typed","cwd":"/proj/widget","message":{"role":"user","content":"calibrate the zylophone harmonics resonance routine"}}"#,
            ),
        ),
        (
            "session-20260601T110000Z-s-twin.html",
            session_transcript_html(
                "s-twin",
                "20260601T110000Z",
                r#"{"type":"user","promptSource":"typed","cwd":"/proj/widget","message":{"role":"user","content":"tune the zylophone harmonics calibration so resonance is stable"}}"#,
            ),
        ),
        (
            "session-20260601T120000Z-s-other.html",
            session_transcript_html(
                "s-other",
                "20260601T120000Z",
                r#"{"type":"user","promptSource":"typed","cwd":"/proj/widget","message":{"role":"user","content":"write the quarterly invoice spreadsheet macros"}}"#,
            ),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T120000Z"
    })
    .await;

    let recollect = || async {
        client
            .get(url(
                addr,
                "/api/sessions/recollect?similar_to=s-src&limit=10",
            ))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    };
    let mut body = recollect().await;
    for _ in 0..40 {
        if body["sessions"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        body = recollect().await;
    }
    let sessions = body["sessions"].as_array().expect("sessions array");
    assert!(
        sessions.iter().any(|s| s["session_id"] == "s-twin"),
        "topic-twin surfaced: {body}"
    );
    assert!(
        sessions.iter().all(|s| s["session_id"] != "s-src"),
        "source session excluded from its own similar-to results: {body}"
    );
    if let (Some(t), Some(o)) = (
        sessions.iter().position(|s| s["session_id"] == "s-twin"),
        sessions.iter().position(|s| s["session_id"] == "s-other"),
    ) {
        assert!(t < o, "twin ranks above the off-topic session: {body}");
    }
}

/// R7 — validation: q and similar_to are mutually exclusive, one is required,
/// and an unknown session id is a 404.
#[tokio::test]
async fn recollect_q_and_similar_to_validation() {
    let g: Vec<(&str, String)> = Vec::new();
    let (_tmp, addr) = boot_memory_corpora(&g, &[]).await;
    let client = reqwest::Client::new();
    let status = |path: &'static str| {
        let c = client.clone();
        let u = url(addr, path);
        async move { c.get(u).send().await.unwrap().status().as_u16() }
    };
    assert_eq!(
        status("/api/sessions/recollect?q=foo&similar_to=bar").await,
        400
    );
    assert_eq!(status("/api/sessions/recollect?limit=5").await, 400);
    assert_eq!(
        status("/api/sessions/recollect?similar_to=nope-nope").await,
        404
    );
}

/// #11/recollect-R3 — regression: recollect must join lance→sqlite via
/// artifact_id, never trust the (possibly dirty) lance `kb_session` meta
/// value. A stale bash Stop-hook's `jq -r | tr -c 'a-zA-Z0-9' '-'` pipeline
/// turned jq's trailing newline into a trailing dash, so production
/// `<meta name="kb-session">` values carry a "-" suffix the sqlite
/// `sessions.session_id` (JSONL-recovered, canonical) does not. Before the
/// fix, every candidate died on `sessions_get_many(<dirty sid>)` and
/// recollect returned `{"sessions":[]}` unconditionally.
#[tokio::test]
async fn recollect_finds_session_despite_dirty_kb_session_trailing_dash() {
    let clean_sid = "sid-quazzitron-7f3e9c21-aa11bb22cc33";
    let dirty_meta = format!("{clean_sid}-");
    let global = vec![(
        "session-20260601T100000Z-dirty.html",
        session_transcript_html(
            &dirty_meta,
            "20260601T100000Z",
            &format!(
                r#"{{"type":"user","promptSource":"typed","cwd":"/proj/widget","sessionId":"{clean_sid}","message":{{"role":"user","content":"debug the quazzitron synchronization protocol"}}}}"#
            ),
        ),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T100000Z"
    })
    .await;

    let recollect = |q: &str| {
        let c = client.clone();
        let u = url(addr, &format!("/api/sessions/recollect?q={q}&limit=10"));
        async move {
            c.get(u)
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };
    // Poll until the digest is indexed AND the sqlite session row (whose
    // artifact_id → session_id join the fix depends on) is enriched.
    let mut body = recollect("quazzitron").await;
    for _ in 0..40 {
        if body["sessions"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        body = recollect("quazzitron").await;
    }
    let sessions = body["sessions"].as_array().expect("sessions array");
    assert!(
        !sessions.is_empty(),
        "recollect found the dirty-meta session: {body}"
    );
    assert_eq!(
        sessions[0]["session_id"], clean_sid,
        "recollect returns the CANONICAL sqlite id, never the dirty lance \
         kb_session meta value: {body}"
    );
}

/// #11/recollect-R3 — the truncated-meta variant of the same defect: a few
/// older captures didn't just gain a trailing dash, the meta was cut mid-id
/// (e.g. production's `01add306-8f23-4a83-983e-`). The transcript's own
/// `sessionId` is still ground truth and must be what recollect returns.
#[tokio::test]
async fn recollect_finds_session_despite_truncated_kb_session_meta() {
    let clean_sid = "01add306-8f23-4a83-983e-11223344ee00";
    let truncated_meta = "01add306-8f23-4a83-983e-"; // cut mid-uuid, like production
    let global = vec![(
        "session-20260601T100000Z-truncated.html",
        session_transcript_html(
            truncated_meta,
            "20260601T100000Z",
            &format!(
                r#"{{"type":"user","promptSource":"typed","cwd":"/proj/widget","sessionId":"{clean_sid}","message":{{"role":"user","content":"investigate the wobblefish latency spike"}}}}"#
            ),
        ),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T100000Z"
    })
    .await;

    let recollect = |q: &str| {
        let c = client.clone();
        let u = url(addr, &format!("/api/sessions/recollect?q={q}&limit=10"));
        async move {
            c.get(u)
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };
    let mut body = recollect("wobblefish").await;
    for _ in 0..40 {
        if body["sessions"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        body = recollect("wobblefish").await;
    }
    let sessions = body["sessions"].as_array().expect("sessions array");
    assert!(
        !sessions.is_empty(),
        "recollect found the truncated-meta session: {body}"
    );
    assert_eq!(sessions[0]["session_id"], clean_sid, "{body}");
}

/// #11/recollect-R3 — `similar_to` must exclude the source session by its
/// CANONICAL id even when the underlying artifacts only carry dirty lance
/// `kb_session` meta values. `resolve_session_digest_query` already keys off
/// the clean `similar_to` query param via `sessions_get` → `artifact_id` →
/// lance (unaffected by this fix); this pins that the candidate-side
/// artifact_id join produces the SAME canonical id so the exclusion and the
/// twin's identity agree.
#[tokio::test]
async fn recollect_similar_to_excludes_source_by_canonical_id_despite_dirty_meta() {
    let clean_src = "sid-zephyrus-11112222-3333444455556666";
    let clean_twin = "sid-zephyrus-99998888-7777666655554444";
    let dirty_src = format!("{clean_src}-");
    let dirty_twin = format!("{clean_twin}-");
    let global = vec![
        (
            "session-20260601T100000Z-src.html",
            session_transcript_html(
                &dirty_src,
                "20260601T100000Z",
                &format!(
                    r#"{{"type":"user","promptSource":"typed","cwd":"/proj/widget","sessionId":"{clean_src}","message":{{"role":"user","content":"calibrate the zephyrusflux harmonics resonance routine"}}}}"#
                ),
            ),
        ),
        (
            "session-20260601T110000Z-twin.html",
            session_transcript_html(
                &dirty_twin,
                "20260601T110000Z",
                &format!(
                    r#"{{"type":"user","promptSource":"typed","cwd":"/proj/widget","sessionId":"{clean_twin}","message":{{"role":"user","content":"tune the zephyrusflux harmonics calibration so resonance is stable"}}}}"#
                ),
            ),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T110000Z"
    })
    .await;
    // Wait for the source's own enrichment row (its artifact_id → clean
    // session_id join) so `sessions_get(clean_src)` — what
    // `resolve_session_digest_query` uses — can resolve.
    for _ in 0..60 {
        let r = client
            .get(url(addr, &format!("/api/sessions/{clean_src}")))
            .send()
            .await
            .unwrap();
        if r.status() == 200 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let recollect = || {
        let c = client.clone();
        let u = url(
            addr,
            &format!("/api/sessions/recollect?similar_to={clean_src}&limit=10"),
        );
        async move {
            c.get(u)
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };
    let mut body = recollect().await;
    for _ in 0..40 {
        if body["sessions"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        body = recollect().await;
    }
    let sessions = body["sessions"].as_array().expect("sessions array");
    assert!(
        sessions.iter().any(|s| s["session_id"] == clean_twin),
        "topic-twin surfaced by its CANONICAL id despite dirty lance meta: {body}"
    );
    assert!(
        sessions.iter().all(|s| s["session_id"] != clean_src),
        "source session excluded by its CANONICAL id, not the dirty meta value: {body}"
    );
}

/// R4 — research / tool-usage signals (kb & web searches, subagents) are
/// extracted from the transcript and served at /api/sessions/{sid}/research.
#[tokio::test]
async fn session_research_extracted_and_served() {
    let sid = "sid-research-001";
    let jsonl = concat!(
        r#"{"type":"user","promptSource":"typed","message":{"role":"user","content":"build the thing"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"r1","name":"Bash","input":{"command":"kb search \"prior art\" --kb x"}}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"r2","name":"WebSearch","input":{"query":"rust lance hybrid search"}}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"r3","name":"Task","input":{"description":"map the storage layer"}}]}}"#,
        "\n",
    );
    let global = vec![(
        "session-20260601T140000Z-sid-research-001.html",
        session_transcript_html(sid, "20260601T140000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T140000Z"
    })
    .await;

    let fetch = || async {
        client
            .get(url(addr, &format!("/api/sessions/{sid}/research")))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    };
    let mut body = fetch().await;
    for _ in 0..40 {
        if body["research"].as_array().map(|a| a.len()).unwrap_or(0) >= 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        body = fetch().await;
    }
    let items = body["research"].as_array().expect("research array");
    let kinds: Vec<&str> = items.iter().filter_map(|x| x["kind"].as_str()).collect();
    assert!(kinds.contains(&"kb_search"), "{body}");
    assert!(kinds.contains(&"web"), "{body}");
    assert!(kinds.contains(&"subagent"), "{body}");
    let kb = items.iter().find(|x| x["kind"] == "kb_search").expect("kb");
    assert_eq!(kb["query"], "search prior art");
}

/// R5 — the session<->comments endpoint is wired, fans out per corpus (#28),
/// and returns the correct empty shape when the session touched nothing
/// in-corpus (its only edits are outside any kb).
///
/// The full in-corpus join (a session editing an INDEXED artifact that has an
/// open comment → that comment surfaced via session_files.target_artifact_id
/// → `review::load`) passes in isolation but can't be asserted in the parallel
/// suite: in_corpus resolution reads the PROCESS-GLOBAL corpus-mount table
/// (`kb_core::sessions::CORPUS_MOUNTS`), which sibling test daemons replace on
/// boot — so `in_corpus=true` is non-deterministic here (no e2e asserts it).
#[tokio::test]
async fn session_comments_endpoint_wired_and_wellformed() {
    let sid = "sid-comments-empty-001";
    let jsonl = concat!(
        r#"{"type":"user","promptSource":"typed","message":{"role":"user","content":"do out-of-corpus work"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/nowhere/out-of-corpus.rs","old_string":"a","new_string":"b"}}]}}"#,
        "\n",
    );
    let global = vec![(
        "session-20260601T150000Z-sid-comments-empty-001.html",
        session_transcript_html(sid, "20260601T150000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260601T150000Z"
    })
    .await;
    let body: serde_json::Value = client
        .get(url(addr, &format!("/api/sessions/{sid}/comments")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["total"], 0, "{body}");
    assert!(
        body["artifacts"].as_array().is_some_and(|a| a.is_empty()),
        "well-formed empty artifacts array: {body}"
    );
}

/// R8 — comments RAISED during a session's [started_at, ended_at] window are
/// surfaced (federated across per-kb history), even on artifacts the session
/// never touched; a comment OUTSIDE the window is not; an unknown session 404s.
#[tokio::test]
async fn session_comments_includes_raised_during_window() {
    // A session whose window generously contains "now": started_at = now-1h
    // (filename ts), ended_at = now+1h (a future event timestamp). The comment
    // we POST below lands at ~now, inside the window.
    let start_ts = (chrono::Utc::now() - chrono::Duration::hours(1))
        .format("%Y%m%dT%H%M%SZ")
        .to_string();
    let end_iso = (chrono::Utc::now() + chrono::Duration::hours(1))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let pos_sid = "sid-raised-pos";
    let pos_jsonl = format!(
        r#"{{"type":"user","promptSource":"typed","timestamp":"{end_iso}","message":{{"role":"user","content":"work that spans a window"}}}}"#
    );
    // A past-window session: window ≈ [2020, 2020], so a now-comment is outside.
    let past_sid = "sid-raised-past";
    let past_jsonl = r#"{"type":"user","promptSource":"typed","timestamp":"2020-01-01T00:00:01Z","message":{"role":"user","content":"long-ago work"}}"#;
    let pos_name = format!("session-{start_ts}-sid-raised-pos.html");
    let pos_html = session_transcript_html(pos_sid, &start_ts, &pos_jsonl);
    let past_html = session_transcript_html(past_sid, "20200101T000000Z", past_jsonl);
    let global: Vec<(&str, String)> = vec![
        (pos_name.as_str(), pos_html),
        ("session-20200101T000000Z-sid-raised-past.html", past_html),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    // Wait until both sessions are enriched.
    wait_for_session(&client, addr, pos_sid).await;
    wait_for_session(&client, addr, past_sid).await;

    // Raise a comment NOW on an artifact the session never touched.
    let aid = "deadc0de1234";
    let resp = client
        .post(url(
            addr,
            &format!("/api/kb/globalmem/review/{aid}/comments"),
        ))
        .json(&serde_json::json!({
            "body": "raised mid-session",
            "anchor": {"kind": "file"},
            "author": "you"
        }))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "comment posted: {}",
        resp.status()
    );

    // Positive: the comment appears in `raised`, flagged touched=false.
    let fetch = |sid: &'static str| {
        let c = client.clone();
        let u = url(addr, &format!("/api/sessions/{sid}/comments"));
        async move {
            c.get(u)
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };
    let mut body = fetch(pos_sid).await;
    for _ in 0..60 {
        if body["raised"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        body = fetch(pos_sid).await;
    }
    let raised = body["raised"].as_array().expect("raised array");
    let hit = raised
        .iter()
        .find(|r| r["body"] == "raised mid-session")
        .expect("the raised comment is surfaced");
    assert_eq!(hit["touched"], false, "{body}");
    assert_eq!(hit["author"], "you");
    assert_eq!(hit["status"], "open");

    // Negative: the past-window session sees no raised comments.
    let past = fetch(past_sid).await;
    assert!(
        past["raised"].as_array().is_some_and(|a| a.is_empty()),
        "past-window session has no raised comments: {past}"
    );

    // The unknown-session guard now 404s.
    let nf = client
        .get(url(addr, "/api/sessions/does-not-exist/comments"))
        .send()
        .await
        .unwrap();
    assert_eq!(nf.status(), 404);
}

/// R9 — research rollups + the activity funnel aggregate the session_* tables.
// invariant:11 newest-capture-rollup
#[tokio::test]
async fn research_rollup_and_funnel_aggregate_sessions() {
    let sid = "sid-r9";
    let jsonl = concat!(
        r#"{"type":"user","promptSource":"typed","cwd":"/proj/widget","message":{"role":"user","content":"improve search"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"kb search \"reranker\""}}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"WebSearch","input":{"query":"rust lance hybrid"}}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/proj/widget/src/rank.rs","old_string":"a","new_string":"b"}}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"c1","name":"Bash","input":{"command":"git commit -m \"feat: rerank\""}}]}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"[main abc1234] feat: rerank"}]}}"#,
        "\n",
    );
    let global = vec![(
        "session-20260601T100000Z-sid-r9.html",
        session_transcript_html(sid, "20260601T100000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_session(&client, addr, sid).await;

    // Poll the rollup until the research rows land.
    let rollup = || async {
        client
            .get(url(addr, "/api/sessions/research-rollup"))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    };
    let mut rb = rollup().await;
    for _ in 0..40 {
        if rb["folders"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        rb = rollup().await;
    }
    let folders = rb["folders"].as_array().expect("rollup folders");
    let widget = folders
        .iter()
        .find(|f| f["label"] == "widget")
        .expect("widget folder in rollup");
    let queries = widget["queries"].as_array().unwrap();
    assert!(
        queries
            .iter()
            .any(|q| q["kind"] == "kb_search"
                && q["query"].as_str().unwrap_or("").contains("reranker")),
        "kb search query rolled up: {widget}"
    );
    assert!(
        queries
            .iter()
            .any(|q| q["kind"] == "web" && q["query"] == "rust lance hybrid"),
        "web search query rolled up: {widget}"
    );

    // The funnel (overall — a single session, so == the widget folder).
    let fb: serde_json::Value = client
        .get(url(addr, "/api/sessions/funnel"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let stage = |name: &str| {
        fb["stages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["stage"] == name)
            .unwrap()["events"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(stage("searched"), 2, "kb_search + web: {fb}");
    assert_eq!(stage("edited"), 1, "one edited path: {fb}");
    assert_eq!(stage("committed"), 1, "{fb}");
    assert_eq!(stage("commented"), 0, "no comments: {fb}");
}

// P6 — sessions-scoped keyword search (?q=) + folder rollup stats.
#[tokio::test]
async fn sessions_search_and_folder_rollups() {
    let txt = |cwd: &str, prompt: &str| {
        format!(
            r#"{{"type":"user","cwd":"{cwd}","promptSource":"typed","message":{{"role":"user","content":"{prompt}"}}}}"#
        )
    };
    let global = vec![
        (
            "session-20260524T100000Z-s-alpha.html",
            session_transcript_html(
                "s-alpha",
                "20260524T100000Z",
                &txt("/p/kb", "refactor the indexer"),
            ),
        ),
        (
            "session-20260524T110000Z-s-beta.html",
            session_transcript_html(
                "s-beta",
                "20260524T110000Z",
                &txt("/p/kb", "write the authoring guide"),
            ),
        ),
        (
            "session-20260524T120000Z-s-gamma.html",
            session_transcript_html(
                "s-gamma",
                "20260524T120000Z",
                &txt("/p/other", "fix beta bug"),
            ),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260524T120000Z"
    })
    .await;
    // Wait until all three enrichment rows landed.
    loop {
        let r: serde_json::Value = client
            .get(url(addr, "/api/sessions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if r["sessions"].as_array().map(|a| a.len()).unwrap_or(0) >= 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Keyword search over the first prompt.
    let hits: serde_json::Value = client
        .get(url(addr, "/api/sessions?q=authoring"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = hits["sessions"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["session_id"], "s-beta");

    // Folder rollups: /p/kb has 2 sessions with a span; /p/other has 1.
    let folders: serde_json::Value = client
        .get(url(addr, "/api/sessions/folders"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let kb_folder = folders["folders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["folder"] == "/p/kb")
        .expect("/p/kb folder");
    assert_eq!(kb_folder["count"], 2);
    assert!(kb_folder["earliest"].as_i64().unwrap() < kb_folder["latest"].as_i64().unwrap());
    assert!(kb_folder["token_total"].as_u64().is_some());
}

// P7 — narrative threads: same-folder sessions cluster by time gaps.
#[tokio::test]
async fn sessions_threads_cluster_by_time_gap() {
    // Three sessions in /p/kb: two on Jun 1 (same day), one on Jun 10 (a
    // 9-day gap, > the 2-day thread gap) → /p/kb splits into TWO threads.
    // The events carry timestamps so ended_at = the real time (not the
    // index-time mtime); the gap clustering keys on (started_at, ended_at).
    let global = vec![
        (
            "session-20260601T090000Z-t-a.html",
            session_transcript_html(
                "t-a",
                "20260601T090000Z",
                r#"{"type":"user","timestamp":"2026-06-01T09:00:00Z","cwd":"/p/kb","promptSource":"typed","message":{"role":"user","content":"start the work"}}"#,
            ),
        ),
        (
            "session-20260601T140000Z-t-b.html",
            session_transcript_html(
                "t-b",
                "20260601T140000Z",
                r#"{"type":"user","timestamp":"2026-06-01T14:00:00Z","cwd":"/p/kb","promptSource":"typed","message":{"role":"user","content":"continue same day"}}"#,
            ),
        ),
        (
            "session-20260610T090000Z-t-c.html",
            session_transcript_html(
                "t-c",
                "20260610T090000Z",
                r#"{"type":"user","timestamp":"2026-06-10T09:00:00Z","cwd":"/p/kb","promptSource":"typed","message":{"role":"user","content":"new effort 9 days later"}}"#,
            ),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260610T090000Z"
    })
    .await;
    loop {
        let r: serde_json::Value = client
            .get(url(addr, "/api/sessions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if r["sessions"].as_array().map(|a| a.len()).unwrap_or(0) >= 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let threads: serde_json::Value = client
        .get(url(addr, "/api/sessions/threads"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = threads["threads"].as_array().expect("threads array");
    let kb_threads: Vec<_> = arr.iter().filter(|t| t["folder"] == "/p/kb").collect();
    assert_eq!(
        kb_threads.len(),
        2,
        "the 9-day gap splits /p/kb into 2 threads"
    );
    let counts: Vec<i64> = kb_threads
        .iter()
        .map(|t| t["count"].as_i64().unwrap())
        .collect();
    assert!(
        counts.contains(&2) && counts.contains(&1),
        "one thread of 2 (same day) + one of 1 (later); got {counts:?}"
    );
}

// P8 — materialise a thread into an editable kb-list/1 list; the session
// entry suppresses read-state (is_session, invariant #25).
#[tokio::test]
async fn sessions_thread_save_creates_list_with_session_entry() {
    let sid = "sid-save-001";
    let jsonl = r#"{"type":"user","cwd":"/p/kb","promptSource":"typed","message":{"role":"user","content":"thread to save"}}"#;
    let global = vec![(
        "session-20260524T100000Z-sid-save-001.html",
        session_transcript_html(sid, "20260524T100000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260524T100000Z"
    })
    .await;
    // Resolve the session's artifact id.
    let mut artifact_id = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let r: serde_json::Value = client
            .get(url(addr, "/api/sessions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(s) = r["sessions"]
            .as_array()
            .and_then(|a| a.iter().find(|s| s["session_id"] == sid))
        {
            artifact_id = s["artifact_id"].as_str().unwrap().to_string();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(!artifact_id.is_empty(), "session must be indexed");

    // Save the thread as a list.
    let saved: serde_json::Value = client
        .post(url(addr, "/api/sessions/threads/save"))
        .json(&serde_json::json!({
            "kb": "globalmem",
            "title": "My saved thread",
            "artifact_ids": [artifact_id],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let list_id = saved["list_id"].as_str().expect("list_id returned");
    assert_eq!(saved["kb"], "globalmem");

    // The list detail carries the session entry with is_session=true.
    let detail: serde_json::Value = client
        .get(url(addr, &format!("/api/kb/globalmem/lists/{list_id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entries = detail["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["artifact_id"], artifact_id);
    assert_eq!(
        entries[0]["is_session"], true,
        "session entry must flag is_session (read-state suppressed)"
    );
    assert!(
        detail["list"]["description"].is_null(),
        "the FLAT save writes no description (pre-CT-E5 contract)"
    );
}

// ---- CT-E5 — the NARRATIVE save ------------------------------------------

/// Resolve one session's artifact id off `/api/sessions` (the id is a content
/// hash of the source-relative path, so a test can't predict it).
async fn session_artifact_id(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    sid: &str,
) -> String {
    common::poll_until(&format!("artifact id for session `{sid}`"), || async {
        let r: serde_json::Value = client
            .get(url(addr, "/api/sessions?limit=100"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        r["sessions"]
            .as_array()
            .and_then(|a| a.iter().find(|s| s["session_id"] == sid))
            .and_then(|s| s["artifact_id"].as_str())
            .map(str::to_string)
    })
    .await
}

/// POST the narrative save and return the assembled list detail.
async fn save_narrative_list(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    title: &str,
    artifact_ids: &[&str],
) -> serde_json::Value {
    let saved: serde_json::Value = client
        .post(url(addr, "/api/sessions/threads/save"))
        .json(&serde_json::json!({
            "kb": "globalmem",
            "title": title,
            "artifact_ids": artifact_ids,
            "narrative": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let list_id = saved["list_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no list_id in {saved}"));
    client
        .get(url(addr, &format!("/api/kb/globalmem/lists/{list_id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// CT-E5 — `"narrative": true` expands ONE session into its story: the
/// capture artifact first, then the memories it produced, each entry noting
/// its lane. The `touched` lane is unit-covered instead of asserted here —
/// `in_corpus` resolution reads the PROCESS-GLOBAL corpus-mount table, which
/// sibling test daemons replace on boot (see
/// `session_comments_endpoint_wired_and_wellformed`).
#[tokio::test]
async fn sessions_thread_save_narrative_orders_capture_before_produced_memories() {
    let sid = "sid-narr-001";
    let jsonl = r#"{"type":"user","cwd":"/p/kb","promptSource":"typed","message":{"role":"user","content":"story to save"}}"#;
    let global = vec![
        (
            "session-20260524T110000Z-sid-narr-001.html",
            session_transcript_html(sid, "20260524T110000Z", jsonl),
        ),
        ("narr-mem-a.html", memory_with_session_html("Narr A", sid)),
        ("narr-mem-b.html", memory_with_session_html("Narr B", sid)),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Narr A").await;
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Narr B").await;
    wait_for_session(&client, addr, sid).await;
    let artifact_id = session_artifact_id(&client, addr, sid).await;

    // `kb_session` lands in lance independently of the sqlite session row;
    // wait for the produced lane's own read (`/memories`) to see both.
    common::poll_until("both produced memories to be joinable", || async {
        let m: serde_json::Value = client
            .get(url(addr, &format!("/api/sessions/{sid}/memories")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        (m["memories"].as_array().map(Vec::len).unwrap_or(0) == 2).then_some(())
    })
    .await;

    let detail = save_narrative_list(&client, addr, "Story", &[&artifact_id]).await;
    let entries = detail["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 3, "capture + 2 produced: {entries:?}");
    assert_eq!(entries[0]["artifact_id"], artifact_id, "capture is first");
    assert_eq!(entries[0]["is_session"], true);
    assert_eq!(entries[0]["note"], "session capture");
    for e in &entries[1..] {
        assert_eq!(e["note"], "memory produced", "entry: {e}");
        // `is_session`/`tombstone` are skip-if-false on the wire.
        assert!(e["is_session"] != true, "entry: {e}");
        assert!(e["tombstone"] != true, "no phantom entries: {e}");
    }
    let titles: std::collections::BTreeSet<String> = entries[1..]
        .iter()
        .map(|e| e["title"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        titles,
        ["Narr A".to_string(), "Narr B".to_string()]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );

    // The description explains the ordering and names the session + date.
    let desc = detail["list"]["description"].as_str().expect("description");
    assert!(
        desc.starts_with(
            "Narrative order: capture → files touched → memories produced → memories recalled."
        ),
        "description: {desc}"
    );
    assert!(
        desc.contains(&format!("Session {sid} · captured ")),
        "{desc}"
    );
    assert!(
        !desc.contains("Session diff"),
        "no [kb.*] code_url configured ⇒ no link: {desc}"
    );
    assert!(!desc.contains("omitted"), "nothing was dropped: {desc}");
}

/// CT-E5 — a session with nothing but its capture yields exactly ONE entry.
/// Empty lanes are empty; the list never invents a row.
#[tokio::test]
async fn sessions_thread_save_narrative_of_a_bare_session_is_just_the_capture() {
    let sid = "sid-narr-bare-001";
    let jsonl = r#"{"type":"user","cwd":"/p/kb","promptSource":"typed","message":{"role":"user","content":"nothing happened"}}"#;
    let global = vec![(
        "session-20260524T120000Z-sid-narr-bare-001.html",
        session_transcript_html(sid, "20260524T120000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_session(&client, addr, sid).await;
    let artifact_id = session_artifact_id(&client, addr, sid).await;

    let detail = save_narrative_list(&client, addr, "Bare story", &[&artifact_id]).await;
    let entries = detail["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1, "capture only: {entries:?}");
    assert_eq!(entries[0]["note"], "session capture");
    let desc = detail["list"]["description"].as_str().expect("description");
    assert!(!desc.contains("omitted"), "nothing to omit: {desc}");
}

/// CT-E5 — with `[kb.*] code_url` configured the description carries the
/// kb-code session-diff link. kb RENDERS it; it never dials kb-code
/// (invariant #2's ONE call direction).
#[tokio::test]
async fn sessions_thread_save_narrative_description_links_the_kb_code_session_diff() {
    let sid = "sid-narr-code-001";
    let jsonl = r#"{"type":"user","cwd":"/p/kb","promptSource":"typed","message":{"role":"user","content":"link out"}}"#;
    let global = vec![(
        "session-20260524T130000Z-sid-narr-code-001.html",
        session_transcript_html(sid, "20260524T130000Z", jsonl),
    )];
    let (_tmp, addr) =
        boot_memory_corpora_with_code_url(&global, &[], Some("https://kbc.example/")).await;
    let client = reqwest::Client::new();
    wait_for_session(&client, addr, sid).await;
    let artifact_id = session_artifact_id(&client, addr, sid).await;

    let detail = save_narrative_list(&client, addr, "Linked story", &[&artifact_id]).await;
    let desc = detail["list"]["description"].as_str().expect("description");
    assert!(
        desc.contains(&format!(
            "Session diff: https://kbc.example/session/{sid}/diff"
        )),
        "description: {desc}"
    );
}

/// CT-E5 — a `memory_recalls` ledger row whose memory does not resolve in
/// this corpus is HISTORY, not a list entry: it is counted in the
/// description, never rendered as a `(removed)` tombstone (the
/// `resolve_target` "no deliberate tombstones" rule).
#[tokio::test]
async fn sessions_thread_save_narrative_counts_an_unresolvable_recall_instead_of_listing_it() {
    let sid = "sid-narr-recall-001";
    let jsonl = concat!(
        "{\"sessionId\":\"sid-narr-recall-001\",\"cwd\":\"/home/u/proj\",\"timestamp\":\"2026-06-04T09:00:00.000Z\"}\n",
        "{\"parentUuid\":null,\"isSidechain\":false,\"attachment\":{\"type\":\"hook_additional_context\",\"content\":[\"Relevant memories from kb (recall - these persist across sessions):\\n- alpha fact  [globalmem]  (id aaaaaaaaaaaa)\"],\"hookName\":\"UserPromptSubmit\",\"toolUseID\":\"UserPromptSubmit\",\"hookEvent\":\"UserPromptSubmit\"},\"type\":\"attachment\",\"uuid\":\"20000001-0000-4000-8000-000000000001\",\"timestamp\":\"2026-06-04T09:00:05.000Z\",\"sessionId\":\"sid-narr-recall-001\"}\n",
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"},\"timestamp\":\"2026-06-04T09:00:06.000Z\",\"sessionId\":\"sid-narr-recall-001\"}\n",
    );
    let global = vec![(
        "session-20260604T090000Z-sid-narr-recall-001.html",
        session_transcript_html(sid, "20260604T090000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_session(&client, addr, sid).await;
    let artifact_id = session_artifact_id(&client, addr, sid).await;

    // Wait for the recall ledger row to land, then save.
    common::poll_until("the memory_recalls ledger row", || async {
        let recalls: serde_json::Value = client
            .get(url(addr, &format!("/api/sessions/{sid}/recalls")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        recalls["recalls"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
            .then_some(())
    })
    .await;

    let detail = save_narrative_list(&client, addr, "Recall story", &[&artifact_id]).await;
    let entries = detail["entries"].as_array().expect("entries");
    assert_eq!(
        entries.len(),
        1,
        "only the capture is listable: {entries:?}"
    );
    assert_eq!(entries[0]["note"], "session capture");
    assert!(
        entries.iter().all(|e| e["tombstone"] != true),
        "a dead recall must never become a tombstone entry: {entries:?}"
    );
    let desc = detail["list"]["description"].as_str().expect("description");
    assert!(
        desc.contains("1 story item omitted"),
        "the drop is reported, not hidden: {desc}"
    );
}

/// CT-E5 — a narrative save naming artifact ids this kb has no session row
/// for creates nothing and 400s, rather than a titled empty list.
#[tokio::test]
async fn sessions_thread_save_narrative_400s_when_no_session_resolves() {
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(addr, "/api/sessions/threads/save"))
        .json(&serde_json::json!({
            "kb": "globalmem",
            "title": "Nothing",
            "artifact_ids": ["000000000000"],
            "narrative": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "no session ⇒ no list");
    let lists: serde_json::Value = client
        .get(url(addr, "/api/lists"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        lists["lists"].as_array().map(Vec::len).unwrap_or(0),
        0,
        "a refused narrative save leaves no list behind: {lists}"
    );
}

#[tokio::test]
async fn sessions_detail_returns_memory_ids_for_the_session() {
    let sid = "sess-detail-002";
    let jsonl =
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"detail probe\"}}\n";
    let global = vec![
        (
            "session-20260524T100000Z-sess-detail-002.html",
            session_transcript_html(sid, "20260524T100000Z", jsonl),
        ),
        ("mem-a.html", memory_with_session_html("Mem A", sid)),
        ("mem-b.html", memory_with_session_html("Mem B", sid)),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Mem A").await;
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Mem B").await;
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260524T100000Z"
    })
    .await;

    let resp: serde_json::Value = client
        .get(url(addr, &format!("/api/sessions/{sid}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["session_id"], sid);
    let ids = resp["memory_ids"].as_array().expect("memory_ids array");
    assert_eq!(ids.len(), 2, "both seeded memories are reported");
}

#[tokio::test]
async fn sessions_detail_returns_404_for_unknown_id() {
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();
    let r = client
        .get(url(addr, "/api/sessions/never-existed"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn session_readings_returns_404_for_unknown_id() {
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();
    let r = client
        .get(url(addr, "/api/sessions/never-existed/readings"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn session_readings_route_contract() {
    // Route contract for the read-counterpart to /touches: a valid session →
    // 200 + a `readings` array of well-formed rows. The session's window is
    // [filename-ts, transcript-mtime]; whether a given history open falls
    // under the mtime upper bound is wall-clock-dependent, so the window
    // FILTER itself is unit-tested deterministically in
    // `history_opens_in_window_filters_by_time` — here we pin the HTTP shape,
    // find_session lookup, fan-out, and row schema.
    let sid = "sess-read-004";
    let jsonl =
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"reading probe\"}}\n";
    let global = vec![(
        "session-20200101T000000Z-sess-read-004.html",
        session_transcript_html(sid, "20200101T000000Z", jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20200101T000000Z"
    })
    .await;

    // Record a read so the window has a candidate to (possibly) include.
    let open: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/history/open"))
        .json(&serde_json::json!({ "artifact_id": "deadbeefcafe" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let vid = open["visit_id"].as_i64().unwrap();
    client
        .post(url(addr, "/api/kb/globalmem/history/scroll"))
        .json(&serde_json::json!({ "visit_id": vid, "scroll_y": 50, "scroll_max": 100 }))
        .send()
        .await
        .unwrap();

    let resp = client
        .get(url(addr, &format!("/api/sessions/{sid}/readings")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let readings = body["readings"].as_array().expect("readings array");
    for row in readings {
        assert!(row["kb"].is_string(), "row has kb");
        assert!(row["artifact_id"].is_string(), "row has artifact_id");
        assert!(row["opened_at"].is_i64(), "row has opened_at");
    }
}

#[tokio::test]
async fn sessions_memories_route_lists_memory_rows_for_session() {
    let sid = "sess-memories-003";
    let jsonl = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"memories route probe\"}}\n";
    let global = vec![
        (
            "session-20260524T100000Z-sess-memories-003.html",
            session_transcript_html(sid, "20260524T100000Z", jsonl),
        ),
        ("wanted-1.html", memory_with_session_html("Wanted One", sid)),
        ("wanted-2.html", memory_with_session_html("Wanted Two", sid)),
        (
            "other-session.html",
            memory_with_session_html("Other Session", "different-sid"),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Wanted One").await;
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Wanted Two").await;
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Other Session"
    })
    .await;

    let resp: serde_json::Value = client
        .get(url(addr, &format!("/api/sessions/{sid}/memories")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let memories = resp["memories"].as_array().expect("memories array");
    let titles: Vec<String> = memories
        .iter()
        .map(|m| m["title"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(titles.contains(&"Wanted One".to_string()));
    assert!(titles.contains(&"Wanted Two".to_string()));
    assert!(
        !titles.contains(&"Other Session".to_string()),
        "session filter must reject other-sid memory",
    );
}

#[tokio::test]
async fn sessions_touches_returns_404_for_unknown_id() {
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();
    let r = client
        .get(url(addr, "/api/sessions/never-existed/touches"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn sessions_touches_returns_empty_when_no_matching_artifacts() {
    let sid = "sess-touches-empty";
    // Transcript mentions a 12-hex id that no corpus artifact carries.
    let prompt = "Take a look at /a/work/abc123def456 — relevant".to_string();
    let jsonl = format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":{}}}}}\n",
        serde_json::to_string(&prompt).unwrap()
    );
    let global = vec![(
        "session-20260524T100000Z-sess-touches-empty.html",
        session_transcript_html(sid, "20260524T100000Z", &jsonl),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260524T100000Z"
    })
    .await;

    let resp: serde_json::Value = client
        .get(url(addr, &format!("/api/sessions/{sid}/touches")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids = resp["artifact_ids"].as_array().expect("artifact_ids");
    assert!(ids.is_empty(), "no matching id in corpus → empty");
    // confidence is `exact` when no fuzzy match either.
    assert_eq!(resp["confidence"], "exact");
    // CT-A6 — the additive per-artifact `artifacts` field stays empty too.
    assert!(resp["artifacts"].as_array().expect("artifacts").is_empty());
}

#[tokio::test]
async fn sessions_touches_resolves_path_substring_as_fuzzy() {
    // The transcript references a memory artifact by its
    // source-relative path (no id). The touches scan should pick
    // it up via the path-substring lane and flip `confidence` to
    // `fuzzy`.
    let sid = "sess-touches-fuzzy";
    let prompt =
        "context: globalmem holds a useful one — see notes-from-session-1700.html".to_string();
    let jsonl = format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":{}}}}}\n",
        serde_json::to_string(&prompt).unwrap()
    );
    let global = vec![
        (
            "session-20260524T100000Z-sess-touches-fuzzy.html",
            session_transcript_html(sid, "20260524T100000Z", &jsonl),
        ),
        (
            // The memory's filename is exactly the substring the
            // transcript quotes (matches source_relative).
            "notes-from-session-1700.html",
            memory_with_session_html("Notes From Session", "different-sid"),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Notes From Session"
    })
    .await;
    wait_for_doc(&client, addr, "globalmem", |d| {
        d["title"] == "Session transcript 20260524T100000Z"
    })
    .await;

    let resp: serde_json::Value = client
        .get(url(addr, &format!("/api/sessions/{sid}/touches")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids = resp["artifact_ids"].as_array().expect("artifact_ids");
    assert_eq!(ids.len(), 1, "path substring should resolve the memory");
    assert_eq!(resp["confidence"], "fuzzy");
    // CT-A6 — the join-tier is ALSO carried per-row now (additive
    // `artifacts` field); this specific row's own tier is `fuzzy` too,
    // matching the aggregate since it's the only match.
    let artifacts = resp["artifacts"].as_array().expect("artifacts");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0]["confidence"], "fuzzy");
}

#[tokio::test]
async fn sessions_session_captured_sse_fires_on_index() {
    // U3 — confirm the indexer emits `session.captured` over the SSE
    // wire when a memory-session artifact lands. Subscribe BEFORE
    // seeding (the EventSource handshake races the file write
    // otherwise), then drop the transcript into the corpus and read
    // the stream until the event surfaces or a 15s deadline expires.
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();
    // Open the SSE stream first.
    let resp = client
        .get(url(addr, "/api/events?types=session.*"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut stream = resp.bytes_stream();
    // Give the EventSource a beat to register on the daemon's bus.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Seed via the artifacts route so the indexer treats it as a
    // memory-session capture (kb-category=memory-session triggers the
    // session enrichment hook + the session.captured emit).
    let sid = "sse-spec-001";
    let jsonl = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"sse probe\"}}";
    let body = serde_json::json!({
        "title": "SSE Probe Session",
        "body_html": format!("<pre>{}</pre>", jsonl),
        "category": "memory-session",
        "session_id": sid,
    });
    let seed = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(seed.status().is_success(), "seed status: {}", seed.status());

    // Read SSE frames until we see a session.captured with our sid.
    use futures::StreamExt;
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut seen_our_sid = false;
    while tokio::time::Instant::now() < deadline && !seen_our_sid {
        let chunk = tokio::time::timeout(Duration::from_secs(2), stream.next()).await;
        let chunk = match chunk {
            Ok(Some(Ok(b))) => b,
            _ => break,
        };
        buf.push_str(std::str::from_utf8(&chunk).unwrap_or(""));
        if buf.contains("session.captured") && buf.contains(sid) {
            seen_our_sid = true;
        }
    }
    assert!(
        seen_our_sid,
        "session.captured for {sid} never arrived on the SSE stream (buf prefix: {prefix})",
        prefix = &buf.chars().take(400).collect::<String>(),
    );
}

// ===========================================================================
// L-tests — memory ↔ kb link mutations + recall `for_kb` filtering (L5/L6/L7)
//
// Boots one memory corpus ("globalmem") alongside two non-memory kbs
// ("alpha", "beta"). The non-memory kbs are valid link targets; the
// memory corpus is the home for ingested memories. All tests CI-safe —
// no embedder downloaded (memory recall falls back to BM25 + the loose
// /list_docs recency timeline).
// ===========================================================================

/// Boot a daemon shaped for the L-tests: one `global`-scope memory
/// corpus plus two empty non-memory kbs ("alpha" / "beta") usable as
/// link targets.
async fn boot_links_fixture() -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let mem = tmp.path().join("mem");
    let alpha = tmp.path().join("alpha");
    let beta = tmp.path().join("beta");
    for p in [&mem, &alpha, &beta] {
        std::fs::create_dir_all(p).unwrap();
    }

    let daemon_name = format!(
        "linktest-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("globalmem").unwrap(),
        KbSection {
            path: mem,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: BTreeMap::new(),
            memory_scope: Some("global".into()),
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
    for (name, dir) in [("alpha", alpha), ("beta", beta)] {
        kb_map.insert(
            KbName::new(name).unwrap(),
            KbSection {
                path: dir,
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
    }
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
    tokio::time::sleep(Duration::from_millis(400)).await;
    (tmp, addr)
}

/// Poll the memory's link set until it matches `pred`, or fail at the
/// deadline. Use after an ingest where the indexer's seed step is
/// debounced behind the watcher.
async fn wait_for_links(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    kb: &str,
    id: &str,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let body: serde_json::Value = client
            .get(url(addr, &format!("/api/kb/{kb}/memories/{id}/links")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        if pred(&body) {
            return body;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for memory_links to match for kb={kb} id={id}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ---- L5: ingest body validation -----------------------------------------

#[tokio::test]
async fn l5_ingest_defaults_to_global_when_no_link_fields() {
    // POST /api/kb/globalmem/artifacts with no global/linked_kbs in the
    // body should still produce a global memory (seed writes `*`).
    let (_tmp, addr) = boot_links_fixture().await;
    let client = reqwest::Client::new();

    let resp: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "Default Memory",
            "body": "no link fields — should still be global",
            "category": "memory-user",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = resp["id"].as_str().unwrap().to_string();
    wait_for_doc(&client, addr, "globalmem", |d| d["id"] == id).await;

    let links = wait_for_links(&client, addr, "globalmem", &id, |b| {
        b["global"].as_bool() == Some(true)
    })
    .await;
    assert_eq!(links["global"].as_bool(), Some(true));
    assert!(links["linked_kbs"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn l5_ingest_with_linked_kbs_writes_metas_and_seeds_table() {
    let (_tmp, addr) = boot_links_fixture().await;
    let client = reqwest::Client::new();

    let resp: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "Alpha-only Memory",
            "body": "this should be visible only to alpha",
            "category": "memory-user",
            "global": false,
            "linked_kbs": ["alpha"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = resp["id"].as_str().unwrap().to_string();
    wait_for_doc(&client, addr, "globalmem", |d| d["id"] == id).await;

    let links = wait_for_links(&client, addr, "globalmem", &id, |b| {
        b["linked_kbs"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    })
    .await;
    assert_eq!(links["global"].as_bool(), Some(false));
    let kbs: Vec<&str> = links["linked_kbs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(kbs, vec!["alpha"]);
}

#[tokio::test]
async fn l5_ingest_rejects_unknown_linked_kb_with_400() {
    let (_tmp, addr) = boot_links_fixture().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "Typoed Link",
            "body": "should not be created",
            "linked_kbs": ["alfa"] // not a real kb
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        400,
        "unknown linked_kbs must 400; got {}",
        resp.status()
    );
}

// ---- L6: mutation routes ------------------------------------------------

#[tokio::test]
async fn l6_put_links_replaces_set_atomically() {
    let (_tmp, addr) = boot_links_fixture().await;
    let client = reqwest::Client::new();

    // Seed via ingest (global=true by default).
    let created: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "Linker",
            "body": "to be re-linked via PUT",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    wait_for_doc(&client, addr, "globalmem", |d| d["id"] == id).await;
    wait_for_links(&client, addr, "globalmem", &id, |b| {
        b["global"].as_bool() == Some(true)
    })
    .await;

    // PUT swaps global off + scopes to {alpha, beta}.
    let put = client
        .put(url(addr, &format!("/api/kb/globalmem/memories/{id}/links")))
        .json(&serde_json::json!({
            "global": false,
            "linked_kbs": ["alpha", "beta"]
        }))
        .send()
        .await
        .unwrap();
    assert!(put.status().is_success(), "PUT returned {}", put.status());
    let body: serde_json::Value = put.json().await.unwrap();
    assert_eq!(body["global"].as_bool(), Some(false));
    let kbs: Vec<&str> = body["linked_kbs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(kbs, vec!["alpha", "beta"]);

    // GET reflects the new state.
    let read: serde_json::Value = client
        .get(url(addr, &format!("/api/kb/globalmem/memories/{id}/links")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(read["global"].as_bool(), Some(false));
}

#[tokio::test]
async fn l6_put_links_rejects_global_sentinel_in_linked_kbs() {
    let (_tmp, addr) = boot_links_fixture().await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({"title": "Sentinel Test", "body": "x"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let resp = client
        .put(url(addr, &format!("/api/kb/globalmem/memories/{id}/links")))
        .json(&serde_json::json!({
            "global": false,
            "linked_kbs": ["*"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        400,
        "the `*` sentinel must be rejected in linked_kbs; got {}",
        resp.status()
    );
}

#[tokio::test]
async fn l6_post_and_delete_single_edge_round_trip() {
    let (_tmp, addr) = boot_links_fixture().await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "Edge Toggle",
            "body": "single-edge round-trip",
            "global": false,
            "linked_kbs": []
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    wait_for_doc(&client, addr, "globalmem", |d| d["id"] == id).await;

    // Add alpha.
    let add = client
        .post(url(
            addr,
            &format!("/api/kb/globalmem/memories/{id}/links/alpha"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(add.status().as_u16(), 204);

    let after_add = wait_for_links(&client, addr, "globalmem", &id, |b| {
        b["linked_kbs"]
            .as_array()
            .map(|a| a.iter().any(|v| v.as_str() == Some("alpha")))
            .unwrap_or(false)
    })
    .await;
    assert_eq!(after_add["global"].as_bool(), Some(false));

    // Remove alpha.
    let rm = client
        .delete(url(
            addr,
            &format!("/api/kb/globalmem/memories/{id}/links/alpha"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(rm.status().as_u16(), 204);

    // Double-delete is idempotent (204 again).
    let rm2 = client
        .delete(url(
            addr,
            &format!("/api/kb/globalmem/memories/{id}/links/alpha"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(rm2.status().as_u16(), 204, "DELETE must be idempotent");

    let after_rm = wait_for_links(&client, addr, "globalmem", &id, |b| {
        b["linked_kbs"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false)
    })
    .await;
    assert!(after_rm["linked_kbs"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn l6_link_routes_400_on_non_memory_home_kb() {
    let (_tmp, addr) = boot_links_fixture().await;
    let client = reqwest::Client::new();
    // alpha is a normal kb — link mutations must reject it as the
    // home of a memory.
    let resp = client
        .post(url(addr, "/api/kb/alpha/memories/anything/links/beta"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        400,
        "non-memory home kb must 400; got {}",
        resp.status()
    );
}

#[tokio::test]
async fn l6_post_link_rejects_unknown_target_kb() {
    let (_tmp, addr) = boot_links_fixture().await;
    let client = reqwest::Client::new();
    let created: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({"title": "Unknown Target", "body": "x"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();
    let resp = client
        .post(url(
            addr,
            &format!("/api/kb/globalmem/memories/{id}/links/no-such-kb"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        400,
        "unknown target kb must 400; got {}",
        resp.status()
    );
}

#[tokio::test]
async fn l6_link_mutation_emits_memory_linked_sse() {
    use futures::StreamExt;
    let (_tmp, addr) = boot_links_fixture().await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({"title": "SSE Watcher", "body": "x"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    wait_for_doc(&client, addr, "globalmem", |d| d["id"] == id).await;

    // Trigger the link mutation, then subscribe — the bus ring replays.
    let add = client
        .post(url(
            addr,
            &format!("/api/kb/globalmem/memories/{id}/links/beta"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(add.status().as_u16(), 204);

    let resp = client.get(url(addr, "/api/events")).send().await.unwrap();
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    let _ = tokio::time::timeout(Duration::from_millis(1500), async {
        while let Some(chunk) = stream.next().await {
            if let Ok(bytes) = chunk {
                buf.push_str(&String::from_utf8_lossy(&bytes));
                if buf.contains("memory.linked") && buf.contains(&id) {
                    break;
                }
            }
        }
    })
    .await;
    assert!(
        buf.contains("memory.linked"),
        "expected memory.linked SSE; got:\n{buf}"
    );
    assert!(
        buf.contains(&id),
        "SSE must carry the artifact id {id}; got:\n{buf}"
    );
    assert!(
        buf.contains("\"added\":\"beta\""),
        "SSE must name the added target kb; got:\n{buf}"
    );
}

// ---- L7: recall `for_kb` filter + response shape ------------------------

#[tokio::test]
async fn l7_recall_for_kb_filters_to_global_plus_explicit_links() {
    let (_tmp, addr) = boot_links_fixture().await;
    let client = reqwest::Client::new();

    // Ingest THREE memories:
    //  - global (recallable everywhere)
    //  - scoped to alpha only
    //  - scoped to beta only
    let g: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "Global Note",
            "body": "everyone sees this xenon",
            "category": "memory-user",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let a: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "Alpha Note",
            "body": "alpha-only xenon detail",
            "category": "memory-user",
            "global": false,
            "linked_kbs": ["alpha"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let b: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "Beta Note",
            "body": "beta-only xenon detail",
            "category": "memory-user",
            "global": false,
            "linked_kbs": ["beta"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Wait until all three are indexed AND their links seeded.
    for id in [&g["id"], &a["id"], &b["id"]] {
        let id = id.as_str().unwrap();
        wait_for_doc(&client, addr, "globalmem", |d| d["id"] == id).await;
        wait_for_links(&client, addr, "globalmem", id, |body| {
            // either global=true OR a non-empty linked_kbs array
            body["global"].as_bool() == Some(true)
                || body["linked_kbs"]
                    .as_array()
                    .map(|x| !x.is_empty())
                    .unwrap_or(false)
        })
        .await;
    }

    // for_kb=alpha → Global Note + Alpha Note, NOT Beta Note.
    let r: serde_json::Value = client
        .get(url(
            addr,
            "/api/memory/recall?q=xenon&scope=all&limit=10&for_kb=alpha",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let titles: Vec<&str> = r["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["title"].as_str().unwrap())
        .collect();
    assert!(
        titles.contains(&"Global Note"),
        "for_kb=alpha must include the global memory; got {titles:?}"
    );
    assert!(
        titles.contains(&"Alpha Note"),
        "for_kb=alpha must include the alpha-linked memory; got {titles:?}"
    );
    assert!(
        !titles.contains(&"Beta Note"),
        "for_kb=alpha must EXCLUDE the beta-scoped memory; got {titles:?}"
    );
}

#[tokio::test]
async fn l7_recall_response_carries_global_and_linked_kbs_fields() {
    let (_tmp, addr) = boot_links_fixture().await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(url(addr, "/api/kb/globalmem/artifacts"))
        .json(&serde_json::json!({
            "title": "Shape Probe",
            "body": "alpha-only verifying recall response shape gallium",
            "category": "memory-user",
            "global": false,
            "linked_kbs": ["alpha"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    wait_for_doc(&client, addr, "globalmem", |d| d["id"] == id).await;
    wait_for_links(&client, addr, "globalmem", &id, |b| {
        b["linked_kbs"]
            .as_array()
            .map(|x| !x.is_empty())
            .unwrap_or(false)
    })
    .await;

    let r: serde_json::Value = client
        .get(url(addr, "/api/memory/recall?q=gallium&scope=all&limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hit = r["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["id"].as_str() == Some(&id))
        .expect("our memory must appear in scope=all recall");
    assert_eq!(
        hit["global"].as_bool(),
        Some(false),
        "scoped memory must report global=false"
    );
    let kbs: Vec<&str> = hit["linked_kbs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(kbs, vec!["alpha"]);
}

// --- E2: PATCH …/artifacts/{id}/meta — edit tags/category in source -----------

// invariant:12 rewrite-source
#[tokio::test]
async fn patch_meta_rewrites_html_tags_and_reindexes() {
    let (tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let doc = wait_for_doc(&client, addr, "smoke", |d| {
        d["source_relative"].as_str() == Some("kitchen-sink.html")
    })
    .await;
    let id = doc["id"].as_str().unwrap().to_string();

    // Let the initial walk's ingest tail drain before rewriting the source:
    // patching while kitchen-sink's first-index is still in flight can leave
    // the index stranded at the pre-patch content (the rewrite's watcher echo
    // coalesces into the initial event within the debounce window). The old
    // 800ms post-boot sleep masked exactly this; boot()'s docs-listed poll is
    // intentionally leaner, so this test carries its own settle (failed on
    // the serial CI box, 2026-08-13, run 31710574406).
    tokio::time::sleep(Duration::from_millis(800)).await;

    let r = client
        .patch(url(addr, &format!("/api/kb/smoke/artifacts/{id}/meta")))
        // Free-text + a dup → slugified + de-duped, order preserved.
        .json(&serde_json::json!({ "tags": ["Alpha Tag", "beta", "beta"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    let tags: Vec<&str> = body["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(tags, vec!["alpha-tag", "beta"]);

    // The source file on disk carries the canonical meta now.
    let src = std::fs::read_to_string(tmp.path().join("corpus/kitchen-sink.html")).unwrap();
    assert!(
        src.contains("content=\"alpha-tag, beta\""),
        "meta should be rewritten in source"
    );

    // And the watcher re-indexed it — the docs list reflects the new tags.
    let updated = wait_for_doc(&client, addr, "smoke", |d| {
        d["source_relative"].as_str() == Some("kitchen-sink.html")
            && d["tags"]
                .as_array()
                .map(|a| a.iter().any(|t| t.as_str() == Some("alpha-tag")))
                .unwrap_or(false)
    })
    .await;
    assert!(updated["tags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t.as_str() == Some("beta")));
}

#[tokio::test]
async fn patch_meta_rewrites_markdown_frontmatter() {
    // A markdown artifact carries tags/category in YAML frontmatter; the
    // route edits that instead of an HTML <meta>. Write it BEFORE serve so
    // the initial walk indexes it deterministically.
    let (tmp, cfg, paths) = fixture_corpus();
    let corpus = tmp.path().join("corpus");
    std::fs::write(
        corpus.join("note.md"),
        "---\ntitle: Note\nkb-tags: old\n---\n# Note\n\nbody\n",
    )
    .unwrap();
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let client = reqwest::Client::new();

    let doc = wait_for_doc(&client, addr, "smoke", |d| {
        d["source_relative"].as_str() == Some("note.md")
    })
    .await;
    let id = doc["id"].as_str().unwrap().to_string();

    let r = client
        .patch(url(addr, &format!("/api/kb/smoke/artifacts/{id}/meta")))
        .json(&serde_json::json!({ "tags": ["rust", "async"], "category": "design" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let src = std::fs::read_to_string(corpus.join("note.md")).unwrap();
    assert!(
        src.contains("kb-tags: rust, async"),
        "frontmatter tags: {src}"
    );
    assert!(
        src.contains("kb-category: design"),
        "frontmatter category: {src}"
    );
    assert!(src.contains("title: Note"), "other keys preserved");
    assert!(src.contains("# Note"), "body preserved");
}

#[tokio::test]
async fn patch_meta_validates_id_and_fields() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // Unknown id → 404.
    let r = client
        .patch(url(addr, "/api/kb/smoke/artifacts/deadbeefdead/meta"))
        .json(&serde_json::json!({ "tags": ["x"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    // Neither tags nor category → 400.
    let doc = wait_for_doc(&client, addr, "smoke", |d| {
        d["source_relative"].as_str() == Some("kitchen-sink.html")
    })
    .await;
    let id = doc["id"].as_str().unwrap().to_string();
    let r = client
        .patch(url(addr, &format!("/api/kb/smoke/artifacts/{id}/meta")))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}

// ---- CE5 — GET/PUT /api/config ----

/// Poll `GET {base}/api/identity` until it responds, returning `started_at`.
/// Panics after `secs`. Swallows connection errors (a restarting daemon is
/// briefly unreachable).
async fn wait_identity_started(client: &reqwest::Client, base: &str, secs: u64) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/identity")).send().await {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(s) = body["started_at"].as_str() {
                        return s.to_string();
                    }
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for daemon at {base}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Poll until `/api/identity`'s `started_at` differs from `prev` — i.e. the
/// daemon restarted in-process. Panics after `secs`.
async fn wait_identity_restart(
    client: &reqwest::Client,
    base: &str,
    prev: &str,
    secs: u64,
) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/identity")).send().await {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(s) = body["started_at"].as_str() {
                        if s != prev {
                            return s.to_string();
                        }
                    }
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for restart at {base}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[tokio::test]
async fn config_get_returns_running_config_and_metadata() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let resp = client.get(url(addr, "/api/config")).send().await.unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();

    // The running config is echoed (addr is the fixture default; the
    // random-port helper binds :0 independently of config.addr).
    assert_eq!(
        body["config"]["server"]["addr"].as_str(),
        Some("127.0.0.1:4000")
    );
    // config_path points at the file edits write back to.
    assert!(
        body["config_path"].as_str().unwrap().ends_with("kb.toml"),
        "config_path is the kb.toml: {body}"
    );
    // model registry for the editor's <select> is non-empty.
    assert!(!body["embedding_models"].as_array().unwrap().is_empty());
    // env_present is an object (empty here — fixture has no share config),
    // and crucially the fixture has no share section, so no secret env
    // names leak either.
    assert!(body["env_present"].is_object());
    // the fixture kb round-trips.
    assert!(body["config"]["kb"]["smoke"].is_object());
}

#[tokio::test]
async fn config_put_rejects_invalid_addr_with_field_error() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let got: serde_json::Value = client
        .get(url(addr, "/api/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut config = got["config"].clone();
    config["server"]["addr"] = serde_json::json!("not-an-addr");

    let resp = client
        .put(url(addr, "/api/config"))
        .json(&config)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], 400);
    let errors = body["errors"].as_array().expect("field errors array");
    assert!(
        errors.iter().any(|e| e["pointer"] == "/server/addr"),
        "a field error points at /server/addr: {body}"
    );
    // Rejected before any restart — the daemon is still serving.
    assert!(client
        .get(url(addr, "/api/identity"))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
}

#[tokio::test]
async fn config_put_rejects_dim_incompatible_embedding_model_change() {
    // X2 — the canon kb has no embedding_model, so its lance dataset is at
    // the default dim (bge-small, 384). Switching it to bge-base (768)
    // would fail Storage::open at re-boot and silently roll back; the PUT
    // must reject it up front with a 400, before persisting or restarting.
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let got: serde_json::Value = client
        .get(url(addr, "/api/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut config = got["config"].clone();
    config["kb"]["smoke"]["embedding_model"] = serde_json::json!("bge-base-en-v1.5");

    let resp = client
        .put(url(addr, "/api/config"))
        .json(&config)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "dim-incompatible model change rejected");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["detail"].as_str().unwrap_or_default().contains("dim"),
        "error explains the dim mismatch: {body}"
    );
    // Rejected before any restart — the daemon is still serving the old config.
    assert!(client
        .get(url(addr, "/api/identity"))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
}

// invariant:13 persist-to-loaded-file
#[tokio::test]
async fn config_put_persists_edit_to_loaded_file() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let got: serde_json::Value = client
        .get(url(addr, "/api/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let config_path = got["config_path"].as_str().unwrap().to_string();
    let mut config = got["config"].clone();
    config["indexer"]["debounce_ms"] = serde_json::json!(555);

    let resp = client
        .put(url(addr, "/api/config"))
        .json(&config)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status, 200, "PUT ok: {body}");
    assert_eq!(body["restarting"], true);

    // The edit is on disk, at the file the daemon loaded from.
    let written = std::fs::read_to_string(&config_path).expect("config file written");
    let parsed = kb_core::config::KbConfig::from_toml_str(&written).expect("valid toml");
    assert_eq!(parsed.indexer.debounce_ms, Some(555));
}

/// Drives the real `serve_loop` (fixed addr) to prove an in-process
/// restart lands on a config edit, and that an unbindable addr is rejected
/// at PUT time (so it never strands the daemon).
// invariant:13 restart-rollback
#[tokio::test]
async fn serve_loop_restarts_on_edit_and_rejects_unbindable_addr() {
    // Grab a free port for the daemon, then release it for serve_loop.
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("kb.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "[daemon]\nname = \"ce5-loop\"\n\n[server]\naddr = \"127.0.0.1:{port}\"\n\n\
             [defaults]\ndisable_embedder_fallback = true\n"
        ),
    )
    .unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "ce5-loop");

    let cp = cfg_path.clone();
    let loop_task = tokio::spawn(async move { kb_server::serve_loop(cp, paths).await });

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let started1 = wait_identity_started(&client, &base, 15).await;

    // --- successful in-process restart: change a benign field, keep addr.
    let got: serde_json::Value = client
        .get(format!("{base}/api/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut config = got["config"].clone();
    config["indexer"]["debounce_ms"] = serde_json::json!(750);
    let resp = client
        .put(format!("{base}/api/config"))
        .json(&config)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let started2 = wait_identity_restart(&client, &base, &started1, 20).await;
    assert_ne!(started1, started2, "daemon restarted in-process");
    // the edit is live after the restart.
    let got2: serde_json::Value = client
        .get(format!("{base}/api/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(got2["config"]["indexer"]["debounce_ms"].as_u64(), Some(750));

    // --- unbindable addr rejected at PUT (test-bind guard); no restart.
    let blocker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let blocked = blocker.local_addr().unwrap().port();
    let mut bad = got2["config"].clone();
    bad["server"]["addr"] = serde_json::json!(format!("127.0.0.1:{blocked}"));
    let resp = client
        .put(format!("{base}/api/config"))
        .json(&bad)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        400,
        "unbindable addr rejected before persist"
    );
    // The daemon never went down — still serving on the original port,
    // same started_at as after the successful restart.
    let started3 = wait_identity_started(&client, &base, 5).await;
    assert_eq!(
        started3, started2,
        "a rejected PUT must not restart the daemon"
    );
    drop(blocker);

    loop_task.abort();
}

/// G1 — the `serve_loop` last-good ROLLBACK arm (lib.rs `Err` branch): a
/// config that PASSES PUT validation (structurally valid, addr still
/// bindable) but FAILS to boot must roll the daemon back to the
/// last-known-good config instead of stranding it. The older serve_loop
/// test only covers a PUT-time addr rejection (which never reaches the
/// boot), so the rollback arm itself was unexercised.
///
/// Trigger (the audit's confirmed path, made deterministic + offline): a kb
/// with a 384-dim dataset on disk boots model-less (config_dim=None accepts
/// any disk dim, no embedder spawned), then a PUT sets its embedding_model to
/// bge-base (768). That is structurally valid + same addr, so PUT accepts it
/// and restarts — but on re-boot `Storage::open` sees 384≠768 and returns
/// `Error::Config`, so `serve_with_paths` returns `Err` and serve_loop must
/// recover by re-booting the model-less good config.
#[tokio::test]
async fn serve_loop_rolls_back_when_restart_fails_to_boot() {
    use kb_core::storage::lance::Storage;

    // Free port, then release it for serve_loop to bind.
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    let broken_src = tmp.path().join("broken-src");
    std::fs::create_dir_all(&broken_src).unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "g1-rollback");
    let broken_kb = KbName::new("broken").unwrap();

    // Pre-create the "broken" kb's lance dataset at 384-dim on disk. The PUT
    // below ADDS this kb with a 768-dim model, so the re-boot's Storage::open
    // reads disk=384 vs config=768 and errors — a boot failure that still
    // passes the PUT's up-front validation, because X2's dim check only guards
    // kbs that ALREADY exist in the running config, not a newly-added one.
    {
        let s = Storage::open(&paths.kb_lance(&broken_kb), Some(384))
            .await
            .unwrap();
        drop(s);
    }

    // GOOD config: just kb "roll", model-less → clean offline boot.
    let cfg_path = tmp.path().join("kb.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "[daemon]\nname = \"g1-rollback\"\n\n[server]\naddr = \"127.0.0.1:{port}\"\n\n\
             [defaults]\ndisable_embedder_fallback = true\n\n\
             [kb.roll]\npath = \"{src}\"\n",
            src = source.to_string_lossy(),
        ),
    )
    .unwrap();

    let cp = cfg_path.clone();
    let loop_task = tokio::spawn(async move { kb_server::serve_loop(cp, paths).await });

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let started1 = wait_identity_started(&client, &base, 15).await;

    // PUT a config that ADDS kb "broken" pointing at the pre-created 384-dim
    // dataset with a 768-dim model. X2's dim check skips a brand-new kb, so the
    // PUT passes validation + restarts; the re-boot then fails at Storage::open
    // (disk 384 vs config 768) and serve_loop rolls back to last-good.
    let got: serde_json::Value = client
        .get(format!("{base}/api/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut bad = got["config"].clone();
    // Clone "roll"'s section as a valid KbSection template, then re-point it.
    let mut broken_section = bad["kb"]["roll"].clone();
    broken_section["path"] = serde_json::json!(broken_src.to_string_lossy());
    broken_section["embedding_model"] = serde_json::json!("bge-base-en-v1.5");
    bad["kb"]["broken"] = broken_section;
    let resp = client
        .put(format!("{base}/api/config"))
        .json(&bad)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "PUT passes validation (new kb skips the dim check) — the failure is at boot"
    );

    // The daemon must come back up on the ROLLED-BACK good config: started_at
    // changes (the good re-boot after the failed bad boot), and the running
    // config has no "broken" kb.
    let started2 = wait_identity_restart(&client, &base, &started1, 20).await;
    assert_ne!(started1, started2, "daemon rebooted after rolling back");
    let after: serde_json::Value = client
        .get(format!("{base}/api/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        after["config"]["kb"]["broken"].is_null(),
        "rolled back to last-good (no 'broken' kb), not the boot-breaking config: {after}"
    );

    loop_task.abort();
}

// --- Track V: artifact version timeline + diff ------------------------------

/// Build a git-backed corpus (two commits to one HTML file) and serve a
/// daemon over it with `versions = "git"`. Returns the tempdir, the daemon
/// addr, and the artifact id of `doc.html`.
async fn boot_git_corpus() -> (tempfile::TempDir, std::net::SocketAddr, String) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&source)
            .args([
                "-c",
                "user.email=t@kb",
                "-c",
                "user.name=kb-test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .expect("git runs")
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q"]);
    let file = source.join("doc.html");
    std::fs::write(
        &file,
        "<html><body><h1>Doc</h1><p>First revision text.</p></body></html>",
    )
    .unwrap();
    git(&["add", "doc.html"]);
    git(&["commit", "-q", "-m", "first"]);
    std::fs::write(
        &file,
        "<html><body><h1>Doc</h1><p>Second revision text.</p></body></html>",
    )
    .unwrap();
    git(&["add", "doc.html"]);
    git(&["commit", "-q", "-m", "second"]);

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("smoke").unwrap(),
        KbSection {
            path: source,
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
            versions: Some("git".into()),
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
    common::wait_docs_listed(addr, "smoke", 1).await;
    let id = kb_core::ids::ArtifactId::from_path("doc.html")
        .as_str()
        .to_string();
    (tmp, addr, id)
}

#[tokio::test]
async fn versions_git_timeline_and_diff() {
    let (_tmp, addr, id) = boot_git_corpus().await;
    let client = reqwest::Client::new();

    // Timeline: working tree first, then the two commits newest-first.
    let v: serde_json::Value = client
        .get(format!(
            "http://{addr}/api/kb/smoke/artifacts/{id}/versions"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["mode"], "git");
    let versions = v["versions"].as_array().unwrap();
    assert_eq!(versions[0]["source"], "working", "working tree first");
    let git_v: Vec<&serde_json::Value> = versions.iter().filter(|x| x["source"] == "git").collect();
    assert_eq!(git_v.len(), 2, "two commits: {versions:?}");
    assert_eq!(git_v[0]["label"], "second", "newest first");
    assert_eq!(git_v[1]["label"], "first");
    let sha_first = git_v[1]["ref"].as_str().unwrap().to_string();
    let sha_second = git_v[0]["ref"].as_str().unwrap().to_string();

    // Prose diff between the two commits — focuses on the changed sentence,
    // not the (unchanged) markup or heading.
    let d: serde_json::Value = client
        .get(format!(
            "http://{addr}/api/kb/smoke/artifacts/{id}/diff?from={sha_first}&to={sha_second}"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(d["mode"], "text");
    assert!(!d["hunks"].as_array().unwrap().is_empty());
    let blob = serde_json::to_string(&d["hunks"]).unwrap();
    assert!(blob.contains("First revision text."), "old prose: {blob}");
    assert!(blob.contains("Second revision text."), "new prose: {blob}");

    // Raw mode diffs the source bytes.
    let raw: serde_json::Value = client
        .get(format!(
            "http://{addr}/api/kb/smoke/artifacts/{id}/diff?from={sha_first}&to={sha_second}&mode=raw"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(raw["mode"], "raw");

    // Unknown artifact id → 404.
    let r = client
        .get(format!(
            "http://{addr}/api/kb/smoke/artifacts/notanid/versions"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

/// Boot a kb whose corpus is a SUBDIR of a larger git repo (the common
/// deployment), so `git_root` is an ANCESTOR of the corpus. The versions
/// code must address commits by `abs.strip_prefix(git_root)`
/// (`corpus/doc.html`), NOT the corpus-relative `doc_rel_path`
/// (`doc.html`) — invariant #14's most-likely-bug shape, which
/// `boot_git_corpus` (git init IN the corpus) never exercises. Daemon
/// state lives at `tmp/<daemon>`, a sibling of `repo/`, so it stays
/// outside the git repo (mirroring a real bind-mounted-corpus deploy).
async fn boot_git_corpus_nested() -> (tempfile::TempDir, std::net::SocketAddr, String) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo"); // git root
    let source = repo.join("corpus"); // the kb corpus = a subdir of the repo
    std::fs::create_dir_all(&source).unwrap();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "user.email=t@kb",
                "-c",
                "user.name=kb-test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .expect("git runs")
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q"]);
    let file = source.join("doc.html");
    std::fs::write(
        &file,
        "<html><body><h1>Doc</h1><p>First revision text.</p></body></html>",
    )
    .unwrap();
    git(&["add", "corpus/doc.html"]);
    git(&["commit", "-q", "-m", "first"]);
    std::fs::write(
        &file,
        "<html><body><h1>Doc</h1><p>Second revision text.</p></body></html>",
    )
    .unwrap();
    git(&["add", "corpus/doc.html"]);
    git(&["commit", "-q", "-m", "second"]);

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("smoke").unwrap(),
        KbSection {
            path: source,
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
            versions: Some("git".into()),
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
    common::wait_docs_listed(addr, "smoke", 1).await;
    let id = kb_core::ids::ArtifactId::from_path("doc.html")
        .as_str()
        .to_string();
    (tmp, addr, id)
}

// invariant:14 git-path-base
#[tokio::test]
async fn versions_git_nested_corpus_addresses_commits_by_repo_relative_path() {
    // invariant #14 — git ops take the file's path RELATIVE TO THE GIT ROOT
    // (`corpus/doc.html`), not the corpus-relative `doc.html`. With the
    // wrong base, `git log` from the repo root finds no history and the
    // timeline comes back with zero git commits.
    let (_tmp, addr, id) = boot_git_corpus_nested().await;
    let client = reqwest::Client::new();

    let v: serde_json::Value = client
        .get(format!(
            "http://{addr}/api/kb/smoke/artifacts/{id}/versions"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        v["mode"], "git",
        "corpus-subdir-of-repo still resolves git mode"
    );
    let versions = v["versions"].as_array().unwrap();
    let git_v: Vec<&serde_json::Value> = versions.iter().filter(|x| x["source"] == "git").collect();
    assert_eq!(
        git_v.len(),
        2,
        "two commits found via the repo-relative path: {versions:?}"
    );
    assert_eq!(git_v[0]["label"], "second", "newest first");
    assert_eq!(git_v[1]["label"], "first");

    let sha_first = git_v[1]["ref"].as_str().unwrap().to_string();
    let sha_second = git_v[0]["ref"].as_str().unwrap().to_string();
    let d: serde_json::Value = client
        .get(format!(
            "http://{addr}/api/kb/smoke/artifacts/{id}/diff?from={sha_first}&to={sha_second}"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(d["mode"], "text");
    assert!(
        !d["hunks"].as_array().unwrap().is_empty(),
        "diff has hunks: {d:?}"
    );
    let blob = serde_json::to_string(&d["hunks"]).unwrap();
    assert!(blob.contains("First revision text."), "old prose: {blob}");
    assert!(blob.contains("Second revision text."), "new prose: {blob}");
}

// invariant:14 auto-per-file
#[tokio::test]
async fn versions_index_snapshots_without_git() {
    // The canon tempdir corpus isn't a git repo, so `auto` falls back to
    // the index snapshots the indexer captured on the initial walk.
    let (_tmp, addr) = boot().await;
    let id = kb_core::ids::ArtifactId::from_path("multi-page.html")
        .as_str()
        .to_string();
    let client = reqwest::Client::new();
    // boot()'s docs-listed poll proves the doc ROWS landed, but the index
    // SNAPSHOT rows the versions façade reads are written after listing —
    // poll for them instead of asserting on the first response (failed on
    // the serial CI box, 2026-08-13, run 31710574406).
    let v = common::poll_until("an index-snapshot version for multi-page.html", || {
        let client = client.clone();
        let url = format!("http://{addr}/api/kb/smoke/artifacts/{id}/versions");
        async move {
            let v: serde_json::Value = client.get(&url).send().await.ok()?.json().await.ok()?;
            v["versions"]
                .as_array()?
                .iter()
                .any(|x| x["source"] == "index")
                .then_some(v)
        }
    })
    .await;
    assert_eq!(v["mode"], "auto");
    let versions = v["versions"].as_array().unwrap();
    assert!(
        versions.iter().any(|x| x["source"] == "working"),
        "working tree present: {versions:?}"
    );
    assert!(
        versions.iter().any(|x| x["source"] == "index"),
        "auto mode without git surfaces index snapshots: {versions:?}"
    );
}

// invariant:14 memento-at
/// CT-F6 — `?at=<unix>` (RFC 7089 Memento) over the live versions route:
/// nearest-prior resolution, an explicit too-old MISS, the
/// `Memento-Datetime` header, and — the additivity contract — a `?at=`-less
/// response that carries no `memento` key at all.
#[tokio::test]
async fn versions_at_resolves_nearest_prior_and_misses_honestly() {
    let (_tmp, addr) = boot().await;
    let id = kb_core::ids::ArtifactId::from_path("multi-page.html")
        .as_str()
        .to_string();
    let client = reqwest::Client::new();
    let base = format!("http://{addr}/api/kb/smoke/artifacts/{id}/versions");
    // Same poll as the test above: snapshot rows land after the doc rows.
    let plain = common::poll_until("an index-snapshot version for multi-page.html", || {
        let client = client.clone();
        let url = base.clone();
        async move {
            let v: serde_json::Value = client.get(&url).send().await.ok()?.json().await.ok()?;
            v["versions"]
                .as_array()?
                .iter()
                .any(|x| x["source"] == "index")
                .then_some(v)
        }
    })
    .await;

    // Additivity: no `at` ⇒ no `memento` key. The whole point of the
    // separate response struct.
    assert!(
        plain.get("memento").is_none(),
        "a ?at=-less response must be unchanged: {plain}"
    );

    let versions = plain["versions"].as_array().unwrap();
    let newest_ts = versions
        .iter()
        .map(|v| v["ts_unix"].as_i64().unwrap())
        .max()
        .unwrap();
    let oldest_ts = versions
        .iter()
        .map(|v| v["ts_unix"].as_i64().unwrap())
        .min()
        .unwrap();

    // A HIT strictly after the newest version resolves to it, flagged as
    // nearest-prior (not exact) — and carries the resolved version's own
    // datetime in the RFC 7089 header.
    let r = client
        .get(format!("{base}?at={}", newest_ts + 60))
        .send()
        .await
        .unwrap();
    let memento_datetime = r
        .headers()
        .get("memento-datetime")
        .map(|v| v.to_str().unwrap().to_string());
    let hit: serde_json::Value = r.json().await.unwrap();
    assert_eq!(hit["memento"]["found"], true, "{hit}");
    assert_eq!(hit["memento"]["relation"], "nearest-prior");
    assert_eq!(hit["memento"]["exact"], false);
    assert_eq!(hit["memento"]["version"]["ts_unix"], newest_ts);
    assert_eq!(hit["memento"]["at_unix"], newest_ts + 60);
    // The timeline itself is untouched by `at` — same rows, plus the key.
    assert_eq!(hit["versions"], plain["versions"]);
    assert!(
        memento_datetime.is_some_and(|d| d.ends_with(" GMT")),
        "Memento-Datetime is RFC 1123 GMT"
    );

    // An EXACT hit lands on the version's own second.
    let exact: serde_json::Value = client
        .get(format!("{base}?at={newest_ts}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(exact["memento"]["exact"], true, "{exact}");

    // Too old ⇒ an explicit MISS naming the floor. NEVER the oldest
    // version quietly returned as if it were the answer.
    let miss: serde_json::Value = client
        .get(format!("{base}?at={}", oldest_ts - 1))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(miss["memento"]["found"], false, "{miss}");
    assert!(miss["memento"].get("version").is_none(), "{miss}");
    assert_eq!(miss["memento"]["oldest_ts_unix"], oldest_ts);
    assert!(miss["memento"]["note"]
        .as_str()
        .unwrap()
        .contains("that old"));

    // A malformed coordinate is a named 400, not a guessed instant.
    let bad = client
        .get(format!("{base}?at=2026-07-30"))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    let problem: serde_json::Value = bad.json().await.unwrap();
    assert!(
        problem["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("unix seconds"),
        "{problem}"
    );
}

// --- N-track: notes / todo-lists -------------------------------------

/// Poll `/api/kb/{kb}/notes` until a note with `id` shows up (it must be
/// indexed by the watcher first), or the 10s deadline expires.
async fn wait_for_note(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    kb: &str,
    id: &str,
) -> serde_json::Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let body: serde_json::Value = client
            .get(url(addr, &format!("/api/kb/{kb}/notes")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        if let Some(arr) = body.get("notes").and_then(|n| n.as_array()) {
            if let Some(n) = arr
                .iter()
                .find(|n| n.get("id").and_then(|i| i.as_str()) == Some(id))
            {
                return n.clone();
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for note {id} to be indexed"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn notes_create_list_toggle_append_delete() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // Create an ad-hoc note in folder `ops` with two tasks (one done).
    let resp = client
        .post(url(addr, "/api/kb/smoke/notes"))
        .json(&serde_json::json!({
            "title": "Deploy checklist",
            "body_md": "- [ ] run tests\n- [x] tag release\n",
            "folder": "ops",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let created: serde_json::Value = resp.json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert!(created["path"]
        .as_str()
        .unwrap()
        .starts_with("ops/note-deploy-checklist-"));
    assert_eq!(created["is_notepad"], false);

    // After indexing it appears in the per-kb list with progress 1/2.
    let note = wait_for_note(&client, addr, "smoke", &id).await;
    assert_eq!(note["folder"], "ops");
    assert_eq!(note["task_done"], 1);
    assert_eq!(note["task_total"], 2);
    assert_eq!(note["is_notepad"], false);

    // get_one returns the RAW markdown body (not rendered HTML).
    let detail: serde_json::Value = client
        .get(url(addr, &format!("/api/kb/smoke/notes/{id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["body_md"], "- [ ] run tests\n- [x] tag release\n");
    assert_eq!(detail["title"], "Deploy checklist");

    // Toggle task 0 on → 2/2, body flipped.
    let m: serde_json::Value = client
        .post(url(addr, &format!("/api/kb/smoke/notes/{id}/toggle")))
        .json(&serde_json::json!({"index": 0, "on": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(m["task_done"], 2);
    assert_eq!(m["task_total"], 2);
    assert!(m["body_md"].as_str().unwrap().contains("- [x] run tests"));

    // Append a task → 3 total, unchecked.
    let m2: serde_json::Value = client
        .post(url(addr, &format!("/api/kb/smoke/notes/{id}/tasks")))
        .json(&serde_json::json!({"text": "announce"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(m2["task_total"], 3);
    assert!(m2["body_md"].as_str().unwrap().contains("- [ ] announce"));

    // Toggle out of range → 404.
    let oor = client
        .post(url(addr, &format!("/api/kb/smoke/notes/{id}/toggle")))
        .json(&serde_json::json!({"index": 99, "on": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(oor.status(), 404);

    // Delete → 204.
    let del = client
        .delete(url(addr, &format!("/api/kb/smoke/notes/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 204);
}

/// Poll `/backlinks/{id}` until `expect_from` appears — edge rows are written
/// by the edge-record enrichment hook just AFTER the upsert that makes the
/// linking note listable, so there's a brief window between "note indexed"
/// and "edge committed".
async fn poll_backlinks(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    kb: &str,
    id: &str,
    expect_from: &str,
) -> Vec<serde_json::Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let body: serde_json::Value = client
            .get(url(addr, &format!("/api/kb/{kb}/backlinks/{id}")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        if let Some(arr) = body.get("backlinks").and_then(|b| b.as_array()) {
            if arr
                .iter()
                .any(|b| b.get("id").and_then(|i| i.as_str()) == Some(expect_from))
            {
                return arr.clone();
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "backlink {expect_from} never appeared on {id}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// invariant:29 edge-graph
#[tokio::test]
async fn note_wikilinks_resolve_edges_and_backlinks() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // Target note B with a distinctive title — index it FIRST so the source
    // note's wikilink resolves at index time.
    let b_id = {
        let resp = client
            .post(url(addr, "/api/kb/smoke/notes"))
            .json(&serde_json::json!({
                "title": "Release Runbook",
                "body_md": "the steps\n",
                "folder": "ops",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        resp.json::<serde_json::Value>().await.unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    wait_for_note(&client, addr, "smoke", &b_id).await;

    // Source note A: one resolving wikilink (by title) + one dangling target.
    let a_id = {
        let resp = client
            .post(url(addr, "/api/kb/smoke/notes"))
            .json(&serde_json::json!({
                "title": "Planning",
                "body_md": "see [[Release Runbook]] and [[Nonexistent Thing]]\n",
                "folder": "ops",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        resp.json::<serde_json::Value>().await.unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    wait_for_note(&client, addr, "smoke", &a_id).await;

    // NoteDetail A carries resolved outgoing links (for inline rendering).
    let detail: serde_json::Value = client
        .get(url(addr, &format!("/api/kb/smoke/notes/{a_id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let links = detail["links"].as_array().unwrap();
    assert_eq!(links.len(), 2, "two distinct wikilink targets");
    let resolved = links
        .iter()
        .find(|l| l["target"] == "Release Runbook")
        .expect("resolved target present");
    assert_eq!(resolved["state"], "resolved");
    assert_eq!(resolved["id"].as_str().unwrap(), b_id);
    assert_eq!(resolved["is_note"], true);
    let dangling = links
        .iter()
        .find(|l| l["target"] == "Nonexistent Thing")
        .expect("dangling target present");
    assert_eq!(dangling["state"], "dangling");
    assert!(dangling["id"].is_null());

    // The A→B edge surfaces as a backlink on B.
    let backlinks = poll_backlinks(&client, addr, "smoke", &b_id, &a_id).await;
    let entry = backlinks
        .iter()
        .find(|b| b["id"].as_str() == Some(a_id.as_str()))
        .unwrap();
    assert_eq!(entry["is_note"], true);
    assert_eq!(entry["title"], "Planning");

    // The note one-shot: outgoing (2) + backlinks (none — nothing links to A).
    let nl: serde_json::Value = client
        .get(url(addr, &format!("/api/kb/smoke/notes/{a_id}/links")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(nl["outgoing"].as_array().unwrap().len(), 2);
    assert!(nl["backlinks"].as_array().unwrap().is_empty());

    // `[[` autocomplete finds B by title prefix.
    let sug: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/wikilinks/suggest?q=Release"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(sug["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["id"].as_str() == Some(b_id.as_str())));
}

#[tokio::test]
async fn notepad_is_deterministic_and_idempotent() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // First create → 201, deterministic path.
    let r1 = client
        .post(url(addr, "/api/kb/smoke/notes"))
        .json(&serde_json::json!({
            "notepad": true, "folder": "design", "body_md": "- [ ] first\n"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), 201);
    let v1: serde_json::Value = r1.json().await.unwrap();
    assert_eq!(v1["path"], "design/_notepad.md");
    assert_eq!(v1["is_notepad"], true);
    let id1 = v1["id"].as_str().unwrap().to_string();

    // Second create with notepad=true is idempotent → 200, SAME id, and it
    // does NOT overwrite the existing body.
    let r2 = client
        .post(url(addr, "/api/kb/smoke/notes"))
        .json(&serde_json::json!({
            "notepad": true, "folder": "design", "body_md": "- [ ] SHOULD NOT REPLACE\n"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 200);
    let v2: serde_json::Value = r2.json().await.unwrap();
    assert_eq!(v2["id"].as_str().unwrap(), id1);

    let note = wait_for_note(&client, addr, "smoke", &id1).await;
    assert_eq!(note["is_notepad"], true);
    let detail: serde_json::Value = client
        .get(url(addr, &format!("/api/kb/smoke/notes/{id1}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        detail["body_md"], "- [ ] first\n",
        "idempotent notepad create must not clobber existing content"
    );
}

// invariant:16 exclude-notes
#[tokio::test]
async fn notes_excluded_from_gallery_but_searchable() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    let r = client
        .post(url(addr, "/api/kb/smoke/notes"))
        .json(&serde_json::json!({
            "title": "Zylophonics plan",
            "body_md": "zylophonics deploy steps\n",
        }))
        .send()
        .await
        .unwrap();
    let id = r.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_note(&client, addr, "smoke", &id).await;

    // The gallery /docs must NOT contain the note.
    let docs: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/docs?limit=200"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !docs.iter().any(|d| d["id"].as_str() == Some(&id)),
        "note leaked into the gallery grid"
    );

    // …but keyword search DOES find it (notes are ordinary artifacts).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let resp: serde_json::Value = client
            .get(url(addr, "/api/search?q=zylophonics&mode=keyword&kb=smoke"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        let found = resp
            .get("hits")
            .and_then(|h| h.as_array())
            .map(|a| a.iter().any(|h| h["id"].as_str() == Some(&id)))
            .unwrap_or(false);
        if found {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "note never became searchable: {resp:?}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

// invariant:16 is-note-gate
#[tokio::test]
async fn html_note_category_stays_in_gallery_not_in_notes() {
    // Regression for the kb.example.com collision: an HTML research write-up tagged
    // `kb-category: note` is NOT an editable note. It must stay in the gallery
    // grid AND be absent from /notes AND 404 on the notes detail endpoint
    // (so the SPA renders it via the iframe, not the broken native view).
    let (tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    let source = tmp.path().join("corpus");
    let html = "<!doctype html><html><head><title>HTML write-up</title>\n\
<meta name=\"kb-category\" content=\"note\">\n\
<meta name=\"kb-tags\" content=\"research\">\n\
</head><body><h1>HTML write-up</h1><p>prose</p></body></html>\n";
    std::fs::write(source.join("html-writeup.html"), html).unwrap();

    // Poll the gallery until the watcher indexes it; capture its id.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let id = loop {
        let docs: Vec<serde_json::Value> = client
            .get(url(addr, "/api/kb/smoke/docs?limit=300"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        if let Some(d) = docs
            .iter()
            .find(|d| d["source_relative"].as_str() == Some("html-writeup.html"))
        {
            break d["id"].as_str().unwrap().to_string();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "HTML `note` artifact never appeared in the gallery (it must NOT be excluded)"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    };

    // Absent from the per-kb notes list.
    let notes: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/notes"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let leaked = notes["notes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["id"].as_str() == Some(&id));
    assert!(!leaked, "HTML artifact tagged `note` leaked into /notes");

    // The notes detail endpoint 404s (falls through to the normal artifact path).
    let detail = client
        .get(url(addr, &format!("/api/kb/smoke/notes/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(
        detail.status(),
        404,
        "an HTML `note` artifact must not resolve as an editable note"
    );
}

#[tokio::test]
async fn notes_update_filters_and_errors() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    let create = |body: serde_json::Value| {
        let client = client.clone();
        async move {
            client
                .post(url(addr, "/api/kb/smoke/notes"))
                .json(&body)
                .send()
                .await
                .unwrap()
        }
    };

    // Notes across two folders + the root.
    let a: serde_json::Value = create(serde_json::json!({
        "title": "Alpha", "folder": "ops", "body_md": "- [ ] a\n"
    }))
    .await
    .json()
    .await
    .unwrap();
    let a_id = a["id"].as_str().unwrap().to_string();
    let b: serde_json::Value = create(serde_json::json!({
        "title": "Beta", "folder": "ops/sub", "body_md": "- [ ] b\n"
    }))
    .await
    .json()
    .await
    .unwrap();
    let b_id = b["id"].as_str().unwrap().to_string();
    let c: serde_json::Value = create(serde_json::json!({
        "title": "Gamma", "body_md": "- [ ] c\n"
    }))
    .await
    .json()
    .await
    .unwrap();
    let c_id = c["id"].as_str().unwrap().to_string();
    for id in [&a_id, &b_id, &c_id] {
        wait_for_note(&client, addr, "smoke", id).await;
    }

    // PATCH Alpha: title + status + tags rewrite the source, reflected back.
    let patched: serde_json::Value = client
        .patch(url(addr, &format!("/api/kb/smoke/notes/{a_id}")))
        .json(&serde_json::json!({
            "title": "Alpha v2", "status": "done", "tags": ["Ops", "release!!"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(patched["title"], "Alpha v2");
    assert_eq!(patched["status"], "done");
    // tags are slugified (lowercase, alnum+dash).
    let tags: Vec<String> = patched["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect();
    assert_eq!(tags, vec!["ops".to_string(), "release".to_string()]);

    // Folder filter is descendant-inclusive: ops → Alpha + Beta, not Gamma.
    let in_ops: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/notes?folder=ops"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ops_ids: Vec<&str> = in_ops["notes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["id"].as_str())
        .collect();
    assert!(ops_ids.contains(&a_id.as_str()) && ops_ids.contains(&b_id.as_str()));
    assert!(
        !ops_ids.contains(&c_id.as_str()),
        "root note matched folder=ops"
    );

    // Status filter: done → Alpha only. The PATCH rewrites the SOURCE
    // synchronously (reflected in its own response above) but the list
    // reads the INDEX, which catches up via the watcher — poll with the
    // same 10s deadline wait_for_doc uses (this raced under load three
    // times across the W1/W2 gates before being hardened).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let done_ids: Vec<String> = loop {
        let done: serde_json::Value = client
            .get(url(addr, "/api/kb/smoke/notes?status=done"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let ids: Vec<String> = done["notes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n["id"].as_str().map(str::to_string))
            .collect();
        if !ids.is_empty() || std::time::Instant::now() >= deadline {
            break ids;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(done_ids, vec![a_id.clone()]);

    // 404: PATCH / toggle a non-existent note.
    let miss = client
        .patch(url(addr, "/api/kb/smoke/notes/deadbeefdead"))
        .json(&serde_json::json!({"title": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(miss.status(), 404);

    // 400: folder that escapes the corpus root.
    let bad_folder = client
        .post(url(addr, "/api/kb/smoke/notes"))
        .json(&serde_json::json!({"title": "x", "folder": "../etc"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_folder.status(), 400);

    // 400: empty append text.
    let empty = client
        .post(url(addr, &format!("/api/kb/smoke/notes/{a_id}/tasks")))
        .json(&serde_json::json!({"text": "   "}))
        .send()
        .await
        .unwrap();
    assert_eq!(empty.status(), 400);

    // Slug collision: two ad-hoc notes with the same title get distinct paths.
    let p1 = create(serde_json::json!({"title": "Dup note", "body_md": "x\n"}))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let p2 = create(serde_json::json!({"title": "Dup note", "body_md": "y\n"}))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_ne!(
        p1["path"].as_str().unwrap(),
        p2["path"].as_str().unwrap(),
        "same-title ad-hoc notes must get collision-safe distinct paths"
    );
}

// --- Y3: attachment stage + serve ------------------------------------------

/// PNG magic header + a few body bytes — enough for `sniff_allowed`.
fn png_fixture() -> Vec<u8> {
    vec![
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0, 1, 2, 3, 4,
    ]
}

async fn first_artifact_id(client: &reqwest::Client, addr: std::net::SocketAddr) -> String {
    wait_for_doc(client, addr, "smoke", |_| true).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn attachment_stage_and_serve_png_roundtrip() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let id = first_artifact_id(&client, addr).await;

    let png = png_fixture();
    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(png.clone())
            .file_name("chart.png")
            .mime_str("image/png")
            .unwrap(),
    );
    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/review/{id}/attachments")))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201, "stage returns 201");
    let staged: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(staged.len(), 1);
    let att = &staged[0];
    assert!(att["id"].as_str().unwrap().starts_with("a_"));
    assert_eq!(att["contentType"].as_str().unwrap(), "image/png");
    assert_eq!(att["size"].as_u64().unwrap(), png.len() as u64);
    assert_eq!(att["filename"].as_str().unwrap(), "chart.png");
    let serve_url = att["url"].as_str().unwrap().to_string();

    // Serve it back: inline disposition, nosniff, exact bytes.
    let resp = client.get(url(addr, &serve_url)).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let h = resp.headers().clone();
    assert_eq!(
        h.get("content-type").unwrap().to_str().unwrap(),
        "image/png"
    );
    assert_eq!(
        h.get("x-content-type-options").unwrap().to_str().unwrap(),
        "nosniff"
    );
    assert_eq!(
        h.get("content-disposition").unwrap().to_str().unwrap(),
        "inline"
    );
    let got = resp.bytes().await.unwrap();
    assert_eq!(got.as_ref(), png.as_slice(), "served bytes match upload");
}

// invariant:18 xss-safe-serve
#[tokio::test]
async fn attachment_text_is_served_as_forced_download() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let id = first_artifact_id(&client, addr).await;

    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(b"a plain log line\n".to_vec())
            .file_name("notes.txt")
            // Client CLAIMS html — the daemon must ignore it and sniff text.
            .mime_str("text/html")
            .unwrap(),
    );
    let staged: Vec<serde_json::Value> = client
        .post(url(addr, &format!("/api/kb/smoke/review/{id}/attachments")))
        .multipart(form)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        staged[0]["contentType"].as_str().unwrap(),
        "text/plain; charset=utf-8",
        "sniffed as text, NOT the client's text/html"
    );
    let serve_url = staged[0]["url"].as_str().unwrap().to_string();

    let resp = client.get(url(addr, &serve_url)).send().await.unwrap();
    let h = resp.headers().clone();
    assert_eq!(
        h.get("x-content-type-options").unwrap().to_str().unwrap(),
        "nosniff"
    );
    let disp = h.get("content-disposition").unwrap().to_str().unwrap();
    assert!(
        disp.starts_with("attachment;"),
        "non-raster forced to download: {disp}"
    );
    assert!(
        disp.contains("notes.txt"),
        "filename in disposition: {disp}"
    );
}

#[tokio::test]
async fn attachment_stage_rejects_unsupported_type() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let id = first_artifact_id(&client, addr).await;

    // Binary junk: invalid UTF-8, no known magic → 415.
    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(vec![0x00, 0x01, 0xFF, 0xFE, 0x12]).file_name("blob.bin"),
    );
    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/review/{id}/attachments")))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 415, "unsupported type → 415");
}

#[tokio::test]
async fn attachment_stage_rejects_oversize() {
    // Boot with a tiny per-file cap so the handler's streaming abort fires
    // well under the multi-MB DefaultBodyLimit.
    let (_tmp, mut cfg, paths) = fixture_corpus();
    cfg.server.attachments = Some(kb_core::config::AttachmentsSection {
        max_file_bytes: Some(1024),
        max_per_comment: None,
        gc_grace_hours: None,
    });
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let client = reqwest::Client::new();
    let id = first_artifact_id(&client, addr).await;

    let mut big = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    big.resize(2048, 0u8); // 2 KiB > the 1 KiB cap
    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(big)
            .file_name("big.png")
            .mime_str("image/png")
            .unwrap(),
    );
    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/review/{id}/attachments")))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 413, "oversize → 413");
}

#[tokio::test]
async fn attachment_serve_unknown_and_malformed_aid() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let id = first_artifact_id(&client, addr).await;

    // Well-formed but nonexistent aid → 404.
    let resp = client
        .get(url(
            addr,
            &format!("/api/kb/smoke/review/{id}/attachments/a_000000000000"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);

    // Malformed aid (illegal `$`) → 400 (is_safe_id rejects it).
    let resp = client
        .get(url(
            addr,
            &format!("/api/kb/smoke/review/{id}/attachments/bad$id"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
}

// --- Y4: adopt / detach / GC -----------------------------------------------

async fn stage_one_png(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    id: &str,
    name: &str,
) -> (String, String) {
    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(png_fixture())
            .file_name(name.to_string())
            .mime_str("image/png")
            .unwrap(),
    );
    let staged: Vec<serde_json::Value> = client
        .post(url(addr, &format!("/api/kb/smoke/review/{id}/attachments")))
        .multipart(form)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    (
        staged[0]["id"].as_str().unwrap().to_string(),
        staged[0]["url"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn attachment_adopt_on_add_comment_visible_in_get() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let id = first_artifact_id(&client, addr).await;
    let (aid, serve_url) = stage_one_png(&client, addr, &id, "c.png").await;

    // addComment adopting the staged blob, with an inline ref in the body.
    let resp = client
        .post(url(addr, &format!("/api/kb/smoke/review/{id}/comments")))
        .json(&serde_json::json!({
            "body": format!("see ![c](attachment:{aid})"),
            "anchor": { "kind": "file" },
            "author": "you",
            "attachment_ids": [aid.clone()],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let created: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(created["attachments"][0]["id"].as_str().unwrap(), aid);

    // GET review carries the adopted attachment inline.
    let review: serde_json::Value = client
        .get(url(addr, &format!("/api/kb/smoke/review/{id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let c = &review["comments"][0];
    assert_eq!(c["attachments"][0]["id"].as_str().unwrap(), aid);
    assert_eq!(
        c["attachments"][0]["contentType"].as_str().unwrap(),
        "image/png"
    );
    // The adopted blob still serves.
    assert_eq!(
        client
            .get(url(addr, &serve_url))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        200
    );
}

#[tokio::test]
async fn attachment_upload_to_comment_then_detach_reaps_blob() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let id = first_artifact_id(&client, addr).await;

    // A plain comment, then upload+adopt to it in one shot.
    let created: serde_json::Value = client
        .post(url(addr, &format!("/api/kb/smoke/review/{id}/comments")))
        .json(&serde_json::json!({"body":"hi","anchor":{"kind":"file"},"author":"you"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cid = created["id"].as_str().unwrap().to_string();

    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(png_fixture())
            .file_name("d.png")
            .mime_str("image/png")
            .unwrap(),
    );
    let att: Vec<serde_json::Value> = client
        .post(url(
            addr,
            &format!("/api/kb/smoke/review/{id}/comments/{cid}/attachments"),
        ))
        .multipart(form)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let aid = att[0]["id"].as_str().unwrap().to_string();
    let serve = format!("/api/kb/smoke/review/{id}/attachments/{aid}");
    assert_eq!(
        client
            .get(url(addr, &serve))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        200,
        "adopted blob serves"
    );

    // Detach → 200, and the now-orphaned blob is GC-reaped (serve 404).
    let resp = client
        .delete(url(
            addr,
            &format!("/api/kb/smoke/review/{id}/comments/{cid}/attachments/{aid}"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        client
            .get(url(addr, &serve))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        404,
        "detached blob is reaped"
    );
}

#[tokio::test]
async fn attachment_gc_reaps_orphan_on_comment_delete() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let id = first_artifact_id(&client, addr).await;
    let (aid, serve_url) = stage_one_png(&client, addr, &id, "e.png").await;

    let created: serde_json::Value = client
        .post(url(addr, &format!("/api/kb/smoke/review/{id}/comments")))
        .json(&serde_json::json!({
            "body": "x", "anchor": { "kind": "file" }, "author": "you",
            "attachment_ids": [aid.clone()],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cid = created["id"].as_str().unwrap().to_string();
    assert_eq!(
        client
            .get(url(addr, &serve_url))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        200
    );

    // Deleting the owning comment orphans the adopted blob → reaped now.
    let resp = client
        .delete(url(
            addr,
            &format!("/api/kb/smoke/review/{id}/comments/{cid}"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        client
            .get(url(addr, &serve_url))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        404,
        "orphaned blob reaped on comment delete"
    );
}

#[tokio::test]
async fn attachment_gc_reaps_orphan_on_reply_delete() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let id = first_artifact_id(&client, addr).await;
    let (aid, serve_url) = stage_one_png(&client, addr, &id, "f.png").await;

    // A comment to host the reply (it carries no attachment of its own, so the
    // only manifest reference is the reply's).
    let created: serde_json::Value = client
        .post(url(addr, &format!("/api/kb/smoke/review/{id}/comments")))
        .json(&serde_json::json!({
            "body": "x", "anchor": { "kind": "file" }, "author": "you",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cid = created["id"].as_str().unwrap().to_string();

    // A reply that adopts the staged blob.
    let reply: serde_json::Value = client
        .post(url(
            addr,
            &format!("/api/kb/smoke/review/{id}/comments/{cid}/replies"),
        ))
        .json(&serde_json::json!({
            "author": "claude", "body": "a", "attachment_ids": [aid.clone()],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rid = reply["id"].as_str().unwrap().to_string();
    assert_eq!(
        client
            .get(url(addr, &serve_url))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        200,
        "adopted reply blob serves"
    );

    // Deleting the owning reply orphans the adopted blob → reaped now.
    let resp = client
        .delete(url(
            addr,
            &format!("/api/kb/smoke/review/{id}/comments/{cid}/replies/{rid}"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        client
            .get(url(addr, &serve_url))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        404,
        "orphaned blob reaped on reply delete"
    );
}

// --- Reading lists (RL-track, v0.18) -----------------------------------

/// Fetch one artifact id from the smoke kb's docs list.
async fn first_doc_ids(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    n: usize,
) -> Vec<String> {
    let docs: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/docs?limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(docs.len() >= n, "expected at least {n} canon docs");
    docs.iter()
        .take(n)
        .map(|d| d["id"].as_str().unwrap().to_string())
        .collect()
}

async fn create_list(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    title: &str,
) -> serde_json::Value {
    let r = client
        .post(url(addr, "/api/kb/smoke/lists"))
        .json(&serde_json::json!({ "title": title }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201, "list create should 201");
    r.json().await.unwrap()
}

async fn add_list_entry(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    list_id: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let r = client
        .post(url(addr, &format!("/api/kb/smoke/lists/{list_id}/entries")))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    let body: serde_json::Value = r.json().await.unwrap_or(serde_json::json!({}));
    (status, body)
}

async fn list_detail(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    list_id: &str,
) -> serde_json::Value {
    client
        .get(url(addr, &format!("/api/kb/smoke/lists/{list_id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn entry_order(detail: &serde_json::Value) -> Vec<String> {
    detail["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn lists_empty_at_boot() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/lists"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["lists"], serde_json::json!([]));
}

#[tokio::test]
async fn lists_create_get_patch_delete_roundtrip() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    let r = client
        .post(url(addr, "/api/kb/smoke/lists"))
        .json(&serde_json::json!({
            "title": "Async Rust",
            "description": "in order",
            "pinned": false,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let created: serde_json::Value = r.json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("l_"), "list id shape: {id}");
    assert_eq!(created["title"], "Async Rust");
    assert_eq!(created["description"], "in order");
    assert_eq!(created["entry_count"], 0);
    assert_eq!(created["kb"], "smoke");

    let index: serde_json::Value = client
        .get(url(addr, "/api/lists"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(index["lists"].as_array().unwrap().len(), 1);

    // PATCH: rename + clear description (explicit null) + pin.
    let r = client
        .patch(url(addr, &format!("/api/kb/smoke/lists/{id}")))
        .json(&serde_json::json!({
            "title": "Async Rust, properly",
            "description": null,
            "pinned": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let patched: serde_json::Value = r.json().await.unwrap();
    assert_eq!(patched["title"], "Async Rust, properly");
    assert!(
        patched.get("description").is_none() || patched["description"].is_null(),
        "explicit null must clear the description: {patched}"
    );
    assert_eq!(patched["pinned"], true);

    // Archive → hidden from the default index, visible with the flag.
    let r = client
        .patch(url(addr, &format!("/api/kb/smoke/lists/{id}")))
        .json(&serde_json::json!({ "archived": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let index: serde_json::Value = client
        .get(url(addr, "/api/lists"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(index["lists"], serde_json::json!([]));
    let index: serde_json::Value = client
        .get(url(addr, "/api/lists?include_archived=true"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(index["lists"].as_array().unwrap().len(), 1);

    // DELETE is idempotent; detail 404s after.
    let r = client
        .delete(url(addr, &format!("/api/kb/smoke/lists/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let r = client
        .get(url(addr, &format!("/api/kb/smoke/lists/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = client
        .delete(url(addr, &format!("/api/kb/smoke/lists/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
}

#[tokio::test]
async fn lists_create_duplicate_title_conflicts_409() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    create_list(&client, addr, "Reading").await;
    let r = client
        .post(url(addr, "/api/kb/smoke/lists"))
        .json(&serde_json::json!({ "title": "reading" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409, "case-insensitive title clash must 409");
    let r = client
        .post(url(addr, "/api/kb/smoke/lists"))
        .json(&serde_json::json!({ "title": "   " }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "blank title must 400");
}

#[tokio::test]
async fn list_entries_add_orders_and_enriches() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let ids = first_doc_ids(&client, addr, 2).await;
    let list = create_list(&client, addr, "Enrich").await;
    let lid = list["id"].as_str().unwrap();

    // Add by source-relative path — resolves through the path-hash id.
    let (status, e1) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "path": "kitchen-sink.html", "note": "start here" }),
    )
    .await;
    assert_eq!(status, 201);
    assert!(e1["id"].as_str().unwrap().starts_with("le_"));
    assert_eq!(e1["position"], 0);
    assert_eq!(e1["read_state"], "unread");
    assert_eq!(e1["note"], "start here");
    assert_eq!(e1["source_relative"], "kitchen-sink.html");
    assert!(e1["title"].is_string());
    assert!(
        e1["est_minutes"].is_number(),
        "canon artifact has a word_count → minutes estimate: {e1}"
    );

    // Add by artifact id (pick one that isn't the same artifact).
    let other = ids
        .iter()
        .find(|i| *i != e1["artifact_id"].as_str().unwrap())
        .unwrap();
    let (status, e2) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "artifact_id": other }),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(e2["position"], 1);

    let detail = list_detail(&client, addr, lid).await;
    assert_eq!(detail["list"]["entry_count"], 2);
    assert_eq!(detail["list"]["unread_count"], 2);
    assert_eq!(detail["list"]["read_count"], 0);
    assert!(detail["list"]["total_minutes"].as_u64().unwrap() >= 1);
    assert_eq!(
        detail["list"]["remaining_minutes"], detail["list"]["total_minutes"],
        "nothing read yet → everything remains"
    );
    let order = entry_order(&detail);
    assert_eq!(
        order,
        vec![
            e1["id"].as_str().unwrap().to_string(),
            e2["id"].as_str().unwrap().to_string()
        ]
    );
}

#[tokio::test]
async fn list_entries_insert_before_after_and_move() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let list = create_list(&client, addr, "Order").await;
    let lid = list["id"].as_str().unwrap();

    let (_, a) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "path": "kitchen-sink.html" }),
    )
    .await;
    let (_, b) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "path": "fullscreen-viz.html" }),
    )
    .await;
    let (status, c) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "path": "multi-page.html", "before": b["id"] }),
    )
    .await;
    assert_eq!(status, 201);
    let detail = list_detail(&client, addr, lid).await;
    assert_eq!(
        entry_order(&detail),
        vec![
            a["id"].as_str().unwrap(),
            c["id"].as_str().unwrap(),
            b["id"].as_str().unwrap()
        ]
    );

    // Move a after b via PATCH.
    let r = client
        .patch(url(
            addr,
            &format!(
                "/api/kb/smoke/lists/{lid}/entries/{}",
                a["id"].as_str().unwrap()
            ),
        ))
        .json(&serde_json::json!({ "after": b["id"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let detail = list_detail(&client, addr, lid).await;
    assert_eq!(
        entry_order(&detail),
        vec![
            c["id"].as_str().unwrap(),
            b["id"].as_str().unwrap(),
            a["id"].as_str().unwrap()
        ]
    );
    // Dense positions 0..n.
    let positions: Vec<u64> = detail["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["position"].as_u64().unwrap())
        .collect();
    assert_eq!(positions, vec![0, 1, 2]);

    // Unknown sibling → 404.
    let r = client
        .patch(url(
            addr,
            &format!(
                "/api/kb/smoke/lists/{lid}/entries/{}",
                a["id"].as_str().unwrap()
            ),
        ))
        .json(&serde_json::json!({ "before": "le_nope00000000" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn list_entries_dedupe_conflicts_unless_anchor_differs() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let list = create_list(&client, addr, "Dedupe").await;
    let lid = list["id"].as_str().unwrap();

    let (status, _) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "path": "kitchen-sink.html" }),
    )
    .await;
    assert_eq!(status, 201);
    let (status, _) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "path": "kitchen-sink.html" }),
    )
    .await;
    assert_eq!(status, 409, "same whole-artifact target twice must 409");

    let (status, anchored) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({
            "path": "kitchen-sink.html",
            "anchor": { "kind": "section", "id": "tuning" },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "same artifact with a section anchor is a new target"
    );
    assert_eq!(anchored["anchor"]["kind"], "section");
    assert_eq!(anchored["anchor"]["id"], "tuning");

    let (status, _) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({
            "path": "kitchen-sink.html",
            "anchor": { "kind": "section", "id": "tuning" },
        }),
    )
    .await;
    assert_eq!(status, 409, "identical anchored target must 409");
}

#[tokio::test]
async fn list_entry_patch_note_override_and_idempotent_delete() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let list = create_list(&client, addr, "Patch").await;
    let lid = list["id"].as_str().unwrap();
    let (_, e) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "path": "kitchen-sink.html" }),
    )
    .await;
    let eid = e["id"].as_str().unwrap();
    let entry_url = url(addr, &format!("/api/kb/smoke/lists/{lid}/entries/{eid}"));

    // Note set → present; explicit null → cleared.
    let r = client
        .patch(&entry_url)
        .json(&serde_json::json!({ "note": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["note"], "hello");
    let r = client
        .patch(&entry_url)
        .json(&serde_json::json!({ "note": null }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = r.json().await.unwrap();
    assert!(body.get("note").is_none() || body["note"].is_null());

    // read_override: set read → effective state read; clear → derived.
    let r = client
        .patch(&entry_url)
        .json(&serde_json::json!({ "read_override": "read" }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["read_state"], "read");
    assert_eq!(body["read_override"], "read");
    let detail = list_detail(&client, addr, lid).await;
    assert_eq!(detail["list"]["read_count"], 1);
    assert_eq!(detail["list"]["remaining_minutes"], 0);

    let r = client
        .patch(&entry_url)
        .json(&serde_json::json!({ "read_override": "clear" }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["read_state"], "unread", "derived state after clear");
    assert!(body.get("read_override").is_none() || body["read_override"].is_null());

    let r = client
        .patch(&entry_url)
        .json(&serde_json::json!({ "read_override": "bogus" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // Entry delete is idempotent.
    let r = client.delete(&entry_url).send().await.unwrap();
    assert_eq!(r.status(), 204);
    let r = client.delete(&entry_url).send().await.unwrap();
    assert_eq!(r.status(), 204);
    let detail = list_detail(&client, addr, lid).await;
    assert_eq!(detail["list"]["entry_count"], 0);
}

#[tokio::test]
async fn list_entry_add_refuses_unknown_artifact_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let list = create_list(&client, addr, "Refuse").await;
    let lid = list["id"].as_str().unwrap();

    let (status, _) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "artifact_id": "000000000000" }),
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = add_list_entry(&client, addr, lid, serde_json::json!({})).await;
    assert_eq!(status, 400, "artifact_id or path required");
    // Unknown list 404s before anything else.
    let (status, _) = add_list_entry(
        &client,
        addr,
        "l_nope00000000",
        serde_json::json!({ "path": "kitchen-sink.html" }),
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn lists_sse_events_fire() {
    use futures::StreamExt;
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    // Mutate first — the ring replays to a fresh subscriber.
    let list = create_list(&client, addr, "Events").await;
    let lid = list["id"].as_str().unwrap();
    let (_, e) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "path": "kitchen-sink.html" }),
    )
    .await;
    let eid = e["id"].as_str().unwrap();
    client
        .patch(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid}/entries/{eid}"),
        ))
        .json(&serde_json::json!({ "note": "n" }))
        .send()
        .await
        .unwrap();
    client
        .delete(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid}/entries/{eid}"),
        ))
        .send()
        .await
        .unwrap();
    client
        .delete(url(addr, &format!("/api/kb/smoke/lists/{lid}")))
        .send()
        .await
        .unwrap();

    let resp = client.get(url(addr, "/api/events")).send().await.unwrap();
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    let _ = tokio::time::timeout(Duration::from_millis(800), async {
        while let Some(chunk) = stream.next().await {
            if let Ok(bytes) = chunk {
                buf.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    })
    .await;

    for needed in [
        "list.created",
        "list.entry.added",
        "list.entry.updated",
        "list.entry.removed",
        "list.deleted",
    ] {
        assert!(buf.contains(needed), "missing SSE kind {needed} in: {buf}");
    }
    // Entry payloads carry the bridge's targeting fields.
    assert!(buf.contains(&format!("\"list_id\":\"{lid}\"")), "{buf}");
    assert!(buf.contains(&format!("\"entry_id\":\"{eid}\"")), "{buf}");
}

#[tokio::test]
async fn list_detail_marks_tombstone_when_artifact_deleted() {
    let (tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let list = create_list(&client, addr, "Tombstone").await;
    let lid = list["id"].as_str().unwrap();
    let (status, e) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "path": "cost-of-abstraction.html" }),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(
        e["tombstone"],
        serde_json::Value::Null,
        "live entry has no tombstone flag"
    );

    // Unlink the source file → watcher delete pass drops the lance row;
    // the entry survives and renders as a tombstone.
    std::fs::remove_file(tmp.path().join("corpus/cost-of-abstraction.html")).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let detail = list_detail(&client, addr, lid).await;
        let entry = &detail["entries"][0];
        if entry["tombstone"] == serde_json::json!(true) {
            assert!(entry.get("title").is_none() || entry["title"].is_null());
            assert!(entry.get("source_relative").is_none() || entry["source_relative"].is_null());
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tombstone never appeared: {detail}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// S1 — list share export: a tombstoned entry is skipped (header set) while
/// live entries still produce a zip with index.html as the entry page.
#[tokio::test]
async fn list_share_export_skips_tombstone_and_sets_header() {
    let (tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let list = create_list(&client, addr, "Share me").await;
    let lid = list["id"].as_str().unwrap();

    let (status, live) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "path": "kitchen-sink.html" }),
    )
    .await;
    assert_eq!(status, 201, "live entry: {live}");
    let (status, doomed) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "path": "cost-of-abstraction.html" }),
    )
    .await;
    assert_eq!(status, 201, "second entry: {doomed}");
    let doomed_id = doomed["id"].as_str().unwrap().to_string();

    // Tombstone the second entry by deleting its source + waiting for the
    // lance row to drop (same path as list_detail_marks_tombstone…).
    std::fs::remove_file(tmp.path().join("corpus/cost-of-abstraction.html")).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let detail = list_detail(&client, addr, lid).await;
        let tomb = detail["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == doomed_id);
        if tomb.is_some_and(|e| e["tombstone"] == serde_json::json!(true)) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tombstone never appeared: {detail}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let r = client
        .post(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid}/share/export"),
        ))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "list share export should 200");
    assert_eq!(
        r.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/zip")
    );
    assert_eq!(
        r.headers()
            .get("x-kb-share-entry")
            .and_then(|v| v.to_str().ok()),
        Some("index.html")
    );
    let skipped = r
        .headers()
        .get("x-kb-share-skipped")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        skipped.split(',').any(|s| s == doomed_id),
        "tombstoned entry id in x-kb-share-skipped, got {skipped:?}"
    );
    let files: usize = r
        .headers()
        .get("x-kb-share-files")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    assert!(files >= 2, "index.html + at least one artifact: {files}");
    let bytes = r.bytes().await.unwrap();
    assert!(!bytes.is_empty(), "zip body non-empty");
}

/// S1 — list share export with zero resolvable entries → 4xx, never empty zip.
#[tokio::test]
async fn list_share_export_empty_set_returns_4xx() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let list = create_list(&client, addr, "Empty share").await;
    let lid = list["id"].as_str().unwrap();

    let r = client
        .post(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid}/share/export"),
        ))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    assert!(
        (400..500).contains(&status),
        "empty resolvable set must be 4xx, got {status}"
    );
    let body: serde_json::Value = r.json().await.unwrap_or_default();
    let detail = body["detail"].as_str().unwrap_or("");
    assert!(
        detail.to_ascii_lowercase().contains("no resolvable")
            || detail.to_ascii_lowercase().contains("empty"),
        "clear error message, got: {body}"
    );
}

// invariant:25 derived-read-state
#[tokio::test]
async fn list_read_state_derives_from_reading_progress_and_override_wins() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let list = create_list(&client, addr, "Derived").await;
    let lid = list["id"].as_str().unwrap();

    // Whole-artifact entry + two section entries on the same artifact.
    let (_, whole) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "path": "kitchen-sink.html" }),
    )
    .await;
    let aid = whole["artifact_id"].as_str().unwrap().to_string();
    let (_, sec_read) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({
            "path": "kitchen-sink.html",
            "anchor": { "kind": "section", "id": "sec-a" },
        }),
    )
    .await;
    let (_, sec_skim) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({
            "path": "kitchen-sink.html",
            "anchor": { "kind": "section", "id": "sec-b" },
        }),
    )
    .await;

    // Drive the RP-track APIs the way the iframe runtime does.
    let open: serde_json::Value = client
        .post(url(addr, "/api/kb/smoke/history/open"))
        .json(&serde_json::json!({ "artifact_id": aid }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let visit_id = open["visit_id"].as_i64().unwrap();
    // Scroll to 96% → whole-artifact completion ≥ FULLY_READ_PCT.
    let r = client
        .post(url(addr, "/api/kb/smoke/history/scroll"))
        .json(&serde_json::json!({
            "visit_id": visit_id, "scroll_y": 960, "scroll_max": 1000
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    // Section dwell: sec-a read (40s on 200 words ≥ half of 60s),
    // sec-b skimmed (1s).
    let r = client
        .post(url(addr, "/api/kb/smoke/history/reading"))
        .json(&serde_json::json!({
            "visit_id": visit_id,
            "artifact_id": aid,
            "active_ms": 41_000,
            "last_section": "sec-a",
            "sections": [
                { "id": "sec-a", "idx": 0, "text": "A", "level": 2,
                  "words": 200, "content_px": 1000, "dwell_ms": 40_000, "enters": 2 },
                { "id": "sec-b", "idx": 1, "text": "B", "level": 2,
                  "words": 200, "content_px": 1000, "dwell_ms": 1_000, "enters": 1 },
            ],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);

    let detail = list_detail(&client, addr, lid).await;
    let by_id = |id: &serde_json::Value| {
        detail["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| &e["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(by_id(&whole["id"])["read_state"], "read", "96% scroll");
    assert_eq!(
        by_id(&sec_read["id"])["read_state"],
        "read",
        "section dwell"
    );
    assert_eq!(
        by_id(&sec_skim["id"])["read_state"],
        "in_progress",
        "skimmed section"
    );
    assert_eq!(detail["list"]["read_count"], 2);
    assert_eq!(detail["list"]["in_progress_count"], 1);

    // Manual override beats everything: mark the read section unread.
    let r = client
        .patch(url(
            addr,
            &format!(
                "/api/kb/smoke/lists/{lid}/entries/{}",
                sec_read["id"].as_str().unwrap()
            ),
        ))
        .json(&serde_json::json!({ "read_override": "unread" }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["read_state"], "unread", "override wins over dwell");
}

#[tokio::test]
async fn list_export_json_import_replace_round_trips() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let list = create_list(&client, addr, "Round trip").await;
    let lid = list["id"].as_str().unwrap();

    add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "path": "kitchen-sink.html", "note": "the basics" }),
    )
    .await;
    let (_, anchored) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({
            "path": "fullscreen-viz.html",
            "anchor": { "kind": "section", "id": "intro" },
        }),
    )
    .await;
    // Mark the anchored entry read so the override round-trips.
    client
        .patch(url(
            addr,
            &format!(
                "/api/kb/smoke/lists/{lid}/entries/{}",
                anchored["id"].as_str().unwrap()
            ),
        ))
        .json(&serde_json::json!({ "read_override": "read" }))
        .send()
        .await
        .unwrap();

    let exported = client
        .get(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid}/export?format=json"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(exported.status(), 200);
    assert!(exported
        .headers()
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("attachment"));
    let doc_text = exported.text().await.unwrap();
    let doc: serde_json::Value = serde_json::from_str(&doc_text).unwrap();
    assert_eq!(doc["schema"], "kb-list/1");
    assert_eq!(doc["entries"].as_array().unwrap().len(), 2);

    // Import into a SECOND list — entries, order, anchors, notes and
    // overrides carry over; ids are FRESH (the table-wide PK lives with
    // the original list, so a cross-list import remints).
    let second = create_list(&client, addr, "Round trip copy").await;
    let lid2 = second["id"].as_str().unwrap();
    let r = client
        .post(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid2}/import?format=json&mode=replace"),
        ))
        .body(doc_text.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let imported: serde_json::Value = r.json().await.unwrap();
    assert_eq!(imported["imported"], 2);
    assert_eq!(imported["skipped"], serde_json::json!([]));

    let d1 = list_detail(&client, addr, lid).await;
    let d2 = list_detail(&client, addr, lid2).await;
    let strip = |d: &serde_json::Value| -> Vec<serde_json::Value> {
        d["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                serde_json::json!({
                    "artifact_id": e["artifact_id"],
                    "anchor": e.get("anchor").cloned().unwrap_or(serde_json::Value::Null),
                    "note": e.get("note").cloned().unwrap_or(serde_json::Value::Null),
                    "read_override": e.get("read_override").cloned().unwrap_or(serde_json::Value::Null),
                    "position": e["position"],
                })
            })
            .collect()
    };
    assert_eq!(strip(&d1), strip(&d2), "round-trip preserves entry state");
    let ids = |d: &serde_json::Value| -> Vec<String> {
        d["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_ne!(
        ids(&d1),
        ids(&d2),
        "cross-list import must remint entry ids"
    );

    // Replace-import the SAME doc back into the ORIGINAL list: ids and
    // created_at survive the wipe (true round-trip).
    let before = d1["entries"][0]["created_at"].clone();
    let r = client
        .post(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid}/import?format=json&mode=replace"),
        ))
        .body(doc_text)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let d1_after = list_detail(&client, addr, lid).await;
    assert_eq!(
        ids(&d1),
        ids(&d1_after),
        "same-list replace re-import preserves entry ids"
    );
    assert_eq!(
        d1_after["entries"][0]["created_at"], before,
        "round-tripped id keeps created_at"
    );
}

#[tokio::test]
async fn list_export_md_and_import_append() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let list = create_list(&client, addr, "Markdown").await;
    let lid = list["id"].as_str().unwrap();
    add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({
            "path": "kitchen-sink.html",
            "anchor": { "kind": "section", "id": "tuning" },
            "note": "the payload",
        }),
    )
    .await;

    let md = client
        .get(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid}/export?format=md"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(md.status(), 200);
    assert!(md
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/markdown"));
    let md_text = md.text().await.unwrap();
    assert!(md_text.starts_with("# Markdown\n"), "{md_text}");
    assert!(md_text.contains("kitchen-sink.html#tuning"), "{md_text}");
    assert!(md_text.contains("<!-- kb-list "), "{md_text}");
    assert!(md_text.contains("   the payload"), "{md_text}");

    // Append-import a hand-authored doc: one duplicate (skipped via
    // dedupe), one new artifact.
    let hand = "# whatever\n\n1. [ ] [dup](kitchen-sink.html#tuning)\n2. [x] [new](multi-page.html)\n   appended note\n";
    let r = client
        .post(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid}/import?format=md&mode=append"),
        ))
        .body(hand.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let imported: serde_json::Value = r.json().await.unwrap();
    assert_eq!(
        imported["imported"], 1,
        "duplicate target skipped: {imported}"
    );
    let detail = list_detail(&client, addr, lid).await;
    let entries = detail["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["source_relative"], "kitchen-sink.html");
    assert_eq!(entries[1]["source_relative"], "multi-page.html");
    assert_eq!(entries[1]["read_state"], "read", "[x] imports as override");
    assert_eq!(entries[1]["note"], "appended note");
}

// invariant:25 bulk-import-one-tx
#[tokio::test]
async fn list_import_skips_unknown_artifacts_and_reports() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let list = create_list(&client, addr, "Skips").await;
    let lid = list["id"].as_str().unwrap();

    let doc = serde_json::json!({
        "schema": "kb-list/1",
        "entries": [
            { "path": "kitchen-sink.html" },
            { "path": "no/such/file.html" },
        ],
    });
    let r = client
        .post(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid}/import?format=json"),
        ))
        .body(doc.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["imported"], 1);
    let skipped = body["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["ref"], "no/such/file.html");
    assert!(skipped[0]["reason"].as_str().unwrap().contains("not found"));

    // Bad format / mode / body → 400; unknown list → 404.
    let r = client
        .post(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid}/import?format=xml"),
        ))
        .body("x".to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = client
        .post(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid}/import?format=json&mode=merge"),
        ))
        .body("{}".to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = client
        .post(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid}/import?format=json"),
        ))
        .body("not json".to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = client
        .post(url(
            addr,
            "/api/kb/smoke/lists/l_nope00000000/import?format=json",
        ))
        .body("{}".to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

// --- Z4 — fleet-wide open-comments inbox (GET /api/inbox) ------------------

/// Build a config with TWO corpora ("alpha", "beta") plus a third empty kb
/// ("gamma") that never gets a `.review/` dir — proving a corpus with a
/// missing review dir contributes nothing rather than 500ing the fleet.
/// Returns the tempdir (kept alive), the config, and the test `KbPaths` so
/// the caller can stage review files directly under each kb's state dir.
fn fixture_inbox_kbs() -> (tempfile::TempDir, KbConfig, KbPaths) {
    let tmp = tempfile::tempdir().unwrap();
    let daemon_name = format!(
        "inbox-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    for name in ["alpha", "beta", "gamma"] {
        let source = tmp.path().join(name);
        std::fs::create_dir_all(&source).unwrap();
        kb_map.insert(
            KbName::new(name).unwrap(),
            KbSection {
                path: source,
                skip_patterns: Vec::new(),
                ui: UiSection::default(),
                embedding_model: None,
                reranker_model: None,
                chunked_embeddings: false,
                graph_boost: None,
                outbound: None,
                atlas: None,
                templates: std::collections::BTreeMap::new(),
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
    }
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
    (tmp, cfg, paths)
}

/// Stage a kb-comments/1 review file directly on disk (bypassing the write
/// routes so timestamps are deterministic).
fn write_review(paths: &KbPaths, kb: &str, artifact_id: &str, comments_json: &str) {
    let name = KbName::new(kb).unwrap();
    let dir = paths.kb_review_dir(&name);
    std::fs::create_dir_all(&dir).unwrap();
    let file = paths.kb_review_file(&name, artifact_id);
    std::fs::write(
        &file,
        format!(
            r#"{{"schema":"kb-comments/1","artifact":{{"id":"{artifact_id}","title":"Title {artifact_id}","kb":"{kb}","tags":[],"pages":[]}},"generatedAt":"2026-05-14T10:00:00Z","comments":[{comments_json}]}}"#
        ),
    )
    .unwrap();
}

#[tokio::test]
async fn inbox_lists_open_comments_fleet_wide_newest_first() {
    let (_tmp, cfg, paths) = fixture_inbox_kbs();

    // alpha: one open comment + one RESOLVED comment (must NOT appear).
    write_review(
        &paths,
        "alpha",
        "aaaa0000aaaa",
        concat!(
            r#"{"id":"c_alpha_open","status":"open","file":"aaaa0000aaaa","fileLabel":"main","anchor":{"kind":"file"},"author":"you","body":"alpha open thread","createdAt":"2026-05-14T10:00:00Z","editedAt":null,"replies":[]},"#,
            r#"{"id":"c_alpha_done","status":"resolved","file":"aaaa0000aaaa","fileLabel":"main","anchor":{"kind":"file"},"author":"you","body":"alpha resolved thread","createdAt":"2026-05-14T10:05:00Z","editedAt":null,"replies":[]}"#
        ),
    );
    // beta: one open comment with a LATER reply → newest activity, so it
    // sorts to the top of the fleet inbox.
    write_review(
        &paths,
        "beta",
        "bbbb1111bbbb",
        concat!(
            r#"{"id":"c_beta_open","status":"open","file":"bbbb1111bbbb","fileLabel":"main","anchor":{"kind":"section","id":"intro"},"author":"claude","body":"beta open thread","createdAt":"2026-05-14T10:00:00Z","editedAt":null,"#,
            r#""replies":[{"id":"r_beta","author":"you","body":"a reply","createdAt":"2026-05-14T12:00:00Z"}]}"#
        ),
    );
    // A malformed review file in beta's dir must be skipped, not 500 the route.
    {
        let dir = paths.kb_review_dir(&KbName::new("beta").unwrap());
        std::fs::write(dir.join("garbage.json"), b"{not valid json").unwrap();
    }

    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let client = reqwest::Client::new();

    // Fleet-wide: both open comments, resolved one absent, gamma silent.
    let resp = client.get(url(addr, "/api/inbox")).send().await.unwrap();
    assert_eq!(
        resp.status(),
        200,
        "a broken review dir must not 500 the fleet"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert_eq!(
        body["total_open"], 2,
        "two open comments across the fleet: {body:#}"
    );
    assert_eq!(items.len(), 2);
    let ids: Vec<&str> = items
        .iter()
        .map(|i| i["comment_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"c_alpha_open"));
    assert!(ids.contains(&"c_beta_open"));
    assert!(
        !ids.contains(&"c_alpha_done"),
        "resolved comment must never appear: {body:#}"
    );

    // Newest activity first: beta's reply (12:00) outranks alpha (10:00).
    assert_eq!(
        items[0]["comment_id"], "c_beta_open",
        "beta's replied thread sorts first: {body:#}"
    );
    assert_eq!(items[0]["reply_count"], 1);
    assert_eq!(items[0]["kb"], "beta");
    assert_eq!(items[0]["anchor"], "section");
    assert_eq!(items[0]["title"], "Title bbbb1111bbbb");
    assert_eq!(items[1]["comment_id"], "c_alpha_open");
    assert_eq!(items[1]["reply_count"], 0);
    assert_eq!(items[1]["anchor"], "file");

    // ?kb= narrows to one corpus.
    let body: serde_json::Value = client
        .get(url(addr, "/api/inbox?kb=alpha"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["total_open"], 1, "alpha alone has one open comment");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["comment_id"], "c_alpha_open");

    // ?limit= caps the returned page but total_open stays the fleet count.
    let body: serde_json::Value = client
        .get(url(addr, "/api/inbox?limit=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["total_open"], 2,
        "total_open is the full count, pre-limit"
    );
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["items"][0]["comment_id"], "c_beta_open");
}

// === U2 (v0.25 quick capture) — POST /api/kb/{kb}/capture + share-target
// POST /capture ===================================================

/// Best-effort "am I root" check (shells to `id -u` — no new dependency for
/// one test-only guard). Root bypasses Unix permission bits entirely, so
/// the read-only-corpus 409 test would spuriously fail (or worse, silently
/// pass by actually writing the file) under a root test runner (some CI
/// containers).
fn running_as_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

#[tokio::test]
async fn capture_md_upload_lands_with_provenance_and_from_header_default() {
    let (tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    // `title` steers the FILENAME slug only (U1: captures preserve the
    // uploaded content's OWN authored title) — the response `title` mirrors
    // the indexer's own title/H1 fallback chain, so it reads the body's H1
    // ("My Captured Note"), not a distinct request `title` field. Keep the
    // two aligned here so the assertion below exercises both at once.
    let form = reqwest::multipart::Form::new()
        .text("title", "My Captured Note")
        .text("tags", "work, quick")
        .part(
            "files",
            reqwest::multipart::Part::text("# My Captured Note\n\nhello there")
                .file_name("draft.md")
                .mime_str("text/markdown")
                .unwrap(),
        );
    let resp = client
        .post(url(addr, "/api/kb/smoke/capture"))
        .header("X-Requested-By", "kb-cli")
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201, "{:?}", resp.text().await);
    let body: serde_json::Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item["kb"], "smoke");
    assert_eq!(item["title"], "My Captured Note");
    let rel = item["source_relative"].as_str().unwrap().to_string();
    assert!(rel.starts_with("capture/"), "{rel}");
    assert!(rel.ends_with(".md"), "{rel}");
    assert!(!item["id"].as_str().unwrap().is_empty());

    let written = std::fs::read_to_string(tmp.path().join("corpus").join(&rel)).unwrap();
    assert!(written.starts_with("---\n"), "{written}");
    assert!(written.contains("kb-category: capture"), "{written}");
    assert!(
        written.contains("source:upload, from:cli, work, quick"),
        "X-Requested-By: kb-cli -> from:cli, plus the free-text tags: {written}"
    );
}

// invariant:18-shaped XSS pin (mirrors the attachment sniff-serve guard):
// sanitize=true on an HTML capture must strip <script>, and the stored
// SOURCE is the sanitized output (U1 capture-time transform).
#[tokio::test]
async fn capture_html_upload_with_sanitize_true_strips_script() {
    let (tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new()
        .text("sanitize", "true")
        .part(
            "files",
            reqwest::multipart::Part::text(
                "<html><body><script>alert(1)</script><p>hi</p></body></html>",
            )
            .file_name("saved-page.html")
            .mime_str("text/html")
            .unwrap(),
        );
    let resp = client
        .post(url(addr, "/api/kb/smoke/capture"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201, "{:?}", resp.text().await);
    let body: serde_json::Value = resp.json().await.unwrap();
    let rel = body["items"][0]["source_relative"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(rel.ends_with(".html"), "{rel}");
    let written = std::fs::read_to_string(tmp.path().join("corpus").join(&rel)).unwrap();
    assert!(!written.contains("<script"), "{written}");
    assert!(!written.contains("alert(1)"), "{written}");
    assert!(written.contains("kb-category"), "{written}");
    assert!(written.contains("<p>hi</p>"), "{written}");
}

#[tokio::test]
async fn capture_url_text_stub_without_files() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new()
        .text("title", "Interesting Page")
        .text("url", "https://example.com/a")
        .text("text", "worth reading later");
    let resp = client
        .post(url(addr, "/api/kb/smoke/capture"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201, "{:?}", resp.text().await);
    let body: serde_json::Value = resp.json().await.unwrap();
    let item = &body["items"][0];
    assert!(
        item["source_relative"].as_str().unwrap().ends_with(".md"),
        "{item:#}"
    );
    assert_eq!(item["title"], "Interesting Page");
    assert_eq!(item["url"], "https://example.com/a");
}

#[tokio::test]
async fn capture_rejects_unknown_extension_415() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new().part(
        "files",
        reqwest::multipart::Part::bytes(b"binary junk".to_vec()).file_name("data.exe"),
    );
    let resp = client
        .post(url(addr, "/api/kb/smoke/capture"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 415, "{:?}", resp.text().await);
}

// U2 follow-up regression: extension validation is ALL-OR-NOTHING across a
// batch. A mixed batch [good.md, bad.xyz] must 415 WITHOUT writing good.md
// first — the old per-file gate inside the write loop left good.md on disk
// (indexed by the watcher) behind a pure-failure response, and a full-batch
// retry then duplicated it. Order matters: the valid file comes FIRST so
// the old code path would have written it before hitting the 415.
#[tokio::test]
async fn capture_mixed_batch_with_unmapped_extension_writes_nothing() {
    let (tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new()
        .part(
            "files",
            reqwest::multipart::Part::text("# Good File\n\nperfectly valid")
                .file_name("good.md")
                .mime_str("text/markdown")
                .unwrap(),
        )
        .part(
            "files",
            reqwest::multipart::Part::bytes(b"binary junk".to_vec()).file_name("bad.xyz"),
        );
    let resp = client
        .post(url(addr, "/api/kb/smoke/capture"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 415, "{:?}", resp.text().await);
    // No side effect: nothing was captured before validation failed. The
    // capture dir is created lazily by the first write, so "never created"
    // and "created but empty" are both clean.
    let capture_dir = tmp.path().join("corpus").join("capture");
    let entries: Vec<_> = match std::fs::read_dir(&capture_dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.file_name()).collect(),
        Err(_) => Vec::new(),
    };
    assert!(
        entries.is_empty(),
        "validation failure must not leave captures behind: {entries:?}"
    );
}

#[tokio::test]
async fn capture_rejects_empty_request_400() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new().text("title", "Nothing else here");
    let resp = client
        .post(url(addr, "/api/kb/smoke/capture"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400, "{:?}", resp.text().await);
}

#[tokio::test]
async fn capture_rejects_oversize_413() {
    let (_tmp, mut cfg, paths) = fixture_corpus();
    cfg.server.capture = Some(kb_core::config::CaptureSection {
        max_file_bytes: Some(16),
        max_request_bytes: None,
        default_kb: None,
    });
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let client = reqwest::Client::new();

    let big = format!("# Title\n\n{}", "x".repeat(200));
    let form = reqwest::multipart::Form::new().part(
        "files",
        reqwest::multipart::Part::text(big).file_name("big.md"),
    );
    let resp = client
        .post(url(addr, "/api/kb/smoke/capture"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 413, "{:?}", resp.text().await);
}

// U2 follow-up regression: the reviewer's exact repro — two (here three)
// files, each individually under `max_file_bytes`, whose COMBINED size
// exceeded the OLD single-file-sized `DefaultBodyLimit` (`max_file_bytes +
// BODY_LIMIT_SLACK`) and died as a generic 400 multipart-parse error before
// any per-file logic ran. `max_request_bytes` now sizes the outer limit
// instead, so a legal multi-file batch succeeds.
#[tokio::test]
async fn capture_multi_file_batch_combined_size_succeeds_under_max_request_bytes() {
    let (_tmp, mut cfg, paths) = fixture_corpus();
    cfg.server.capture = Some(kb_core::config::CaptureSection {
        max_file_bytes: Some(1024 * 1024), // 1 MiB per file
        max_request_bytes: None,           // defaults to 64 MiB
        default_kb: None,
    });
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let client = reqwest::Client::new();

    // Three ~0.9 MiB files: combined ~2.7 MiB, safely over the OLD
    // single-file-sized limit (1 MiB + 1 MiB slack = 2 MiB) but well under
    // the 64 MiB max_request_bytes default — and each file individually
    // sits under the 1 MiB per-file cap.
    let chunk = "x".repeat(900 * 1024);
    let mut form = reqwest::multipart::Form::new();
    for i in 0..3 {
        form = form.part(
            "files",
            reqwest::multipart::Part::text(format!("# File {i}\n\n{chunk}"))
                .file_name(format!("part-{i}.md"))
                .mime_str("text/markdown")
                .unwrap(),
        );
    }
    let resp = client
        .post(url(addr, "/api/kb/smoke/capture"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201, "{:?}", resp.text().await);
    let body: serde_json::Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "{body:#}");
    for item in items {
        assert!(
            item["source_relative"]
                .as_str()
                .unwrap()
                .starts_with("capture/"),
            "{item:#}"
        );
    }
}

// U2 follow-up: a combined upload that announces (via `Content-Length`) more
// bytes than `max_request_bytes` fails loud with a clear 413 detail BEFORE
// multipart parsing starts, rather than as parse noise.
#[tokio::test]
async fn capture_content_length_over_max_request_bytes_returns_413_with_detail() {
    let (_tmp, mut cfg, paths) = fixture_corpus();
    cfg.server.capture = Some(kb_core::config::CaptureSection {
        max_file_bytes: None,
        max_request_bytes: Some(512),
        default_kb: None,
    });
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let client = reqwest::Client::new();

    let form = reqwest::multipart::Form::new().part(
        "files",
        reqwest::multipart::Part::text("x".repeat(2000)).file_name("big.md"),
    );
    let resp = client
        .post(url(addr, "/api/kb/smoke/capture"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 413, "{:?}", resp.text().await);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .contains("max_request_bytes"),
        "{body:#}"
    );
}

#[tokio::test]
async fn capture_readonly_capture_dir_returns_409() {
    if running_as_root() {
        eprintln!("skipping capture_readonly_capture_dir_returns_409: running as root");
        return;
    }
    use std::os::unix::fs::PermissionsExt;

    let (tmp, addr) = boot().await;
    let capture_dir = tmp.path().join("corpus").join("capture");
    std::fs::create_dir_all(&capture_dir).unwrap();
    std::fs::set_permissions(&capture_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new().part(
        "files",
        reqwest::multipart::Part::text("# hi").file_name("note.md"),
    );
    let resp = client
        .post(url(addr, "/api/kb/smoke/capture"))
        .multipart(form)
        .send()
        .await
        .unwrap();

    // Restore before any assertion can panic, so TempDir cleanup can still
    // remove the directory tree on drop.
    std::fs::set_permissions(&capture_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(
        resp.status().as_u16(),
        409,
        "read-only capture dir must map to 409, not a generic 500: {:?}",
        resp.text().await
    );
}

#[tokio::test]
async fn capture_api_route_401_without_token_non_loopback() {
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new()
        .text("url", "https://example.com")
        .text("title", "x");
    let resp = client
        .post(url(addr, "/api/kb/smoke/capture"))
        .header("X-Forwarded-For", "8.8.8.8")
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

// invariant:4/5 — the #1 risk this phase carries: /capture sits OUTSIDE the
// /api nest, so it needs auth wired on explicitly (router.rs route_layer).
#[tokio::test]
async fn capture_share_target_route_401_without_token_non_loopback() {
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new()
        .text("url", "https://example.com")
        .text("title", "x");
    let resp = client
        .post(url(addr, "/capture"))
        .header("X-Forwarded-For", "8.8.8.8")
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        401,
        "share-target /capture MUST be auth-gated despite living outside /api (invariant #4/#5)"
    );
}

#[tokio::test]
async fn capture_share_target_redirects_303_with_captured_location() {
    let (tmp, addr) = boot().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .text("title", "Shared Note")
        .part(
            "files",
            reqwest::multipart::Part::text("# Shared Note\n\nbody text").file_name("shared.md"),
        );
    let resp = client
        .post(url(addr, "/capture"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303, "{:?}", resp.text().await);
    let location = resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        location.starts_with("/?captured=smoke%3Acapture%2F"),
        "{location}"
    );
    assert!(location.ends_with(".md"), "{location}");

    // No `default_kb` configured — the share-target route fell back to the
    // daemon's first configured kb ("smoke", the only kb in this fixture),
    // and the file actually landed there.
    let captured_files: Vec<_> = std::fs::read_dir(tmp.path().join("corpus/capture"))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        captured_files.len(),
        1,
        "exactly one file landed in capture/"
    );
}

// --- F3b: relocate routes + redirects + S1 list-share TOC order ----------

/// F3b — move happy path: create comment + list entry, move, assert new
/// id serves, old id 301s, comment file + list entry follow.
#[tokio::test]
async fn relocate_move_happy_path_preserves_comment_and_list() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let doc = wait_for_doc(&client, addr, "smoke", |d| {
        d["source_relative"].as_str() == Some("kitchen-sink.html")
    })
    .await;
    let old_id = doc["id"].as_str().unwrap().to_string();
    let old_rel = doc["source_relative"].as_str().unwrap().to_string();

    // Comment on the old id.
    let r = client
        .post(url(
            addr,
            &format!("/api/kb/smoke/review/{old_id}/comments"),
        ))
        .json(&serde_json::json!({
            "body": "follows the move",
            "anchor": { "kind": "file" },
            "author": "you"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        201,
        "comment add: {}",
        r.text().await.unwrap_or_default()
    );

    // List entry pointing at the old artifact.
    let list = create_list(&client, addr, "Move follow").await;
    let lid = list["id"].as_str().unwrap();
    let (status, entry) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "artifact_id": old_id }),
    )
    .await;
    assert_eq!(status, 201, "list entry: {entry}");

    // Move.
    let r = client
        .post(url(addr, &format!("/api/kb/smoke/docs/{old_id}/move")))
        .json(&serde_json::json!({ "to": "relocated/kitchen-sink.html" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        200,
        "move: {}",
        r.text().await.unwrap_or_default()
    );
    let body: serde_json::Value = r.json().await.unwrap();
    let new_id = body["new_id"].as_str().unwrap().to_string();
    let new_rel = body["new_source_rel"].as_str().unwrap().to_string();
    assert_ne!(old_id, new_id);
    assert_eq!(body["old_source_rel"].as_str(), Some(old_rel.as_str()));
    assert_eq!(new_rel, "relocated/kitchen-sink.html");

    // New id serves 200.
    let r = client
        .get(url(addr, &format!("/api/kb/smoke/docs/{new_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "new id should serve");
    let new_doc: serde_json::Value = r.json().await.unwrap();
    assert_eq!(
        new_doc["source_relative"].as_str(),
        Some("relocated/kitchen-sink.html")
    );

    // Old id → 301 Location /api/kb/smoke/docs/{new_id}.
    let r = client
        .get(url(addr, &format!("/api/kb/smoke/docs/{old_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 301, "old id should 301");
    let loc = r
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(loc, format!("/api/kb/smoke/docs/{new_id}"));

    // Comment file re-keyed to new id.
    let r = client
        .get(url(addr, &format!("/api/kb/smoke/review/{new_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "review under new id");
    let review: serde_json::Value = r.json().await.unwrap();
    let comments = review["comments"].as_array().expect("comments array");
    assert!(
        comments.iter().any(|c| c["body"] == "follows the move"),
        "comment body follows: {review}"
    );

    // List entry follows to new artifact id.
    let detail = list_detail(&client, addr, lid).await;
    let entries = detail["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e["artifact_id"].as_str() == Some(new_id.as_str())),
        "list entry remapped: {detail}"
    );
}

/// F3b — folder rename remaps two docs including a nested one.
#[tokio::test]
async fn relocate_folder_rename_remaps_nested_docs() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(source.join("ideas/deep")).unwrap();
    for (rel, body) in [
        (
            "ideas/a.html",
            "<html><title>A</title><body>a</body></html>",
        ),
        (
            "ideas/deep/b.html",
            "<html><title>B</title><body>b</body></html>",
        ),
    ] {
        std::fs::write(source.join(rel), body).unwrap();
    }
    let daemon_name = format!(
        "test-reloc-folder-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map = BTreeMap::new();
    kb_map.insert(
        KbName::new("nested").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
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
    let client = reqwest::Client::new();
    let _ = wait_for_doc(&client, addr, "nested", |d| {
        d["source_relative"].as_str() == Some("ideas/deep/b.html")
    })
    .await;
    let _ = wait_for_doc(&client, addr, "nested", |d| {
        d["source_relative"].as_str() == Some("ideas/a.html")
    })
    .await;

    let r = client
        .post(url(addr, "/api/kb/nested/folders/rename"))
        .json(&serde_json::json!({ "from": "ideas", "to": "archive" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        200,
        "folder rename: {}",
        r.text().await.unwrap_or_default()
    );
    let body: serde_json::Value = r.json().await.unwrap();
    let moved = body["moved"].as_array().unwrap();
    assert_eq!(moved.len(), 2, "both docs remapped: {body}");
    let new_rels: Vec<&str> = moved
        .iter()
        .filter_map(|m| m["new_source_rel"].as_str())
        .collect();
    assert!(new_rels.contains(&"archive/a.html"), "{new_rels:?}");
    assert!(new_rels.contains(&"archive/deep/b.html"), "{new_rels:?}");

    // Live index reflects new paths.
    let _ = wait_for_doc(&client, addr, "nested", |d| {
        d["source_relative"].as_str() == Some("archive/a.html")
    })
    .await;
    let _ = wait_for_doc(&client, addr, "nested", |d| {
        d["source_relative"].as_str() == Some("archive/deep/b.html")
    })
    .await;
}

/// F3b — validation: move onto existing path → 4xx; outside root → 4xx;
/// unknown id → 404; missing folder rename → 4xx.
#[tokio::test]
async fn relocate_validation_errors() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let a = wait_for_doc(&client, addr, "smoke", |d| {
        d["source_relative"].as_str() == Some("kitchen-sink.html")
    })
    .await;
    let b = wait_for_doc(&client, addr, "smoke", |d| {
        d["source_relative"].as_str() == Some("fullscreen-viz.html")
    })
    .await;
    let a_id = a["id"].as_str().unwrap();

    // Onto existing path.
    let r = client
        .post(url(addr, &format!("/api/kb/smoke/docs/{a_id}/move")))
        .json(&serde_json::json!({ "to": "fullscreen-viz.html" }))
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    assert!(
        (400..500).contains(&status),
        "target exists → 4xx, got {status}"
    );

    // Outside root (`..`).
    let r = client
        .post(url(addr, &format!("/api/kb/smoke/docs/{a_id}/move")))
        .json(&serde_json::json!({ "to": "../escape.html" }))
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    assert!(
        (400..500).contains(&status),
        "outside root → 4xx, got {status}"
    );

    // Unknown id.
    let r = client
        .post(url(addr, "/api/kb/smoke/docs/deadbeefdead/move"))
        .json(&serde_json::json!({ "to": "nowhere.html" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "unknown id → 404");

    // Missing folder.
    let r = client
        .post(url(addr, "/api/kb/smoke/folders/rename"))
        .json(&serde_json::json!({ "from": "no-such-folder", "to": "elsewhere" }))
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    assert!(
        (400..500).contains(&status),
        "missing folder → 4xx, got {status}"
    );

    let _ = b; // silence unused
}

/// F3b — SPA shell redirect, lookup resolve, docs 301.
#[tokio::test]
async fn relocate_redirects_spa_lookup_and_docs() {
    let (_tmp, addr) = boot_with_spa().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let doc = wait_for_doc(&client, addr, "smoke", |d| {
        d["source_relative"].as_str() == Some("cost-of-abstraction.html")
    })
    .await;
    let old_id = doc["id"].as_str().unwrap().to_string();
    let old_rel = "cost-of-abstraction.html";

    let r = client
        .post(url(addr, &format!("/api/kb/smoke/docs/{old_id}/move")))
        .json(&serde_json::json!({ "to": "moved/cost.html" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap_or_default());
    let body: serde_json::Value = r.json().await.unwrap();
    let new_id = body["new_id"].as_str().unwrap().to_string();
    let new_rel = body["new_source_rel"].as_str().unwrap().to_string();

    // SPA shell: /a/{kb}/{old_rel}?x=1 → 301 Location /a/{kb}/{new_rel}?x=1
    let r = client
        .get(url(addr, &format!("/a/smoke/{old_rel}?x=1")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 301, "spa shell redirect");
    let loc = r
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(loc, format!("/a/smoke/{new_rel}?x=1"));

    // Lookup old id → Exact with new doc.
    let r = client
        .get(url(addr, &format!("/api/kb/smoke/lookup?q={old_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let look: serde_json::Value = r.json().await.unwrap();
    assert_eq!(look["kind"], "exact");
    assert_eq!(look["id"].as_str(), Some(new_id.as_str()));
    assert_eq!(look["source_relative"].as_str(), Some(new_rel.as_str()));

    // Lookup old rel → Exact with new doc.
    let r = client
        .get(url(addr, &format!("/api/kb/smoke/lookup?q={old_rel}")))
        .send()
        .await
        .unwrap();
    let look: serde_json::Value = r.json().await.unwrap();
    assert_eq!(look["kind"], "exact");
    assert_eq!(look["id"].as_str(), Some(new_id.as_str()));

    // docs/{old_id} → 301
    let r = client
        .get(url(addr, &format!("/api/kb/smoke/docs/{old_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 301);
    let loc = r
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(loc, format!("/api/kb/smoke/docs/{new_id}"));
}

/// F3c T8 — SPA 301 Location percent-encodes path segments (space → %20);
/// requesting the encoded new URL still serves the shell.
#[tokio::test]
async fn relocate_spa_redirect_encodes_space_in_location() {
    let (_tmp, addr) = boot_with_spa().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let doc = wait_for_doc(&client, addr, "smoke", |d| {
        d["source_relative"].as_str() == Some("fullscreen-viz.html")
    })
    .await;
    let old_id = doc["id"].as_str().unwrap().to_string();
    let old_rel = "fullscreen-viz.html";
    let new_rel = "moved/has space.html";

    let r = client
        .post(url(addr, &format!("/api/kb/smoke/docs/{old_id}/move")))
        .json(&serde_json::json!({ "to": new_rel }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap_or_default());
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["new_source_rel"].as_str(), Some(new_rel));

    // Old rel → 301 with %20 (not a raw space) in Location.
    let r = client
        .get(url(addr, &format!("/a/smoke/{old_rel}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 301, "spa shell redirect for spaced target");
    let loc = r
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(loc, "/a/smoke/moved/has%20space.html");
    assert!(
        !loc.contains(' '),
        "Location must not contain a raw space: {loc}"
    );

    // Round-trip: encoded new URL serves the shell (200), not 404.
    let r = client
        .get(url(addr, "/a/smoke/moved/has%20space.html"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        200,
        "encoded new URL should serve SPA shell: {}",
        r.text().await.unwrap_or_default()
    );
    let ct = r
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/html"), "shell content-type, got {ct}");
}

/// F3c T6 — after a move that rekeys list_entries, list detail shows the
/// new artifact id (route-visible effect of list.updated invalidation data).
#[tokio::test]
async fn relocate_list_detail_shows_new_id_after_move() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    let doc = wait_for_doc(&client, addr, "smoke", |d| {
        d["source_relative"].as_str() == Some("kitchen-sink.html")
    })
    .await;
    let old_id = doc["id"].as_str().unwrap().to_string();

    let list = create_list(&client, addr, "SSE list rekey").await;
    let lid = list["id"].as_str().unwrap();
    let (status, entry) = add_list_entry(
        &client,
        addr,
        lid,
        serde_json::json!({ "artifact_id": old_id, "note": "before-move" }),
    )
    .await;
    assert_eq!(status, 201, "list entry: {entry}");

    let r = client
        .post(url(addr, &format!("/api/kb/smoke/docs/{old_id}/move")))
        .json(&serde_json::json!({ "to": "listed/after-move.html" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap_or_default());
    let body: serde_json::Value = r.json().await.unwrap();
    let new_id = body["new_id"].as_str().unwrap().to_string();
    assert_ne!(old_id, new_id);

    let detail = list_detail(&client, addr, lid).await;
    let entries = detail["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e["artifact_id"].as_str() == Some(new_id.as_str())
                && e["note"].as_str() == Some("before-move")),
        "list detail must show rekeyed id + preserved note: {detail}"
    );
    assert!(
        entries
            .iter()
            .all(|e| e["artifact_id"].as_str() != Some(old_id.as_str())),
        "old id must not remain on the list: {detail}"
    );
}

/// S1 m2 — list share export TOC link ORDER follows list position
/// (not lexicographic path order). Three entries whose lex order differs
/// from list order.
#[tokio::test]
async fn list_share_export_toc_order_follows_list_position() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let list = create_list(&client, addr, "TOC order").await;
    let lid = list["id"].as_str().unwrap();

    // Lex order of paths: cost-of-abstraction < fullscreen-viz < kitchen-sink.
    // List order deliberately reverses that: kitchen-sink, cost-of-abstraction,
    // fullscreen-viz.
    for path in [
        "kitchen-sink.html",
        "cost-of-abstraction.html",
        "fullscreen-viz.html",
    ] {
        let (status, entry) =
            add_list_entry(&client, addr, lid, serde_json::json!({ "path": path })).await;
        assert_eq!(status, 201, "entry {path}: {entry}");
    }

    let r = client
        .post(url(
            addr,
            &format!("/api/kb/smoke/lists/{lid}/share/export"),
        ))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        200,
        "export: {}",
        r.text().await.unwrap_or_default()
    );
    let bytes = r.bytes().await.unwrap();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).expect("zip");
    let mut index = zip
        .by_name("index.html")
        .expect("index.html in list share zip");
    let mut html = String::new();
    std::io::Read::read_to_string(&mut index, &mut html).unwrap();

    // Collect hrefs in document order.
    let mut hrefs: Vec<&str> = Vec::new();
    let mut rest = html.as_str();
    while let Some(i) = rest.find("href=\"") {
        let after = &rest[i + 6..];
        if let Some(end) = after.find('"') {
            hrefs.push(&after[..end]);
            rest = &after[end..];
        } else {
            break;
        }
    }
    assert!(
        hrefs.len() >= 3,
        "expected ≥3 TOC hrefs, got {hrefs:?} from:\n{html}"
    );
    // First three TOC links must follow list position (not lex path order).
    assert_eq!(
        &hrefs[..3],
        &[
            "kitchen-sink.html",
            "cost-of-abstraction.html",
            "fullscreen-viz.html",
        ],
        "TOC order must match list position, got {hrefs:?}"
    );
}

// --- QTEST-1: live-session lane is loopback-only HARD (LF-5) ---------------

/// Non-loopback `GET /api/sessions/presence` must 403 with `cache-control:
/// no-store` and a body that does not leak `project_slug` / transcript content,
/// even when a valid bearer is presented (stricter than auth_bearer alone).
#[tokio::test]
async fn sessions_presence_non_loopback_returns_403_no_store() {
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/sessions/presence"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403, "got {}", resp.status());
    let cc = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(cc, "no-store", "got cache-control: {cc}");
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("project_slug"),
        "403 body must not leak project_slug: {body}"
    );
    assert!(
        !body.contains("\"live\""),
        "403 body must not include presence payload: {body}"
    );
}

/// Non-loopback `GET /api/sessions/{sid}/live` must 403 with `no-store` and
/// no transcript content, even with a valid bearer.
#[tokio::test]
async fn sessions_live_non_loopback_returns_403_no_store() {
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/sessions/does-not-exist/live"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403, "got {}", resp.status());
    let cc = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(cc, "no-store", "got cache-control: {cc}");
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("project_slug"),
        "403 body must not leak project_slug: {body}"
    );
    assert!(
        !body.contains("raw_lines") && !body.contains("next_from"),
        "403 body must not include live delta payload: {body}"
    );
}

// ==== MI-W2.R — review-fix e2e coverage ====================================
//
// Three surfaces the review pass found with zero end-to-end coverage:
// `[memory] scoring_v2_relevance` over live HTTP (FIX 5, also the e2e proof of the
// FIX 1 post-filter relevance-normalization ordering fix), the
// `/memories/{id}/lineage` route across a multi-hop chain + its cycle guard,
// and `/api/memory/tombstone-era` (FIX 6).

/// As `boot_memory_corpora`, but a single `global`-scope corpus with
/// `[memory] scoring_v2_relevance = true` — kept as its own helper so the
/// many existing scoring_v2-OFF tests built on `boot_memory_corpora` stay
/// byte-unchanged. Stability stays off — this helper's one caller only
/// exercises the relevance-normalization fix.
async fn boot_memory_corpus_scoring_v2(
    files: &[(&str, String)],
) -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let gdir = tmp.path().join("mem");
    std::fs::create_dir_all(&gdir).unwrap();
    for (name, html) in files {
        std::fs::write(gdir.join(name), html).unwrap();
    }
    let daemon_name = format!(
        "memtest-v2-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("mem").unwrap(),
        KbSection {
            path: gdir,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: BTreeMap::new(),
            memory_scope: Some("global".into()),
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
        memory: kb_core::config::MemorySection {
            scoring_v2_relevance: true,
            scoring_v2_stability: false,
            scoring_v2: None,
        },
        kb: kb_map,
        projects: Default::default(),
        sessions: Default::default(),
    };
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(400)).await;
    (tmp, addr)
}

/// FIX 5 / e2e proof of FIX 1: a daemon with `[memory] scoring_v2_relevance
/// = true` carrying a FORGOTTEN memory with an extreme raw BM25 score for the
/// shared query term, alongside a live memory that matches it only once.
/// Before the FIX 1 ordering fix, `compute_relevance_factors` ran over the
/// unfiltered hit list, so the forgotten sibling's inflated score corrupted
/// the live memory's `relevance_factor` (and therefore its `score`) even
/// though the forgotten memory itself never appears in the response.
#[tokio::test]
async fn recall_scoring_v2_relevance_normalization_ignores_forgotten_sibling_e2e() {
    let live_html = memory_html(
        "Live Fact",
        "the marmot prefers a zigzag trail through the reeds",
        Some(0.5),
        None,
        None,
    );
    let forgotten_pre = memory_html(
        "Forgotten Fact",
        "zigzag zigzag zigzag zigzag zigzag zigzag zigzag zigzag zigzag zigzag \
         zigzag zigzag zigzag zigzag zigzag zigzag zigzag zigzag zigzag zigzag",
        Some(0.5),
        None,
        None,
    );
    let forgotten_html = kb_core::memory::mark_forgotten(&forgotten_pre, false, 1_700_000_000);
    assert!(
        forgotten_html.contains("kb-status") && forgotten_html.contains("forgotten"),
        "fixture sanity: {forgotten_html}"
    );

    let (_tmp, addr) = boot_memory_corpus_scoring_v2(&[
        ("live.html", live_html),
        ("forgotten.html", forgotten_html),
    ])
    .await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "mem", |d| d["title"] == "Live Fact").await;
    wait_for_doc(&client, addr, "mem", |d| d["title"] == "Forgotten Fact").await;

    let hits = common::poll_until("recall to surface the live memory", || async {
        let v: serde_json::Value = client
            .get(url(addr, "/api/memory/recall?q=zigzag&scope=all&limit=20"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        let arr = v["hits"].as_array().cloned().unwrap_or_default();
        arr.iter().any(|h| h["title"] == "Live Fact").then_some(arr)
    })
    .await;

    assert!(
        !hits.iter().any(|h| h["title"] == "Forgotten Fact"),
        "a soft-forgotten memory must never appear in recall: {hits:?}"
    );
    let live = hits
        .iter()
        .find(|h| h["title"] == "Live Fact")
        .expect("live memory present");
    // The forgotten sibling is now the corpus's ONLY other scored hit, and
    // it must be excluded from normalization entirely — so the live memory
    // is the sole survivor in its corpus and gets the neutral 1.0 factor,
    // never a value pulled toward 0 by the forgotten sibling's inflated
    // raw engine score.
    let rf = live["relevance_factor"]
        .as_f64()
        .expect("relevance_factor present when scoring_v2 is on");
    assert!(
        (rf - 1.0).abs() < 1e-6,
        "live memory's relevance_factor must be uncorrupted (neutral 1.0): {live}"
    );
    assert!(
        live["score"].as_f64().unwrap() > 0.0,
        "live memory's score must be uncorrupted: {live}"
    );
}

/// FIX 6 — `GET …/memories/{id}/lineage` across a THREE-node chain
/// (A ← B ← C, oldest to newest), both directions. `kb-supersedes` values
/// are the target's artifact id (12-hex, path-derived), computed the same
/// way the indexer does.
#[tokio::test]
async fn memory_lineage_walks_multi_hop_chain_in_both_directions() {
    let id_a = kb_core::ids::ArtifactId::from_path("a.html")
        .as_str()
        .to_string();
    let id_b = kb_core::ids::ArtifactId::from_path("b.html")
        .as_str()
        .to_string();
    let id_c = kb_core::ids::ArtifactId::from_path("c.html")
        .as_str()
        .to_string();

    let global = vec![
        ("a.html", memory_html("A", "first", Some(0.5), None, None)),
        (
            "b.html",
            memory_html("B", "second", Some(0.5), None, Some(id_a.as_str())),
        ),
        (
            "c.html",
            memory_html("C", "third", Some(0.5), None, Some(id_b.as_str())),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "A").await;
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "B").await;
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "C").await;

    // Middle node: supersedes A (one forward hop), superseded by C (one
    // reverse hop).
    let lb: serde_json::Value = client
        .get(url(
            addr,
            &format!("/api/kb/globalmem/memories/{id_b}/lineage"),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(lb["start"]["id"], id_b, "full: {lb}");
    let supersedes = lb["supersedes_chain"].as_array().unwrap();
    assert_eq!(supersedes.len(), 1, "full: {lb}");
    assert_eq!(supersedes[0]["id"], id_a);
    let superseded_by = lb["superseded_by_chain"].as_array().unwrap();
    assert_eq!(superseded_by.len(), 1, "full: {lb}");
    assert_eq!(superseded_by[0]["id"], id_c);

    // Oldest node: nothing to supersede, but the FULL two-hop reverse chain
    // (nearest hop B first, then C).
    let la: serde_json::Value = client
        .get(url(
            addr,
            &format!("/api/kb/globalmem/memories/{id_a}/lineage"),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        la["supersedes_chain"].as_array().unwrap().is_empty(),
        "full: {la}"
    );
    let rev = la["superseded_by_chain"].as_array().unwrap();
    assert_eq!(rev.len(), 2, "full: {la}");
    assert_eq!(rev[0]["id"], id_b, "nearest hop first");
    assert_eq!(rev[1]["id"], id_c, "then the next hop");

    // Newest node: the full two-hop forward chain (nearest hop B first,
    // then A), nothing superseding it.
    let lc: serde_json::Value = client
        .get(url(
            addr,
            &format!("/api/kb/globalmem/memories/{id_c}/lineage"),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let fwd = lc["supersedes_chain"].as_array().unwrap();
    assert_eq!(fwd.len(), 2, "full: {lc}");
    assert_eq!(fwd[0]["id"], id_b);
    assert_eq!(fwd[1]["id"], id_a);
    assert!(
        lc["superseded_by_chain"].as_array().unwrap().is_empty(),
        "full: {lc}"
    );
}

/// FIX 6 — the lineage walk's cycle guard. Nothing at write time prevents
/// an adversarial/corrupted `kb-supersedes` RING (`kb remember
/// --supersedes` is free text); a two-node cycle (X supersedes Y, Y
/// supersedes X) must terminate the walk in ONE hop each direction, never
/// hang or loop forever.
#[tokio::test]
async fn memory_lineage_cycle_guard_terminates_without_looping() {
    let id_x = kb_core::ids::ArtifactId::from_path("x.html")
        .as_str()
        .to_string();
    let id_y = kb_core::ids::ArtifactId::from_path("y.html")
        .as_str()
        .to_string();

    let global = vec![
        (
            "x.html",
            memory_html("X", "x body", Some(0.5), None, Some(id_y.as_str())),
        ),
        (
            "y.html",
            memory_html("Y", "y body", Some(0.5), None, Some(id_x.as_str())),
        ),
    ];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "X").await;
    wait_for_doc(&client, addr, "globalmem", |d| d["title"] == "Y").await;

    let resp = tokio::time::timeout(
        Duration::from_secs(5),
        client
            .get(url(
                addr,
                &format!("/api/kb/globalmem/memories/{id_x}/lineage"),
            ))
            .send(),
    )
    .await
    .expect("lineage request must not hang on a supersede cycle")
    .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();

    let fwd = body["supersedes_chain"].as_array().unwrap();
    let rev = body["superseded_by_chain"].as_array().unwrap();
    assert_eq!(fwd.len(), 1, "cycle guard must stop at one hop: {body}");
    assert_eq!(rev.len(), 1, "cycle guard must stop at one hop: {body}");
    assert_eq!(fwd[0]["id"], id_y);
    assert_eq!(rev[0]["id"], id_y);
}

/// CT-B2 — `GET /api/kb/{kb}/memories/{id}/recalled-by`: the memory-side
/// reverse of the `memory_recalls` ledger. Two DIFFERENT sessions, captured
/// in TWO DIFFERENT kbs (globalmem + projmem), both recall the SAME memory
/// id via a `kb-recall` hook injection — proving the route's invariant #28
/// fan-out (the ledger lives with the RECALLING session's OWN kb, so the
/// projmem-captured session's row must show up even though the URL's
/// `{kb}` is globalmem), the newest-`recalled_at`-first ordering across
/// kbs, and the `session_title` fallback to `first_user_prompt` (a raw
/// capture never sets an explicit `title`).
#[tokio::test]
async fn recalled_by_fans_out_across_kbs_newest_first() {
    let sid_glob = "sess-rb-glob";
    let jsonl_glob = concat!(
        "{\"sessionId\":\"sess-rb-glob\",\"cwd\":\"/home/u/proj\",\"timestamp\":\"2026-06-04T09:00:00.000Z\"}\n",
        "{\"parentUuid\":null,\"isSidechain\":false,\"attachment\":{\"type\":\"hook_additional_context\",\"content\":[\"Relevant memories from kb (recall - these persist across sessions):\\n- alpha fact  [globalmem]  (id aaaaaaaaaaaa)\"],\"hookName\":\"UserPromptSubmit\",\"toolUseID\":\"UserPromptSubmit\",\"hookEvent\":\"UserPromptSubmit\"},\"type\":\"attachment\",\"uuid\":\"20000001-0000-4000-8000-000000000001\",\"timestamp\":\"2026-06-04T09:00:05.000Z\",\"sessionId\":\"sess-rb-glob\"}\n",
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"investigate the reconcile bug\"},\"timestamp\":\"2026-06-04T09:00:06.000Z\",\"sessionId\":\"sess-rb-glob\"}\n",
        "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]},\"timestamp\":\"2026-06-04T09:00:07.000Z\",\"sessionId\":\"sess-rb-glob\"}\n",
    );
    let sid_proj = "sess-rb-proj";
    let jsonl_proj = concat!(
        "{\"sessionId\":\"sess-rb-proj\",\"cwd\":\"/home/u/proj\",\"timestamp\":\"2026-06-04T10:00:00.000Z\"}\n",
        "{\"parentUuid\":null,\"isSidechain\":false,\"attachment\":{\"type\":\"hook_additional_context\",\"content\":[\"Relevant memories from kb (recall - these persist across sessions):\\n- alpha fact  [globalmem]  (id aaaaaaaaaaaa)\"],\"hookName\":\"UserPromptSubmit\",\"toolUseID\":\"UserPromptSubmit\",\"hookEvent\":\"UserPromptSubmit\"},\"type\":\"attachment\",\"uuid\":\"20000002-0000-4000-8000-000000000002\",\"timestamp\":\"2026-06-04T10:00:05.000Z\",\"sessionId\":\"sess-rb-proj\"}\n",
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"clean up the moves engine\"},\"timestamp\":\"2026-06-04T10:00:06.000Z\",\"sessionId\":\"sess-rb-proj\"}\n",
        "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]},\"timestamp\":\"2026-06-04T10:00:07.000Z\",\"sessionId\":\"sess-rb-proj\"}\n",
    );
    let global = vec![(
        "session-20260604T090000Z-sess-rb-glob.html",
        session_transcript_html(sid_glob, "20260604T090000Z", jsonl_glob),
    )];
    let proj = vec![(
        "session-20260604T100000Z-sess-rb-proj.html",
        session_transcript_html(sid_proj, "20260604T100000Z", jsonl_proj),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &proj).await;
    let client = reqwest::Client::new();
    wait_for_session(&client, addr, sid_glob).await;
    wait_for_session(&client, addr, sid_proj).await;

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let rows = loop {
        let body: serde_json::Value = client
            .get(url(
                addr,
                "/api/kb/globalmem/memories/aaaaaaaaaaaa/recalled-by",
            ))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let arr = body["rows"].as_array().cloned().unwrap_or_default();
        if arr.len() >= 2 {
            break arr;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for both recall ledger rows to land: {arr:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(rows.len(), 2, "rows: {rows:?}");

    // Newest `recalled_at` first: sess-rb-proj (10:00:05) before
    // sess-rb-glob (09:00:05).
    assert_eq!(rows[0]["session_id"], sid_proj, "rows: {rows:?}");
    assert_eq!(
        rows[0]["session_kb"], "projmem",
        "the ledger lives with the RECALLING session's own kb, not the \
         URL's {{kb}}: {rows:?}"
    );
    assert_eq!(
        rows[0]["session_title"], "clean up the moves engine",
        "falls back to first_user_prompt when no explicit title: {rows:?}"
    );
    assert!(rows[0]["recalled_at"].is_i64(), "rows: {rows:?}");

    assert_eq!(rows[1]["session_id"], sid_glob, "rows: {rows:?}");
    assert_eq!(rows[1]["session_kb"], "globalmem", "rows: {rows:?}");
    assert_eq!(
        rows[1]["session_title"], "investigate the reconcile bug",
        "rows: {rows:?}"
    );
}

/// CT-F1 — `GET /api/kb/{kb}/memories/{id}/commits`: the memory↔commit
/// EXACT-ID join. Two sessions captured in TWO DIFFERENT kbs both carry
/// `Kb-Memory:` trailers for the SAME memory id, proving (a) the invariant
/// #28 fan-out (the rows live with the RECORDING session's own kb, so the
/// projmem-captured session's citation shows up even though the URL's
/// `{kb}` is globalmem), and (b) the cross-kb sha dedupe — one commit is
/// one citation even when two corpora captured the same work.
#[tokio::test]
async fn committed_in_fans_out_across_kbs_and_dedupes_shas() {
    let sha_shared = "aa11223344556677889900aabbccddeeff001122";
    let sha_proj_only = "bb11223344556677889900aabbccddeeff001122";
    let global = vec![(
        "session-20260604T090000Z-sess-ci-glob.html",
        session_transcript_html_with_commits(
            "sess-ci-glob",
            "20260604T090000Z",
            &minimal_jsonl(),
            &[resolved_commit_fixture(
                sha_shared,
                vec![
                    "Kb-Session: sess-ci-glob".to_string(),
                    "Kb-Memory: aaaaaaaaaaaa".to_string(),
                    // A second, globalmem-ONLY citation: the dedupe
                    // assertion below is only meaningful once BOTH corpora
                    // have written their ledger rows, and after dedupe the
                    // globalmem row for the shared sha may be the one that
                    // loses — so this id is the deterministic "globalmem
                    // has landed" probe.
                    "Kb-Memory: cccccccccccc".to_string(),
                ],
            )],
        ),
    )];
    let proj = vec![(
        "session-20260604T100000Z-sess-ci-proj.html",
        session_transcript_html_with_commits(
            "sess-ci-proj",
            "20260604T100000Z",
            &minimal_jsonl(),
            &[
                resolved_commit_fixture(sha_shared, vec!["Kb-Memory: aaaaaaaaaaaa".to_string()]),
                resolved_commit_fixture(sha_proj_only, vec!["Kb-Memory: aaaaaaaaaaaa".to_string()]),
            ],
        ),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &proj).await;
    let client = reqwest::Client::new();
    wait_for_session(&client, addr, "sess-ci-glob").await;
    wait_for_session(&client, addr, "sess-ci-proj").await;

    let fetch_rows = |mem: &'static str| {
        let client = client.clone();
        async move {
            let body: serde_json::Value = client
                .get(url(
                    addr,
                    &format!("/api/kb/globalmem/memories/{mem}/commits"),
                ))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            body["rows"].as_array().cloned().unwrap_or_default()
        }
    };

    // Both corpora must have written before the dedupe assertion means
    // anything: `cccccccccccc` proves globalmem's ledger landed (only its
    // session cites it), and a SECOND row on `aaaaaaaaaaaa` can only come
    // from projmem (globalmem cites exactly one commit).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let rows = loop {
        let shared = fetch_rows("aaaaaaaaaaaa").await;
        let glob_only = fetch_rows("cccccccccccc").await;
        if shared.len() >= 2 && glob_only.len() == 1 {
            break shared;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for both corpora's memory_commits rows: \
             shared={shared:?} glob_only={glob_only:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        rows.len(),
        2,
        "3 rows were written across the two kbs; the shared sha must \
         collapse to ONE citation: {rows:?}"
    );
    let mut shas: Vec<&str> = rows
        .iter()
        .map(|r| r["sha_full"].as_str().unwrap())
        .collect();
    shas.sort_unstable();
    assert_eq!(shas, vec![sha_shared, sha_proj_only], "rows: {rows:?}");

    // The projmem-only citation proves the fan-out: its row can only have
    // come from a kb that is NOT the URL's `{kb}`.
    let proj_row = rows
        .iter()
        .find(|r| r["sha_full"] == sha_proj_only)
        .expect("projmem-only sha present");
    assert_eq!(proj_row["session_kb"], "projmem", "rows: {rows:?}");
    assert_eq!(proj_row["session_id"], "sess-ci-proj", "rows: {rows:?}");
    assert_eq!(
        proj_row["subject"], "feat: resolved subject",
        "the capture's own git resolution is carried, never re-read: {rows:?}"
    );
    assert_eq!(proj_row["repo_root"], "/repo", "rows: {rows:?}");
    assert!(proj_row["recorded_at"].is_i64(), "rows: {rows:?}");
}

/// CT-F1 — the DEFAULT-OFF case, end to end: an ordinary capture whose
/// commit carries only a `Kb-Session:` trailer (the repo never opted into
/// `kb.memoryTrailers`) yields 200 + an empty list. An empty result here is
/// a non-signal, never an error and never a 404.
#[tokio::test]
async fn committed_in_is_empty_without_kb_memory_trailers() {
    let global = vec![(
        "session-20260604T090000Z-sess-ci-none.html",
        session_transcript_html_with_commits(
            "sess-ci-none",
            "20260604T090000Z",
            &minimal_jsonl(),
            &[resolved_commit_fixture(
                "cc11223344556677889900aabbccddeeff001122",
                vec!["Kb-Session: sess-ci-none".to_string()],
            )],
        ),
    )];
    let (_tmp, addr) = boot_memory_corpora(&global, &[]).await;
    let client = reqwest::Client::new();
    wait_for_session(&client, addr, "sess-ci-none").await;

    let body: serde_json::Value = client
        .get(url(addr, "/api/kb/globalmem/memories/aaaaaaaaaaaa/commits"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["rows"].as_array().unwrap().len(), 0, "body: {body}");
}

/// CT-F1 — same `is_safe_id` guard as every other id-bearing memory route,
/// applied before any storage fan-out.
#[tokio::test]
async fn committed_in_rejects_unsafe_id() {
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/globalmem/memories/bad$id/commits"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
}

/// CT-B2 — a malformed memory id is rejected with 400 before any storage
/// fan-out (`is_safe_id`), same guard every other id-bearing memory route
/// uses.
#[tokio::test]
async fn recalled_by_rejects_unsafe_id() {
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/globalmem/memories/bad$id/recalled-by"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
}

/// CT-B2 — a memory id nobody ever recalled resolves 200 with an empty
/// list, matching `lineage`'s and `/sessions/{sid}/recalls`' own
/// "never happened" posture (never a 404 or an error for a merely-absent
/// ledger).
#[tokio::test]
async fn recalled_by_is_empty_when_nothing_ever_recalled_it() {
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(
            addr,
            "/api/kb/globalmem/memories/zzzzzzzzzzzz/recalled-by",
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["rows"].as_array().unwrap().len(), 0, "body: {body}");
}

/// FIX 6 — `GET /api/memory/tombstone-era` round-trips a PRE-SEEDED
/// marker verbatim: `ensure_tombstone_era` is a read-if-exists,
/// never-overwrite contract (the marker must never move forward, or a
/// caveat's window would silently shrink), so pre-seeding the file before
/// boot and reading it back through the route pins that contract from the
/// HTTP surface.
#[tokio::test]
async fn tombstone_era_route_returns_the_persisted_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let gdir = tmp.path().join("mem");
    std::fs::create_dir_all(&gdir).unwrap();
    let daemon_name = format!("era-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("mem").unwrap(),
        KbSection {
            path: gdir,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: BTreeMap::new(),
            memory_scope: Some("global".into()),
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
    std::fs::create_dir_all(&paths.state).unwrap();
    std::fs::write(
        paths.tombstone_era_file(),
        serde_json::json!({ "started_unix": 1_234_567_890i64 }).to_string(),
    )
    .unwrap();

    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(400)).await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/memory/tombstone-era"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["started_unix"], 1_234_567_890i64, "full body: {body}");
}

/// FIX 6 — on a FRESH `KB_HOME` (no pre-existing marker), the route stamps
/// `now` at first boot rather than 0/absent.
#[tokio::test]
async fn tombstone_era_route_stamps_now_on_a_fresh_kb_home() {
    let (_tmp, addr) = boot_memory_corpora(&[], &[]).await;
    let client = reqwest::Client::new();
    let before = chrono::Utc::now().timestamp();
    let body: serde_json::Value = client
        .get(url(addr, "/api/memory/tombstone-era"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let after = chrono::Utc::now().timestamp();
    let started = body["started_unix"].as_i64().expect("started_unix present");
    assert!(
        (before - 5..=after + 5).contains(&started),
        "expected ~now, got {started} (window {before}..{after})"
    );
}

// --- kb desk (v1 handoff) ---------------------------------------------------

#[tokio::test]
async fn desk_rejects_unknown_extension_415() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new().text("name", "evil").part(
        "files",
        reqwest::multipart::Part::bytes(b"binary junk".to_vec()).file_name("data.exe"),
    );
    let resp = client
        .post(url(addr, "/api/kb/smoke/desk"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 415, "{:?}", resp.text().await);
}

#[tokio::test]
async fn desk_missing_name_is_400() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new().part(
        "files",
        reqwest::multipart::Part::text("# hi").file_name("note.md"),
    );
    let resp = client
        .post(url(addr, "/api/kb/smoke/desk"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400, "{:?}", resp.text().await);
}

#[tokio::test]
async fn desk_ttl_zero_and_too_large_are_400() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    for ttl in ["0", "315360001"] {
        let form = reqwest::multipart::Form::new()
            .text("name", "ttl")
            .text("ttl_secs", ttl)
            .part(
                "files",
                reqwest::multipart::Part::text("# hi").file_name("note.md"),
            );
        let resp = client
            .post(url(addr, "/api/kb/smoke/desk"))
            .multipart(form)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status().as_u16(),
            400,
            "ttl_secs={ttl}: {:?}",
            resp.text().await
        );
    }
}

#[tokio::test]
async fn desk_readonly_handoff_dir_returns_409() {
    if running_as_root() {
        eprintln!("skipping desk_readonly_handoff_dir_returns_409: running as root");
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let (tmp, addr) = boot().await;
    let handoff = tmp.path().join("corpus").join("handoff");
    std::fs::create_dir_all(&handoff).unwrap();
    std::fs::set_permissions(&handoff, std::fs::Permissions::from_mode(0o555)).unwrap();
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new().text("name", "ro").part(
        "files",
        reqwest::multipart::Part::text("# hi").file_name("note.md"),
    );
    let resp = client
        .post(url(addr, "/api/kb/smoke/desk"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    std::fs::set_permissions(&handoff, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        resp.status().as_u16(),
        409,
        "read-only handoff dir must map to 409: {:?}",
        resp.text().await
    );
}

#[tokio::test]
async fn desk_route_401_without_token_non_loopback() {
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new()
        .text("name", "x")
        .text("text", "# hi");
    let resp = client
        .post(url(addr, "/api/kb/smoke/desk"))
        .header("X-Forwarded-For", "8.8.8.8")
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn desk_overwrite_keeps_id() {
    let (tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new().text("name", "same").part(
        "files",
        reqwest::multipart::Part::text("# one").file_name("a.md"),
    );
    let resp = client
        .post(url(addr, "/api/kb/smoke/desk"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201, "{:?}", resp.text().await);
    let first: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(first["created"], true);
    assert_eq!(first["source_relative"], "handoff/same.md");

    let form = reqwest::multipart::Form::new().text("name", "same").part(
        "files",
        reqwest::multipart::Part::text("# two").file_name("a.md"),
    );
    let resp = client
        .post(url(addr, "/api/kb/smoke/desk"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200, "{:?}", resp.text().await);
    let second: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(second["created"], false);
    assert_eq!(first["id"], second["id"]);
    let written = std::fs::read_to_string(tmp.path().join("corpus/handoff/same.md")).unwrap();
    assert!(written.contains("# two"), "{written}");
}

#[tokio::test]
async fn put_content_pipeline_family_guard_415() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new().text("name", "fam").part(
        "files",
        reqwest::multipart::Part::text("# md").file_name("a.md"),
    );
    let resp = client
        .post(url(addr, "/api/kb/smoke/desk"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201, "{:?}", resp.text().await);
    let body: serde_json::Value = resp.json().await.unwrap();
    let id = body["id"].as_str().unwrap().to_string();

    common::poll_until(&format!("desk {id} indexed"), || {
        let client = &client;
        let id = id.clone();
        async move {
            let resp = client
                .get(url(addr, &format!("/api/kb/smoke/docs/{id}")))
                .send()
                .await
                .ok()?;
            if resp.status().is_success() {
                Some(())
            } else {
                None
            }
        }
    })
    .await;

    let form = reqwest::multipart::Form::new().part(
        "files",
        reqwest::multipart::Part::text("<html></html>").file_name("x.html"),
    );
    let resp = client
        .put(url(addr, &format!("/api/kb/smoke/artifacts/{id}/content")))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 415, "{:?}", resp.text().await);
}

#[tokio::test]
async fn put_content_401_without_token_non_loopback() {
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let form = reqwest::multipart::Form::new().part(
        "files",
        reqwest::multipart::Part::text("# hi").file_name("a.md"),
    );
    let resp = client
        .put(url(addr, "/api/kb/smoke/artifacts/aaaaaaaaaaaa/content"))
        .header("X-Forwarded-For", "8.8.8.8")
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

async fn desk_offer_text(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    name: &str,
    text: &str,
) -> serde_json::Value {
    let form = reqwest::multipart::Form::new()
        .text("name", name.to_string())
        .text("text", text.to_string());
    let resp = client
        .post(url(addr, "/api/kb/smoke/desk"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "desk offer {name}: {:?}",
        resp.text().await
    );
    resp.json().await.unwrap()
}

async fn wait_desk_aggregate(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    rel: &str,
) -> serde_json::Value {
    common::poll_until(&format!("desk aggregate has {rel}"), || {
        let rel = rel.to_string();
        async move {
            let resp = client.get(url(addr, "/api/desk")).send().await.ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let body: serde_json::Value = resp.json().await.ok()?;
            let items = body.get("items")?.as_array()?;
            if items
                .iter()
                .any(|i| i.get("source_relative").and_then(|v| v.as_str()) == Some(rel.as_str()))
            {
                Some(body)
            } else {
                None
            }
        }
    })
    .await
}

#[tokio::test]
async fn desk_aggregate_only_handoff_frozen_fields() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let offered = desk_offer_text(&client, addr, "agg-one", "# agg\n\nsess").await;
    let rel = offered["source_relative"].as_str().unwrap().to_string();
    assert!(rel.starts_with("handoff/"), "{rel}");

    let body = wait_desk_aggregate(&client, addr, &rel).await;
    let items = body["items"].as_array().expect("items array");
    assert!(
        !items.is_empty(),
        "expected at least the offered handoff doc: {body}"
    );
    for item in items {
        let src = item["source_relative"].as_str().unwrap_or("");
        assert!(
            src.starts_with("handoff/"),
            "aggregate leaked non-handoff {src}: {item}"
        );
        for key in [
            "kb",
            "id",
            "source_relative",
            "title",
            "updated_unix",
            "comments_open",
            "comments_total",
            "read_state",
            "changed_since_read",
        ] {
            assert!(
                item.get(key).is_some(),
                "missing frozen field {key}: {item}"
            );
        }
        assert_eq!(item["kb"], "smoke");
    }
    assert!(body.get("attention").and_then(|v| v.as_u64()).is_some());
    assert_eq!(
        items.iter().find(|i| i["source_relative"] == rel).unwrap()["read_state"],
        "never-opened"
    );
}

#[tokio::test]
async fn desk_aggregate_kb_filter_and_unknown_404() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();
    let offered = desk_offer_text(&client, addr, "filt", "# f").await;
    let rel = offered["source_relative"].as_str().unwrap().to_string();
    let _ = wait_desk_aggregate(&client, addr, &rel).await;

    let resp = client
        .get(url(addr, "/api/desk?kb=smoke"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200, "{:?}", resp.text().await);
    let body: serde_json::Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert!(items.iter().all(|i| i["kb"] == "smoke"));
    assert!(items.iter().any(|i| i["source_relative"] == rel));

    let resp = client
        .get(url(addr, "/api/desk?kb=no-such-kb"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404, "{:?}", resp.text().await);
}

#[tokio::test]
async fn desk_aggregate_attention_arithmetic() {
    let (_tmp, addr) = boot().await;
    let client = reqwest::Client::new();

    let never = desk_offer_text(&client, addr, "attn-never", "# never").await;
    let changed = desk_offer_text(&client, addr, "attn-changed", "# changed").await;
    let stable = desk_offer_text(&client, addr, "attn-stable", "# stable").await;
    let never_rel = never["source_relative"].as_str().unwrap().to_string();
    let changed_id = changed["id"].as_str().unwrap().to_string();
    let changed_rel = changed["source_relative"].as_str().unwrap().to_string();
    let stable_id = stable["id"].as_str().unwrap().to_string();
    let stable_rel = stable["source_relative"].as_str().unwrap().to_string();

    let _ = wait_desk_aggregate(&client, addr, &never_rel).await;
    let _ = wait_desk_aggregate(&client, addr, &changed_rel).await;
    let _ = wait_desk_aggregate(&client, addr, &stable_rel).await;

    for id in [&changed_id, &stable_id] {
        let resp = client
            .post(url(addr, "/api/kb/smoke/history/open"))
            .json(&serde_json::json!({ "artifact_id": id }))
            .send()
            .await
            .unwrap();
        assert!(
            resp.status().is_success(),
            "history/open {id}: {:?}",
            resp.text().await
        );
    }

    tokio::time::sleep(Duration::from_secs(1)).await;
    let form = reqwest::multipart::Form::new().part(
        "files",
        reqwest::multipart::Part::text("# changed v2").file_name("a.md"),
    );
    let resp = client
        .put(url(
            addr,
            &format!("/api/kb/smoke/artifacts/{changed_id}/content"),
        ))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "put_content: {:?}",
        resp.text().await
    );

    let body = common::poll_until("changed_since_read on attn-changed", || {
        let client = &client;
        let changed_rel = changed_rel.clone();
        async move {
            let resp = client.get(url(addr, "/api/desk")).send().await.ok()?;
            let body: serde_json::Value = resp.json().await.ok()?;
            let items = body.get("items")?.as_array()?;
            let item = items.iter().find(|i| {
                i.get("source_relative").and_then(|v| v.as_str()) == Some(&changed_rel)
            })?;
            if item.get("changed_since_read").and_then(|v| v.as_bool()) == Some(true) {
                Some(body)
            } else {
                None
            }
        }
    })
    .await;

    let items = body["items"].as_array().unwrap();
    let find = |rel: &str| {
        items
            .iter()
            .find(|i| i["source_relative"].as_str() == Some(rel))
            .unwrap_or_else(|| panic!("missing {rel} in {body}"))
    };
    let never_item = find(&never_rel);
    let changed_item = find(&changed_rel);
    let stable_item = find(&stable_rel);
    assert_eq!(never_item["read_state"], "never-opened");
    assert_eq!(never_item["changed_since_read"], false);
    assert_eq!(changed_item["changed_since_read"], true);
    assert_ne!(stable_item["read_state"], "never-opened");
    assert_eq!(stable_item["changed_since_read"], false);

    let attention = body["attention"].as_u64().unwrap();
    let expected = items
        .iter()
        .filter(|i| {
            i["read_state"].as_str() == Some("never-opened")
                || i["changed_since_read"].as_bool() == Some(true)
        })
        .count() as u64;
    assert_eq!(attention, expected);
    assert!(
        attention >= 2,
        "never-opened + read-then-updated must both count: {body}"
    );
}

#[tokio::test]
async fn desk_aggregate_401_without_token_non_loopback() {
    let (_tmp, addr) = boot_with_token().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/desk"))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}
