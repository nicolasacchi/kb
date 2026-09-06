//! CT-F5 — `GET /api/kb/{kb}/slo`, `POST …/slo/snapshot`, `GET
//! …/slo/snapshots`. Boots the in-process daemon (the `coderefs.rs` /
//! `exclusions.rs` convention) and drives the three routes end-to-end.
//!
//! What these tests are FOR: the wire contract and the honest-`unknown`
//! paths. The arithmetic itself is pinned by `kb_core::slo`'s unit tests
//! (pure, fixture-driven, every unknown branch) and the reads by
//! `sqlite.rs`'s — this file proves the plumbing between them, and that a
//! corpus with nothing to measure produces a FULL report of `unknown`s
//! rather than an error or a fabricated zero.
use crate::common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, SloSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::time::Duration;

/// A doc that cites code: two path-shaped hints and one symbol hint, so the
/// coderef indicator has a non-trivial numerator AND denominator.
fn citing_doc(title: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title></head>\
         <body><h1>{title}</h1>\
         <p>see <code>app/models/order.rb</code> and \
            <code>lib/indexer.rb:42</code>, handled by \
            <code>Orders::Indexer</code></p></body></html>"
    )
}

async fn boot(
    files: &[(&str, String)],
    slo: Option<SloSection>,
) -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    for (name, html) in files {
        std::fs::write(source.join(name), html).unwrap();
    }

    let daemon_name = format!(
        "test-slo-{}",
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
            slo,
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
    (tmp, addr)
}

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

async fn get_json(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    path: &str,
) -> serde_json::Value {
    client
        .get(url(addr, path))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn indicator<'a>(report: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    report["indicators"]
        .as_array()
        .expect("indicators array")
        .iter()
        .find(|i| i["key"] == key)
        .unwrap_or_else(|| panic!("indicator {key} missing"))
}

