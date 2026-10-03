//! v0.44 I1 (A3-5) — a dead query embedder must be NAMED in `degraded[]`.
//!
//! The query embed used to be `if let Ok(..) = embed_query(..).await {..}`:
//! an Err was discarded and every vector arm silently fell back to BM25,
//! returning a 200 that still read as hybrid/semantic with `degraded` absent
//! (federated search, memory recall, the /api/context artifact match and
//! sessions recollect). These tests boot a real daemon whose
//! `kb-embedder` is a fake script that embeds documents fine and then — once
//! the flag file appears — answers every embed with an IPC error, and assert
//! the fallback is reported on both routes while the keyword hits survive.
//!
//! Own test binary on purpose: it mutates process-global env
//! (`KB_EMBEDDER_BIN`, `KB_FAKE_EMBED_FAIL`, `KB_FAKE_EMBED_SLOW`).
#![cfg(unix)]

mod common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

const FAKE_EMBEDDER: &str = r#"#!/bin/sh
vec=$(awk 'BEGIN{printf "["; for(i=0;i<384;i++){printf "%s%s", (i?",":""), (i==0?"1.0":"0.0")} printf "]"}')
printf '{"kind":"ready","model":"bge-small-en-v1.5","dim":384}\n'
while IFS= read -r line; do
  case "$line" in
    *'"kind":"shutdown"'*) exit 0 ;;
    *'"kind":"embed"'*)
      rid=$(printf '%s' "$line" | sed 's/.*"req_id":\([0-9][0-9]*\).*/\1/')
      if [ -e "$KB_FAKE_EMBED_SLOW" ]; then sleep 4; fi
      if [ -e "$KB_FAKE_EMBED_FAIL" ]; then
        printf '{"kind":"error","req_id":%s,"msg":"fake embedder down"}\n' "$rid"
      else
        printf '{"kind":"embed_ok","req_id":%s,"vectors":[%s]}\n' "$rid" "$vec"
      fi
      ;;
  esac
done
"#;

fn memory_html(title: &str, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>{title}</title><meta name="kb-category" content="memory-user"><meta name="kb-salience" content="0.5"></head><body><h1>{title}</h1><p>{body}</p></body></html>"#
    )
}

