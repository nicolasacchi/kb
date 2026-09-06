//! W3 T-d — `POST /api/kb/{kb}/atlas/history/backfill` end-to-end.
//!
//! Boots the in-process daemon (the `atlas_points_memo.rs` convention) and
//! drives the real route: the 202 + PLAN contract (cut points computed from
//! `mtime_unix`, doc counts, the `reconstructed` provenance and the honesty
//! note stated ON THE WIRE), and the `?frames=` bounds.
//!
//! This corpus has no embedding model, so its docs carry no embeddings and
//! nothing can be laid out — the run therefore writes no frames. That is the
//! honest outcome and it is exactly what this test asserts: the PLAN is
//! still returned (an operator sees what would be reconstructed), and the
//! frame list stays empty rather than filling with zero-point stills. The
//! written/skipped/idempotence behaviour is unit-covered against a real
//! storage actor in `kb_core::atlas`'s tests, where embeddings can be
//! injected directly.
use crate::common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

fn doc(title: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title></head>\
         <body><h1>{title}</h1><p>body text for {title}</p></body></html>"
    )
}

async fn boot() -> (tempfile::TempDir, std::net::SocketAddr, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("a.html"), doc("Doc A")).unwrap();
    std::fs::write(source.join("b.html"), doc("Doc B")).unwrap();
    std::fs::write(source.join("c.html"), doc("Doc C")).unwrap();

    let daemon_name = format!(
        "test-atlas-backfill-{}",
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

#[tokio::test(flavor = "multi_thread")]
async fn backfill_returns_the_plan_and_labels_it_reconstructed() {
    // MI test-hardening (2026-08) — serialize against the other atlas tests
    // in this crate's suite (full corpus scan + PCA per call is this
    // crate's heaviest lance/datafusion workload); see
    // `tests/common/mod.rs`'s doc for the full rationale.
    let _lance_guard = common::atlas_lance_lock().lock().await;
    let (_tmp, addr, _source) = boot().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(url(addr, "/api/kb/smoke/atlas/history/backfill?frames=3"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let body: serde_json::Value = resp.json().await.unwrap();

    // The plan comes back BEFORE the work: this is what the CLI prints.
    assert_eq!(body["status"], "started");
    assert_eq!(body["kb"], "smoke");
    assert!(body["run"].as_str().is_some_and(|r| !r.is_empty()));
    // Provenance + the honesty sentence are stated on the wire, so no
    // client has to infer (or soften) the claim.
    assert_eq!(body["provenance"], "reconstructed");
    let note = body["note"].as_str().unwrap_or_default();
    assert!(note.contains("not recorded history"), "note = {note}");

    let cuts = body["cuts"].as_array().expect("cuts array");
    assert!(!cuts.is_empty() && cuts.len() <= 3, "cuts = {cuts:?}");
    // Cut points are ascending, and the last one covers the whole corpus.
    let mut prev = i64::MIN;
    for c in cuts {
        let at = c["cut_unix"].as_i64().unwrap();
        assert!(at > prev, "cuts must ascend: {cuts:?}");
        prev = at;
    }
    assert_eq!(cuts.last().unwrap()["doc_count"], 3);

    // No embedding model ⇒ no embeddings ⇒ nothing to lay out. The honest
    // result is an unchanged (empty) frame list, not zero-point stills.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let history: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/atlas/history"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(history["frames"].as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn backfill_rejects_a_frame_count_outside_its_bounds() {
    // See `backfill_returns_the_plan_and_labels_it_reconstructed` — same
    // lance-pool-contention rationale.
    let _lance_guard = common::atlas_lance_lock().lock().await;
    let (_tmp, addr, _source) = boot().await;
    let client = reqwest::Client::new();

    for frames in ["0", "13", "100"] {
        let resp = client
            .post(url(
                addr,
                &format!("/api/kb/smoke/atlas/history/backfill?frames={frames}"),
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "?frames={frames} must be refused, never silently clamped"
        );
    }
}
