//! kbt2 HUBGUARD — the launch guard rails over the REAL router with a
//! STATEFUL fake unix runner: `list` reports the rows, `launch` adds one
//! after a delay (widening the check/spawn race the launch lock closes).

mod common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, TerminalSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tracing_subscriber::fmt::MakeWriter;

const FIXTURE_TOKEN: &str = "test-token-fixture-deadbeef";
const DEADLINE: Duration = Duration::from_secs(10);

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
        project_slugs: Vec::new(),
        default_search_category: None,
        code_url: None,
        decay_policy: None,
        versions: None,
        git_dir: None,
        git_prefix: None,
        reading_progress: None,
        search: Default::default(),
        indexable_extensions: None,
        reconcile_secs: None,
        capture_dir: None,
        resurface: None,
        slo: None,
        id_patterns: Vec::new(),
    }
}

#[derive(Clone, Copy)]
enum ListMode {
    Rows,
    Err,
    Malformed,
}

struct Fake {
    socket: std::path::PathBuf,
    conns: Arc<AtomicUsize>,
    rows: Arc<Mutex<Vec<serde_json::Value>>>,
    ops: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn launches(&self) -> usize {
        self.ops
            .lock()
            .unwrap()
            .iter()
            .filter(|o| *o == "launch")
            .count()
    }
}

fn row(id: &str, dead: bool) -> serde_json::Value {
    serde_json::json!({
        "id": id, "harness": "omp", "cwd": "/work/proj", "pane_pid": 4242,
        "cols": 120, "rows": 40, "pane_dead": dead,
    })
}

fn spawn_fake(dir: &std::path::Path, initial: Vec<serde_json::Value>, mode: ListMode) -> Fake {
    let socket = dir.join("runner.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let rows = Arc::new(Mutex::new(initial));
    let ops: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (c2, r2, o2) = (conns.clone(), rows.clone(), ops.clone());
    tokio::spawn(async move {
        let seq = Arc::new(AtomicUsize::new(0));
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            c2.fetch_add(1, Ordering::SeqCst);
            let (rows, ops, seq) = (r2.clone(), o2.clone(), seq.clone());
            tokio::spawn(async move {
                let mut reader = tokio::io::BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.is_err() {
                    return;
                }
                let req: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
                let op = req["op"].as_str().unwrap_or_default().to_string();
                ops.lock().unwrap().push(op.clone());
                let reply = match op.as_str() {
                    "list" => match mode {
                        ListMode::Rows => serde_json::json!({
                            "status": "ok", "data": rows.lock().unwrap().clone()
                        }),
                        ListMode::Err => serde_json::json!({
                            "status": "err", "code": "internal", "message": "boom"
                        }),
                        ListMode::Malformed => serde_json::json!({
                            "status": "ok", "data": "not-a-list"
                        }),
                    },
                    "launch" => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        let n = seq.fetch_add(1, Ordering::SeqCst);
                        let id = format!("new{n:05}");
                        rows.lock().unwrap().push(row(&id, false));
                        serde_json::json!({"status": "ok", "data": {"id": id}})
                    }
                    _ => serde_json::json!({
                        "status": "err", "code": "not-found", "message": "no script"
                    }),
                };
                let mut stream = reader.into_inner();
                let _ = stream.write_all(reply.to_string().as_bytes()).await;
                let _ = stream.write_all(b"\n").await;
            });
        }
    });
    Fake {
        socket,
        conns,
        rows,
        ops,
    }
}

async fn boot(
    socket: std::path::PathBuf,
    max_terms: Option<u64>,
    with_token: bool,
) -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("seed.md"), "# seed\n\none doc\n").unwrap();
    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(KbName::new("smoke").unwrap(), kb_section(source));
    let cfg = KbConfig {
        daemon: DaemonSection {
            name: Some(daemon_name.clone()),
        },
        ui: UiSection::default(),
        kb: kb_map,
        defaults: DefaultsSection {
            embedding_model: None,
            disable_embedder_fallback: true,
        },
        terminal: TerminalSection {
            enabled: Some(true),
            runner_socket: Some(socket),
            max_terms,
            ..Default::default()
        },
        ..Default::default()
    };
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    if with_token {
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.token_file(), FIXTURE_TOKEN).unwrap();
    }
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    common::wait_http_up(addr).await;
    (tmp, addr)
}

