//! `POST /api/terms/{id}/files/keep` and `GET /api/terms/{id}/kept/{artifact_id}`
//! (FXSAVE) over the REAL router with a FAKE scripted unix runner that
//! speaks `fs_get` (a header line, then exactly `len` raw bytes) and a real
//! tempdir files corpus.
//!
//! The fake records every request, so "offsets 0/8Mi/16Mi", "the runner was
//! never dialled" and "the spool is empty afterwards" are observable. The
//! gate matrix rows live in `terms_gate.rs`. Every payload is synthetic.

mod common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, TerminalFilesSection, TerminalSection,
    UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use sha2::Digest;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

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
    /// An `fs_get` at an offset >= this never answers.
    stall_from_offset: Option<u64>,
}

#[derive(Default)]
struct Script {
    files: HashMap<String, FileSpec>,
    /// path -> runner error code.
    errs: HashMap<String, String>,
}

struct Fake {
    conns: Arc<AtomicUsize>,
    reqs: Arc<Mutex<Vec<serde_json::Value>>>,
    script: Arc<Mutex<Script>>,
    socket: std::path::PathBuf,
}

impl Fake {
    fn fs_gets(&self, path: &str) -> Vec<u64> {
        self.reqs
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["op"] == "fs_get" && r["path"] == path)
            .map(|r| r["offset"].as_u64().unwrap())
            .collect()
    }

    fn add_file(&self, path: &str, data: Vec<u8>) {
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        self.add_named(path, &name, data);
    }

    fn add_named(&self, path: &str, name: &str, data: Vec<u8>) {
        self.script.lock().unwrap().files.insert(
            path.to_string(),
            FileSpec {
                name: name.to_string(),
                data: Arc::new(data),
                mtime: 1_700_000_000,
                ..Default::default()
            },
        );
    }

    fn edit(&self, path: &str, f: impl FnOnce(&mut FileSpec)) {
        f(self.script.lock().unwrap().files.get_mut(path).unwrap());
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
                let offset = parsed["offset"].as_u64().unwrap_or(0);
                let len = parsed["len"].as_u64().unwrap_or(0);
                let stall = s
                    .lock()
                    .unwrap()
                    .files
                    .get(&path)
                    .is_some_and(|f| f.stall_from_offset.is_some_and(|o| offset >= o));
                if stall {
                    tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                    return;
                }
                let (head, body): (String, Vec<u8>) = {
                    let sc = s.lock().unwrap();
                    if let Some(code) = sc.errs.get(&path) {
                        (err_line(code), Vec::new())
                    } else if let Some(f) = sc.files.get(&path) {
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
                                    "{{\"status\":\"ok\",\"data\":{{\"size\":{total},\"len\":{want},\"mtime_unix\":{mtime},\"name\":{name}}}}}\n"
                                ),
                                f.data[start..start + want].to_vec(),
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
    files_root: std::path::PathBuf,
    cache: std::path::PathBuf,
}

/// Boot the daemon against `socket`; the files corpus is `files` when set.
async fn boot_with(
    socket: &std::path::Path,
    files: bool,
    prep: impl FnOnce(&std::path::Path, &std::path::Path),
) -> Stack {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("seed.md"), "# seed\n\none doc\n").unwrap();
    let files_root = tmp.path().join("files-corpus");
    std::fs::create_dir_all(&files_root).unwrap();
    std::fs::write(files_root.join("seed.md"), "# files seed\n\nx\n").unwrap();
    prep(&files_root, &source);
    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(KbName::new("smoke").unwrap(), kb_section(source));
    kb_map.insert(
        KbName::new("files").unwrap(),
        kb_section(files_root.clone()),
    );
    let terminal = TerminalSection {
        enabled: Some(true),
        runner_socket: Some(socket.to_path_buf()),
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
    let cache = paths.cache.clone();
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    common::wait_http_up(addr).await;
    Stack {
        _tmp: tmp,
        addr,
        files_root,
        cache,
    }
}

async fn boot(fake: &Fake, files: bool) -> Stack {
    boot_with(&fake.socket, files, |_, _| {}).await
}

fn keep(addr: std::net::SocketAddr, body: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .post(format!("http://{addr}/api/terms/{TERM_ID}/files/keep"))
        .header("Origin", format!("http://{addr}"))
        .header("x-kb-intent", "terminal")
        .header("content-type", "application/json")
        .body(body.to_string())
}

fn kept_get(addr: std::net::SocketAddr, artifact_id: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .get(format!(
            "http://{addr}/api/terms/{TERM_ID}/kept/{artifact_id}"
        ))
        .header("x-kb-intent", "terminal")
}

fn filler(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(31).wrapping_add(i >> 8)) as u8)
        .collect()
}

