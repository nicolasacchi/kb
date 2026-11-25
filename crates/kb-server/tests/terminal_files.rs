//! `POST /api/terms/{id}/files` (FXUP) over the REAL router with a FAKE
//! stateful unix runner that speaks the chunked upload ops, plus the
//! artifact-origin refusal of stored `.kbblob` originals.
//!
//! The fake records every op (and per-append body sizes), reassembles the
//! appended bytes and answers a scripted `upload_commit`, so "the relay
//! never held more than one chunk", "abort was sent" and "the runner was
//! never dialled" are observable assertions. Every payload is generated
//! here (synthetic filler).

mod common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, TerminalFilesSection, TerminalSection,
    UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use sha2::Digest;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

const TERM_ID: &str = "abcd1234";
const MIB: usize = 1024 * 1024;
/// The caps spelled out here on purpose: a change to either constant must
/// fail this file, not silently follow.
const FILE_CAP: usize = 32 * MIB;
const CHUNK: usize = 8 * MIB;
const UPLOAD_TOKEN: &str = "0123456789abcdef0123456789abcdef";

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

#[derive(Clone, Debug)]
struct Ev {
    op: String,
    line: serde_json::Value,
    body_len: usize,
}

#[derive(Default)]
struct Script {
    data: Vec<u8>,
    declared_mime: String,
    name: String,
    /// `begin` answers `err` with this runner code.
    begin_err: Option<String>,
    /// The nth (0-based) `append` answers `err`.
    fail_append_at: Option<usize>,
    appends: usize,
    /// The `kind` the commit reports.
    kind: String,
}

struct Fake {
    conns: Arc<AtomicUsize>,
    events: Arc<Mutex<Vec<Ev>>>,
    script: Arc<Mutex<Script>>,
    socket: std::path::PathBuf,
}

impl Fake {
    fn ops(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.op.clone())
            .collect()
    }

    fn appends(&self) -> Vec<(u64, u64)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.op == "upload_append")
            .map(|e| {
                (
                    e.line["offset"].as_u64().unwrap(),
                    e.line["len"].as_u64().unwrap(),
                )
            })
            .collect()
    }

    fn has_op(&self, op: &str) -> bool {
        self.ops().iter().any(|o| o == op)
    }
}

fn spawn_fake(dir: &std::path::Path) -> Fake {
    let sock = dir.join("runner.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let events: Arc<Mutex<Vec<Ev>>> = Arc::new(Mutex::new(Vec::new()));
    let script = Arc::new(Mutex::new(Script {
        kind: "pdf".to_string(),
        ..Default::default()
    }));
    let (c, e, s) = (conns.clone(), events.clone(), script.clone());
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            c.fetch_add(1, Ordering::SeqCst);
            let (e, s) = (e.clone(), s.clone());
            tokio::spawn(async move {
                let mut reader = tokio::io::BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.is_err() {
                    return;
                }
                let parsed: serde_json::Value =
                    serde_json::from_str(line.trim()).unwrap_or_default();
                let op = parsed["op"].as_str().unwrap_or_default().to_string();
                let mut body = Vec::new();
                if op == "upload_append" {
                    let len = parsed["len"].as_u64().unwrap_or(0) as usize;
                    body = vec![0u8; len];
                    let read = tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        reader.read_exact(&mut body),
                    )
                    .await;
                    if !matches!(read, Ok(Ok(_))) {
                        return;
                    }
                }
                e.lock().unwrap().push(Ev {
                    op: op.clone(),
                    line: parsed.clone(),
                    body_len: body.len(),
                });
                let reply = {
                    let mut sc = s.lock().unwrap();
                    match op.as_str() {
                        "upload_begin" => {
                            sc.data.clear();
                            sc.appends = 0;
                            sc.name = parsed["name"].as_str().unwrap_or_default().to_string();
                            sc.declared_mime =
                                parsed["mime"].as_str().unwrap_or_default().to_string();
                            if let Some(code) = sc.begin_err.clone() {
                                format!(
                                    "{{\"status\":\"err\",\"code\":\"{code}\",\"message\":\"\"}}"
                                )
                            } else {
                                format!(
                                    "{{\"status\":\"ok\",\"data\":{{\"upload\":\"{UPLOAD_TOKEN}\",\"chunk_max\":{CHUNK}}}}}"
                                )
                            }
                        }
                        "upload_append" => {
                            let n = sc.appends;
                            sc.appends += 1;
                            if sc.fail_append_at == Some(n) {
                                "{\"status\":\"err\",\"code\":\"io-error\",\"message\":\"disk\"}"
                                    .to_string()
                            } else {
                                assert_eq!(
                                    parsed["offset"].as_u64().unwrap() as usize,
                                    sc.data.len(),
                                    "append offset must equal the bytes received so far"
                                );
                                sc.data.extend_from_slice(&body);
                                format!(
                                    "{{\"status\":\"ok\",\"data\":{{\"received\":{}}}}}",
                                    sc.data.len()
                                )
                            }
                        }
                        "upload_commit" => {
                            let sha = hex::encode(sha2::Sha256::digest(&sc.data));
                            format!(
                                "{{\"status\":\"ok\",\"data\":{{\"path\":\"/run/s/{TERM_ID}/up/{}-f.bin\",\"bytes\":{},\"mime\":\"{}\",\"kind\":\"{}\",\"sha256\":\"{sha}\",\"name\":\"{}-f.bin\"}}}}",
                                &sha[..12],
                                sc.data.len(),
                                sc.declared_mime,
                                sc.kind,
                                &sha[..12]
                            )
                        }
                        _ => "{\"status\":\"ok\",\"data\":{}}".to_string(),
                    }
                };
                let mut stream = reader.into_inner();
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.write_all(b"\n").await;
            });
        }
    });
    Fake {
        conns,
        events,
        script,
        socket: sock,
    }
}

