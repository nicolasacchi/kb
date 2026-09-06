//! Integration tests for ARTIFACT HOST GRAMMAR v2 (kb-qualified artifact
//! subdomains) + federated (kb,id)-composite attribution — CT-B-RUST.
//!
//! Two kbs sharing a source-relative path hash to the SAME artifact id
//! (`kb_core::ids::ArtifactId::from_path` hashes only the rel path, not
//! the kb). Pre-v2, the bare `<id>.artifacts.<suffix>` subdomain always
//! resolved to whichever kb sorts first (alphabetical `BTreeMap` walk),
//! and federated (`scope=all`) search silently conflated the two docs
//! into one row. This file boots a real two-kb daemon with a deliberate
//! collision and pins:
//!
//! (a) a QUALIFIED host (`{kb}--{id}.artifacts...`) serves each kb's own
//!     bytes, disambiguating the collision;
//! (b) a BARE host (`{id}.artifacts...`) keeps the pre-v2 first-wins
//!     behaviour, byte-identical;
//! (c) `scope=all` search returns BOTH colliding docs, each attributed to
//!     its own kb (title/source_relative/kb all correct per row).

mod common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

fn kb_section(path: std::path::PathBuf) -> KbSection {
    KbSection {
        path,
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
    }
}

/// Boot a daemon with TWO kbs — "alpha" and "beta" — each owning a file at
/// the SAME source-relative path (`index.html`), so both hash to the
/// IDENTICAL artifact id (`ArtifactId::from_path` hashes only the rel
/// path — invariant #2's DCB writeup, and the ARTIFACT HOST GRAMMAR v2
/// background this milestone fixes). Each body carries a distinct,
/// unique-word marker so keyword search can tell them apart.
async fn boot_with_colliding_kbs() -> (
    tempfile::TempDir,
    std::net::SocketAddr,
    /* the id both kbs share */ String,
) {
    let tmp = tempfile::tempdir().unwrap();
    let alpha_root = tmp.path().join("alpha-src");
    let beta_root = tmp.path().join("beta-src");
    std::fs::create_dir_all(&alpha_root).unwrap();
    std::fs::create_dir_all(&beta_root).unwrap();
    // "zorquack" is a standalone WORD shared by both docs (the FTS query
    // term below must match as a whole token, not a substring); the
    // "alphamarker"/"betamarker" tokens distinguish which body a raw HTTP
    // GET actually served.
    std::fs::write(
        alpha_root.join("index.html"),
        "<!doctype html><html><body><h1>alphamarker zorquack collision doc</h1></body></html>",
    )
    .unwrap();
    std::fs::write(
        beta_root.join("index.html"),
        "<!doctype html><html><body><h1>betamarker zorquack collision doc</h1></body></html>",
    )
    .unwrap();

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(KbName::new("alpha").unwrap(), kb_section(alpha_root));
    kb_map.insert(KbName::new("beta").unwrap(), kb_section(beta_root));

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
    assert_eq!(
        alpha_id, beta_id,
        "fixture must produce a genuine id collision (both kbs share the \
         `index.html` rel path) — got alpha={alpha_id} beta={beta_id}"
    );
    (tmp, addr, alpha_id)
}

#[tokio::test]
async fn qualified_host_disambiguates_colliding_ids() {
    // (a) — a `{kb}--{id}` subdomain must serve THAT kb's own bytes, even
    // though a BARE `{id}` subdomain can't tell the two apart.
    let (_tmp, addr, id) = boot_with_colliding_kbs().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(url(addr, "/"))
        .header("Host", format!("alpha--{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "alpha qualified got {}",
        resp.status()
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("alphamarker"),
        "expected alpha's own body via the qualified host, got: {}",
        &body[..body.len().min(200)]
    );

    let resp = client
        .get(url(addr, "/"))
        .header("Host", format!("beta--{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "beta qualified got {}",
        resp.status()
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("betamarker"),
        "expected beta's own body via the qualified host, got: {}",
        &body[..body.len().min(200)]
    );

    // A syntactically-qualified host naming a kb that doesn't exist falls
    // back to the legacy bare walk on the FULL label (grammar spec) —
    // which finds no `<label>.html` file either, so it honestly 404s
    // rather than silently resolving to some other kb.
    let resp = client
        .get(url(addr, "/"))
        .header("Host", format!("nope--{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "unknown kb_enc must not resolve to any kb"
    );
}

#[tokio::test]
async fn bare_host_keeps_pre_v2_first_wins_on_a_collision() {
    // (b) — the pre-v2 behaviour must stay byte-identical: a bare `{id}`
    // subdomain resolves to the alphabetically-first kb owning the id.
    let (_tmp, addr, id) = boot_with_colliding_kbs().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(url(addr, "/"))
        .header("Host", format!("{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "bare host got {}",
        resp.status()
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("alphamarker"),
        "bare host on a collision must still pick the alphabetically-first \
         kb (alpha < beta), got: {}",
        &body[..body.len().min(200)]
    );
}

#[tokio::test]
async fn federated_search_attributes_both_colliding_docs_to_their_own_kb() {
    // (c) — scope=all must surface BOTH colliding docs as separate hits,
    // each correctly attributed (kb + title), never merged into one row
    // and never silently dropping the second corpus's hit.
    let (_tmp, addr, id) = boot_with_colliding_kbs().await;
    let client = reqwest::Client::new();

    let v: serde_json::Value = client
        .get(url(
            addr,
            "/api/search?q=zorquack&mode=keyword&scope=all&limit=10",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hits = v["hits"].as_array().expect("hits array");
    assert_eq!(
        hits.len(),
        2,
        "both colliding docs must survive the federated merge as distinct rows: {v:?}"
    );
    let mut by_kb: std::collections::HashMap<String, &serde_json::Value> =
        std::collections::HashMap::new();
    for h in hits {
        assert_eq!(
            h["id"].as_str().unwrap(),
            id,
            "both rows share the colliding id"
        );
        by_kb.insert(h["kb"].as_str().unwrap().to_string(), h);
    }
    assert_eq!(
        by_kb.len(),
        2,
        "each hit must carry its OWN kb attribution: {hits:?}"
    );
    let alpha_hit = by_kb.get("alpha").expect("alpha hit present");
    let beta_hit = by_kb.get("beta").expect("beta hit present");
    assert_eq!(alpha_hit["source_relative"].as_str().unwrap(), "index.html");
    assert_eq!(beta_hit["source_relative"].as_str().unwrap(), "index.html");
}
