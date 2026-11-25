//! `GET /api/terms/{id}/ls` and `GET /api/terms/{id}/file` (FXLS) over the
//! REAL router with a FAKE scripted unix runner that speaks `fs_ls` and
//! `fs_get` (a header line, then exactly `len` raw bytes).
//!
//! The fake records every request, so "one request per chunk, offsets
//! 0/8Mi/16Mi", "the runner was never dialled" and "the relay did not pull
//! the whole file for a stalled reader" are observable assertions. The gate
//! matrix itself (every refusal row, runner never dialled) lives in
//! `terms_gate.rs`, which lists both routes. Every payload is synthetic
//! filler generated here.

mod common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, TerminalSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

const TERM_ID: &str = "abcd1234";
const MIB: usize = 1024 * 1024;
/// The caps spelled out on purpose: a change to either constant must fail
/// this file, not silently follow.
const FILE_CAP: usize = 32 * MIB;
const CHUNK: usize = 8 * MIB;

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

/// One scripted file.
#[derive(Clone, Default)]
struct FileSpec {
    data: Arc<Vec<u8>>,
    /// The `name` the runner puts in the header.
    name: String,
    mtime: i64,
    /// An `fs_get` at an offset >= this fails with `io`.
    fail_from_offset: Option<u64>,
    /// An `fs_get` at an offset >= this answers a different mtime.
    mtime_changes_from_offset: Option<u64>,
    /// Report this size in the header instead of the data length.
    size_override: Option<u64>,
}

#[derive(Default)]
struct Script {
    files: HashMap<String, FileSpec>,
    /// path -> runner error code, for either op.
    errs: HashMap<String, String>,
    /// The `fs_ls` data to answer.
    ls: Option<serde_json::Value>,
}

struct Fake {
    conns: Arc<AtomicUsize>,
    reqs: Arc<Mutex<Vec<serde_json::Value>>>,
    script: Arc<Mutex<Script>>,
    socket: std::path::PathBuf,
}

impl Fake {
    fn requests(&self, op: &str) -> Vec<serde_json::Value> {
        self.reqs
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["op"] == op)
            .cloned()
            .collect()
    }

    fn add_file(&self, path: &str, data: Vec<u8>) {
        self.script.lock().unwrap().files.insert(
            path.to_string(),
            FileSpec {
                name: path.rsplit('/').next().unwrap_or(path).to_string(),
                data: Arc::new(data),
                mtime: 1_700_000_000,
                ..Default::default()
            },
        );
    }
}

fn err_line(code: &str) -> String {
    format!("{{\"status\":\"err\",\"code\":\"{code}\",\"message\":\"scripted {code}\"}}\n")
}

fn spawn_fake(dir: &std::path::Path) -> Fake {
    let sock = dir.join("runner.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let reqs: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let script = Arc::new(Mutex::new(Script::default()));
    let (c, r, s) = (conns.clone(), reqs.clone(), script.clone());
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            c.fetch_add(1, Ordering::SeqCst);
            let (r, s) = (r.clone(), s.clone());
            tokio::spawn(async move {
                let mut reader = tokio::io::BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.is_err() {
                    return;
                }
                let parsed: serde_json::Value =
                    serde_json::from_str(line.trim()).unwrap_or_default();
                r.lock().unwrap().push(parsed.clone());
                let path = parsed["path"].as_str().unwrap_or_default().to_string();
                let op = parsed["op"].as_str().unwrap_or_default().to_string();
                // (header line, body bytes)
                let (head, body): (String, Vec<u8>) = {
                    let sc = s.lock().unwrap();
                    if let Some(code) = sc.errs.get(&path) {
                        (err_line(code), Vec::new())
                    } else if op == "fs_ls" {
                        match &sc.ls {
                            Some(d) => {
                                (format!("{{\"status\":\"ok\",\"data\":{d}}}\n"), Vec::new())
                            }
                            None => (err_line("not-found"), Vec::new()),
                        }
                    } else if let Some(f) = sc.files.get(&path) {
                        let offset = parsed["offset"].as_u64().unwrap_or(0);
                        let len = parsed["len"].as_u64().unwrap_or(0);
                        if f.fail_from_offset.is_some_and(|o| offset >= o) {
                            (err_line("io"), Vec::new())
                        } else {
                            let total = f.data.len() as u64;
                            let want = len.min(total.saturating_sub(offset)) as usize;
                            let start = (offset as usize).min(f.data.len());
                            let mtime = if f.mtime_changes_from_offset.is_some_and(|o| offset >= o)
                            {
                                f.mtime + 1
                            } else {
                                f.mtime
                            };
                            let name = serde_json::to_string(&f.name).unwrap();
                            (
                                format!(
                                    "{{\"status\":\"ok\",\"data\":{{\"size\":{},\"len\":{},\"mtime_unix\":{},\"name\":{}}}}}\n",
                                    f.size_override.unwrap_or(total),
                                    if f.size_override.is_some() { 0 } else { want },
                                    mtime,
                                    name
                                ),
                                if f.size_override.is_some() {
                                    Vec::new()
                                } else {
                                    f.data[start..start + want].to_vec()
                                },
                            )
                        }
                    } else {
                        (err_line("not-found"), Vec::new())
                    }
                };
                let mut stream = reader.into_inner();
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
            });
        }
    });
    Fake {
        conns,
        reqs,
        script,
        socket: sock,
    }
}