struct Stack {
    _tmp: tempfile::TempDir,
    addr: std::net::SocketAddr,
    /// The files corpus source root, when one is configured.
    files_root: Option<std::path::PathBuf>,
}

async fn boot(fake: &Fake, files: bool, prep: impl FnOnce(&std::path::Path)) -> Stack {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("seed.md"), "# seed\n\none doc\n").unwrap();
    let files_root = tmp.path().join("files-corpus");
    std::fs::create_dir_all(&files_root).unwrap();
    std::fs::write(files_root.join("seed.md"), "# files seed\n\nx\n").unwrap();
    prep(&files_root);
    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(KbName::new("smoke").unwrap(), kb_section(source));
    kb_map.insert(
        KbName::new("files").unwrap(),
        kb_section(files_root.clone()),
    );
    let terminal = TerminalSection {
        enabled: Some(true),
        runner_socket: Some(fake.socket.clone()),
        files: files.then(|| TerminalFilesSection {
            kb: Some("files".to_string()),
        }),
        ..Default::default()
    };
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
        terminal,
        ..Default::default()
    };
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    common::wait_http_up(addr).await;
    Stack {
        _tmp: tmp,
        addr,
        files_root: files.then_some(files_root),
    }
}

fn files_req(addr: std::net::SocketAddr, query: &str, body: Vec<u8>) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .post(format!("http://{addr}/api/terms/{TERM_ID}/files{query}"))
        .header("Origin", format!("http://{addr}"))
        .header("x-kb-intent", "terminal")
        .body(body)
}

/// Synthetic filler that is not a repeat of one byte (so a mis-ordered
/// chunk cannot reassemble identically).
fn filler(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(31).wrapping_add(i >> 8)) as u8)
        .collect()
}

/// One raw HTTP/1.1 POST over a TCP socket: the caller controls the headers
/// and sends fewer or more body bytes than it declares.
async fn raw_post(
    addr: std::net::SocketAddr,
    query: &str,
    headers: &[String],
    body: &[u8],
) -> String {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut head = format!(
        "POST /api/terms/{TERM_ID}/files{query} HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\nx-kb-intent: terminal\r\nConnection: close\r\n"
    );
    for h in headers {
        head.push_str(h);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).await.unwrap();
    let _ = s.write_all(body).await;
    let _ = s.shutdown().await;
    let mut out = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), s.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

