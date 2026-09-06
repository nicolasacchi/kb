//! X3 (v0.24) — exclusion API integration tests. Boots the in-process
//! daemon against a tempdir corpus (the end_to_end.rs convention) and
//! exercises the GET/POST/DELETE `/api/kb/{kb}/exclusions` lifecycle:
//! exclude drops the doc from the index (file untouched on disk), list
//! enriches with artifact_id + present_on_disk, re-include brings the doc
//! back, and the `artifact.excluded`/`artifact.included` SSE kinds ride
//! the bus + the schema enum.

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

fn canon_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/canon")
}

/// Tempdir corpus: two flat canon files + one subfolder doc (exercises the
/// percent-encoded `{path}` segment on DELETE). Returns (tmp, addr, source).
async fn boot() -> (tempfile::TempDir, std::net::SocketAddr, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(source.join("sub")).unwrap();
    for name in ["kitchen-sink.html", "multi-page.html"] {
        std::fs::copy(canon_dir().join(name), source.join(name))
            .unwrap_or_else(|e| panic!("copy {name}: {e}"));
    }
    std::fs::write(
        source.join("sub/nested.html"),
        "<!doctype html><html><head><title>nested</title></head><body><h1>nested</h1></body></html>",
    )
    .unwrap();

    let daemon_name = format!(
        "test-excl-{}",
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

/// Poll the docs list until `pred(ids)` holds (the exclude cascade +
/// re-include reindex both ride the async ingest channel).
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

async fn lookup_id(client: &reqwest::Client, addr: std::net::SocketAddr, q: &str) -> String {
    let body: serde_json::Value = client
        .get(url(addr, &format!("/api/kb/smoke/lookup?q={q}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("lookup {q} → {body}"))
        .to_string()
}

#[tokio::test]
async fn exclusions_lifecycle_roundtrip() {
    let (_tmp, addr, source) = boot().await;
    let client = reqwest::Client::new();

    // Fresh daemon: no exclusions, all three docs indexed.
    let empty: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/exclusions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(empty.is_empty(), "fresh kb starts with no exclusions");
    let before = wait_for_docs(&client, addr, "initial index", |ids| ids.len() == 3).await;
    let target_id = lookup_id(&client, addr, "kitchen-sink.html").await;
    assert!(before.contains(&target_id));

    // Exclude → 200 with newly_excluded=true and the doc's real id.
    let resp = client
        .post(url(addr, "/api/kb/smoke/exclusions"))
        .json(&serde_json::json!({"path": "kitchen-sink.html", "note": "flaky viz"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["newly_excluded"].as_bool(), Some(true));
    assert_eq!(body["path"].as_str(), Some("kitchen-sink.html"));
    assert_eq!(
        body["artifact_id"].as_str(),
        Some(target_id.as_str()),
        "the deterministic path-derived id must match the indexed doc's id"
    );

    // The KeepUserData cascade drops the index row (async, via the ingest
    // channel) — but the SOURCE FILE is untouched on disk.
    wait_for_docs(&client, addr, "exclusion cascade", |ids| {
        ids.len() == 2 && !ids.contains(&target_id)
    })
    .await;
    assert!(
        source.join("kitchen-sink.html").exists(),
        "exclusion never touches the source file"
    );

    // List reflects it, enriched.
    let listed: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/exclusions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["path"].as_str(), Some("kitchen-sink.html"));
    assert_eq!(listed[0]["note"].as_str(), Some("flaky viz"));
    assert_eq!(listed[0]["artifact_id"].as_str(), Some(target_id.as_str()));
    assert_eq!(listed[0]["present_on_disk"].as_bool(), Some(true));
    assert!(listed[0]["excluded_at"].as_i64().unwrap() > 0);

    // Idempotent re-POST: 200, newly_excluded=false.
    let body: serde_json::Value = client
        .post(url(addr, "/api/kb/smoke/exclusions"))
        .json(&serde_json::json!({"path": "./kitchen-sink.html"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["newly_excluded"].as_bool(),
        Some(false),
        "re-POST (even in unnormalised ./ form) is idempotent"
    );

    // Re-include → doc comes back via the forced reindex nudge.
    let resp = client
        .delete(url(addr, "/api/kb/smoke/exclusions/kitchen-sink.html"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["was_excluded"].as_bool(), Some(true));
    wait_for_docs(&client, addr, "re-include reindex", |ids| {
        ids.len() == 3 && ids.contains(&target_id)
    })
    .await;

    // Exclusion list is empty again; a second DELETE is a no-op.
    let listed: Vec<serde_json::Value> = client
        .get(url(addr, "/api/kb/smoke/exclusions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed.is_empty());
    let body: serde_json::Value = client
        .delete(url(addr, "/api/kb/smoke/exclusions/kitchen-sink.html"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["was_excluded"].as_bool(), Some(false));
}

#[tokio::test]
async fn subfolder_exclusion_deletes_via_percent_encoded_segment() {
    let (_tmp, addr, _source) = boot().await;
    let client = reqwest::Client::new();
    wait_for_docs(&client, addr, "initial index", |ids| ids.len() == 3).await;
    let nested_id = lookup_id(&client, addr, "sub%2Fnested.html").await;

    let body: serde_json::Value = client
        .post(url(addr, "/api/kb/smoke/exclusions"))
        .json(&serde_json::json!({"path": "sub/nested.html"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["newly_excluded"].as_bool(), Some(true));
    wait_for_docs(&client, addr, "subfolder exclusion", |ids| {
        !ids.contains(&nested_id)
    })
    .await;

    // DELETE with the '/' travelling as %2F in ONE path segment.
    let resp = client
        .delete(url(addr, "/api/kb/smoke/exclusions/sub%2Fnested.html"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "percent-encoded segment must route");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["was_excluded"].as_bool(), Some(true));
    assert_eq!(body["path"].as_str(), Some("sub/nested.html"));
    wait_for_docs(&client, addr, "subfolder re-include", |ids| {
        ids.contains(&nested_id)
    })
    .await;
}

#[tokio::test]
async fn exclude_rejects_empty_and_traversal_paths() {
    let (_tmp, addr, _source) = boot().await;
    let client = reqwest::Client::new();

    for bad in ["", "  ./ ", "../evil.html", "sub/../../evil.html"] {
        let resp = client
            .post(url(addr, "/api/kb/smoke/exclusions"))
            .json(&serde_json::json!({"path": bad}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "POST path {bad:?} must be rejected");
    }
    // Same guard on the DELETE side (include_file joins onto the root).
    let resp = client
        .delete(url(addr, "/api/kb/smoke/exclusions/..%2Fevil.html"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "DELETE traversal path must be rejected");
}

#[tokio::test]
async fn exclusions_unknown_kb_is_404() {
    let (_tmp, addr, _source) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, "/api/kb/nope/exclusions"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let resp = client
        .post(url(addr, "/api/kb/nope/exclusions"))
        .json(&serde_json::json!({"path": "a.html"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn exclusion_events_ride_the_bus_and_schema() {
    use futures::StreamExt;
    let (_tmp, addr, _source) = boot().await;
    let client = reqwest::Client::new();
    wait_for_docs(&client, addr, "initial index", |ids| ids.len() == 3).await;

    // One full cycle → both intent-level kinds land in the replay ring.
    client
        .post(url(addr, "/api/kb/smoke/exclusions"))
        .json(&serde_json::json!({"path": "multi-page.html"}))
        .send()
        .await
        .unwrap();
    client
        .delete(url(addr, "/api/kb/smoke/exclusions/multi-page.html"))
        .send()
        .await
        .unwrap();

    // Fresh subscriber replays the ring; read a bounded window.
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
    assert!(
        buf.contains("artifact.excluded"),
        "excluded event missing from stream: {buf}"
    );
    assert!(
        buf.contains("artifact.included"),
        "included event missing from stream: {buf}"
    );

    // Both kinds are introspectable via the schema surface.
    let schema: serde_json::Value = client
        .get(url(addr, "/api/events.schema.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let types: Vec<&str> = schema["types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(types.contains(&"artifact.excluded"));
    assert!(types.contains(&"artifact.included"));
    for kind in ["artifact.excluded", "artifact.included"] {
        let body: serde_json::Value = client
            .get(url(addr, &format!("/api/events/schema/{kind}/v1")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            body["payload"],
            serde_json::json!(["kb", "path", "artifact_id"]),
            "payload fields for {kind}"
        );
    }
}