fn sha_hex(b: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(b))
}

async fn problem(resp: reqwest::Response) -> (u16, String) {
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

fn spool_leftovers(cache: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(cache.join("term-upload-spool"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

async fn assert_spool_empty(cache: &std::path::Path, what: &str) {
    for _ in 0..100 {
        if spool_leftovers(cache).is_empty() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("{what}: spool not empty: {:?}", spool_leftovers(cache));
}

/// Keep `path` and return the 200 answer.
async fn keep_ok(addr: std::net::SocketAddr, body: &str) -> serde_json::Value {
    let resp = keep(addr, body).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200, "{body}");
    assert_eq!(hdr(&resp, "x-content-type-options"), "nosniff");
    assert_eq!(hdr(&resp, "cache-control"), "no-store");
    resp.json().await.unwrap()
}

/// The id of the doc whose path ends with `suffix` in `kb` (polls: the
/// watcher indexes asynchronously).
async fn doc_id(addr: std::net::SocketAddr, kb: &str, suffix: &str) -> String {
    let suffix = suffix.to_string();
    let kb = kb.to_string();
    common::poll_until(&format!("doc {suffix} indexed in {kb}"), || {
        let (suffix, kb) = (suffix.clone(), kb.clone());
        async move {
            let docs: Vec<serde_json::Value> =
                reqwest::get(format!("http://{addr}/api/kb/{kb}/docs?limit=200"))
                    .await
                    .ok()?
                    .json()
                    .await
                    .ok()?;
            docs.iter()
                .find(|d| d["path"].as_str().is_some_and(|p| p.ends_with(&suffix)))
                .and_then(|d| d["id"].as_str().map(str::to_string))
        }
    })
    .await
}

/// GET the kept original, retrying a 404 until the stub is indexed.
async fn kept_ready(addr: std::net::SocketAddr, artifact_id: &str) -> reqwest::Response {
    let id = artifact_id.to_string();
    common::poll_until("the kept original readable", || {
        let id = id.clone();
        async move {
            let resp = kept_get(addr, &id).send().await.ok()?;
            (resp.status().as_u16() == 200).then_some(resp)
        }
    })
    .await
}

/// The `.kbblob` whose name starts with the first 12 hex of `sha`.
fn find_blob(root: &std::path::Path, sha: &str) -> std::path::PathBuf {
    fn walk(dir: &std::path::Path, sha: &str, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, sha, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("kbblob")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&sha[..12]))
            {
                out.push(p);
            }
        }
    }
    let mut v = Vec::new();
    walk(root, sha, &mut v);
    v.sort();
    v.pop().expect("a .kbblob")
}

// ------------------------------------------------------------------ keep

#[tokio::test]
async fn cap_is_kept_cap_plus_one_is_413_and_no_spool_is_left() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let data = filler(FILE_CAP);
    fake.add_file("big.bin", data.clone());
    fake.add_file("bigger.bin", filler(FILE_CAP + 1));
    let st = boot(&fake, true).await;

    let v = keep_ok(st.addr, r#"{"path":"big.bin"}"#).await;
    assert_eq!(v["kb"]["status"], "stored", "{v}");
    assert_eq!(v["bytes"].as_u64().unwrap(), FILE_CAP as u64);
    assert_eq!(v["sha256"], sha_hex(&data));
    assert_eq!(v["name"], "big.bin");
    assert!(v["kb"]["id"].as_str().is_some_and(|s| !s.is_empty()));
    let m = CHUNK as u64;
    assert_eq!(fake.fs_gets("big.bin"), vec![0, m, 2 * m, 3 * m]);
    assert_spool_empty(&st.cache, "after a 32 MiB keep").await;

    let resp = keep(st.addr, r#"{"path":"bigger.bin"}"#)
        .send()
        .await
        .unwrap();
    let (status, urn) = problem(resp).await;
    assert_eq!(status, 413);
    assert_eq!(urn, "urn:kb:errors:payload-too-large");
    assert_eq!(fake.fs_gets("bigger.bin"), vec![0], "refused on the head");
    assert_spool_empty(&st.cache, "after a 32 MiB + 1 refusal").await;
}

#[tokio::test]
async fn a_20_mib_file_is_pulled_as_three_chunks_at_running_offsets() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.add_file("d/twenty.bin", filler(20 * MIB));
    let st = boot(&fake, true).await;
    let v = keep_ok(st.addr, r#"{"path":"d/twenty.bin","root":"cwd"}"#).await;
    assert_eq!(v["kb"]["status"], "stored", "{v}");
    let m = CHUNK as u64;
    assert_eq!(fake.fs_gets("d/twenty.bin"), vec![0, m, 2 * m]);
}

#[tokio::test]
async fn not_configured_is_a_typed_409_and_the_runner_is_never_dialled() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.add_file("a.txt", b"hello".to_vec());
    let st = boot(&fake, false).await;
    let resp = keep(st.addr, r#"{"path":"a.txt"}"#).send().await.unwrap();
    let (status, urn) = problem(resp).await;
    assert_eq!(status, 409);
    assert_eq!(urn, "urn:kb:errors:terminal-files-not-configured");
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn request_validation_is_a_400_and_never_dials_the_runner() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let st = boot(&fake, true).await;
    for body in [
        "not json".to_string(),
        "{}".to_string(),
        r#"{"path":""}"#.to_string(),
        r#"{"path":"a","root":"etc"}"#.to_string(),
        r#"{"path":"a","index":2}"#.to_string(),
        r#"{"path":"a","index":"1"}"#.to_string(),
        r#"{"path":"a","extra":1}"#.to_string(),
        r#"{"path":"a\u0000b"}"#.to_string(),
    ] {
        let resp = keep(st.addr, &body).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 400, "{body}");
    }
    // Over the 4 KiB body limit is 413 (the extractor refusal).
    let big = format!(r#"{{"path":"a","pad":"{}"}}"#, "x".repeat(5000));
    let resp = keep(st.addr, &big).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 413);
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn index_zero_stores_a_metadata_only_stub_and_index_one_inlines_escaped_scrubbed_text() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let text = b"<script>alert(\"x\")</script> & AKIAABCDEFGHIJKLMNOP\n".to_vec();
    for n in ["a.txt", "b.txt", "c.txt"] {
        fake.add_file(n, text.clone());
    }
    let st = boot(&fake, true).await;
    let read = |rel: &str| std::fs::read_to_string(st.files_root.join(rel)).unwrap();

    // Default (absent) and an explicit 0: no inlined content at all.
    let v = keep_ok(st.addr, r#"{"path":"a.txt"}"#).await;
    assert_eq!(v["kb"]["inlined"], false, "{v}");
    let stub = read(v["kb"]["stub_rel"].as_str().unwrap());
    assert!(!stub.contains("id=\"content\""), "{stub}");
    assert!(!stub.contains("script") && !stub.contains("AKIA"), "{stub}");
    let v0 = keep_ok(st.addr, r#"{"path":"a.txt","index":0}"#).await;
    assert_eq!(v0["kb"]["stub_rel"], v["kb"]["stub_rel"]);

    // index 1 (number) and true: escaped, scrubbed, no raw tag.
    for (n, flag) in [("b.txt", "1"), ("c.txt", "true")] {
        let v = keep_ok(st.addr, &format!(r#"{{"path":"{n}","index":{flag}}}"#)).await;
        assert_eq!(v["kb"]["inlined"], true, "{v}");
        let stub = read(v["kb"]["stub_rel"].as_str().unwrap());
        assert!(stub.contains("<pre id=\"content\">"), "{stub}");
        assert!(
            stub.contains("&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp;"),
            "{stub}"
        );
        assert!(!stub.contains("<script"), "nothing raw: {stub}");
        assert!(!stub.contains("AKIAABCDEFGHIJKLMNOP"), "scrubbed: {stub}");
    }
}

#[tokio::test]
async fn keeping_the_same_file_twice_is_created_false_with_the_same_id() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.add_file("a.txt", filler(1000));
    let st = boot(&fake, true).await;
    let first = keep_ok(st.addr, r#"{"path":"a.txt"}"#).await;
    assert_eq!(first["kb"]["created"], true);
    let second = keep_ok(st.addr, r#"{"path":"a.txt"}"#).await;
    assert_eq!(second["kb"]["created"], false, "{second}");
    assert_eq!(second["kb"]["id"], first["kb"]["id"]);
    assert_eq!(second["sha256"], first["sha256"]);
}

#[tokio::test]
async fn runner_refusals_map_to_typed_problems() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    {
        let mut sc = fake.script.lock().unwrap();
        sc.errs.insert("link".into(), "not-a-file".into());
        sc.errs.insert("../x".into(), "outside-root".into());
    }
    fake.add_file("empty.txt", Vec::new());
    let st = boot(&fake, true).await;
    for (body, status, urn) in [
        (
            r#"{"path":"link"}"#,
            422,
            "urn:kb:errors:terminal-not-a-file",
        ),
        (
            r#"{"path":"../x"}"#,
            403,
            "urn:kb:errors:terminal-outside-root",
        ),
        (
            r#"{"path":"missing.txt"}"#,
            404,
            "urn:kb:errors:terminal-not-found",
        ),
        (r#"{"path":"empty.txt"}"#, 400, "urn:kb:errors:bad-request"),
    ] {
        let resp = keep(st.addr, body).send().await.unwrap();
        assert_eq!(problem(resp).await, (status, urn.to_string()), "{body}");
    }
    assert_spool_empty(&st.cache, "after refusals").await;

    // The runner is down: 503, typed.
    let down = boot_with(&tmp.path().join("no-such.sock"), true, |_, _| {}).await;
    let resp = keep(down.addr, r#"{"path":"a.txt"}"#).send().await.unwrap();
    assert_eq!(
        problem(resp).await,
        (503, "urn:kb:errors:terminal-runner-unavailable".to_string())
    );
}

#[tokio::test]
async fn a_file_that_changes_mid_read_is_409_and_a_runner_error_is_502_with_no_spool_left() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.add_file("moving.bin", filler(20 * MIB));
    fake.edit("moving.bin", |f| {
        f.mtime_changes_from_offset = Some(CHUNK as u64)
    });
    fake.add_file("broken.bin", filler(20 * MIB));
    fake.edit("broken.bin", |f| f.fail_from_offset = Some(CHUNK as u64));
    let st = boot(&fake, true).await;

    let resp = keep(st.addr, r#"{"path":"moving.bin"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(
        problem(resp).await,
        (409, "urn:kb:errors:terminal-file-changed".to_string())
    );
    assert_eq!(fake.fs_gets("moving.bin"), vec![0, CHUNK as u64]);
    assert_spool_empty(&st.cache, "after a mid-stream change").await;

    let resp = keep(st.addr, r#"{"path":"broken.bin"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 502);
    assert_spool_empty(&st.cache, "after a mid-stream runner error").await;
}

#[tokio::test]
async fn a_dropped_request_mid_stream_leaves_no_spool() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.add_file("stall.bin", filler(20 * MIB));
    fake.edit("stall.bin", |f| f.stall_from_offset = Some(CHUNK as u64));
    let st = boot(&fake, true).await;
    let addr = st.addr;
    let handle = tokio::spawn(async move {
        let _ = keep(addr, r#"{"path":"stall.bin"}"#).send().await;
    });
    for _ in 0..100 {
        if fake.fs_gets("stall.bin").contains(&(CHUNK as u64)) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        fake.fs_gets("stall.bin").contains(&(CHUNK as u64)),
        "the keep reached its second chunk"
    );
    assert!(
        !spool_leftovers(&st.cache).is_empty(),
        "a spool exists while the stream is in flight"
    );
    handle.abort();
    assert_spool_empty(&st.cache, "after the client went away").await;
}

// --------------------------------------------------------------- blob read

#[tokio::test]
async fn a_kept_original_is_always_an_octet_stream_attachment() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let page = b"<html><script>document.title='x'</script></html>".to_vec();
    fake.add_file("x.html", page.clone());
    let sniffed = filler(3000);
    fake.add_named("weird", "../a\"b\u{202E}.html", sniffed.clone());
    let st = boot(&fake, true).await;

    let v = keep_ok(st.addr, r#"{"path":"x.html"}"#).await;
    let id = v["kb"]["id"].as_str().unwrap().to_string();
    let resp = kept_ready(st.addr, &id).await;
    assert_eq!(hdr(&resp, "content-type"), "application/octet-stream");
    assert_eq!(
        hdr(&resp, "content-disposition"),
        "attachment; filename=\"x.html\"; filename*=UTF-8''x.html"
    );
    assert_eq!(hdr(&resp, "x-content-type-options"), "nosniff");
    assert_eq!(hdr(&resp, "cache-control"), "no-store");
    assert_eq!(hdr(&resp, "content-length"), page.len().to_string());
    assert_eq!(resp.bytes().await.unwrap().to_vec(), page);

    // An extra query (a caller path) is never read.
    let resp = kept_get(st.addr, &id)
        .query(&[("path", "/etc/passwd"), ("root", "up")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.bytes().await.unwrap().to_vec(), page);

    // A hostile stored name is sanitised in the disposition.
    let v = keep_ok(st.addr, r#"{"path":"weird"}"#).await;
    let id = v["kb"]["id"].as_str().unwrap().to_string();
    let resp = kept_ready(st.addr, &id).await;
    assert_eq!(
        hdr(&resp, "content-disposition"),
        "attachment; filename=\"ab.html\"; filename*=UTF-8''ab.html"
    );
    assert_eq!(hdr(&resp, "content-type"), "application/octet-stream");
    assert_eq!(
        resp.bytes().await.unwrap().to_vec(),
        sniffed,
        "byte-identical"
    );
}

const NOT_A_STUB: &str =
    "<!doctype html><html><head><title>plain</title></head><body><h1>plain</h1></body></html>";
// Claims to be an upload stub but has no sibling blob.
const BLOBLESS_STUB: &str = "<!doctype html><html><head><title>b</title><meta name=\"kb-category\" content=\"upload\"><meta name=\"kb-upload-sha256\" content=\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"><meta name=\"kb-upload-name\" content=\"b.bin\"></head><body><h1>b</h1></body></html>";

#[tokio::test]
async fn every_non_stub_case_is_the_same_404_with_no_existence_oracle() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.add_file("a.txt", b"original bytes".to_vec());
    fake.add_file("b.txt", b"second original".to_vec());
    let st = boot_with(&fake.socket, true, |files, smoke| {
        std::fs::write(files.join("plain.html"), NOT_A_STUB).unwrap();
        std::fs::write(files.join("blobless.html"), BLOBLESS_STUB).unwrap();
        std::fs::write(smoke.join("only-smoke.html"), NOT_A_STUB).unwrap();
    })
    .await;

    // A real kept file, to tamper with and to delete.
    let a = keep_ok(st.addr, r#"{"path":"a.txt"}"#).await;
    let a_id = a["kb"]["id"].as_str().unwrap().to_string();
    let a_blob = {
        let resp = kept_ready(st.addr, &a_id).await;
        assert_eq!(resp.bytes().await.unwrap().as_ref(), b"original bytes");
        find_blob(&st.files_root, a["sha256"].as_str().unwrap())
    };
    let b = keep_ok(st.addr, r#"{"path":"b.txt"}"#).await;
    let b_id = b["kb"]["id"].as_str().unwrap().to_string();
    kept_ready(st.addr, &b_id).await;
    let b_blob = find_blob(&st.files_root, b["sha256"].as_str().unwrap());
    assert_ne!(a_blob, b_blob, "two distinct blobs");

    let plain_id = doc_id(st.addr, "files", "plain.html").await;
    let blobless_id = doc_id(st.addr, "files", "blobless.html").await;
    let seed_id = doc_id(st.addr, "files", "seed.md").await;
    let smoke_id = doc_id(st.addr, "smoke", "only-smoke.html").await;

    // Tamper `a` (sha mismatch) and delete `b` (blob missing).
    std::fs::write(&a_blob, b"tampered bytes").unwrap();
    std::fs::remove_file(&b_blob).unwrap();

    let mut bodies: Vec<(String, String)> = Vec::new();
    for (what, id) in [
        ("unknown id", "0123456789abcdef".to_string()),
        ("invalid id chars", "not.an.id".to_string()),
        ("non-stub doc", plain_id),
        ("markdown doc", seed_id),
        ("stub without a blob", blobless_id),
        ("id of a doc in another kb", smoke_id),
        ("sha mismatch", a_id),
        ("deleted blob", b_id),
    ] {
        let resp = kept_get(st.addr, &id).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 404, "{what}");
        assert_eq!(
            hdr(&resp, "content-type"),
            "application/problem+json",
            "{what}"
        );
        assert_eq!(hdr(&resp, "cache-control"), "no-store", "{what}");
        bodies.push((what.to_string(), resp.text().await.unwrap()));
    }
    // With no files corpus configured the answer is the same again.
    let unconfigured = boot(&fake, false).await;
    let resp = kept_get(unconfigured.addr, "0123456789abcdef")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    bodies.push(("not configured".to_string(), resp.text().await.unwrap()));

    let (first_what, first) = &bodies[0];
    for (what, body) in &bodies {
        assert_eq!(body, first, "`{what}` differs from `{first_what}`");
    }
    assert!(!first.contains("tampered") && !first.contains("original"));
}