async fn wait_for_op(fake: &Fake, op: &str) {
    for _ in 0..100 {
        if fake.has_op(op) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("runner never saw `{op}`; saw {:?}", fake.ops());
}

// ---------------------------------------------------------------- relay

#[tokio::test]
async fn a_20_mib_pdf_streams_as_8_8_4_chunks_and_reassembles_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake, false, |_| {}).await;
    let data = filler(20 * MIB);
    let resp = files_req(st.addr, "?name=Report%20Q3.pdf", data.clone())
        .header("Content-Type", "application/pdf")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/json"
    );
    assert_eq!(
        resp.headers().get("x-content-type-options").unwrap(),
        "nosniff"
    );
    assert_eq!(resp.headers().get("cache-control").unwrap(), "no-store");
    let v: serde_json::Value = resp.json().await.unwrap();
    let ops = fake.ops();
    assert_eq!(ops.first().map(String::as_str), Some("upload_begin"));
    assert_eq!(ops.last().map(String::as_str), Some("upload_commit"));
    assert_eq!(
        fake.appends(),
        vec![
            (0, CHUNK as u64),
            (CHUNK as u64, CHUNK as u64),
            (2 * CHUNK as u64, 4 * MIB as u64)
        ]
    );
    {
        let ev = fake.events.lock().unwrap();
        assert_eq!(ev[0].line["size"].as_u64().unwrap(), 20 * MIB as u64);
        assert_eq!(ev[0].line["name"], "Report Q3.pdf");
        assert_eq!(ev[0].line["mime"], "application/pdf");
        let max = ev.iter().map(|e| e.body_len).max().unwrap();
        assert!(max <= CHUNK, "no relay body above one chunk, saw {max}");
    }
    assert_eq!(fake.script.lock().unwrap().data, data, "byte-identical");
    assert_eq!(v["bytes"].as_u64().unwrap(), 20 * MIB as u64);
    assert_eq!(v["kind"], "pdf");
    assert!(
        v["mime"].is_null(),
        "the declared mime is never echoed: {v}"
    );
    assert!(v["path"]
        .as_str()
        .unwrap()
        .starts_with("/run/s/abcd1234/up/"));
    assert_eq!(v["kb"]["status"], "skipped");
    assert_eq!(v["kb"]["reason"], "not-configured");
}

#[tokio::test]
async fn an_image_answers_the_runner_sniffed_mime_and_never_the_declared_one() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.script.lock().unwrap().kind = "image".to_string();
    let st = boot(&fake, false, |_| {}).await;
    // The fake echoes the declared mime as the commit mime, standing in for
    // the runner's sniffed one on an image.
    let resp = files_req(st.addr, "?name=a.png", filler(64))
        .header("Content-Type", "image/png")
        .send()
        .await
        .unwrap();
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["mime"], "image/png");
    // A hostile declared type on a non-image is never echoed.
    let tmp2 = tempfile::tempdir().unwrap();
    let fake2 = spawn_fake(tmp2.path());
    let st2 = boot(&fake2, false, |_| {}).await;
    let resp = files_req(st2.addr, "?name=a.bin", filler(64))
        .header("Content-Type", "text/html")
        .send()
        .await
        .unwrap();
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(v["mime"].is_null(), "{v}");
}

#[tokio::test]
async fn size_boundaries_cap_and_cap_plus_one() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake, false, |_| {}).await;

    // Exactly 32 MiB is accepted (four full chunks).
    let resp = files_req(st.addr, "?name=big.bin", vec![7u8; FILE_CAP])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(fake.appends().len(), 4);
    let before = fake.conns.load(Ordering::SeqCst);

    // 32 MiB + 1: 413 before any body byte is read, runner not dialled.
    let out = raw_post(
        st.addr,
        "?name=big.bin",
        &[format!("Content-Length: {}", FILE_CAP + 1)],
        b"",
    )
    .await;
    assert!(out.starts_with("HTTP/1.1 413"), "{out}");
    assert!(out.contains("payload-too-large"), "{out}");
    assert_eq!(fake.conns.load(Ordering::SeqCst), before);

    // Zero bytes and a missing Content-Length are 400.
    let resp = files_req(st.addr, "?name=z.bin", Vec::new())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let out = raw_post(
        st.addr,
        "?name=z.bin",
        &["Transfer-Encoding: chunked".to_string()],
        b"5\r\nhello\r\n0\r\n\r\n",
    )
    .await;
    assert!(out.starts_with("HTTP/1.1 400"), "{out}");
    assert!(out.contains("length required"), "{out}");
    assert_eq!(fake.conns.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn one_chunk_versus_two_at_the_8_mib_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake, false, |_| {}).await;
    let resp = files_req(st.addr, "?name=a.bin", filler(CHUNK))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(fake.appends(), vec![(0, CHUNK as u64)]);
    fake.events.lock().unwrap().clear();
    *fake.script.lock().unwrap() = Script {
        kind: "pdf".to_string(),
        ..Default::default()
    };
    let resp = files_req(st.addr, "?name=a.bin", filler(CHUNK + 1))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(fake.appends(), vec![(0, CHUNK as u64), (CHUNK as u64, 1)]);
}

