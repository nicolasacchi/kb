//! T1 (v0.24) — server-side `/api/events` payload filters (`kb:` /
//! `artifact:`) + the embedder-health fields on `GET /api/metrics`.
//! Boots the in-process daemon with TWO kbs so the kb filter has
//! something to drop. These filters exist for CLI consumers
//! (`kb events --follow --kb/--artifact`); the SPA's SharedWorker keeps
//! consuming the unfiltered stream (invariant #24) — nothing here
//! touches its subscription.

use crate::common;

use futures::StreamExt;
use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::time::Duration;

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
    }
}

/// Two-kb boot: `alpha` and `beta` each hold one artifact, so the ring
/// replays one `artifact.indexed` per kb to a fresh subscriber.
async fn boot_two_kbs() -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    for name in ["alpha", "beta"] {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}-doc.html")),
            format!(
                "<!doctype html><html><head><title>{name} doc</title></head>\
                 <body><h1>{name} doc</h1><p>corpus {name}</p></body></html>"
            ),
        )
        .unwrap();
        kb_map.insert(KbName::new(name).unwrap(), kb_section(dir));
    }
    let daemon_name = format!(
        "test-evfilter-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
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
    // Wait until both docs are listed — their artifact.indexed events
    // have then landed in the ring.
    common::wait_docs_listed(addr, "alpha", 1).await;
    common::wait_docs_listed(addr, "beta", 1).await;
    (tmp, addr)
}

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

/// Open an SSE subscription and buffer everything it sends for `window`.
async fn read_window(addr: std::net::SocketAddr, path: &str, window: Duration) -> String {
    let client = reqwest::Client::new();
    let resp = client.get(url(addr, path)).send().await.unwrap();
    assert!(resp.status().is_success(), "got {}", resp.status());
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    let _ = tokio::time::timeout(window, async {
        while let Some(chunk) = stream.next().await {
            if let Ok(bytes) = chunk {
                buf.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    })
    .await;
    buf
}

/// Every envelope payload in the buffered stream (`data:` lines parsed,
/// synthetic lag/gap and keep-alives excluded by the payload-key probe).
fn payloads(buf: &str) -> Vec<serde_json::Value> {
    buf.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str::<serde_json::Value>(d.trim()).ok())
        .filter_map(|v| v.get("payload").cloned())
        .collect()
}

async fn first_doc_id(addr: std::net::SocketAddr, kb: &str) -> String {
    let body: Vec<serde_json::Value> = reqwest::Client::new()
        .get(url(addr, &format!("/api/kb/{kb}/docs?limit=10")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body.first()
        .and_then(|d| d["id"].as_str())
        .unwrap_or_else(|| panic!("no docs in {kb}"))
        .to_string()
}

#[tokio::test]
async fn filter_kb_keeps_only_that_kbs_events() {
    let (_tmp, addr) = boot_two_kbs().await;
    let buf = read_window(
        addr,
        "/api/events?filter=kb:alpha",
        Duration::from_millis(800),
    )
    .await;
    let got = payloads(&buf);
    assert!(
        got.iter().any(|p| p["kb"] == "alpha"),
        "alpha's ring events must pass the kb:alpha filter; stream:\n{buf}"
    );
    for p in &got {
        assert_eq!(
            p["kb"], "alpha",
            "non-alpha payload leaked through kb:alpha: {p}"
        );
    }
    // metrics.tick has no `kb` key — the payload filter must drop it too.
    assert!(
        !buf.contains("event: metrics.tick"),
        "kb-less metrics.tick leaked through kb:alpha:\n{buf}"
    );
}

#[tokio::test]
async fn filter_artifact_keeps_only_events_referencing_that_artifact() {
    let (_tmp, addr) = boot_two_kbs().await;
    let alpha_id = first_doc_id(addr, "alpha").await;
    let buf = read_window(
        addr,
        &format!("/api/events?filter=artifact:{alpha_id}"),
        Duration::from_millis(800),
    )
    .await;
    let got = payloads(&buf);
    assert!(
        !got.is_empty(),
        "alpha's artifact.indexed must pass artifact:{alpha_id}; stream:\n{buf}"
    );
    for p in &got {
        let referenced = ["artifact_id", "hash", "id"]
            .iter()
            .any(|k| p[*k] == alpha_id.as_str());
        assert!(referenced, "payload does not reference {alpha_id}: {p}");
    }
}

#[tokio::test]
async fn combined_kb_and_artifact_filter_is_conjunctive() {
    // kb:alpha AND artifact:<beta's id> can never both hold — the window
    // must contain zero envelope frames (only keep-alives).
    let (_tmp, addr) = boot_two_kbs().await;
    let beta_id = first_doc_id(addr, "beta").await;
    let buf = read_window(
        addr,
        &format!("/api/events?filter=kb:alpha,artifact:{beta_id}"),
        Duration::from_millis(800),
    )
    .await;
    let got = payloads(&buf);
    assert!(
        got.is_empty(),
        "contradictory kb+artifact filter must drop everything, got: {got:?}"
    );
}

#[tokio::test]
async fn metrics_route_reports_embedder_health_fields() {
    // T1 — /api/metrics carries the same embedder signal metrics.tick
    // streams. No embedder is configured here, so: not degraded, zero
    // respawns — and the fields must EXIST (older builds lacked them).
    let (_tmp, addr) = boot_two_kbs().await;
    let body: serde_json::Value = reqwest::Client::new()
        .get(url(addr, "/api/metrics"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["embedder_degraded"],
        serde_json::json!(false),
        "embedder_degraded missing/wrong: {body}"
    );
    assert_eq!(
        body["embedder_respawn_count"],
        serde_json::json!(0),
        "embedder_respawn_count missing/wrong: {body}"
    );
}