async fn get_json(addr: std::net::SocketAddr, path: &str) -> serde_json::Value {
    reqwest::Client::new()
        .get(format!("http://{addr}{path}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn has_lane(body: &serde_json::Value, lane: &str, class: &str) -> bool {
    body["degraded"].as_array().is_some_and(|a| {
        a.iter()
            .any(|d| d["lane"] == lane && d["error_class"] == class)
    })
}

/// Any degraded entry of `class`. The timeout half asserts the CLASS, not the
/// lane: once the embed has eaten the whole `deadline_ms`, the per-corpus
/// arms behind it have zero budget left and are themselves named timeout.
fn has_class(body: &serde_json::Value, class: &str) -> bool {
    body["degraded"]
        .as_array()
        .is_some_and(|a| a.iter().any(|d| d["error_class"] == class))
}

#[tokio::test]
async fn dead_query_embedder_is_named_in_degraded_for_search_and_recall() {
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("fake-embedder.sh");
    std::fs::write(&script, FAKE_EMBEDDER).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let flag = tmp.path().join("embedder-down.flag");
    std::env::set_var("KB_EMBEDDER_BIN", &script);
    std::env::set_var("KB_FAKE_EMBED_FAIL", &flag);
    let slow_flag = tmp.path().join("embedder-slow.flag");
    std::env::set_var("KB_FAKE_EMBED_SLOW", &slow_flag);

    let mem = tmp.path().join("mem");
    std::fs::create_dir_all(&mem).unwrap();
    std::fs::write(
        mem.join("one.html"),
        memory_html(
            "Marmot Trail",
            "the marmot prefers a zigzag trail through the reeds",
        ),
    )
    .unwrap();
    std::fs::write(
        mem.join("two.html"),
        memory_html(
            "Reed Notes",
            "reeds grow along the zigzag trail by the river",
        ),
    )
    .unwrap();

    // A plain (non-memory) corpus: /api/context's artifact match skips
    // memory-scoped kbs, and recollect fans out over every kb.
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(
        docs.join("plain.html"),
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Otter Ledger</title></head><body><h1>Otter Ledger</h1><p>otters keep a ledger of river stones</p></body></html>"#,
    )
    .unwrap();

    let daemon_name = format!(
        "embdeg-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    let section = |path: std::path::PathBuf, memory_scope: Option<&str>| KbSection {
        path,
        skip_patterns: Vec::new(),
        ui: UiSection::default(),
        embedding_model: Some("bge-small-en-v1.5".into()),
        reranker_model: None,
        chunked_embeddings: false,
        graph_boost: None,
        outbound: None,
        atlas: None,
        templates: BTreeMap::new(),
        memory_scope: memory_scope.map(str::to_string),
        project_slugs: Vec::new(),
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
        id_patterns: Vec::new(),
    };
    kb_map.insert(KbName::new("mem").unwrap(), section(mem, Some("global")));
    kb_map.insert(KbName::new("docs").unwrap(), section(docs, None));
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
    common::wait_docs_listed(addr, "mem", 2).await;
    common::wait_docs_listed(addr, "docs", 1).await;

    // Control: with a healthy embedder neither route reports a degraded lane.
    let ok_search = get_json(addr, "/api/search?q=zigzag&scope=all&mode=hybrid").await;
    assert!(
        ok_search["hits"].as_array().is_some_and(|h| !h.is_empty()),
        "healthy hybrid search must return hits: {ok_search}"
    );
    assert!(
        !has_lane(&ok_search, "search.vector", "embed"),
        "a healthy embedder must not be reported degraded: {ok_search}"
    );
    let ok_recall = get_json(addr, "/api/memory/recall?q=zigzag&scope=all").await;
    assert!(
        !has_lane(&ok_recall, "recall.vector", "embed"),
        "a healthy embedder must not be reported degraded: {ok_recall}"
    );

    // The embedder dies. Fresh query strings so the query-embed cache cannot
    // answer them.
    std::fs::write(&flag, b"down").unwrap();

    let search = get_json(addr, "/api/search?q=river&scope=all&mode=hybrid").await;
    assert!(
        search["hits"].as_array().is_some_and(|h| !h.is_empty()),
        "keyword fallback must still return the BM25 hits: {search}"
    );
    assert!(
        has_lane(&search, "search.vector", "embed"),
        "federated hybrid search with a dead embedder must name the lane in degraded[]: {search}"
    );

    let recall = get_json(addr, "/api/memory/recall?q=marmot&scope=all").await;
    assert!(
        recall["hits"].as_array().is_some_and(|h| !h.is_empty()),
        "keyword fallback must still return the BM25 memories: {recall}"
    );
    assert!(
        has_lane(&recall, "recall.vector", "embed"),
        "recall with a dead embedder must name the lane in degraded[]: {recall}"
    );

    // The same dead embedder on the other two vector lanes that used to
    // discard the Err (`if let Ok`): the /api/context artifact match and
    // sessions recollect.
    let ctx = get_json(addr, "/api/context?q=ledger%20stones").await;
    assert!(
        has_lane(&ctx, "artifacts.vector", "embed"),
        "/api/context with a dead embedder must name artifacts.vector: {ctx}"
    );
    let rec = get_json(addr, "/api/sessions/recollect?q=otter%20ledgers").await;
    assert!(
        has_lane(&rec, "recollect.vector", "embed"),
        "recollect with a dead embedder must name recollect.vector: {rec}"
    );

    // The TIMEOUT half: the embedder is alive but slow (4 s per embed) and the
    // caller's budget is 200 ms. Each lane must say `timeout`, not `embed`,
    // and not stay silent. Fresh queries again so the embed cache cannot help.
    std::fs::remove_file(&flag).unwrap();
    std::fs::write(&slow_flag, b"slow").unwrap();
    let started = std::time::Instant::now();
    let search = get_json(
        addr,
        "/api/search?q=sedge&scope=all&mode=hybrid&deadline_ms=200",
    )
    .await;
    assert!(
        has_class(&search, "timeout"),
        "a slow embedder past deadline_ms must be named timeout on search: {search}"
    );
    let recall = get_json(addr, "/api/memory/recall?q=heron&scope=all&deadline_ms=200").await;
    assert!(
        has_class(&recall, "timeout"),
        "a slow embedder past deadline_ms must be named timeout on recall: {recall}"
    );
    let rec = get_json(addr, "/api/sessions/recollect?q=badger&deadline_ms=200").await;
    assert!(
        has_class(&rec, "timeout"),
        "a slow embedder past deadline_ms must be named timeout on recollect: {rec}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(8),
        "the 200 ms budget must bound the request, not the 4 s embedder"
    );
}
