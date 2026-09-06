//! SC3 (v0.24) — `GET /api/kb/{kb}/facets` generation-keyed memo. Boots the
//! in-process daemon (the end_to_end.rs convention) with `[server] metrics =
//! true` so `/api/metrics`'s `detailed.pipeline.storage` block exposes the
//! storage actor's per-`StorageKind` call counts — `list_docs`/`edge_counts`
//! both fall through to `StorageKind::Read` (see `kb_core::metrics::
//! StorageKind::from_msg`'s catch-all), so a stable "read" count across two
//! back-to-back `/facets` calls at the SAME index generation proves the
//! second call hit the memo instead of re-scanning.

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
        "test-facets-{}",
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

async fn facets(client: &reqwest::Client, addr: std::net::SocketAddr) -> serde_json::Value {
    client
        .get(url(addr, "/api/kb/smoke/facets"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// Storage actor's cumulative "read" `StorageKind` count — `list_docs` and
/// `edge_counts` both fall through `StorageKind::from_msg`'s catch-all, so
/// this counter only climbs when the facets memo misses and re-scans.
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

fn category_bucket<'a>(body: &'a serde_json::Value, value: &str) -> Option<&'a serde_json::Value> {
    body["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["value"] == value)
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
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let ids = doc_ids(client, addr).await;
        if pred(&ids) {
            return ids;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}; docs = {ids:?}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// SC3 — a second `/facets` call at the same index generation must hit the
/// memo (zero extra storage reads), and a subsequent doc upsert (which
/// bumps the generation, invariant #15) must force a rebuild that surfaces
/// the new doc's category.
#[tokio::test]
async fn facets_memo_hits_then_invalidates_on_new_doc() {
    let _lance_guard = common::atlas_lance_lock().lock().await;
    let (_tmp, addr, source) = boot().await;
    let client = reqwest::Client::new();
    wait_for_docs(&client, addr, "initial index", |ids| ids.len() == 2).await;

    // Warm the memo (first call may or may not scan depending on prior
    // /docs traffic — irrelevant here, nothing else has hit the daemon).
    let first = facets(&client, addr).await;
    assert!(category_bucket(&first, "alpha").is_some());
    assert!(category_bucket(&first, "beta").is_some());
    assert!(
        category_bucket(&first, "gamma").is_none(),
        "third doc not indexed yet"
    );

    let after_first = read_count(&client, addr).await;

    // Same generation: the second call must be a full memo hit — zero
    // additional storage-actor reads.
    let second = facets(&client, addr).await;
    let after_second = read_count(&client, addr).await;
    assert_eq!(second, first, "identical response on a memo hit");
    assert_eq!(
        after_second, after_first,
        "a repeat /facets call at the same generation must add zero storage reads"
    );

    // Add a third categorized doc — an UpsertDoc bumps the generation
    // (invariant #15), which must force the facets memo to rebuild.
    std::fs::write(source.join("c.html"), doc("Doc C", "gamma")).unwrap();
    wait_for_docs(&client, addr, "third doc indexed", |ids| ids.len() == 3).await;

    let third = facets(&client, addr).await;
    assert!(
        category_bucket(&third, "gamma").is_some(),
        "new doc's category must appear after the generation bump: {third}"
    );
    let after_third = read_count(&client, addr).await;
    assert!(
        after_third > after_second,
        "the generation bump must force a rescan (memo miss), not a stale serve"
    );
}

/// X2/X3 — excluding a doc removes it from the index via the KeepUserData
/// cascade, which bumps the generation (invariant #15). The facets memo
/// must pick this up: the excluded doc's category leaves the aggregate.
#[tokio::test]
async fn excluded_doc_leaves_its_facet_bucket() {
    let _lance_guard = common::atlas_lance_lock().lock().await;
    let (_tmp, addr, _source) = boot().await;
    let client = reqwest::Client::new();
    wait_for_docs(&client, addr, "initial index", |ids| ids.len() == 2).await;

    let before = facets(&client, addr).await;
    let alpha_before = category_bucket(&before, "alpha").expect("alpha present pre-exclude");
    assert_eq!(alpha_before["count"].as_u64(), Some(1));
    assert!(category_bucket(&before, "beta").is_some());
    let before_reads = read_count(&client, addr).await;

    // Exclude the "alpha" doc — cascades to a delete, bumping the generation.
    let resp = client
        .post(url(addr, "/api/kb/smoke/exclusions"))
        .json(&serde_json::json!({"path": "a.html"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    wait_for_docs(&client, addr, "exclusion cascade", |ids| ids.len() == 1).await;

    let after = facets(&client, addr).await;
    assert!(
        category_bucket(&after, "alpha").is_none(),
        "excluded doc's category must leave the facet aggregate: {after}"
    );
    assert!(
        category_bucket(&after, "beta").is_some(),
        "the non-excluded doc's category survives"
    );
    let after_reads = read_count(&client, addr).await;
    assert!(
        after_reads > before_reads,
        "exclusion's generation bump must force the facets memo to rebuild, not serve stale"
    );
}
