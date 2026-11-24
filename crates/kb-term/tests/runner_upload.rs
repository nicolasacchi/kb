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
    abort_upload, append_chunk, begin_upload, commit_upload, store_upload, sweep_partials, up_dir,
    CHUNK_MAX_BYTES, FILE_MAX_BYTES, MAX_OPEN_PARTIALS, PARTIAL_TTL, UPLOAD_MAX_BYTES,
    UPLOAD_MAX_FILES, UPLOAD_MAX_TOTAL_BYTES,
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

#[test]
fn concurrent_identical_uploads_all_succeed_with_one_file_and_no_temp_leftovers() {
    const N: usize = 8;
    for round in 0..20u32 {
        let tmp = sessions();
        let bytes = Arc::new(png(1000 + round));
        let dir = Arc::new(tmp.path().to_path_buf());
        let barrier = Arc::new(std::sync::Barrier::new(N));
        let handles: Vec<_> = (0..N)
            .map(|_| {
                let (bytes, dir, barrier) = (bytes.clone(), dir.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    store_upload(&dir, ID, "image/png", &bytes)
                })
            })
            .collect();
        let want = up_dir(tmp.path(), ID).join(format!("{}.png", sha_hex(&bytes)));
        for h in handles {
            let got = h.join().unwrap().expect("every concurrent upload succeeds");
            assert_eq!(got.path, want);
        }
        let names = entries(&up_dir(tmp.path(), ID));
        assert_eq!(names, vec![format!("{}.png", sha_hex(&bytes))], "{names:?}");
    }
}

// ------------------------------------------------- chunked ops (FXRUN)

const MIB: usize = 1024 * 1024;

/// Deterministic synthetic bytes that are neither text nor a picture.
fn blob(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| ((i * 31 + 7) % 251) as u8 | 0x80)
        .collect()
}

fn partial_leftovers(dir: &Path) -> Vec<String> {
    entries(dir)
        .into_iter()
        .filter(|n| n.starts_with(".tmp-up-"))
        .collect()
}

/// Begin + one append + commit straight through the API.
fn put(
    dir: &Path,
    name: &str,
    mime: &str,
    bytes: &[u8],
) -> Result<kb_term::runner::upload::Committed, kb_term::runner::RunnerError> {
    let token = begin_upload(dir, ID, name, mime, bytes.len() as u64)?;
    if !bytes.is_empty() {
        append_chunk(dir, ID, &token, 0, bytes)?;
    }
    commit_upload(dir, ID, &token)
}

#[test]
fn any_kind_is_stored_with_a_slugged_sha_prefixed_name_and_no_execute_bit() {
    let tmp = sessions();
    let cases: [(&str, &str, Vec<u8>, &str, &str); 5] = [
        (
            "Report Q3.PDF",
            "application/pdf",
            b"%PDF-1.7\nsynthetic".to_vec(),
            "pdf",
            "report-q3.pdf",
        ),
        (
            "data.json",
            "application/json",
            br#"{"a":1}"#.to_vec(),
            "text",
            "data.json",
        ),
        (
            "x.sh",
            "text/x-sh",
            b"#!/bin/sh\necho synthetic\n".to_vec(),
            "text",
            "x.sh",
        ),
        (
            "../../etc/passwd",
            "",
            b"root:synthetic\n".to_vec(),
            "text",
            "passwd.txt",
        ),
        (
            "dump weird",
            "application/octet-stream",
            blob(300),
            "opaque",
            "dump-weird.bin",
        ),
    ];
    for (name, mime, bytes, kind, stored) in cases {
        let got = put(tmp.path(), name, mime, &bytes).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let sha = sha_hex(&bytes);
        assert_eq!(got.sha256, sha);
        assert_eq!(got.kind, kind, "{name}");
        assert_eq!(got.name, format!("{}-{stored}", &sha[..12]), "{name}");
        assert_eq!(got.path, up_dir(tmp.path(), ID).join(&got.name));
        assert_eq!(std::fs::read(&got.path).unwrap(), bytes);
        let mode = std::fs::metadata(&got.path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o177,
            0,
            "{name}: mode {mode:o} must be 0600 at most"
        );
        assert_eq!(mode & 0o777, 0o600);
    }
    assert!(
        partial_leftovers(&up_dir(tmp.path(), ID)).is_empty(),
        "no partial remains"
    );
}

