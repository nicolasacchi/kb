//! FF-F — federated fan-out latency bench (ignored; run explicitly).
//!
//! Boots N independent per-kb corpora (each its own storage actor) and times
//! the scope=all / fleet endpoints. To get a before/after, compare the current
//! build (`FANOUT_CAP = 8`, parallel) against a rebuild with `FANOUT_CAP = 1`
//! (`buffered(1)` runs one corpus at a time, in submission order — the SAME
//! code path with concurrency removed, so the delta isolates the fan-out).
//!
//!   cargo test --profile fast -p kb-server --test fanout_bench -- --ignored --nocapture
//!
//! Loopback bind → invariant-4 auth bypass, so no token is needed.

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

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

async fn time_endpoint(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    path: &str,
    warm: usize,
    n: usize,
) -> (f64, f64, f64) {
    let url = format!("http://{addr}{path}");
    for _ in 0..warm {
        let _ = client
            .get(&url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
    }
    let mut samples: Vec<f64> = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        let _ = client
            .get(&url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = samples[samples.len() / 2];
    let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)];
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    (p50, p95, mean)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "latency bench; run explicitly with --ignored --nocapture"]
async fn federated_fanout_latency() {
    const N_CORPORA: usize = 16;
    const DOCS_PER: usize = 25;
    let tmp = tempfile::tempdir().unwrap();
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    for i in 0..N_CORPORA {
        let name = format!("kb{i:02}");
        let root = tmp.path().join(&name);
        std::fs::create_dir_all(&root).unwrap();
        for d in 0..DOCS_PER {
            std::fs::write(
                root.join(format!("doc-{d:03}.html")),
                format!(
                    "<!doctype html><html><head><title>{name} doc {d}</title></head>\
                     <body><h1>{name} note {d}</h1><p>federated fanout latency corpus \
                     benchmark widget {d}</p></body></html>"
                ),
            )
            .unwrap();
        }
        kb_map.insert(KbName::new(&name).unwrap(), kb_section(root));
    }
    let daemon_name = "fanout-bench".to_string();
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
    // Let the initial walk index all corpora.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let client = reqwest::Client::new();

    println!("FANOUT_BENCH  N_CORPORA={N_CORPORA} DOCS_PER={DOCS_PER}");
    for (label, path) in [
        ("/api/stats", "/api/stats"),
        ("/api/kbs", "/api/kbs"),
        ("/api/sessions", "/api/sessions?limit=20"),
        ("/api/lists", "/api/lists"),
        ("/api/notes", "/api/notes"),
        (
            "/api/search scope=all kw",
            "/api/search?q=widget&mode=keyword&scope=all&limit=20",
        ),
    ] {
        let (p50, p95, mean) = time_endpoint(&client, addr, path, 10, 60).await;
        println!("FANOUT_BENCH  {label:28} p50={p50:7.2}ms  p95={p95:7.2}ms  mean={mean:7.2}ms");
    }
}