struct Stack {
    _tmp: tempfile::TempDir,
    addr: std::net::SocketAddr,
}

async fn boot(socket: &std::path::Path) -> Stack {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("seed.md"), "# seed\n\none doc\n").unwrap();
    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(KbName::new("smoke").unwrap(), kb_section(source));
    let terminal = TerminalSection {
        enabled: Some(true),
        runner_socket: Some(socket.to_path_buf()),
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
    Stack { _tmp: tmp, addr }
}

/// A browser-shaped same-origin fetch: the intent header, no Origin.
fn get(addr: std::net::SocketAddr, route: &str, query: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .get(format!("http://{addr}/api/terms/{TERM_ID}/{route}{query}"))
        .header("x-kb-intent", "terminal")
}

fn filler(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(31).wrapping_add(i >> 8)) as u8)
        .collect()
}

async fn problem_type(resp: reqwest::Response) -> (u16, String) {
    let status = resp.status().as_u16();
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    (status, v["type"].as_str().unwrap_or_default().to_string())
}

fn hdr(resp: &reqwest::Response, name: &str) -> String {
    resp.headers()
        .get(name)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

// ------------------------------------------------------------ download

#[tokio::test]
async fn a_download_is_always_an_attachment_octet_stream_even_for_html() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let page = b"<html><script>document.title='x'</script></html>".to_vec();
    fake.add_file("x.html", page.clone());
    let st = boot(&fake.socket).await;
    let resp = get(st.addr, "file", "?path=x.html").send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(hdr(&resp, "content-type"), "application/octet-stream");
    assert_eq!(
        hdr(&resp, "content-disposition"),
        "attachment; filename=\"x.html\"; filename*=UTF-8''x.html"
    );
    assert_eq!(hdr(&resp, "x-content-type-options"), "nosniff");
    assert_eq!(hdr(&resp, "cache-control"), "no-store");
    assert_eq!(hdr(&resp, "content-length"), page.len().to_string());
    assert_eq!(resp.bytes().await.unwrap().to_vec(), page);
    // One runner call: the first chunk, always asked at the 8 MiB cap.
    let calls = fake.requests("fs_get");
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0]["root"], "cwd");
    assert_eq!(calls[0]["path"], "x.html");
    assert_eq!(calls[0]["offset"], 0);
    assert_eq!(calls[0]["len"], CHUNK);
    assert_eq!(calls[0]["id"], TERM_ID);
}