#[test]
fn a_picture_takes_its_extension_from_the_bytes_and_octet_stream_is_accepted() {
    let tmp = sessions();
    let got = put(
        tmp.path(),
        "holiday.exe",
        "application/octet-stream",
        &png(40),
    )
    .unwrap();
    assert_eq!(got.kind, "image");
    assert_eq!(got.mime, "image/png");
    assert!(got.name.ends_with("-holiday.png"), "{}", got.name);
}

#[test]
fn a_declared_picture_must_agree_and_a_failed_commit_leaves_nothing() {
    let tmp = sessions();
    let up = up_dir(tmp.path(), ID);
    for (declared, bytes) in [
        ("image/png", jpeg()),
        ("image/jpeg", png(41)),
        ("image/png", b"plain text".to_vec()),
        ("image/svg+xml", b"<svg/>".to_vec()),
    ] {
        let e = put(tmp.path(), "p.png", declared, &bytes).unwrap_err();
        assert_eq!(e.code(), "bad-image", "{declared}");
    }
    assert!(
        entries(&up).is_empty(),
        "stored nothing and no partial: {:?}",
        entries(&up)
    );
}

#[test]
fn the_same_file_twice_is_one_file_and_dedupe_beats_the_quota() {
    let tmp = sessions();
    let a = put(tmp.path(), "a.txt", "text/plain", b"hello synthetic").unwrap();
    let b = put(tmp.path(), "a.txt", "text/plain", b"hello synthetic").unwrap();
    assert_eq!(a, b);
    assert_eq!(entries(&up_dir(tmp.path(), ID)).len(), 1);
    for i in 0..(UPLOAD_MAX_FILES - 1) {
        put(
            tmp.path(),
            "n.txt",
            "text/plain",
            format!("file {i}").as_bytes(),
        )
        .unwrap();
    }
    assert_eq!(entries(&up_dir(tmp.path(), ID)).len(), UPLOAD_MAX_FILES);
    let again =
        put(tmp.path(), "a.txt", "text/plain", b"hello synthetic").expect("dedupe at quota");
    assert_eq!(again, a);
    let e = put(tmp.path(), "z.txt", "text/plain", b"one too many").unwrap_err();
    assert_eq!(e.code(), "too-large");
    assert!(partial_leftovers(&up_dir(tmp.path(), ID)).is_empty());
}

#[test]
fn begin_refuses_a_size_over_the_file_cap_and_a_hostile_target() {
    let tmp = sessions();
    let e = begin_upload(tmp.path(), ID, "big.bin", "", FILE_MAX_BYTES + 1).unwrap_err();
    assert_eq!(e.code(), "too-large");
    begin_upload(tmp.path(), ID, "ok.bin", "", FILE_MAX_BYTES).expect("exactly the cap");
    assert_eq!(
        begin_upload(tmp.path(), "../x", "a", "", 1)
            .unwrap_err()
            .code(),
        "invalid-id"
    );
    assert_eq!(
        begin_upload(tmp.path(), "zzzz9999", "a", "", 1)
            .unwrap_err()
            .code(),
        "not-found"
    );
}

#[test]
fn offset_overrun_and_incomplete_commit_are_refused_and_the_partial_survives() {
    let tmp = sessions();
    let t = begin_upload(tmp.path(), ID, "f.bin", "", 10).unwrap();
    append_chunk(tmp.path(), ID, &t, 0, &[0x80; 4]).unwrap();
    // Wrong offsets (ahead and behind).
    for off in [0u64, 2, 9] {
        let e = append_chunk(tmp.path(), ID, &t, off, &[0x81; 2]).unwrap_err();
        assert_eq!(e.code(), "bad-request", "offset {off}");
    }
    // Past the declared size.
    assert_eq!(
        append_chunk(tmp.path(), ID, &t, 4, &[0x82; 7])
            .unwrap_err()
            .code(),
        "bad-request"
    );
    // Commit with received < size.
    assert_eq!(
        commit_upload(tmp.path(), ID, &t).unwrap_err().code(),
        "bad-request"
    );
    // The partial is intact: resuming at the right offset completes it.
    assert_eq!(append_chunk(tmp.path(), ID, &t, 4, &[0x83; 6]).unwrap(), 10);
    let got = commit_upload(tmp.path(), ID, &t).unwrap();
    assert_eq!(got.bytes, 10);
    assert_eq!(
        std::fs::read(&got.path).unwrap(),
        [[0x80u8; 4].as_slice(), [0x83u8; 6].as_slice()].concat()
    );
}

