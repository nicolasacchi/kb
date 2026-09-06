//! M-a (v0.28+) — `GET /api/kb/{kb}/atlas/points` generation-keyed memo.
//! Boots the in-process daemon (the `facets_memo.rs` convention) with
//! `[server] metrics = true` so `/api/metrics`'s
//! `detailed.pipeline.storage` block exposes the storage actor's
//! per-`StorageKind` call counts — `list_docs_with_atlas` falls through to
//! `StorageKind::Read` (see `kb_core::metrics::StorageKind::from_msg`'s
//! catch-all), so a stable "read" count across two back-to-back
//! `/atlas/points` calls at the SAME index generation proves the second
//! call hit the memo instead of re-scanning the whole corpus again.
use crate::common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

fn doc(title: &str, category: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title>\
         <meta name=\"kb-category\" content=\"{category}\"></head>\
         <body><h1>{title}</h1><p>body text for {title}</p></body></html>"
    )
}

/// Tempdir corpus with two categorized docs (`alpha` / `beta`). Metrics are
/// enabled so `/api/metrics` surfaces the storage-actor read count.
async fn boot() -> (tempfile::TempDir, std::net::SocketAddr, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("a.html"), doc("Doc A", "alpha")).unwrap();
    std::fs::write(source.join("b.html"), doc("Doc B", "beta")).unwrap();

    let daemon_name = format!(
        "test-atlas-points-{}",
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
        server: ServerSection {
            metrics: true,
            ..Default::default()
        },
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

async fn atlas_points(client: &reqwest::Client, addr: std::net::SocketAddr) -> serde_json::Value {
    client
        .get(url(addr, "/api/kb/smoke/atlas/points"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// Storage actor's cumulative "read" `StorageKind` count — `list_docs_with_
/// atlas` falls through `StorageKind::from_msg`'s catch-all, so this
/// counter only climbs when the atlas-points memo misses and re-scans.
async fn read_count(client: &reqwest::Client, addr: std::net::SocketAddr) -> u64 {
    let body: serde_json::Value = client
        .get(url(addr, "/api/metrics"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let storage = body["detailed"]["pipeline"]["storage"]
        .as_array()
        .expect("detailed metrics must be enabled");
    storage
        .iter()
        .find(|v| v["kind"] == "read")
        .and_then(|v| v["count"].as_u64())
        .expect("a 'read' StorageKind entry")
}

async fn doc_ids(client: &reqwest::Client, addr: std::net::SocketAddr) -> Vec<String> {
    let body: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/docs?limit=100"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body.iter()
        .filter_map(|d| d["id"].as_str().map(str::to_string))
        .collect()
}

async fn wait_for_docs(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    what: &str,
    pred: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    common::poll_until(what, || async {
        let ids = doc_ids(client, addr).await;
        pred(&ids).then_some(ids)
    })
    .await
}

/// A second `/atlas/points` call at the same index generation must hit the
/// memo (zero extra storage reads, byte-identical body), and a subsequent
/// doc upsert (which bumps the generation, invariant #15) must force a
/// rebuild that surfaces the new doc.
#[tokio::test]
async fn atlas_points_memo_hits_then_invalidates_on_new_doc() {
    // MI test-hardening (2026-08) — serialize this binary's atlas tests:
    // each does a full corpus scan + PCA, the heaviest lance/datafusion
    // workload in this crate's suite, and running several concurrently has
    // produced memory-pool exhaustion under host load. See `common`'s
    // module doc for the full rationale + scope.
    let _lance_guard = common::atlas_lance_lock().lock().await;
    let (_tmp, addr, source) = boot().await;
    let client = reqwest::Client::new();
    wait_for_docs(&client, addr, "initial index", |ids| ids.len() == 2).await;

    // Warm the memo.
    let first = atlas_points(&client, addr).await;
    assert_eq!(first["total"].as_u64(), Some(2));
    let points = first["points"].as_array().unwrap();
    assert_eq!(points.len(), 2);

    let after_first = read_count(&client, addr).await;

    // Same generation: the second call must be a full memo hit — zero
    // additional storage-actor reads, and an identical response body.
    let second = atlas_points(&client, addr).await;
    let after_second = read_count(&client, addr).await;
    assert_eq!(second, first, "identical response on a memo hit");
    assert_eq!(
        after_second, after_first,
        "a repeat /atlas/points call at the same generation must add zero storage reads"
    );

    // Add a third doc — an UpsertDoc bumps the generation (invariant #15),
    // which must force the atlas-points memo to rebuild and pick up the
    // new doc rather than serving the stale 2-point snapshot.
    std::fs::write(source.join("c.html"), doc("Doc C", "gamma")).unwrap();
    wait_for_docs(&client, addr, "third doc indexed", |ids| ids.len() == 3).await;

    let third = atlas_points(&client, addr).await;
    assert_eq!(
        third["total"].as_u64(),
        Some(3),
        "the new doc must appear after the generation bump: {third}"
    );
    let after_third = read_count(&client, addr).await;
    assert!(
        after_third > after_second,
        "the generation bump must force a rescan (memo miss), not a stale serve"
    );
}

/// The whole point of this route: it must return every doc in the corpus,
/// not just the paged gallery's default 200-doc window (the measured bug
/// this endpoint exists to fix). A small corpus can't reproduce >200 docs
/// in a fast test, so this pins the *shape* guarantee instead — `total`
/// always equals `points.len()`, and every cluster count in `clusters`
/// sums to at most `total` (never inflated, never silently dropped).
#[tokio::test]
async fn atlas_points_total_matches_points_len_and_cluster_counts_are_bounded() {
    // See `atlas_points_memo_hits_then_invalidates_on_new_doc` — same
    // lance-pool-contention rationale.
    let _lance_guard = common::atlas_lance_lock().lock().await;
    let (_tmp, addr, _source) = boot().await;
    let client = reqwest::Client::new();
    wait_for_docs(&client, addr, "initial index", |ids| ids.len() == 2).await;

    let body = atlas_points(&client, addr).await;
    let total = body["total"].as_u64().unwrap();
    let points = body["points"].as_array().unwrap();
    assert_eq!(total, points.len() as u64);

    let clusters = body["clusters"].as_array().unwrap();
    let cluster_sum: u64 = clusters.iter().map(|c| c["count"].as_u64().unwrap()).sum();
    assert!(
        cluster_sum <= total,
        "cluster counts must never exceed the total point count"
    );
}