#[tokio::test]
async fn the_same_headers_hold_for_a_sender_named_svg_and_an_up_root_file() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.add_file("0123456789ab-pic.svg", b"<svg onload=alert(1)/>".to_vec());
    let st = boot(&fake.socket).await;
    let resp = get(st.addr, "file", "?root=up&path=0123456789ab-pic.svg")
        .header("Origin", format!("http://{}", st.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(hdr(&resp, "content-type"), "application/octet-stream");
    assert!(hdr(&resp, "content-disposition").starts_with("attachment;"));
    assert_eq!(hdr(&resp, "x-content-type-options"), "nosniff");
    assert_eq!(hdr(&resp, "cache-control"), "no-store");
    assert_eq!(fake.requests("fs_get")[0]["root"], "up");
}

#[tokio::test]
async fn the_download_file_name_is_sanitised_whatever_the_runner_or_the_path_says() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake.socket).await;
    for (header_name, want) in [
        (
            "../a\"b.html",
            "attachment; filename=\"ab.html\"; filename*=UTF-8''ab.html",
        ),
        (
            "caffè.txt",
            "attachment; filename=\"caff_.txt\"; filename*=UTF-8''caff%C3%A8.txt",
        ),
        (
            "ctl\u{1}\u{7f}x\ny.txt",
            "attachment; filename=\"ctlxy.txt\"; filename*=UTF-8''ctlxy.txt",
        ),
        (
            "..",
            "attachment; filename=\"download\"; filename*=UTF-8''download",
        ),
    ] {
        fake.script.lock().unwrap().files.insert(
            "f.bin".to_string(),
            FileSpec {
                name: header_name.to_string(),
                data: Arc::new(b"abc".to_vec()),
                mtime: 5,
                ..Default::default()
            },
        );
        let resp = get(st.addr, "file", "?path=f.bin").send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 200, "{header_name:?}");
        assert_eq!(hdr(&resp, "content-disposition"), want, "{header_name:?}");
    }
    // An empty header name falls back to the final component of the path.
    fake.script.lock().unwrap().files.insert(
        "dir/g.bin".to_string(),
        FileSpec {
            name: String::new(),
            data: Arc::new(b"abc".to_vec()),
            mtime: 5,
            ..Default::default()
        },
    );
    let resp = get(st.addr, "file", "?path=dir%2Fg.bin")
        .send()
        .await
        .unwrap();
    assert_eq!(
        hdr(&resp, "content-disposition"),
        "attachment; filename=\"g.bin\"; filename*=UTF-8''g.bin"
    );
}

#[tokio::test]
async fn an_empty_file_is_an_empty_200_with_a_zero_length() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.add_file("empty", Vec::new());
    let st = boot(&fake.socket).await;
    let resp = get(st.addr, "file", "?path=empty").send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let declared = hdr(&resp, "content-length");
    assert!(declared == "0" || declared.is_empty(), "{declared:?}");
    assert!(resp.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_20_mib_file_streams_one_runner_request_per_chunk_and_arrives_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let data = filler(20 * MIB);
    fake.add_file("big.bin", data.clone());
    let st = boot(&fake.socket).await;
    let resp = get(st.addr, "file", "?path=big.bin").send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(hdr(&resp, "content-length"), (20 * MIB).to_string());
    let body = resp.bytes().await.unwrap();
    assert!(body.as_ref() == data.as_slice(), "byte-identical");
    let calls = fake.requests("fs_get");
    let seen: Vec<(u64, u64)> = calls
        .iter()
        .map(|c| (c["offset"].as_u64().unwrap(), c["len"].as_u64().unwrap()))
        .collect();
    assert_eq!(
        seen,
        vec![
            (0, CHUNK as u64),
            (CHUNK as u64, CHUNK as u64),
            (2 * CHUNK as u64, 4 * MIB as u64)
        ],
        "offsets 0/8Mi/16Mi, never a request above one chunk"
    );
}

#[tokio::test]
async fn a_file_of_exactly_the_cap_streams_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let data = filler(FILE_CAP);
    fake.add_file("cap.bin", data.clone());
    let st = boot(&fake.socket).await;
    let resp = get(st.addr, "file", "?path=cap.bin").send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len(), FILE_CAP);
    assert!(body.as_ref() == data.as_slice());
    assert_eq!(fake.requests("fs_get").len(), 4);
}

#[tokio::test]
async fn a_file_over_the_cap_is_413_before_any_body_even_from_a_lenient_runner() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.script.lock().unwrap().files.insert(
        "huge.bin".to_string(),
        FileSpec {
            name: "huge.bin".to_string(),
            data: Arc::new(Vec::new()),
            mtime: 1,
            size_override: Some(FILE_CAP as u64 + 1),
            ..Default::default()
        },
    );
    let st = boot(&fake.socket).await;
    let resp = get(st.addr, "file", "?path=huge.bin").send().await.unwrap();
    let (status, urn) = problem_type(resp).await;
    assert_eq!(
        (status, urn.as_str()),
        (413, "urn:kb:errors:payload-too-large")
    );
    assert_eq!(
        fake.requests("fs_get").len(),
        1,
        "no second chunk was asked for"
    );
}