#[test]
fn abort_removes_the_partial_and_is_idempotent_and_tokens_are_scoped() {
    let tmp = sessions();
    let other = "wxyz5678";
    std::fs::write(tmp.path().join(format!("{other}.sock")), b"").unwrap();
    let t = begin_upload(tmp.path(), ID, "f.bin", "", 4).unwrap();
    assert_eq!(
        partial_leftovers(&up_dir(tmp.path(), ID)).len(),
        2,
        "data + meta"
    );
    // Another session's id cannot reach it; a hostile token never reaches a path.
    assert_eq!(
        append_chunk(tmp.path(), other, &t, 0, b"abcd")
            .unwrap_err()
            .code(),
        "not-found"
    );
    assert_eq!(
        commit_upload(tmp.path(), other, &t).unwrap_err().code(),
        "not-found"
    );
    for bad in [
        "../../x",
        "",
        "ABCDEF0123456789ABCDEF0123456789",
        "zz",
        &t[..31],
    ] {
        assert_eq!(
            abort_upload(tmp.path(), ID, bad).unwrap_err().code(),
            "bad-request",
            "{bad:?}"
        );
        assert_eq!(
            append_chunk(tmp.path(), ID, bad, 0, b"a")
                .unwrap_err()
                .code(),
            "bad-request"
        );
        assert_eq!(
            commit_upload(tmp.path(), ID, bad).unwrap_err().code(),
            "bad-request"
        );
    }
    // Aborting from the wrong session does not remove it.
    abort_upload(tmp.path(), other, &t).unwrap();
    assert_eq!(partial_leftovers(&up_dir(tmp.path(), ID)).len(), 2);
    abort_upload(tmp.path(), ID, &t).unwrap();
    assert!(partial_leftovers(&up_dir(tmp.path(), ID)).is_empty());
    // Idempotent, and an unknown (well-formed) token is ok.
    abort_upload(tmp.path(), ID, &t).unwrap();
    abort_upload(tmp.path(), ID, &"0".repeat(32)).unwrap();
}

#[test]
fn reservations_count_against_the_byte_quota_the_file_quota_and_the_open_cap() {
    let tmp = sessions();
    let up = up_dir(tmp.path(), ID);
    std::fs::create_dir_all(&up).unwrap();
    // Stored files hold 64 MiB; one open 32 MiB reservation brings it to 96.
    let filler = std::fs::File::create(up.join("filler.bin")).unwrap();
    filler.set_len(64 * MIB as u64).unwrap();
    drop(filler);
    let t1 = begin_upload(tmp.path(), ID, "a.bin", "", FILE_MAX_BYTES).unwrap();
    // 96 + 32 = 128: exactly fits; 96 + 32 + 1 does not.
    let t2 = begin_upload(tmp.path(), ID, "b.bin", "", FILE_MAX_BYTES).unwrap();
    let e = begin_upload(tmp.path(), ID, "c.bin", "", 1).unwrap_err();
    assert_eq!(e.code(), "too-large");
    // Aborting frees the reservation.
    abort_upload(tmp.path(), ID, &t2).unwrap();
    let t3 = begin_upload(tmp.path(), ID, "c.bin", "", 1).unwrap();
    abort_upload(tmp.path(), ID, &t1).unwrap();
    abort_upload(tmp.path(), ID, &t3).unwrap();
    // The open-partials cap.
    let tokens: Vec<_> = (0..MAX_OPEN_PARTIALS)
        .map(|_| begin_upload(tmp.path(), ID, "p.bin", "", 1).unwrap())
        .collect();
    let e = begin_upload(tmp.path(), ID, "p.bin", "", 1).unwrap_err();
    assert_eq!(e.code(), "too-large");
    assert!(e.message().contains("open"), "{}", e.message());
    abort_upload(tmp.path(), ID, &tokens[0]).unwrap();
    begin_upload(tmp.path(), ID, "p.bin", "", 1).expect("a slot freed");
}

