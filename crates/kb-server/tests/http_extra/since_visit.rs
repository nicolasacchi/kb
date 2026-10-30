//! OL IDEA2 — `GET /api/kb/{kb}/artifacts/{id}/since-visit`. Boots the
//! in-process daemon (the `docs_asof.rs` convention) and drives it via
//! `reqwest`. Mtimes are forced with `File::set_modified`, never sleeps.
use crate::common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn doc(status: &str, risks: &str) -> String {
    format!(
        "<!doctype html><html><head><title>Plan</title></head><body>\
         <h1>Plan</h1><h2>Status</h2><p>{status}</p>\
         <h2>Risks</h2><p>{risks}</p><h2>Appendix</h2><p>tail</p></body></html>"
    )
}

fn session_doc() -> String {
    "<!doctype html><html><head><title>S</title>\
     <meta name=\"kb-category\" content=\"memory-session\"></head>\
     <body><pre>hi</pre></body></html>"
        .to_string()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Force a file's mtime to `now + offset` seconds.
fn set_mtime(path: &Path, offset: i64) {
    let t = std::time::UNIX_EPOCH + Duration::from_secs((now() + offset) as u64);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

async fn boot(
    files: &[(&str, String)],
    versions: Option<&str>,
    mtime_offset: Option<i64>,
) -> (tempfile::TempDir, std::net::SocketAddr, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    for (name, html) in files {
        std::fs::write(source.join(name), html).unwrap();
        if let Some(off) = mtime_offset {
            set_mtime(&source.join(name), off);
        }
    }

    let daemon_name = format!(
        "test-sv-{}",
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

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

struct Rig {
    _tmp: tempfile::TempDir,
    addr: std::net::SocketAddr,
    source: PathBuf,
    ids: BTreeMap<String, String>,
    c: reqwest::Client,
}

async fn rig(files: &[(&str, String)], versions: Option<&str>, mtime_offset: Option<i64>) -> Rig {
    let (tmp, addr, source) = boot(files, versions, mtime_offset).await;
    let c = reqwest::Client::new();
    let want = files.len();
    let ids = common::poll_until("all docs indexed", || async {
        let docs: Vec<Value> = c
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
    Rig {
        _tmp: tmp,
        addr,
        source,
        ids,
        c,
    }
}

impl Rig {
    async fn sv(&self, name: &str) -> Value {
        self.get(&format!(
            "/api/kb/smoke/artifacts/{}/since-visit",
            self.ids[name]
        ))
        .await
    }
    async fn get(&self, path: &str) -> Value {
        self.c
            .get(url(self.addr, path))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn open(&self, name: &str, source: &str) {
        let r = self
            .c
            .post(url(self.addr, "/api/kb/smoke/history/open"))
            .json(&json!({"artifact_id": self.ids[name], "source": source}))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "open: {}", r.status());
    }
    /// Rewrite a file and force its mtime to `now + offset`.
    fn rewrite(&self, name: &str, html: &str, offset: i64) {
        let p = self.source.join(name);
        std::fs::write(&p, html).unwrap();
        set_mtime(&p, offset);
    }
    /// Age every recorded visit by `secs` (the route has no clock input, so
    /// a FINISHED visit is made by rewriting the ledger directly).
    fn backdate(&self, secs: i64) {
        let daemon = format!(
            "test-sv-{}",
            self._tmp.path().file_name().unwrap().to_string_lossy()
        );
        let db =
            KbPaths::rooted_at(self._tmp.path(), daemon).kb_sqlite(&KbName::new("smoke").unwrap());
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.busy_timeout(Duration::from_secs(10)).unwrap();
        conn.execute(
            "UPDATE history SET started_at = started_at - ?1, updated_at = updated_at - ?1
             WHERE kind = 'open'",
            [secs],
        )
        .unwrap();
    }
    async fn history_len(&self) -> usize {
        let v = self.get("/api/kb/smoke/history?limit=100").await;
        match v.as_array() {
            Some(a) => a.len(),
            None => v
                .as_object()
                .and_then(|o| o.values().find_map(|x| x.as_array().map(Vec::len)))
                .unwrap_or(0),
        }
    }
}

#[tokio::test]
async fn since_visit_bad_or_unknown_id_404() {
    let r = rig(&[("p.html", doc("ok", "low"))], None, None).await;
    for path in [
        "/api/kb/smoke/artifacts/not-hex/since-visit".to_string(),
        "/api/kb/smoke/artifacts/ffffffffffff/since-visit".to_string(),
        format!("/api/kb/nope/artifacts/{}/since-visit", r.ids["p.html"]),
    ] {
        let resp = r.c.get(url(r.addr, &path)).send().await.unwrap();
        assert_eq!(resp.status(), 404, "{path}");
        let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
        assert!(ct.contains("problem+json"), "{path}: {ct}");
    }
}

#[tokio::test]
async fn since_visit_no_visit_without_a_web_open() {
    let r = rig(&[("p.html", doc("ok", "low"))], None, None).await;
    let v = r.sv("p.html").await;
    assert_eq!(v["state"], "no_visit");
    assert!(v.get("sections").is_none());
}

// invariant:19 an agent read is not the operator's visit
#[tokio::test]
async fn since_visit_ignores_cli_opens() {
    let r = rig(&[("p.html", doc("ok", "low"))], None, None).await;
    r.open("p.html", "cli").await;
    r.rewrite("p.html", &doc("ok", "high"), 120);
    let v = r.sv("p.html").await;
    assert_eq!(v["state"], "no_visit");
}

// A live visit that has not seen a change is not a baseline yet.
#[tokio::test]
async fn since_visit_live_unmodified_visit_is_no_visit() {
    let r = rig(&[("p.html", doc("ok", "low"))], None, None).await;
    r.open("p.html", "web").await;
    assert_eq!(r.sv("p.html").await["state"], "no_visit");
}

#[tokio::test]
async fn since_visit_unchanged_when_not_modified() {
    // File older than the (backdated, finished) visit: the cheap path.
    let r = rig(&[("p.html", doc("ok", "low"))], None, Some(-10_000)).await;
    r.open("p.html", "web").await;
    r.backdate(7200);
    let v = r.sv("p.html").await;
    assert_eq!(v["state"], "unchanged", "{v}");
    assert_eq!(v["baseline"], "previous_visit");
    assert!(v.get("modified").is_none());
    assert!(
        v.get("memento").is_none(),
        "no façade work on the cheap path"
    );
}

#[tokio::test]
async fn since_visit_finished_visit_is_previous_baseline() {
    let r = rig(&[("p.html", doc("ok", "low"))], None, Some(-10_000)).await;
    r.open("p.html", "web").await;
    r.backdate(7200);
    r.rewrite("p.html", &doc("ok", "much higher"), -100);
    let v = common::poll_until("since-visit reports the change", || async {
        let v = r.sv("p.html").await;
        (v["state"] == "changed").then_some(v)
    })
    .await;
    assert_eq!(v["baseline"], "previous_visit");
    assert_eq!(v["sections"][0]["id"], "kb-h-risks");
    assert_eq!(v["memento"]["version"]["source"], "index");
}

// invariant:14 the changed sections come from the façade's nearest-prior pick
#[tokio::test]
async fn since_visit_changed_names_the_section() {
    let r = rig(&[("p.html", doc("ok", "low"))], None, None).await;
    r.open("p.html", "web").await;
    r.rewrite("p.html", &doc("ok", "much higher"), 120);
    let v = common::poll_until("since-visit reports the change", || async {
        let v = r.sv("p.html").await;
        (v["state"] == "changed").then_some(v)
    })
    .await;
    assert_eq!(v["baseline"], "current_visit");
    let secs = v["sections"].as_array().unwrap();
    assert_eq!(secs.len(), 1, "{v}");
    assert_eq!(secs[0]["id"], "kb-h-risks");
    assert_eq!(secs[0]["title"], "Risks");
    assert_eq!(secs[0]["level"], 2);
    assert_eq!(secs[0]["change"], "changed");
    assert_eq!(v["memento"]["found"], true);
    assert_eq!(v["memento"]["version"]["source"], "index");
    assert_eq!(v["added_lines"], 1);
    assert_eq!(v["removed_lines"], 1);
}

// invariant:14 since-visit's memento IS the versions?at= memento
#[tokio::test]
async fn since_visit_memento_equals_versions_at_memento() {
    let r = rig(&[("p.html", doc("ok", "low"))], None, None).await;
    r.open("p.html", "web").await;
    r.rewrite("p.html", &doc("ok", "much higher"), 120);
    let v = common::poll_until("since-visit reports the change", || async {
        let v = r.sv("p.html").await;
        (v["state"] == "changed").then_some(v)
    })
    .await;
    let at = v["last_visit_unix"].as_i64().unwrap();
    let per = r
        .get(&format!(
            "/api/kb/smoke/artifacts/{}/versions?at={at}",
            r.ids["p.html"]
        ))
        .await;
    assert_eq!(v["memento"], per["memento"]);
}

#[tokio::test]
async fn since_visit_markup_only_change_is_unchanged_modified() {
    let r = rig(&[("p.html", doc("ok", "low"))], None, None).await;
    r.open("p.html", "web").await;
    r.rewrite(
        "p.html",
        &doc("ok", "low").replace("<p>low</p>", "<p><b>low</b></p>"),
        120,
    );
    let v = common::poll_until("since-visit sees the mtime move", || async {
        let v = r.sv("p.html").await;
        (v["modified"] == true).then_some(v)
    })
    .await;
    assert_eq!(v["state"], "unchanged");
    assert!(v.get("sections").is_none());
}

#[tokio::test]
async fn since_visit_below_floor_is_changed_no_delta_naming_oldest() {
    // mtime in the future BEFORE boot: the only snapshot is newer than the visit.
    let r = rig(&[("p.html", doc("ok", "low"))], None, Some(3600)).await;
    r.open("p.html", "web").await;
    let v = r.sv("p.html").await;
    assert_eq!(v["state"], "changed_no_delta", "{v}");
    assert_eq!(v["reason"], "no_version");
    assert_eq!(v["memento"]["found"], false);
    assert!(v["memento"]["oldest_ts_unix"].as_i64().unwrap() > now() + 3000);
    assert!(v["memento"].get("version").is_none());
}

#[tokio::test]
async fn since_visit_mode_off_is_changed_no_delta() {
    let r = rig(&[("p.html", doc("ok", "low"))], Some("off"), None).await;
    r.open("p.html", "web").await;
    set_mtime(&r.source.join("p.html"), 120);
    let v = r.sv("p.html").await;
    assert_eq!(v["state"], "changed_no_delta");
    assert_eq!(v["reason"], "mode_off");
    assert_eq!(v["mode"], "off");
}

#[tokio::test]
async fn since_visit_transcript_unavailable() {
    let r = rig(&[("s.html", session_doc())], None, None).await;
    let v = r.sv("s.html").await;
    assert_eq!(v["state"], "unavailable");
    assert_eq!(v["reason"], "transcript");
}

// invariant:8 the route is read-only
#[tokio::test]
async fn since_visit_writes_no_history_row() {
    let r = rig(&[("p.html", doc("ok", "low"))], None, None).await;
    r.open("p.html", "web").await;
    let before = r.history_len().await;
    for _ in 0..3 {
        r.sv("p.html").await;
    }
    assert_eq!(r.history_len().await, before);
}