#[tokio::test]
async fn a_stalled_reader_does_not_make_the_relay_pull_the_whole_file() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.add_file("cap.bin", filler(FILE_CAP));
    let st = boot(&fake.socket).await;
    let mut s = tokio::net::TcpStream::connect(st.addr).await.unwrap();
    let req = format!(
        "GET /api/terms/{TERM_ID}/file?path=cap.bin HTTP/1.1\r\nHost: {}\r\nx-kb-intent: terminal\r\nConnection: close\r\n\r\n",
        st.addr
    );
    s.write_all(req.as_bytes()).await.unwrap();
    // Read a sliver of the response and then stop reading altogether.
    let mut sliver = [0u8; 512];
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        s.read_exact(&mut sliver),
    )
    .await
    .expect("headers deadline")
    .unwrap();
    assert!(std::str::from_utf8(&sliver[..12])
        .unwrap()
        .starts_with("HTTP/1.1 200"));
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let pulled = fake.requests("fs_get").len();
    assert!(
        pulled < 4,
        "a 4-chunk file must not be pulled whole for a reader that stopped, saw {pulled} chunk requests"
    );
    drop(s);
}

#[tokio::test]
async fn a_chunk_that_fails_mid_stream_ends_the_body_short_of_its_content_length() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.add_file("big.bin", filler(20 * MIB));
    fake.script
        .lock()
        .unwrap()
        .files
        .get_mut("big.bin")
        .unwrap()
        .fail_from_offset = Some(CHUNK as u64);
    let st = boot(&fake.socket).await;
    let resp = get(st.addr, "file", "?path=big.bin").send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200, "the first chunk was fine");
    assert_eq!(hdr(&resp, "content-length"), (20 * MIB).to_string());
    let body = tokio::time::timeout(std::time::Duration::from_secs(20), resp.bytes())
        .await
        .expect("the broken transfer must end, not hang");
    assert!(
        body.is_err(),
        "a truncated body must be an error, not a short success"
    );
}

#[tokio::test]
async fn a_file_that_changes_mid_transfer_is_cut_not_spliced() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.add_file("big.bin", filler(20 * MIB));
    fake.script
        .lock()
        .unwrap()
        .files
        .get_mut("big.bin")
        .unwrap()
        .mtime_changes_from_offset = Some(CHUNK as u64);
    let st = boot(&fake.socket).await;
    let resp = get(st.addr, "file", "?path=big.bin").send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = tokio::time::timeout(std::time::Duration::from_secs(20), resp.bytes())
        .await
        .expect("the transfer must end");
    assert!(body.is_err());
}

// -------------------------------------------------------------- listing

#[tokio::test]
async fn ls_relays_the_runner_listing_and_forwards_root_and_path() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let listing = serde_json::json!({
        "root": "up", "dir": "sub",
        "entries": [
            {"name":"d","kind":"dir","bytes":0,"mtime_unix":1700000000,"hidden":false},
            {"name":".f","kind":"file","bytes":7,"mtime_unix":1700000100,"hidden":true}
        ],
        "truncated": true, "skipped": 3
    });
    fake.script.lock().unwrap().ls = Some(listing.clone());
    let st = boot(&fake.socket).await;
    let resp = get(st.addr, "ls", "?path=sub&root=up")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(hdr(&resp, "content-type"), "application/json");
    assert_eq!(hdr(&resp, "cache-control"), "no-store");
    let got: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(got, listing);
    let calls = fake.requests("fs_ls");
    assert_eq!(calls.len(), 1);
    assert_eq!(
        (
            calls[0]["root"].as_str(),
            calls[0]["path"].as_str(),
            calls[0]["id"].as_str()
        ),
        (Some("up"), Some("sub"), Some(TERM_ID))
    );
    // Defaults: root `cwd`, path the root itself.
    let resp = get(st.addr, "ls", "").send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let calls = fake.requests("fs_ls");
    assert_eq!(
        (calls[1]["root"].as_str(), calls[1]["path"].as_str()),
        (Some("cwd"), Some(""))
    );
}

#[tokio::test]
async fn ls_with_an_unreadable_runner_answer_is_a_502_not_a_forged_listing() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.script.lock().unwrap().ls = Some(serde_json::json!({"entries": "not-a-list"}));
    let st = boot(&fake.socket).await;
    let resp = get(st.addr, "ls", "").send().await.unwrap();
    let (status, urn) = problem_type(resp).await;
    assert_eq!(
        (status, urn.as_str()),
        (502, "urn:kb:errors:terminal-bad-reply")
    );
}

// --------------------------------------------------------------- errors