#[test]
fn the_file_limit_is_enforced_at_commit_and_a_refused_commit_removes_its_partial() {
    let tmp = sessions();
    for i in 0..(UPLOAD_MAX_FILES - 1) {
        put(tmp.path(), "n.txt", "", format!("file {i}").as_bytes()).unwrap();
    }
    let a = begin_upload(tmp.path(), ID, "a.bin", "", 1).unwrap();
    let b = begin_upload(tmp.path(), ID, "b.bin", "", 1).unwrap();
    append_chunk(tmp.path(), ID, &a, 0, &[0x80]).unwrap();
    append_chunk(tmp.path(), ID, &b, 0, &[0x81]).unwrap();
    commit_upload(tmp.path(), ID, &a).expect("takes the 64th slot");
    assert_eq!(
        commit_upload(tmp.path(), ID, &b).unwrap_err().code(),
        "too-large"
    );
    assert_eq!(entries(&up_dir(tmp.path(), ID)).len(), UPLOAD_MAX_FILES);
}

#[test]
fn a_commit_refused_by_the_quota_frees_its_reservation() {
    let tmp = sessions();
    let up = up_dir(tmp.path(), ID);
    let t = begin_upload(tmp.path(), ID, "a.bin", "", 4).unwrap();
    append_chunk(tmp.path(), ID, &t, 0, &[0x90; 4]).unwrap();
    // A stored file lands afterwards and eats the whole byte quota.
    let filler = std::fs::File::create(up.join("filler.bin")).unwrap();
    filler.set_len(UPLOAD_MAX_TOTAL_BYTES - 2).unwrap();
    drop(filler);
    assert_eq!(
        commit_upload(tmp.path(), ID, &t).unwrap_err().code(),
        "too-large"
    );
    assert!(
        partial_leftovers(&up).is_empty(),
        "the failed commit removed its partial"
    );
}

#[test]
fn idle_partials_are_swept_on_begin_and_stored_files_never_are() {
    let tmp = sessions();
    let up = up_dir(tmp.path(), ID);
    let stored = put(tmp.path(), "keep.txt", "", b"stored synthetic").unwrap();
    let fresh = begin_upload(tmp.path(), ID, "fresh.bin", "", 4).unwrap();
    let old = begin_upload(tmp.path(), ID, "old.bin", "", 4).unwrap();
    let age = std::time::SystemTime::now() - PARTIAL_TTL - Duration::from_secs(60);
    for name in entries(&up) {
        // Age the old partial's files and the stored file alike.
        if name.contains(&old) || name == stored.name {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(up.join(&name))
                .unwrap();
            f.set_modified(age).unwrap();
        }
    }
    let next = begin_upload(tmp.path(), ID, "next.bin", "", 4).unwrap();
    let left = entries(&up);
    assert!(
        !left.iter().any(|n| n.contains(&old)),
        "old partial swept: {left:?}"
    );
    assert!(
        left.iter().any(|n| n.contains(&fresh)),
        "fresh partial kept: {left:?}"
    );
    assert!(left.iter().any(|n| n.contains(&next)));
    assert!(left.contains(&stored.name), "stored file kept: {left:?}");
    // The standalone sweep honours its ttl argument: age the survivors by
    // 10 s; a 1 h ttl keeps them, a 5 s ttl removes them.
    let ten_s_ago = std::time::SystemTime::now() - Duration::from_secs(10);
    for name in partial_leftovers(&up) {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(up.join(&name))
            .unwrap();
        f.set_modified(ten_s_ago).unwrap();
    }
    sweep_partials(&up, Duration::from_secs(3600));
    assert_eq!(
        partial_leftovers(&up).len(),
        4,
        "two partials, data + meta each"
    );
    sweep_partials(&up, Duration::from_secs(5));
    assert!(partial_leftovers(&up).is_empty());
    assert!(entries(&up).contains(&stored.name));
}

// ------------------------------------------------- chunked ops over the socket

async fn open(
    sock: &Path,
) -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let stream = UnixStream::connect(sock).await.unwrap();
    let (rd, wr) = stream.into_split();
    (BufReader::new(rd), wr)
}

async fn call(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    wr: &mut tokio::net::unix::OwnedWriteHalf,
    line: String,
    body: &[u8],
) -> serde_json::Value {
    let mut out = format!("{line}\n").into_bytes();
    out.extend_from_slice(body);
    wr.write_all(&out).await.unwrap();
    read_reply(reader).await
}

fn begin_line(name: &str, mime: &str, size: usize) -> String {
    format!(
        "{{\"v\":1,\"op\":\"upload_begin\",\"id\":\"{ID}\",\"name\":\"{name}\",\"mime\":\"{mime}\",\"size\":{size}}}"
    )
}

