//! The runner's `upload` op (T18): the storage rules through the public
//! `kb_term::runner::upload` API, and the wire protocol (a request line,
//! then exactly `len` raw bytes) through a real `runner.sock`. Every
//! picture is synthetic: a magic header plus filler, generated here.
//! Every socket read carries a hard deadline.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use kb_term::runner::supervisor::DirectSupervisor;
use kb_term::runner::upload::{
    store_upload, up_dir, UPLOAD_MAX_BYTES, UPLOAD_MAX_FILES, UPLOAD_MAX_TOTAL_BYTES,
};
use kb_term::runner::{Runner, RunnerConfig};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const ID: &str = "abcd1234";
const DEADLINE: Duration = Duration::from_secs(10);

fn png(tag: u32) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    v.extend_from_slice(&tag.to_be_bytes());
    v.extend(std::iter::repeat_n(0xAB, 32));
    v
}

fn jpeg() -> Vec<u8> {
    let mut v = vec![0xff, 0xd8, 0xff, 0xe0];
    v.extend(std::iter::repeat_n(0x11, 32));
    v
}

fn gif() -> Vec<u8> {
    let mut v = b"GIF89a".to_vec();
    v.extend(std::iter::repeat_n(0x22, 32));
    v
}

fn webp() -> Vec<u8> {
    let mut v = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
    v.extend(std::iter::repeat_n(0x33, 32));
    v
}

fn sha_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A sessions dir with one live session (its holder socket file exists).
fn sessions() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(format!("{ID}.sock")), b"").unwrap();
    tmp
}

fn code(r: Result<kb_term::runner::upload::Stored, kb_term::runner::RunnerError>) -> String {
    r.expect_err("expected a refusal").code().to_string()
}

fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn the_four_picture_types_are_stored_under_the_sniffed_extension() {
    let tmp = sessions();
    let cases: [(&str, Vec<u8>, &str); 4] = [
        ("image/png", png(1), "png"),
        ("image/jpeg", jpeg(), "jpg"),
        ("image/gif", gif(), "gif"),
        ("image/webp", webp(), "webp"),
    ];
    for (mime, bytes, ext) in &cases {
        let got = store_upload(tmp.path(), ID, mime, bytes).expect(mime);
        assert_eq!(got.mime, *mime);
        assert_eq!(got.bytes, bytes.len() as u64);
        assert_eq!(
            got.sha256,
            sha_hex(bytes),
            "the name is the sha256 of the bytes"
        );
        assert_eq!(
            got.path,
            up_dir(tmp.path(), ID).join(format!("{}.{ext}", sha_hex(bytes)))
        );
        assert_eq!(std::fs::read(&got.path).unwrap(), *bytes, "stored verbatim");
    }
}

#[test]
fn only_pictures_are_accepted_whatever_the_declared_type() {
    let tmp = sessions();
    let svg: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
    let html: &[u8] = b"<html><body>synthetic</body></html>";
    let pdf: &[u8] = b"%PDF-1.7\n synthetic";
    let text: &[u8] = b"just some synthetic text";
    for (declared, bytes) in [
        ("image/svg+xml", svg),
        ("image/png", svg),
        ("text/html", html),
        ("image/png", html),
        ("application/pdf", pdf),
        ("image/png", pdf),
        ("text/plain", text),
        ("image/png", text),
        ("image/png", &b""[..]),
    ] {
        let r = store_upload(tmp.path(), ID, declared, bytes);
        assert_eq!(
            code(r),
            "bad-image",
            "{declared} over {:?}",
            &bytes[..bytes.len().min(8)]
        );
    }
    assert!(
        !up_dir(tmp.path(), ID).exists() || entries(&up_dir(tmp.path(), ID)).is_empty(),
        "a refused upload leaves nothing on disk"
    );
}

#[test]
fn a_declared_type_that_disagrees_with_the_bytes_is_refused() {
    let tmp = sessions();
    // Declared PNG over JPEG bytes (and the reverse).
    assert_eq!(
        code(store_upload(tmp.path(), ID, "image/png", &jpeg())),
        "bad-image"
    );
    assert_eq!(
        code(store_upload(tmp.path(), ID, "image/jpeg", &png(2))),
        "bad-image"
    );
    assert_eq!(code(store_upload(tmp.path(), ID, "", &png(2))), "bad-image");
}

#[test]
fn the_cap_is_enforced_by_length_before_anything_is_written() {
    let tmp = sessions();
    let mut big = png(3);
    big.resize(UPLOAD_MAX_BYTES as usize + 1, 0);
    assert_eq!(
        code(store_upload(tmp.path(), ID, "image/png", &big)),
        "too-large"
    );
    // Exactly the cap is fine.
    big.truncate(UPLOAD_MAX_BYTES as usize);
    store_upload(tmp.path(), ID, "image/png", &big).expect("exactly the cap");
}