#[tokio::test]
async fn query_validation_is_a_400_and_never_dials_the_runner() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake, false, |_| {}).await;
    let long = "x".repeat(256);
    for q in [
        "".to_string(),
        "?name=".to_string(),
        format!("?name={long}"),
        "?name=a%00b".to_string(),
        "?name=a.bin&index=2".to_string(),
    ] {
        let resp = files_req(st.addr, &q, filler(16)).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 400, "{q}");
    }
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

// -------------------------------------------------------------- failures

#[tokio::test]
async fn the_runner_up_quota_refusal_is_a_413_with_its_own_urn() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.script.lock().unwrap().begin_err = Some("too-large".to_string());
    let st = boot(&fake, false, |_| {}).await;
    let resp = files_req(st.addr, "?name=a.bin", filler(1024))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 413);
    let text = resp.text().await.unwrap();
    assert!(
        text.contains("urn:kb:errors:terminal-upload-quota"),
        "{text}"
    );
    assert!(!fake.has_op("upload_append"));
}

#[tokio::test]
async fn a_runner_failure_mid_append_aborts_the_upload() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.script.lock().unwrap().fail_append_at = Some(1);
    let st = boot(&fake, false, |_| {}).await;
    let resp = files_req(st.addr, "?name=a.bin", filler(20 * MIB))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 502);
    assert!(fake.has_op("upload_abort"), "{:?}", fake.ops());
    assert!(!fake.has_op("upload_commit"));
}

#[tokio::test]
async fn a_short_body_aborts_and_stores_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake, true, |_| {}).await;
    let out = raw_post(
        st.addr,
        "?name=a.bin",
        &["Content-Length: 100000".to_string()],
        &filler(10),
    )
    .await;
    // The connection may be torn down before a response is written; when one
    // arrives it is a 400.
    if !out.is_empty() {
        assert!(out.starts_with("HTTP/1.1 400"), "{out}");
    }
    wait_for_op(&fake, "upload_abort").await;
    assert!(!fake.has_op("upload_commit"));
    let root = st.files_root.unwrap();
    let mut blobs = 0;
    for e in walk(&root) {
        if e.extension().and_then(|x| x.to_str()) == Some("kbblob") {
            blobs += 1;
        }
    }
    assert_eq!(blobs, 0, "nothing stored in kb");
}

#[tokio::test]
async fn a_runner_that_is_down_answers_503_without_hanging() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake, false, |_| {}).await;
    std::fs::remove_file(&fake.socket).unwrap();
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        files_req(st.addr, "?name=a.bin", filler(1024)).send(),
    )
    .await
    .expect("must not hang")
    .unwrap();
    assert_eq!(resp.status().as_u16(), 503);
    let text = resp.text().await.unwrap();
    assert!(text.contains("runner-unavailable"), "{text}");
}

// ----------------------------------------------------------------- kb keep

fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[tokio::test]
async fn a_configured_files_kb_receives_the_blob_and_a_metadata_only_stub() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake, true, |_| {}).await;
    let data = filler(300_000);
    let resp = files_req(st.addr, "?name=Report%20Q3.pdf", data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["kb"]["status"], "stored", "{v}");
    assert_eq!(v["kb"]["created"], true);
    let root = st.files_root.unwrap();
    let stub_rel = v["kb"]["stub_rel"].as_str().unwrap();
    let id = v["kb"]["id"].as_str().unwrap();
    assert!(!id.is_empty());
    // `<yyyy>/<mm>/<term id>/<stub>.html`
    let parts: Vec<&str> = stub_rel.split('/').collect();
    assert_eq!(parts.len(), 4, "{stub_rel}");
    assert_eq!(parts[2], TERM_ID);
    let stub = std::fs::read_to_string(root.join(stub_rel)).unwrap();
    assert!(!stub.contains("<pre id=\"content\">"), "metadata only");
    let blob = walk(&root.join(parts[0]).join(parts[1]).join(parts[2]))
        .into_iter()
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("kbblob"))
        .expect("a .kbblob beside the stub");
    assert_eq!(std::fs::read(blob).unwrap(), data);
}