async fn launch(addr: std::net::SocketAddr, body: &str) -> (u16, serde_json::Value) {
    let resp = tokio::time::timeout(
        DEADLINE,
        reqwest::Client::new()
            .post(format!("http://{addr}/api/terms"))
            .header("Origin", format!("http://{addr}"))
            .header("x-kb-intent", "terminal")
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send(),
    )
    .await
    .expect("launch deadline")
    .unwrap();
    let status = resp.status().as_u16();
    let v = resp.json().await.unwrap_or_default();
    (status, v)
}

fn body(cwd: &str) -> String {
    format!("{{\"harness\":\"omp\",\"cwd\":\"{cwd}\"}}")
}

// ----------------------------------------------------- captured tracing

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// One process-wide capture (the server's tasks run on other threads, so a
/// thread-local default would miss them). Tests tell their lines apart by
/// a unique cwd basename.
fn capture() -> &'static Capture {
    static CAP: OnceLock<Capture> = OnceLock::new();
    CAP.get_or_init(|| {
        let cap = Capture::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(cap.clone())
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let _ = tracing::subscriber::set_global_default(sub);
        cap
    })
}

fn audit_lines(marker: &str) -> Vec<String> {
    let text = String::from_utf8_lossy(&capture().0.lock().unwrap()).to_string();
    text.lines()
        .filter(|l| l.contains("launch_audit") && l.contains(marker))
        .map(String::from)
        .collect()
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn cap_refuses_with_typed_429() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(
        tmp.path(),
        vec![row("aaaa0001", false), row("aaaa0002", false)],
        ListMode::Rows,
    );
    let (_t, addr) = boot(fake.socket.clone(), Some(2), false).await;
    let (status, v) = launch(addr, &body("/work/proj")).await;
    assert_eq!(status, 429, "{v}");
    assert_eq!(v["type"], "urn:kb:errors:terminal-cap", "{v}");
    assert!(v["detail"].as_str().unwrap().contains("2 of 2"), "{v}");
    assert_eq!(
        fake.launches(),
        0,
        "a refused launch never reaches the runner"
    );
}

#[tokio::test]
async fn dead_panes_do_not_count() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(
        tmp.path(),
        vec![row("aaaa0001", false), row("aaaa0002", true)],
        ListMode::Rows,
    );
    let (_t, addr) = boot(fake.socket.clone(), Some(2), false).await;
    let (status, v) = launch(addr, &body("/work/proj")).await;
    assert_eq!(status, 201, "{v}");
    assert_eq!(fake.launches(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_launches_at_cap_minus_one_admit_exactly_one() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(
        tmp.path(),
        vec![row("aaaa0001", false), row("aaaa0002", false)],
        ListMode::Rows,
    );
    let (_t, addr) = boot(fake.socket.clone(), Some(3), false).await;
    let b = body("/work/proj");
    let ((s1, v1), (s2, v2)) = tokio::join!(launch(addr, &b), launch(addr, &b));
    let mut statuses = [s1, s2];
    statuses.sort();
    assert_eq!(statuses, [201, 429], "{v1} / {v2}");
    assert_eq!(fake.launches(), 1, "exactly one launch reached the runner");
    assert_eq!(fake.rows.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn max_terms_zero_refuses_every_launch() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), vec![], ListMode::Rows);
    let (_t, addr) = boot(fake.socket.clone(), Some(0), false).await;
    for _ in 0..2 {
        let (status, v) = launch(addr, &body("/work/proj")).await;
        assert_eq!(status, 429, "{v}");
        assert_eq!(v["type"], "urn:kb:errors:terminal-cap", "{v}");
    }
    assert_eq!(fake.launches(), 0);
    // Body validation still comes first.
    let (status, _) = launch(addr, "{\"harness\":\"emacs\"}").await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn runner_list_error_fails_closed() {
    // Err reply -> the existing runner-error mapping (internal -> 502).
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), vec![], ListMode::Err);
    let (_t, addr) = boot(fake.socket.clone(), None, false).await;
    let (status, v) = launch(addr, &body("/work/proj")).await;
    assert_eq!(status, 502, "{v}");
    assert_eq!(fake.launches(), 0);

    // Unparseable data -> 502 bad reply.
    let tmp2 = tempfile::tempdir().unwrap();
    let fake2 = spawn_fake(tmp2.path(), vec![], ListMode::Malformed);
    let (_t2, addr2) = boot(fake2.socket.clone(), None, false).await;
    let (status, v) = launch(addr2, &body("/work/proj")).await;
    assert_eq!(status, 502, "{v}");
    assert_eq!(v["type"], "urn:kb:errors:terminal-bad-reply", "{v}");
    assert_eq!(fake2.launches(), 0);

    // Runner down -> 503.
    let tmp3 = tempfile::tempdir().unwrap();
    let (_t3, addr3) = boot(tmp3.path().join("absent.sock"), None, false).await;
    let (status, v) = launch(addr3, &body("/work/proj")).await;
    assert_eq!(status, 503, "{v}");
    assert_eq!(
        v["type"], "urn:kb:errors:terminal-runner-unavailable",
        "{v}"
    );
}