#[test]
fn hostile_or_unknown_ids_are_refused_and_nothing_is_created_outside_the_session_dir() {
    let tmp = sessions();
    let before = entries(tmp.path());
    for bad in [
        "../x",
        "a/b",
        "ABC",
        "../../etc",
        "abcd1234/../..",
        "abc",
        "abcd\u{0}1234",
        "",
    ] {
        let r = store_upload(tmp.path(), bad, "image/png", &png(4));
        assert_eq!(code(r), "invalid-id", "{bad:?}");
    }
    // A well-formed id with no session: not-found, no directory created.
    let r = store_upload(tmp.path(), "zzzz9999", "image/png", &png(4));
    assert_eq!(code(r), "not-found");
    assert_eq!(entries(tmp.path()), before, "the sessions dir is unchanged");
}

#[test]
fn the_file_is_0600_and_the_directory_is_0700() {
    let tmp = sessions();
    let got = store_upload(tmp.path(), ID, "image/png", &png(5)).unwrap();
    let file_mode = std::fs::metadata(&got.path).unwrap().permissions().mode() & 0o777;
    let dir_mode = std::fs::metadata(up_dir(tmp.path(), ID))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(file_mode, 0o600);
    assert_eq!(dir_mode, 0o700);
}

#[test]
fn the_same_bytes_twice_are_one_file() {
    let tmp = sessions();
    let a = store_upload(tmp.path(), ID, "image/png", &png(6)).unwrap();
    let b = store_upload(tmp.path(), ID, "image/png", &png(6)).unwrap();
    assert_eq!(a, b);
    assert_eq!(entries(&up_dir(tmp.path(), ID)).len(), 1);
}

#[test]
fn dedupe_wins_over_the_quota_and_a_new_picture_past_it_is_refused() {
    let tmp = sessions();
    for i in 0..UPLOAD_MAX_FILES as u32 {
        store_upload(tmp.path(), ID, "image/png", &png(100 + i)).unwrap();
    }
    assert_eq!(entries(&up_dir(tmp.path(), ID)).len(), UPLOAD_MAX_FILES);
    // The 65th DISTINCT picture is refused, naming the quota.
    let err = store_upload(tmp.path(), ID, "image/png", &png(9999)).unwrap_err();
    assert_eq!(err.code(), "too-large");
    assert!(
        err.message().contains("quota"),
        "message: {}",
        err.message()
    );
    // Re-sending a stored picture at the quota succeeds without a rewrite.
    let again = store_upload(tmp.path(), ID, "image/png", &png(100)).expect("dedupe at quota");
    assert_eq!(again.sha256, sha_hex(&png(100)));
    assert_eq!(entries(&up_dir(tmp.path(), ID)).len(), UPLOAD_MAX_FILES);
}

#[test]
fn the_byte_quota_refuses_a_picture_that_would_cross_it() {
    let tmp = sessions();
    let up = up_dir(tmp.path(), ID);
    std::fs::create_dir_all(&up).unwrap();
    // A sparse file occupying all but 10 bytes of the byte quota.
    let filler = std::fs::File::create(up.join("filler.bin")).unwrap();
    filler.set_len(UPLOAD_MAX_TOTAL_BYTES - 10).unwrap();
    drop(filler);
    assert_eq!(
        code(store_upload(tmp.path(), ID, "image/png", &png(7))),
        "too-large"
    );
}

#[test]
fn a_dead_sessions_up_dir_is_swept_and_a_live_ones_is_kept() {
    let tmp = sessions();
    store_upload(tmp.path(), ID, "image/png", &png(8)).unwrap();
    let dead = "deadsess01";
    std::fs::create_dir_all(up_dir(tmp.path(), dead)).unwrap();
    std::fs::write(up_dir(tmp.path(), dead).join("x.png"), png(1)).unwrap();
    kb_term::runner::upload::sweep_orphans(tmp.path());
    assert!(!up_dir(tmp.path(), dead).exists());
    assert_eq!(entries(&up_dir(tmp.path(), ID)).len(), 1);
}

// ---------------------------------------------------------------- socket

struct Served {
    _tmp: tempfile::TempDir,
    sock: PathBuf,
    sessions: PathBuf,
    shutdown: tokio::sync::broadcast::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

async fn serve_runner() -> Served {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run");
    let sessions = run_dir.join("s");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join(format!("{ID}.sock")), b"").unwrap();
    let sock_dir = run_dir.join("runner");
    std::fs::create_dir_all(&sock_dir).unwrap();
    let sock = sock_dir.join("runner.sock");
    let cfg = RunnerConfig::test_config(run_dir, "/bin/true".to_string(), tmp.path().join("conf"));
    let runner: Arc<Runner<DirectSupervisor>> = Arc::new(Runner::direct(cfg));
    let listener = kb_term::runner::server::bind_runner_socket(&sock).unwrap();
    let (shutdown, rx) = tokio::sync::broadcast::channel::<()>(1);
    let sock_for_task = sock.clone();
    let task = tokio::spawn(async move {
        let _ = kb_term::runner::server::serve(runner, listener, sock_for_task, rx).await;
    });
    Served {
        _tmp: tmp,
        sock,
        sessions,
        shutdown,
        task,
    }
}

