//! OL TIME1 — `GET /api/kb/{kb}/docs/as-of?at=&ids=`, the Library time
//! scrubber's batched per-artifact as-of overlay. Boots the in-process daemon
//! (the `coderefs.rs` convention) and drives it via `reqwest`. Pure-logic
//! pins (order, deadline, caps, auth) live beside the handler in
//! `routes/docs_asof.rs`.
use crate::common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

pub(crate) fn page(title: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title></head>\
         <body><h1>{title}</h1><p>hello {title}</p></body></html>"
    )
}

pub(crate) fn session_doc(title: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title>\
         <meta name=\"kb-category\" content=\"memory-session\"></head>\
         <body><pre>hi</pre></body></html>"
    )
}

async fn boot(
    files: &[(&str, String)],
    versions: Option<&str>,
) -> (tempfile::TempDir, std::net::SocketAddr, PathBuf) {
    boot_with(files, versions, None).await
}

/// `boot` plus an optional `[kb.*.outbound]` section (OL TIME2's artifact
/// tests drive the serve-path scrub through it).
pub(crate) async fn boot_with(
    files: &[(&str, String)],
    versions: Option<&str>,
    outbound: Option<kb_core::config::OutboundSection>,
) -> (tempfile::TempDir, std::net::SocketAddr, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    for (name, html) in files {
        std::fs::write(source.join(name), html).unwrap();
    }

    let daemon_name = format!(
        "test-asof-{}",
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
            outbound,
            atlas: None,
            templates: BTreeMap::new(),
            memory_scope: None,
            project_slugs: Vec::new(),
            default_search_category: None,
            code_url: None,
            decay_policy: None,
            versions: versions.map(str::to_string),
            reading_progress: None,
            search: Default::default(),
            indexable_extensions: None,
            reconcile_secs: None,
            capture_dir: None,
            resurface: None,
            slo: None,
            id_patterns: Vec::new(),
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

pub(crate) fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Boot, wait for every file to be indexed, return `(rel → id)`.
async fn boot_ids(
    files: &[(&str, String)],
    versions: Option<&str>,
) -> (
    tempfile::TempDir,
    std::net::SocketAddr,
    BTreeMap<String, String>,
    reqwest::Client,
) {
    let (tmp, addr, _src) = boot(files, versions).await;
    let client = reqwest::Client::new();
    let want = files.len();
    let ids = common::poll_until("all docs indexed", || async {
        let docs: Vec<Value> = client
            .get(url(addr, "/api/kb/smoke/docs?limit=50"))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        (docs.len() >= want).then(|| {
            docs.iter()
                .map(|d| {
                    (
                        d["source_relative"].as_str().unwrap().to_string(),
                        d["id"].as_str().unwrap().to_string(),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
    })
    .await;
    (tmp, addr, ids, client)
}

async fn as_of(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    query: &str,
) -> reqwest::Response {
    client
        .get(url(addr, &format!("/api/kb/smoke/docs/as-of?{query}")))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn asof_item_equals_per_artifact_memento() {
    let (_t, addr, ids, c) = boot_ids(&[("a.html", page("A"))], None).await;
    let id = &ids["a.html"];
    let at = now() + 60;
    let body: Value = as_of(&c, addr, &format!("at={at}&ids={id}"))
        .await
        .json()
        .await
        .unwrap();
    let per: Value = c
        .get(url(
            addr,
            &format!("/api/kb/smoke/artifacts/{id}/versions?at={at}"),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["items"][0]["state"], "hit");
    assert_eq!(body["items"][0]["memento"], per["memento"]);
    assert_eq!(body["at"], at);
    assert_eq!(body["mode"], per["mode"]);
    assert_eq!(body["summary"]["hit"], 1);
}

#[tokio::test]
async fn asof_below_floor_is_named_miss_with_oldest_ts() {
    let (_t, addr, ids, c) = boot_ids(&[("a.html", page("A"))], None).await;
    let id = &ids["a.html"];
    let body: Value = as_of(&c, addr, &format!("at=1000&ids={id}"))
        .await
        .json()
        .await
        .unwrap();
    let item = &body["items"][0];
    assert_eq!(item["state"], "miss");
    let m = &item["memento"];
    assert_eq!(m["found"], false);
    assert!(
        m.get("version").is_none(),
        "a miss never degrades to the oldest version"
    );
    assert!(m["oldest_ts_unix"].as_i64().unwrap() > 1000);
    assert!(m["note"].as_str().unwrap().contains("that old"));
    assert_eq!(body["summary"]["miss"], 1);
}

#[tokio::test]
async fn asof_unknown_id_state() {
    let (_t, addr, ids, c) = boot_ids(&[("a.html", page("A"))], None).await;
    let real = &ids["a.html"];
    let body: Value = as_of(
        &c,
        addr,
        &format!("at={}&ids=ffffffffffff,{real},not-an-id", now()),
    )
    .await
    .json()
    .await
    .unwrap();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["state"], "unknown");
    assert_eq!(items[1]["id"].as_str().unwrap(), real);
    assert_eq!(items[1]["state"], "hit");
    assert_eq!(items[2]["state"], "unknown");
    assert_eq!(body["summary"]["unknown"], 2);
}

#[tokio::test]
async fn asof_memory_session_unavailable_transcript() {
    let (_t, addr, ids, c) = boot_ids(&[("s.html", session_doc("S"))], None).await;
    let id = &ids["s.html"];
    let body: Value = as_of(&c, addr, &format!("at={}&ids={id}", now()))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["items"][0]["state"], "unavailable");
    assert_eq!(body["items"][0]["reason"], "transcript");
    assert!(body["items"][0].get("memento").is_none());
}

#[tokio::test]
async fn asof_mode_off_unavailable() {
    let (_t, addr, ids, c) = boot_ids(&[("a.html", page("A"))], Some("off")).await;
    let id = &ids["a.html"];
    let body: Value = as_of(&c, addr, &format!("at={}&ids={id}", now()))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["mode"], "off");
    assert_eq!(body["items"][0]["state"], "unavailable");
    assert_eq!(body["items"][0]["reason"], "mode_off");
}

/// Snapshot-only mode on a corpus with no git repo: the overlay still
/// answers from the index snapshots / working tree, never a 500.
#[tokio::test]
async fn asof_git_absent_no_500() {
    let (_t, addr, ids, c) = boot_ids(&[("a.html", page("A"))], Some("index")).await;
    let id = &ids["a.html"];
    let resp = as_of(&c, addr, &format!("at={}&ids={id}", now() + 60)).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["mode"], "index");
    assert_eq!(body["items"][0]["state"], "hit");
}

#[tokio::test]
async fn asof_bad_requests_are_400_problem_json() {
    let (_t, addr, ids, c) = boot_ids(&[("a.html", page("A"))], None).await;
    let id = &ids["a.html"];
    let over = (0..101)
        .map(|i| format!("{i:012x}"))
        .collect::<Vec<_>>()
        .join(",");
    for q in [
        format!("ids={id}"),              // missing at
        format!("at=yesterday&ids={id}"), // malformed at
        "at=1000".to_string(),            // missing ids
        "at=1000&ids=".to_string(),       // empty ids
        format!("at=1000&ids={over}"),    // over the cap, not truncated
    ] {
        let r = as_of(&c, addr, &q).await;
        assert_eq!(r.status(), 400, "{q}");
        let ct = r.headers()["content-type"].to_str().unwrap().to_string();
        assert!(ct.contains("problem+json"), "{q}: {ct}");
    }
    // Exactly at the cap is accepted.
    let at_cap = (0..100)
        .map(|i| format!("{i:012x}"))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        as_of(&c, addr, &format!("at=1000&ids={at_cap}"))
            .await
            .status(),
        200
    );
}

/// OL TIME2 dropped the future-`at` refusal: any instant is answered, and a
/// far-future one lands on the working tree (which wins at `t >= mtime`).
#[tokio::test]
async fn asof_far_future_at_is_a_working_tree_hit() {
    let (_t, addr, ids, c) = boot_ids(&[("a.html", page("A"))], None).await;
    let id = &ids["a.html"];
    let far = now() + 400 * 86_400;
    let resp = as_of(&c, addr, &format!("at={far}&ids={id}")).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["items"][0]["state"], "hit");
    assert_eq!(body["items"][0]["memento"]["version"]["source"], "working");
}

/// `/docs` is NOT an as-of surface: `at` is ignored and the bytes match.
#[tokio::test]
async fn docs_ignores_at_param_byte_identical() {
    let (_t, addr, _ids, c) = boot_ids(&[("a.html", page("A")), ("b.html", page("B"))], None).await;
    let plain = c
        .get(url(addr, "/api/kb/smoke/docs?limit=50"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let with_at = c
        .get(url(addr, "/api/kb/smoke/docs?limit=50&at=1000"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(plain, with_at);
}