fn append_line(token: &str, offset: usize, len: usize) -> String {
    format!(
        "{{\"v\":1,\"op\":\"upload_append\",\"id\":\"{ID}\",\"upload\":\"{token}\",\"offset\":{offset},\"len\":{len}}}"
    )
}

fn commit_line(token: &str) -> String {
    format!("{{\"v\":1,\"op\":\"upload_commit\",\"id\":\"{ID}\",\"upload\":\"{token}\"}}")
}

#[tokio::test]
async fn twenty_mib_in_three_chunks_over_two_connections_round_trips() {
    let served = serve_runner().await;
    let bytes = blob(20 * MIB);
    let (mut r1, mut w1) = open(&served.sock).await;
    let begin = call(
        &mut r1,
        &mut w1,
        begin_line("Big File.BIN", "application/octet-stream", bytes.len()),
        b"",
    )
    .await;
    assert_eq!(begin["status"], "ok", "{begin}");
    assert_eq!(begin["data"]["chunk_max"], CHUNK_MAX_BYTES);
    let token = begin["data"]["upload"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 32);
    let a = call(
        &mut r1,
        &mut w1,
        append_line(&token, 0, 8 * MIB),
        &bytes[..8 * MIB],
    )
    .await;
    assert_eq!(a["data"]["received"], 8 * MIB, "{a}");
    let b = call(
        &mut r1,
        &mut w1,
        append_line(&token, 8 * MIB, 8 * MIB),
        &bytes[8 * MIB..16 * MIB],
    )
    .await;
    assert_eq!(b["data"]["received"], 16 * MIB, "{b}");
    // The last chunk and the commit arrive on a DIFFERENT connection.
    let (mut r2, mut w2) = open(&served.sock).await;
    let c = call(
        &mut r2,
        &mut w2,
        append_line(&token, 16 * MIB, 4 * MIB),
        &bytes[16 * MIB..],
    )
    .await;
    assert_eq!(c["data"]["received"], 20 * MIB, "{c}");
    let done = call(&mut r2, &mut w2, commit_line(&token), b"").await;
    assert_eq!(done["status"], "ok", "{done}");
    let d = &done["data"];
    assert_eq!(d["sha256"], sha_hex(&bytes));
    assert_eq!(d["kind"], "opaque");
    assert_eq!(d["bytes"], 20 * MIB);
    let want_name = format!("{}-big-file.bin", &sha_hex(&bytes)[..12]);
    assert_eq!(d["name"], want_name);
    let path = PathBuf::from(d["path"].as_str().unwrap());
    assert_eq!(path, served.sessions.join(ID).join("up").join(&want_name));
    assert_eq!(sha_hex(&std::fs::read(&path).unwrap()), sha_hex(&bytes));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(partial_leftovers(&up_dir(&served.sessions, ID)).is_empty());
    served.stop().await;
}

#[tokio::test]
async fn an_offset_mismatch_closes_the_connection_and_a_new_one_resumes() {
    let served = serve_runner().await;
    let (mut r, mut w) = open(&served.sock).await;
    let begin = call(&mut r, &mut w, begin_line("f.txt", "text/plain", 10), b"").await;
    let token = begin["data"]["upload"].as_str().unwrap().to_string();
    let a = call(&mut r, &mut w, append_line(&token, 0, 4), b"abcd").await;
    assert_eq!(a["data"]["received"], 4);
    let bad = call(&mut r, &mut w, append_line(&token, 0, 3), b"").await;
    assert_eq!(bad["status"], "err", "{bad}");
    assert_eq!(bad["code"], "bad-request");
    let mut rest = Vec::new();
    let n = tokio::time::timeout(DEADLINE, r.read_to_end(&mut rest))
        .await
        .expect("close")
        .unwrap();
    assert_eq!(
        n, 0,
        "the connection ends after a refusal that left a body unread"
    );
    let (mut r2, mut w2) = open(&served.sock).await;
    let b = call(&mut r2, &mut w2, append_line(&token, 4, 6), b"efghij").await;
    assert_eq!(b["data"]["received"], 10, "{b}");
    let done = call(&mut r2, &mut w2, commit_line(&token), b"").await;
    assert_eq!(done["data"]["kind"], "text", "{done}");
    assert_eq!(
        std::fs::read(done["data"]["path"].as_str().unwrap()).unwrap(),
        b"abcdefghij"
    );
    served.stop().await;
}