impl Served {
    async fn stop(self) {
        let _ = self.shutdown.send(());
        let _ = tokio::time::timeout(DEADLINE, self.task).await;
    }
}

async fn read_reply(reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> serde_json::Value {
    let mut line = String::new();
    let n = tokio::time::timeout(DEADLINE, reader.read_line(&mut line))
        .await
        .expect("reply deadline")
        .expect("reply read");
    assert!(n > 0, "runner closed without replying");
    serde_json::from_str(line.trim()).expect("reply is JSON")
}

fn request_line(id: &str, mime: &str, len: u64) -> String {
    format!("{{\"v\":1,\"op\":\"upload\",\"id\":\"{id}\",\"mime\":\"{mime}\",\"len\":{len}}}\n")
}

#[tokio::test]
async fn upload_over_the_socket_stores_the_body_and_answers_the_path() {
    let served = serve_runner().await;
    let stream = UnixStream::connect(&served.sock).await.unwrap();
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    let bytes = png(10);
    // The request line and the body arrive in ONE write: the runner must
    // keep the body bytes its line reader already buffered.
    let mut out = request_line(ID, "image/png", bytes.len() as u64).into_bytes();
    out.extend_from_slice(&bytes);
    wr.write_all(&out).await.unwrap();
    let reply = read_reply(&mut reader).await;
    assert_eq!(reply["status"], "ok", "reply: {reply}");
    let data = &reply["data"];
    assert_eq!(data["bytes"], bytes.len());
    assert_eq!(data["mime"], "image/png");
    assert_eq!(data["sha256"], sha_hex(&bytes));
    let path = PathBuf::from(data["path"].as_str().unwrap());
    assert!(path.starts_with(served.sessions.join(ID).join("up")));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    // The connection stays usable: a second upload on it works.
    let second = jpeg();
    let mut out2 = request_line(ID, "image/jpeg", second.len() as u64).into_bytes();
    out2.extend_from_slice(&second);
    wr.write_all(&out2).await.unwrap();
    let reply2 = read_reply(&mut reader).await;
    assert_eq!(reply2["status"], "ok", "reply: {reply2}");
    served.stop().await;
}

#[tokio::test]
async fn a_len_over_the_cap_is_refused_without_reading_the_body() {
    let served = serve_runner().await;
    let stream = UnixStream::connect(&served.sock).await.unwrap();
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    // Send ONLY the line: a runner that waited for 8 MiB + 1 body bytes
    // would hit the read deadline instead of answering.
    wr.write_all(request_line(ID, "image/png", UPLOAD_MAX_BYTES + 1).as_bytes())
        .await
        .unwrap();
    let reply = read_reply(&mut reader).await;
    assert_eq!(reply["status"], "err", "reply: {reply}");
    assert_eq!(reply["code"], "too-large");
    // The connection ends: body bytes that were never sent cannot be skipped.
    let mut rest = Vec::new();
    let n = tokio::time::timeout(DEADLINE, reader.read_to_end(&mut rest))
        .await
        .expect("close deadline")
        .unwrap();
    assert_eq!(n, 0, "nothing follows the refusal");
    assert!(
        !up_dir(&served.sessions, ID).exists() || entries(&up_dir(&served.sessions, ID)).is_empty(),
        "nothing stored"
    );
    served.stop().await;
}

#[tokio::test]
async fn a_short_body_leaves_nothing_on_disk() {
    let served = serve_runner().await;
    let stream = UnixStream::connect(&served.sock).await.unwrap();
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    let bytes = png(11);
    let mut out = request_line(ID, "image/png", bytes.len() as u64 + 50).into_bytes();
    out.extend_from_slice(&bytes); // 50 bytes short
    wr.write_all(&out).await.unwrap();
    wr.shutdown().await.unwrap(); // EOF mid-body
    let reply = read_reply(&mut reader).await;
    assert_eq!(reply["status"], "err", "reply: {reply}");
    assert_eq!(reply["code"], "io");
    assert!(
        !up_dir(&served.sessions, ID).exists() || entries(&up_dir(&served.sessions, ID)).is_empty(),
        "a short body stores nothing, not even a temporary file"
    );
    served.stop().await;
}

#[tokio::test]
async fn a_bad_id_or_unknown_session_is_refused_before_the_body_is_read() {
    let served = serve_runner().await;
    for (id, want) in [("../x", "invalid-id"), ("zzzz9999", "not-found")] {
        let stream = UnixStream::connect(&served.sock).await.unwrap();
        let (rd, mut wr) = stream.into_split();
        let mut reader = BufReader::new(rd);
        // Line only: the refusal must not wait for a body.
        wr.write_all(request_line(id, "image/png", 100).as_bytes())
            .await
            .unwrap();
        let reply = read_reply(&mut reader).await;
        assert_eq!(reply["code"], want, "id {id}: {reply}");
    }
    served.stop().await;
}