/// A corpus with nothing to measure must still answer a FULL report — four
/// indicators, all present. Dropping an unmeasurable row would make a broken
/// input look like a missing feature.
#[tokio::test]
async fn an_empty_corpus_reports_four_honest_unknowns() {
    let (_tmp, addr) = boot(&[], None).await;
    let client = reqwest::Client::new();
    let body = get_json(&client, addr, "/api/kb/smoke/slo").await;

    assert_eq!(body["grammar"], "kb-slo/1");
    assert_eq!(body["kb"], "smoke");
    assert_eq!(body["indicators"].as_array().unwrap().len(), 4);
    assert_eq!(body["warn_count"], 0);

    for key in [
        "coderef_resolution_pct",
        "ledger_parse_failure_pct",
        "capture_freshness_hours",
    ] {
        let i = indicator(&body, key);
        assert!(i["value"].is_null(), "{key} must be null, not 0: {i}");
        assert_eq!(i["status"], "unknown", "{key}");
        assert!(
            i["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "{key} must say WHY it is unknown"
        );
    }
    // The orphan count is the one honest zero: a count over an empty set
    // needs no inference.
    let orphans = indicator(&body, "orphan_kb_sessions");
    assert_eq!(orphans["value"], 0.0);
    assert_eq!(orphans["status"], "unknown", "no target ⇒ no verdict");
}

/// The doc↔code lane end-to-end: two path-shaped hints out of three, judged
/// against a configured minimum.
#[tokio::test]
async fn coderef_indicator_measures_path_shape_and_honours_its_target() {
    let (_tmp, addr) = boot(
        &[("cites.html", citing_doc("Cites"))],
        Some(SloSection {
            // 2 of 3 hints are path-shaped = 66.67%, below this minimum.
            coderef_resolution_pct: Some(90.0),
            ..Default::default()
        }),
    )
    .await;
    let client = reqwest::Client::new();

    // The code-ref hook runs as part of indexing, so poll until it lands.
    let body = common::poll_until("a measured coderef indicator", || async {
        let b = get_json(&client, addr, "/api/kb/smoke/slo").await;
        (!indicator(&b, "coderef_resolution_pct")["value"].is_null()).then_some(b)
    })
    .await;

    let i = indicator(&body, "coderef_resolution_pct");
    assert_eq!(i["value"], 66.67, "2 of 3 hints carry a path shape: {i}");
    assert_eq!(i["target"], 90.0);
    assert_eq!(i["direction"], "higher_is_better");
    assert_eq!(i["status"], "warn");
    assert_eq!(body["warn_count"], 1);
}

/// A measured value with no configured target is `unknown`, never a free
/// `ok` — the wire must carry that, not just the pure layer.
#[tokio::test]
async fn a_measured_indicator_without_a_target_stays_unknown_on_the_wire() {
    let (_tmp, addr) = boot(&[("cites.html", citing_doc("Cites"))], None).await;
    let client = reqwest::Client::new();
    let body = common::poll_until("a measured coderef indicator", || async {
        let b = get_json(&client, addr, "/api/kb/smoke/slo").await;
        (!indicator(&b, "coderef_resolution_pct")["value"].is_null()).then_some(b)
    })
    .await;
    let i = indicator(&body, "coderef_resolution_pct");
    assert_eq!(i["value"], 66.67);
    assert!(i["target"].is_null());
    assert_eq!(i["status"], "unknown");
}

/// The append-only log: every run lands (no skip-if-unchanged), rows carry
/// the target they were judged by, and an `unknown` stores NULL.
#[tokio::test]
async fn snapshots_append_every_run_and_read_back_newest_first() {
    let (_tmp, addr) = boot(
        &[],
        Some(SloSection {
            coderef_resolution_pct: Some(80.0),
            ..Default::default()
        }),
    )
    .await;
    let client = reqwest::Client::new();

    let empty = get_json(&client, addr, "/api/kb/smoke/slo/snapshots").await;
    assert_eq!(empty["kb"], "smoke");
    assert!(empty["rows"].as_array().unwrap().is_empty());

    for _ in 0..2 {
        let r: serde_json::Value = client
            .post(url(addr, "/api/kb/smoke/slo/snapshot"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(r["appended"], 4);
        // The route echoes what it stored, so a caller renders the same
        // reading it recorded rather than a second, slightly-later read.
        assert_eq!(r["report"]["indicators"].as_array().unwrap().len(), 4);
        assert_eq!(r["report"]["computed_at_unix"], r["taken_at_unix"]);
        tokio::time::sleep(Duration::from_millis(1_100)).await;
    }

    let log = get_json(&client, addr, "/api/kb/smoke/slo/snapshots?limit=100").await;
    let rows = log["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 8, "identical readings are NOT deduped away");
    let ts: Vec<i64> = rows
        .iter()
        .map(|r| r["taken_at_unix"].as_i64().unwrap())
        .collect();
    assert!(ts.windows(2).all(|w| w[0] >= w[1]), "newest first: {ts:?}");

    let coderef = rows
        .iter()
        .find(|r| r["indicator"] == "coderef_resolution_pct")
        .expect("row present");
    assert!(coderef["value"].is_null(), "an unknown stores NULL, not 0");
    assert_eq!(coderef["target"], 80.0, "the judged-by target rides along");
    assert_eq!(coderef["status"], "unknown");

    // `limit` is honoured (and clamped) rather than ignored.
    let one = get_json(&client, addr, "/api/kb/smoke/slo/snapshots?limit=1").await;
    assert_eq!(one["rows"].as_array().unwrap().len(), 1);
}

/// An unknown kb is a 404 problem+json, not an empty report.
#[tokio::test]
async fn an_unknown_kb_is_a_problem_json_not_an_empty_report() {
    let (_tmp, addr) = boot(&[], None).await;
    let client = reqwest::Client::new();
    let r = client
        .get(url(addr, "/api/kb/nope/slo"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}