#[tokio::test]
async fn every_runner_refusal_maps_to_its_status_and_urn_on_both_routes() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake.socket).await;
    let table: [(&str, u16, &str); 8] = [
        ("not-found", 404, "urn:kb:errors:terminal-not-found"),
        ("not-a-file", 422, "urn:kb:errors:terminal-not-a-file"),
        ("too-large", 413, "urn:kb:errors:payload-too-large"),
        ("outside-root", 403, "urn:kb:errors:terminal-outside-root"),
        ("bad-request", 400, "urn:kb:errors:bad-request"),
        ("invalid-id", 400, "urn:kb:errors:bad-request"),
        ("io", 502, "urn:kb:errors:terminal-bad-reply"),
        ("internal", 502, "urn:kb:errors:terminal-bad-reply"),
    ];
    for (code, status, urn) in table {
        fake.script
            .lock()
            .unwrap()
            .errs
            .insert("p".to_string(), code.to_string());
        for route in ["ls", "file"] {
            let resp = get(st.addr, route, "?path=p").send().await.unwrap();
            assert_eq!(hdr(&resp, "cache-control"), "no-store", "{route} {code}");
            let (got_status, got_urn) = problem_type(resp).await;
            assert_eq!(
                (got_status, got_urn.as_str()),
                (status, urn),
                "{route} {code}"
            );
        }
    }
}

#[tokio::test]
async fn a_dead_runner_is_a_503_runner_unavailable_on_both_routes_without_hanging() {
    let tmp = tempfile::tempdir().unwrap();
    // Nothing listens: a socket path that does not exist.
    let st = boot(&tmp.path().join("no-such-runner.sock")).await;
    for route in ["ls", "file"] {
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            get(st.addr, route, "?path=a").send(),
        )
        .await
        .expect("must answer, not hang")
        .unwrap();
        let (status, urn) = problem_type(resp).await;
        assert_eq!(
            (status, urn.as_str()),
            (503, "urn:kb:errors:terminal-runner-unavailable"),
            "{route}"
        );
    }
}

#[tokio::test]
async fn a_runner_socket_removed_after_boot_is_a_503() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.add_file("a", b"x".to_vec());
    let st = boot(&fake.socket).await;
    let ok = get(st.addr, "file", "?path=a").send().await.unwrap();
    assert_eq!(ok.status().as_u16(), 200);
    std::fs::remove_file(&fake.socket).unwrap();
    let resp = get(st.addr, "file", "?path=a").send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 503);
}

// ----------------------------------------------------------- validation

#[tokio::test]
async fn malformed_requests_are_400_and_the_runner_is_never_dialled() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake.socket).await;
    let long = "a".repeat(4097);
    let exactly = "a".repeat(4096);
    let cases: Vec<(&str, String)> = vec![
        ("file", String::new()),
        ("file", "?path=".to_string()),
        ("file", "?root=cwd".to_string()),
        ("file", format!("?path={long}")),
        ("file", "?path=a%00b".to_string()),
        ("file", "?path=a&root=home".to_string()),
        ("file", "?path=a&root=".to_string()),
        ("file", "?path=a&root=CWD".to_string()),
        ("ls", format!("?path={long}")),
        ("ls", "?path=a%00b".to_string()),
        ("ls", "?root=/etc".to_string()),
        ("ls", "?root=up%00".to_string()),
    ];
    for (route, q) in &cases {
        let resp = get(st.addr, route, q).send().await.unwrap();
        let shown = &q[..q.len().min(40)];
        assert_eq!(resp.status().as_u16(), 400, "{route} {shown}");
        let (_s, urn) = problem_type(resp).await;
        assert_eq!(urn, "urn:kb:errors:bad-request", "{route} {shown}");
    }
    // A bad terminal id is a 400 too.
    let resp = reqwest::Client::new()
        .get(format!("http://{}/api/terms/NOT_VALID!/ls", st.addr))
        .header("x-kb-intent", "terminal")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    assert_eq!(
        fake.conns.load(Ordering::SeqCst),
        0,
        "the runner was never dialled"
    );
    // The longest legal path is NOT refused by the relay (the runner judges it).
    let resp = get(st.addr, "ls", &format!("?path={exactly}"))
        .send()
        .await
        .unwrap();
    assert_ne!(resp.status().as_u16(), 400);
    assert_eq!(fake.requests("fs_ls").len(), 1);
}

#[tokio::test]
async fn a_request_without_the_intent_header_never_reaches_the_runner() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.add_file("a", b"x".to_vec());
    let st = boot(&fake.socket).await;
    for route in ["ls", "file"] {
        let resp = reqwest::Client::new()
            .get(format!(
                "http://{}/api/terms/{TERM_ID}/{route}?path=a",
                st.addr
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status().as_u16(),
            403,
            "{route}: a bare <a href> is refused"
        );
    }
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}