#[tokio::test]
async fn index_one_inlines_text_escaped_and_index_zero_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake, true, |_| {}).await;
    let root = st.files_root.clone().unwrap();
    let text = b"<b>hi</b> & notes\n".to_vec();
    for (q, inlined) in [
        ("?name=notes.txt&index=1", true),
        ("?name=other.txt", false),
    ] {
        *fake.script.lock().unwrap() = Script {
            kind: "text".to_string(),
            ..Default::default()
        };
        let mut body = text.clone();
        if !inlined {
            body.extend_from_slice(b"different\n");
        }
        let resp = files_req(st.addr, q, body)
            .header("Content-Type", "text/plain")
            .send()
            .await
            .unwrap();
        let v: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(v["kb"]["status"], "stored", "{v}");
        assert_eq!(v["kb"]["inlined"], inlined, "{v}");
        let stub =
            std::fs::read_to_string(root.join(v["kb"]["stub_rel"].as_str().unwrap())).unwrap();
        assert_eq!(stub.contains("<pre id=\"content\">"), inlined);
        if inlined {
            assert!(stub.contains("&lt;b&gt;hi&lt;/b&gt; &amp; notes"), "{stub}");
            assert!(!stub.contains("<b>hi</b>"), "escaped, never raw: {stub}");
        }
    }
}

#[tokio::test]
async fn a_full_files_corpus_skips_with_quota_and_the_upload_still_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake, true, |root| {
        // A sparse 2 GiB original fills the corpus quota without disk use.
        let f = std::fs::File::create(root.join("filler.kbblob")).unwrap();
        f.set_len(kb_core::filestore::FILES_QUOTA_BYTES).unwrap();
    })
    .await;
    let resp = files_req(st.addr, "?name=a.bin", filler(4096))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["kb"]["status"], "skipped", "{v}");
    assert_eq!(v["kb"]["reason"], "quota");
    assert!(v["path"].as_str().unwrap().contains("/up/"));
}

#[tokio::test]
async fn an_unwritable_files_corpus_fails_io_and_the_upload_still_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake, true, |root| {
        // A regular file where the `<yyyy>` directory must go.
        let year = chrono::Utc::now().format("%Y").to_string();
        std::fs::write(root.join(year), b"in the way").unwrap();
    })
    .await;
    let resp = files_req(st.addr, "?name=a.bin", filler(4096))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["kb"]["status"], "failed", "{v}");
    assert_eq!(v["kb"]["reason"], "io");
    assert!(v["path"].as_str().unwrap().contains("/up/"));
}

// --------------------------------------------- artifact origin refusal

#[tokio::test]
async fn a_kbblob_is_never_served_on_an_artifact_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake, true, |root| {
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(
            root.join("a/x.html"),
            "<!doctype html><html><head><title>x</title></head><body><h1>x page</h1></body></html>",
        )
        .unwrap();
        std::fs::write(root.join("a/x.css"), "body{}").unwrap();
        std::fs::write(root.join("a/x.pdf.kbblob"), b"%PDF-fake").unwrap();
        std::fs::write(root.join("c.kbblob"), b"root blob").unwrap();
        std::fs::write(root.join("a/b/Y.KBBLOB"), b"upper blob").unwrap();
    })
    .await;
    common::wait_docs_listed(st.addr, "files", 2).await;
    let docs: Vec<serde_json::Value> =
        reqwest::get(format!("http://{}/api/kb/files/docs?limit=20", st.addr))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let x = docs
        .iter()
        .find(|d| {
            d["source_relative"].as_str() == Some("a/x.html")
                || d["path"].as_str().is_some_and(|p| p.ends_with("a/x.html"))
        })
        .unwrap_or_else(|| panic!("a/x.html not listed: {docs:?}"));
    let id = x["id"].as_str().unwrap().to_string();
    let c = reqwest::Client::new();
    let get = |path: &'static str| {
        let c = c.clone();
        let id = id.clone();
        let addr = st.addr;
        async move {
            c.get(format!("http://{addr}{path}"))
                .header("Host", format!("files--{id}.artifacts.localhost"))
                .send()
                .await
                .unwrap()
        }
    };
    // Control: an ordinary sibling asset is still served.
    assert_eq!(get("/x.css").await.status().as_u16(), 200);
    // The sibling blob, the walk-up blob, and the upper-case extension.
    for p in ["/x.pdf.kbblob", "/c.kbblob", "/b/Y.KBBLOB"] {
        let resp = get(p).await;
        assert_eq!(resp.status().as_u16(), 404, "{p}");
        let text = resp.text().await.unwrap();
        assert!(!text.contains("PDF") && !text.contains("blob"), "{text}");
    }
    // A missing file answers the SAME 404 body (no existence oracle).
    let missing = get("/nope.kbblob").await;
    assert_eq!(missing.status().as_u16(), 404);
    assert_eq!(missing.text().await.unwrap(), "artifact not found");
}