#[tokio::test]
async fn audit_line_has_no_prompt_or_full_path() {
    let _ = capture();
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), vec![], ListMode::Rows);
    let (_t, addr) = boot(fake.socket.clone(), Some(1), false).await;

    // Accepted: resume sid present, handoff excluded with resume, so use
    // two launches: one with a resume sid, one refused at the cap.
    let (status, _) = launch(
        addr,
        "{\"harness\":\"omp\",\"cwd\":\"/secret/parent/auditproj\",\"resume_sid\":\"sid-audit-1\"}",
    )
    .await;
    assert_eq!(status, 201);
    let lines = audit_lines("auditproj");
    assert_eq!(lines.len(), 1, "{lines:?}");
    let l = &lines[0];
    assert!(l.contains("terminal launch attempt"), "{l}");
    assert!(l.contains("harness=omp"), "{l}");
    assert!(l.contains("cwd_base=\"auditproj\""), "{l}");
    assert!(l.contains("resume_sid=\"sid-audit-1\""), "{l}");
    assert!(l.contains("identity="), "{l}");
    assert!(l.contains("outcome=accepted"), "{l}");
    assert!(!l.contains("/secret") && !l.contains("parent"), "{l}");

    // Refused at the cap (1 live): one more line, handoff token absent.
    let (status, _) = launch(
        addr,
        "{\"harness\":\"claude\",\"cwd\":\"/secret/parent/capproj\",\"handoff_from\":\"handoff-secret-77\"}",
    )
    .await;
    assert_eq!(status, 429);
    let lines = audit_lines("capproj");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("outcome=refused-cap"), "{}", lines[0]);
    assert!(lines[0].contains("harness=claude"), "{}", lines[0]);
    assert!(!lines[0].contains("handoff-secret-77"), "{}", lines[0]);
    assert!(!lines[0].contains("/secret"), "{}", lines[0]);

    // Bad request after the gate: exactly one line, unparsed harness is `-`.
    let (status, _) = launch(
        addr,
        "{\"harness\":\"emacs\",\"cwd\":\"/secret/parent/badproj\"}",
    )
    .await;
    assert_eq!(status, 400);
    let lines = audit_lines("badproj");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("outcome=bad-request"), "{}", lines[0]);
    assert!(lines[0].contains("harness=-"), "{}", lines[0]);
    assert!(!lines[0].contains("emacs"), "{}", lines[0]);
    for l in capture_all() {
        assert!(!l.contains("handoff-secret-77"), "{l}");
    }
}

fn capture_all() -> Vec<String> {
    audit_lines("")
}

#[tokio::test]
async fn gate_unchanged_for_launch() {
    let _ = capture();
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), vec![], ListMode::Rows);
    let (_t, addr) = boot(fake.socket.clone(), None, true).await;
    let c = reqwest::Client::new();
    let url = format!("http://{addr}/api/terms");
    let b = body("/secret/gatecwd");
    let post = || {
        c.post(&url)
            .header("Content-Type", "application/json")
            .body(b.clone())
    };
    let cases: Vec<(&str, reqwest::RequestBuilder, &str)> = vec![
        (
            "no Origin",
            post().header("x-kb-intent", "terminal"),
            "terminal-origin-refused",
        ),
        (
            "wrong Origin",
            post()
                .header("Origin", "https://evil.example")
                .header("x-kb-intent", "terminal"),
            // A foreign Origin may be refused by the daemon-wide origin
            // layer before the terminal gate; either way it is a 403.
            "origin-refused",
        ),
        (
            "no intent",
            post().header("Origin", format!("http://{addr}")),
            "terminal-intent-required",
        ),
        (
            "token caller",
            post()
                .header("Origin", format!("http://{addr}"))
                .header("x-kb-intent", "terminal")
                .header("Authorization", format!("Bearer {FIXTURE_TOKEN}")),
            "terminal-token-refused",
        ),
    ];
    for (name, req, urn) in cases {
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{name}");
        let text = resp.text().await.unwrap();
        assert!(text.contains(urn), "{name}: {text}");
    }
    assert_eq!(
        fake.conns.load(Ordering::SeqCst),
        0,
        "gate never reaches the runner"
    );
    assert!(
        audit_lines("gatecwd").is_empty(),
        "a gate refusal logs no launch line"
    );
}