#[tokio::test]
async fn a_chunk_over_the_cap_is_refused_without_reading_the_body() {
    let served = serve_runner().await;
    let (mut r, mut w) = open(&served.sock).await;
    let begin = call(&mut r, &mut w, begin_line("f.bin", "", 20 * MIB), b"").await;
    let token = begin["data"]["upload"].as_str().unwrap().to_string();
    // Line only: a runner waiting for the body would hit the read deadline.
    let reply = call(&mut r, &mut w, append_line(&token, 0, 8 * MIB + 1), b"").await;
    assert_eq!(reply["status"], "err", "{reply}");
    assert_eq!(reply["code"], "too-large");
    let mut rest = Vec::new();
    let n = tokio::time::timeout(DEADLINE, r.read_to_end(&mut rest))
        .await
        .expect("close")
        .unwrap();
    assert_eq!(n, 0);
    served.stop().await;
}

#[tokio::test]
async fn an_append_past_the_declared_size_and_an_early_commit_are_refused() {
    let served = serve_runner().await;
    let (mut r, mut w) = open(&served.sock).await;
    let begin = call(&mut r, &mut w, begin_line("f.bin", "", 4), b"").await;
    let token = begin["data"]["upload"].as_str().unwrap().to_string();
    let early = call(&mut r, &mut w, commit_line(&token), b"").await;
    assert_eq!(early["code"], "bad-request", "{early}");
    let over = call(&mut r, &mut w, append_line(&token, 0, 5), b"").await;
    assert_eq!(over["code"], "bad-request", "{over}");
    let (mut r2, mut w2) = open(&served.sock).await;
    let big = call(
        &mut r2,
        &mut w2,
        begin_line("big.bin", "", 32 * MIB + 1),
        b"",
    )
    .await;
    assert_eq!(big["code"], "too-large", "{big}");
    served.stop().await;
}

#[tokio::test]
async fn a_short_chunk_body_closes_the_connection_and_leaves_the_offset_unchanged() {
    let served = serve_runner().await;
    let (mut r, mut w) = open(&served.sock).await;
    let begin = call(&mut r, &mut w, begin_line("f.bin", "", 100), b"").await;
    let token = begin["data"]["upload"].as_str().unwrap().to_string();
    let mut out = format!("{}\n", append_line(&token, 0, 50)).into_bytes();
    out.extend_from_slice(&[0x80; 20]); // 30 bytes short
    w.write_all(&out).await.unwrap();
    w.shutdown().await.unwrap();
    let reply = read_reply(&mut r).await;
    assert_eq!(reply["code"], "io", "{reply}");
    let (mut r2, mut w2) = open(&served.sock).await;
    // The offset is still 0: an append at 0 succeeds, one at 20 does not.
    let bad = call(&mut r2, &mut w2, append_line(&token, 20, 1), b"").await;
    assert_eq!(bad["code"], "bad-request", "{bad}");
    let (mut r3, mut w3) = open(&served.sock).await;
    let good = call(&mut r3, &mut w3, append_line(&token, 0, 100), &[0x81; 100]).await;
    assert_eq!(good["data"]["received"], 100, "{good}");
    served.stop().await;
}

#[tokio::test]
async fn abort_and_a_declared_picture_mismatch_work_over_the_socket() {
    let served = serve_runner().await;
    let (mut r, mut w) = open(&served.sock).await;
    let begin = call(
        &mut r,
        &mut w,
        begin_line("p.png", "image/png", jpeg().len()),
        b"",
    )
    .await;
    let token = begin["data"]["upload"].as_str().unwrap().to_string();
    let jp = jpeg();
    let a = call(&mut r, &mut w, append_line(&token, 0, jp.len()), &jp).await;
    assert_eq!(a["status"], "ok", "{a}");
    let done = call(&mut r, &mut w, commit_line(&token), b"").await;
    assert_eq!(done["code"], "bad-image", "{done}");
    assert!(partial_leftovers(&up_dir(&served.sessions, ID)).is_empty());
    // Abort of an unknown token is ok, over the wire too.
    let ab = call(
        &mut r,
        &mut w,
        format!(
            "{{\"v\":1,\"op\":\"upload_abort\",\"id\":\"{ID}\",\"upload\":\"{}\"}}",
            "0".repeat(32)
        ),
        b"",
    )
    .await;
    assert_eq!(ab["status"], "ok", "{ab}");
    let bad = call(&mut r, &mut w, commit_line("../../etc"), b"").await;
    assert_eq!(bad["code"], "bad-request", "{bad}");
    served.stop().await;
}
