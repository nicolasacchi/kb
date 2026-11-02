//! RANK3 - score-weighted all-corpora fusion must apply to DEFAULT hybrid
//! search on a fleet that contains an empty keyword-only corpus.
//!
//! Before the fix `contributing` counted every arm whose `hits` was `Some`,
//! even an empty list, so one empty corpus without an embedder (arm: no vector
//! lane) made `fed_scores_comparable` false and the merge fell back to
//! rank-only: the top hit followed corpus-name order, not relevance.
//!
//! Own test binary on purpose: it mutates process-global env
//! (`KB_EMBEDDER_BIN`).
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

fn doc(title: &str, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>{title}</title></head><body><h1>{title}</h1><p>{body}</p></body></html>"#
    )
}

#[tokio::test]
async fn hybrid_scope_all_weights_by_score_despite_empty_keyword_corpus() {
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("fake-embedder.sh");
    std::fs::write(&script, FAKE_EMBEDDER).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("KB_EMBEDDER_BIN", &script);
    std::env::set_var("KB_FAKE_EMBED_FAIL", tmp.path().join("never-down.flag"));
    std::env::set_var("KB_FAKE_EMBED_SLOW", tmp.path().join("never-slow.flag"));

    // alpha sorts first by name and holds the WEAK match; beta holds the
    // strong one (title match -> title boost -> higher hybrid score).
    let alpha = tmp.path().join("alpha");
    let beta = tmp.path().join("beta");
    let empty = tmp.path().join("zempty");
    for p in [&alpha, &beta, &empty] {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(
        alpha.join("weak.html"),
        doc("Gardening diary", "one passing mention of zephyrine here"),
    )
    .unwrap();
    std::fs::write(
        beta.join("strong.html"),
        doc("Zephyrine guide", "zephyrine zephyrine zephyrine"),
    )
    .unwrap();

    let daemon_name = format!(
        "rank3-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    let section = |path: std::path::PathBuf, embeds: bool| KbSection {
        path,
        skip_patterns: Vec::new(),
        ui: UiSection::default(),
        embedding_model: embeds.then(|| "bge-small-en-v1.5".to_string()),
        reranker_model: None,
        chunked_embeddings: false,
        graph_boost: None,
        outbound: None,
        atlas: None,
        templates: BTreeMap::new(),
        memory_scope: None,
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
    kb_map.insert(KbName::new("alpha").unwrap(), section(alpha, true));
    kb_map.insert(KbName::new("beta").unwrap(), section(beta, true));
    // Empty AND embedder-less: its arm returns Some(vec![]) with no vector lane.
    kb_map.insert(KbName::new("zempty").unwrap(), section(empty, false));
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

    let body: serde_json::Value = reqwest::Client::new()
        .get(format!(
            "http://{addr}/api/search?q=zephyrine&scope=all&mode=hybrid&limit=10"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let kbs: Vec<String> = body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["kb"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(kbs.len(), 2, "{body}");
    assert_eq!(
        kbs[0], "beta",
        "hybrid scope=all must order by relevance, not corpus name: {body}"
    );
}
