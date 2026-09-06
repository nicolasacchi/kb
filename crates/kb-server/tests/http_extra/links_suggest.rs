//! CT-F3 — `GET /api/kb/{kb}/links/suggest` + `POST /api/kb/{kb}/links/apply`.
//! Boots the in-process daemon (the `coderefs.rs` convention) over a three
//! file corpus and drives the whole unlinked-mention loop end to end:
//!
//! 1. a Markdown source mentions another artifact's title with no edge →
//!    an APPLICABLE queue row,
//! 2. an HTML artifact mentioning the same title → a row that is REPORTED
//!    with the honest invariant #29 note and never rewritten,
//! 3. apply splices a real `[[wikilink]]` into the `.md` on disk, and
//! 4. the same apply run twice is a loud 404 (the mention is now a link),
//!    while applying from the HTML source is a loud 400.
//!
//! The engine's grammar (floor, code-fence skip, ambiguity, self-skip) is
//! pinned by `kb_core::mentions`' unit tests; this file pins the WIRE and
//! the on-disk effect.
use crate::common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// The mentioning source: prose names the checklist, no `[[…]]` anywhere.
const RUNBOOK_MD: &str = "# Ops runbook\n\nRead the Deployment checklist before shipping.\n";

/// The mentioned artifact — its `# H1` is the title the scan matches.
const DEPLOY_MD: &str = "# Deployment checklist\n\nOne step per line.\n";

/// An HTML artifact mentioning the same title: reported, never applicable
/// (the render path keeps `[[…]]` literal — invariant #29).
const RESEARCH_HTML: &str = "<!doctype html><html><head><title>Research on shipping</title>\
     </head><body><h1>Research on shipping</h1>\
     <p>Compare with the Deployment checklist.</p></body></html>";

async fn boot(files: &[(&str, &str)]) -> (tempfile::TempDir, std::net::SocketAddr, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    for (name, body) in files {
        std::fs::write(source.join(name), body).unwrap();
    }

    let daemon_name = format!(
        "test-links-{}",
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

async fn docs(client: &reqwest::Client, addr: std::net::SocketAddr) -> Vec<serde_json::Value> {
    client
        .get(url(addr, "/api/kb/smoke/docs?limit=50"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap_or_default()
}

/// Wait until all `n` fixtures are indexed, then map `source_relative` → id.
async fn wait_for_ids(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    n: usize,
) -> std::collections::HashMap<String, String> {
    common::poll_until(&format!("{n} docs indexed"), || async {
        let d = docs(client, addr).await;
        (d.len() >= n).then(|| {
            d.into_iter()
                .filter_map(|v| {
                    Some((
                        v["source_relative"].as_str()?.to_string(),
                        v["id"].as_str()?.to_string(),
                    ))
                })
                .collect::<std::collections::HashMap<_, _>>()
        })
    })
    .await
}

async fn suggestions(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
) -> Vec<serde_json::Value> {
    let body: serde_json::Value = client
        .get(url(addr, "/api/kb/smoke/links/suggest"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body["suggestions"].as_array().cloned().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn suggest_reports_unlinked_mentions_and_apply_authors_the_wikilink() {
    let (_tmp, addr, source) = boot(&[
        ("runbook.md", RUNBOOK_MD),
        ("deploy.md", DEPLOY_MD),
        ("research.html", RESEARCH_HTML),
    ])
    .await;
    let client = reqwest::Client::new();
    let ids = wait_for_ids(&client, addr, 3).await;
    let runbook = ids["runbook.md"].clone();
    let deploy = ids["deploy.md"].clone();
    let research = ids["research.html"].clone();

    // The queue is derived from the indexed corpus, so poll until the
    // fixtures' rows (and their edges) have settled.
    let rows = common::poll_until("two unlinked-mention rows", || async {
        let rows = suggestions(&client, addr).await;
        (rows.len() >= 2).then_some(rows)
    })
    .await;

    let md_row = rows
        .iter()
        .find(|s| s["src"]["id"] == runbook.as_str())
        .unwrap_or_else(|| panic!("no row for runbook.md: {rows:#?}"));
    assert_eq!(md_row["dst"]["id"], deploy.as_str());
    assert_eq!(md_row["matched"], "Deployment checklist");
    assert_eq!(md_row["match_kind"], "title");
    assert_eq!(md_row["target"], "Deployment checklist");
    assert_eq!(md_row["applicable"], true);
    assert!(md_row["note"].is_null(), "applicable rows carry no excuse");

    let html_row = rows
        .iter()
        .find(|s| s["src"]["id"] == research.as_str())
        .unwrap_or_else(|| panic!("no row for research.html: {rows:#?}"));
    assert_eq!(html_row["applicable"], false);
    let note = html_row["note"].as_str().unwrap_or_default();
    assert!(
        note.contains("#29"),
        "the refusal must name the ruling: {note:?}"
    );

    // Applying from the HTML source is refused loudly, with the tree
    // untouched.
    let before = std::fs::read_to_string(source.join("research.html")).unwrap();
    let resp = client
        .post(url(addr, "/api/kb/smoke/links/apply"))
        .header("X-Requested-By", "kb-cli")
        .json(&serde_json::json!({ "src": research, "dst": deploy }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let problem: serde_json::Value = resp.json().await.unwrap();
    assert!(
        problem["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("#29"),
        "{problem:#?}"
    );
    assert_eq!(
        std::fs::read_to_string(source.join("research.html")).unwrap(),
        before,
        "a refused apply must not touch the file"
    );

    // The Markdown source IS rewritten — with a real wikilink.
    let resp = client
        .post(url(addr, "/api/kb/smoke/links/apply"))
        .header("X-Requested-By", "kb-cli")
        .json(&serde_json::json!({ "src": runbook, "dst": deploy }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let applied: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(applied["wikilink"], "[[Deployment checklist]]");
    assert_eq!(applied["source_relative"], "runbook.md");
    let on_disk = std::fs::read_to_string(source.join("runbook.md")).unwrap();
    assert_eq!(
        on_disk,
        "# Ops runbook\n\nRead the [[Deployment checklist]] before shipping.\n"
    );

    // Idempotence is honest, not silent: the mention is now a link, so the
    // second attempt is a 404 rather than a second splice.
    let resp = client
        .post(url(addr, "/api/kb/smoke/links/apply"))
        .header("X-Requested-By", "kb-cli")
        .json(&serde_json::json!({ "src": runbook, "dst": deploy }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(
        std::fs::read_to_string(source.join("runbook.md")).unwrap(),
        on_disk,
        "a 404 apply must not touch the file"
    );

    // …and the queue drops the row it just resolved.
    let rows = suggestions(&client, addr).await;
    assert!(
        !rows.iter().any(|s| s["src"]["id"] == runbook.as_str()),
        "the linked pair is no longer an unlinked mention: {rows:#?}"
    );
}
