//! The runner's `fs_ls` / `fs_get` ops (FXLS): the containment and bound
//! rules through the public `kb_term::runner::files` API on real temp
//! trees, and the wire protocol (a header line, then exactly `len` raw
//! bytes) through a real `runner.sock`. Every file is synthetic filler
//! generated here. Every blocking call or socket read carries a deadline.

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use kb_term::runner::files::{fs_get, fs_ls, LS_MAX_ENTRIES, PATH_MAX_BYTES};
use kb_term::runner::supervisor::DirectSupervisor;
use kb_term::runner::upload::{up_dir, CHUNK_MAX_BYTES, FILE_MAX_BYTES};
use kb_term::runner::{Runner, RunnerConfig, RunnerError};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const ID: &str = "abcd1234";
const DEADLINE: Duration = Duration::from_secs(10);
const MIB: usize = 1024 * 1024;

fn code<T: std::fmt::Debug>(r: Result<T, RunnerError>) -> &'static str {
    r.expect_err("expected a refusal").code()
}

fn filler(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(31).wrapping_add(i >> 8)) as u8)
        .collect()
}

fn sha_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A tree: `<tmp>/root/{a.txt, sub/b.txt, ...}` and `<tmp>/outside/secret.txt`.
struct Tree {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
}

fn tree() -> Tree {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("a.txt"), b"alpha").unwrap();
    std::fs::write(root.join("sub").join("b.txt"), b"bravo").unwrap();
    std::fs::write(outside.join("secret.txt"), b"top secret").unwrap();
    Tree {
        _tmp: tmp,
        root,
        outside,
    }
}

/// Run `f` on a thread with a hard deadline (a FIFO must never hang the runner).
fn with_deadline<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(DEADLINE)
        .expect("the call must return, a fifo must never block it")
}

fn mkfifo(path: &Path) {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `c` is a valid NUL-terminated path for the call.
    let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo failed");
}

// ------------------------------------------------------------ containment

#[test]
fn a_regular_file_under_the_root_is_served_relative_or_absolute() {
    let t = tree();
    for p in ["a.txt", "./a.txt", "sub/../a.txt"] {
        let (head, bytes) = fs_get(&t.root, p, 0, 64).unwrap();
        assert_eq!(bytes, b"alpha", "{p}");
        assert_eq!((head.size, head.len, head.name.as_str()), (5, 5, "a.txt"));
        assert!(head.mtime_unix > 1_000_000_000, "{p}");
    }
    let abs = t.root.join("sub").join("b.txt");
    let (head, bytes) = fs_get(&t.root, abs.to_str().unwrap(), 0, 64).unwrap();
    assert_eq!(bytes, b"bravo");
    assert_eq!(head.name, "b.txt");
}

#[test]
fn dotdot_and_absolute_paths_outside_the_root_are_refused_as_outside_root() {
    let t = tree();
    let esc_abs = format!("{}/../outside/secret.txt", t.root.display());
    let outside_abs = t.outside.join("secret.txt");
    for p in [
        "../outside/secret.txt",
        "sub/../../outside/secret.txt",
        "..",
        "../",
        esc_abs.as_str(),
        outside_abs.to_str().unwrap(),
        "/etc/passwd",
        "/dev/null",
        "/proc/self/exe",
        "/",
    ] {
        assert_eq!(code(fs_get(&t.root, p, 0, 64)), "outside-root", "get {p}");
        assert_eq!(code(fs_ls(&t.root, "cwd", p)), "outside-root", "ls {p}");
    }
}

#[test]
fn a_string_prefix_sibling_of_the_root_is_not_inside_it() {
    let t = tree();
    let sibling = PathBuf::from(format!("{}2", t.root.display()));
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(sibling.join("x.txt"), b"x").unwrap();
    let p = sibling.join("x.txt");
    assert_eq!(
        code(fs_get(&t.root, p.to_str().unwrap(), 0, 8)),
        "outside-root"
    );
}

#[test]
fn symlinks_are_never_followed_to_a_file_a_directory_a_loop_or_a_magic_link() {
    let t = tree();
    let sym = |target: &Path, name: &str| {
        std::os::unix::fs::symlink(target, t.root.join(name)).unwrap();
    };
    sym(&t.outside.join("secret.txt"), "link_out_file");
    sym(&t.outside, "link_out_dir");
    // An in-root link is refused as well: no symlink is ever followed.
    sym(&t.root.join("a.txt"), "link_in");
    sym(&t.root.join("sub"), "link_in_dir");
    sym(Path::new("/proc/self/exe"), "link_magic");
    sym(&t.root.join("loop_b"), "loop_a");
    sym(&t.root.join("loop_a"), "loop_b");
    for p in [
        "link_out_file",
        "link_out_dir/secret.txt",
        "link_in",
        "link_in_dir/b.txt",
        "link_magic",
        "loop_a",
        "loop_a/x",
    ] {
        assert_eq!(code(fs_get(&t.root, p, 0, 64)), "not-a-file", "get {p}");
    }
    for p in ["link_out_dir", "link_in_dir", "loop_b"] {
        assert_eq!(code(fs_ls(&t.root, "cwd", p)), "not-a-file", "ls {p}");
    }
}

#[test]
fn a_root_reached_through_a_link_works_and_still_contains() {
    // The root path itself is trusted (it is the recorded launch cwd), so a
    // root reached through a link works; what is inside it is judged by the kernel.
    let t = tree();
    let link_root = t._tmp.path().join("root_link");
    std::os::unix::fs::symlink(&t.root, &link_root).unwrap();
    let (_, bytes) = fs_get(&link_root, "a.txt", 0, 8).unwrap();
    assert_eq!(bytes, b"alpha");
    assert_eq!(
        code(fs_get(&link_root, "../outside/secret.txt", 0, 8)),
        "outside-root"
    );
}

#[test]
fn a_fifo_a_socket_and_a_directory_are_not_files_and_a_fifo_never_blocks() {
    let t = tree();
    mkfifo(&t.root.join("pipe"));
    let _listener = std::os::unix::net::UnixListener::bind(t.root.join("sock")).unwrap();
    let root = t.root.clone();
    let codes = with_deadline(move || {
        ["pipe", "sock", "sub", "."]
            .iter()
            .map(|p| fs_get(&root, p, 0, 8).map(|_| ()).map_err(|e| e.code()))
            .collect::<Vec<_>>()
    });
    for c in codes {
        assert_eq!(c, Err("not-a-file"));
    }
    let root = t.root.clone();
    let ls_fifo = with_deadline(move || fs_ls(&root, "cwd", "pipe").map_err(|e| e.code()));
    assert_eq!(ls_fifo.unwrap_err(), "not-a-file");
}

#[test]
fn a_device_node_is_refused_even_when_named_absolutely_and_a_listing_of_a_file_is_not_a_file() {
    let t = tree();
    assert_eq!(code(fs_get(&t.root, "/dev/null", 0, 8)), "outside-root");
    assert_eq!(code(fs_ls(&t.root, "cwd", "a.txt")), "not-a-file");
    assert_eq!(code(fs_ls(&t.root, "cwd", "nope")), "not-found");
    assert_eq!(code(fs_get(&t.root, "nope", 0, 8)), "not-found");
    assert_eq!(code(fs_get(&t.root, "a.txt/x", 0, 8)), "not-found");
}

#[test]
fn an_over_long_path_or_component_or_a_nul_is_a_bad_request() {
    let t = tree();
    let long = "a/".repeat(PATH_MAX_BYTES);
    assert_eq!(code(fs_get(&t.root, &long, 0, 8)), "bad-request");
    assert_eq!(code(fs_ls(&t.root, "cwd", &long)), "bad-request");
    let exact_over = "a".repeat(PATH_MAX_BYTES + 1);
    assert_eq!(code(fs_get(&t.root, &exact_over, 0, 8)), "bad-request");
    let long_component = "c".repeat(300);
    assert_eq!(code(fs_get(&t.root, &long_component, 0, 8)), "bad-request");
    assert_eq!(code(fs_get(&t.root, "a\0b", 0, 8)), "bad-request");
}

// ------------------------------------------------------------------ caps

#[test]
fn a_file_of_exactly_the_cap_is_served_and_one_byte_over_is_too_large_before_any_byte() {
    let t = tree();
    let at_cap = t.root.join("at_cap.bin");
    std::fs::File::create(&at_cap)
        .unwrap()
        .set_len(FILE_MAX_BYTES)
        .unwrap();
    let (head, bytes) = fs_get(&t.root, "at_cap.bin", 0, CHUNK_MAX_BYTES).unwrap();
    assert_eq!(head.size, FILE_MAX_BYTES);
    assert_eq!(bytes.len() as u64, CHUNK_MAX_BYTES);
    // The last chunk of a cap-sized file is reachable.
    let (head, bytes) = fs_get(&t.root, "at_cap.bin", FILE_MAX_BYTES - 10, 100).unwrap();
    assert_eq!((head.len, bytes.len()), (10, 10));

    let over = t.root.join("over.bin");
    std::fs::File::create(&over)
        .unwrap()
        .set_len(FILE_MAX_BYTES + 1)
        .unwrap();
    for offset in [0, CHUNK_MAX_BYTES] {
        assert_eq!(
            code(fs_get(&t.root, "over.bin", offset, 1024)),
            "too-large",
            "offset {offset}"
        );
    }
}

#[test]
fn a_chunk_over_the_chunk_cap_is_refused_and_the_cap_itself_is_served() {
    let t = tree();
    std::fs::write(t.root.join("c.bin"), filler(MIB)).unwrap();
    assert_eq!(
        code(fs_get(&t.root, "c.bin", 0, CHUNK_MAX_BYTES + 1)),
        "too-large"
    );
    let (head, _) = fs_get(&t.root, "c.bin", 0, CHUNK_MAX_BYTES).unwrap();
    assert_eq!(head.len, MIB as u64, "clipped at end of file");
}

#[test]
fn twenty_mib_in_chunks_reassembles_with_an_equal_sha256() {
    let t = tree();
    let data = filler(20 * MIB);
    std::fs::write(t.root.join("big.bin"), &data).unwrap();
    let mut got = Vec::new();
    let mut offset = 0u64;
    let mut sizes = Vec::new();
    loop {
        let (head, bytes) = fs_get(&t.root, "big.bin", offset, CHUNK_MAX_BYTES).unwrap();
        assert_eq!(head.size, data.len() as u64);
        if bytes.is_empty() {
            break;
        }
        sizes.push(bytes.len());
        offset += bytes.len() as u64;
        got.extend_from_slice(&bytes);
    }
    assert_eq!(sizes, vec![8 * MIB, 8 * MIB, 4 * MIB]);
    assert_eq!(sha_hex(&got), sha_hex(&data));
}

#[test]
fn an_offset_past_the_end_is_refused_at_the_end_is_empty_and_an_empty_file_is_served() {
    let t = tree();
    assert_eq!(code(fs_get(&t.root, "a.txt", 6, 8)), "bad-request");
    let (head, bytes) = fs_get(&t.root, "a.txt", 5, 8).unwrap();
    assert_eq!((head.size, head.len, bytes.len()), (5, 0, 0));
    std::fs::write(t.root.join("empty"), b"").unwrap();
    let (head, bytes) = fs_get(&t.root, "empty", 0, 8).unwrap();
    assert_eq!((head.size, head.len, bytes.len()), (0, 0, 0));
}

// -------------------------------------------------------------- listings

#[test]
fn a_listing_of_500_entries_is_whole_and_501_is_truncated_to_the_first_500() {
    let t = tree();
    let dir = t.root.join("many");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..LS_MAX_ENTRIES {
        std::fs::write(dir.join(format!("f{i:04}")), b"").unwrap();
    }
    let at_cap = fs_ls(&t.root, "cwd", "many").unwrap();
    assert_eq!(at_cap.entries.len(), LS_MAX_ENTRIES);
    assert!(!at_cap.truncated, "exactly the cap is not truncated");

    std::fs::write(dir.join("f0500"), b"").unwrap();
    let over = fs_ls(&t.root, "cwd", "many").unwrap();
    assert_eq!(over.entries.len(), LS_MAX_ENTRIES);
    assert!(over.truncated, "one over the cap says so");
    assert_eq!(over.entries[0].name, "f0000");
    assert_eq!(over.entries[LS_MAX_ENTRIES - 1].name, "f0499");
    assert!(over.entries.iter().all(|e| e.name != "f0500"));
}

#[test]
fn a_listing_is_directories_first_then_bytewise_with_honest_metadata() {
    let t = tree();
    let d = t.root.join("ls");
    std::fs::create_dir_all(d.join("adir")).unwrap();
    std::fs::create_dir_all(d.join("Zdir")).unwrap();
    for (n, body) in [
        ("é.txt", "12345"),
        ("a.txt", "12"),
        ("B.txt", ""),
        (".hidden", "1234567"),
    ] {
        std::fs::write(d.join(n), body).unwrap();
    }
    let out = fs_ls(&t.root, "cwd", "ls").unwrap();
    let got: Vec<(&str, &str, u64, bool)> = out
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.kind.as_str(), e.bytes, e.hidden))
        .collect();
    assert_eq!(
        got,
        vec![
            ("Zdir", "dir", 0, false),
            ("adir", "dir", 0, false),
            (".hidden", "file", 7, true),
            ("B.txt", "file", 0, false),
            ("a.txt", "file", 2, false),
            ("é.txt", "file", 5, false),
        ]
    );
    assert!(!out.truncated);
    assert_eq!(
        (out.root.as_str(), out.dir.as_str(), out.skipped),
        ("cwd", "ls", 0)
    );
    assert!(out.entries.iter().all(|e| e.mtime_unix > 1_000_000_000));
    // The same directory under every spelling of its name.
    let same = fs_ls(&t.root, "cwd", "ls/").unwrap();
    assert_eq!(same.entries, out.entries);
    let abs = d.to_str().unwrap();
    assert_eq!(fs_ls(&t.root, "cwd", abs).unwrap().entries, out.entries);
}

#[test]
fn symlinks_fifos_sockets_and_unrepresentable_names_are_left_out_and_counted() {
    let t = tree();
    let d = t.root.join("odd");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("ok.txt"), b"x").unwrap();
    std::os::unix::fs::symlink(t.outside.join("secret.txt"), d.join("link")).unwrap();
    mkfifo(&d.join("pipe"));
    let _l = std::os::unix::net::UnixListener::bind(d.join("sock")).unwrap();
    // A name that is not UTF-8 and one with a control character.
    std::fs::write(d.join(std::ffi::OsStr::from_bytes(b"bad\xff.txt")), b"x").unwrap();
    std::fs::write(d.join("ctl\u{1}name"), b"x").unwrap();
    let root = t.root.clone();
    let out = with_deadline(move || fs_ls(&root, "cwd", "odd")).unwrap();
    let names: Vec<&str> = out.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["ok.txt"]);
    assert_eq!(out.skipped, 5, "link, pipe, sock, bad name, control name");
}

#[test]
fn the_root_lists_under_every_spelling_and_dir_is_empty() {
    let t = tree();
    for p in ["", ".", "./", t.root.to_str().unwrap()] {
        let out = fs_ls(&t.root, "up", p).unwrap();
        assert_eq!(out.dir, "", "{p:?}");
        assert_eq!(out.root, "up");
        let names: Vec<&str> = out.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["sub", "a.txt"], "{p:?}");
    }
}

// ----------------------------------------------------- over the socket

struct Served {
    _tmp: tempfile::TempDir,
    sock: PathBuf,
    sessions: PathBuf,
    cwd: PathBuf,
    shutdown: tokio::sync::broadcast::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

/// A runner whose "tmux" is a tiny script answering `@kb_cwd` with `cwd`
/// (or nothing when `cwd_recorded` is false): no real tmux is involved.
async fn serve_runner(cwd_recorded: bool) -> Served {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run");
    let sessions = run_dir.join("s");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join(format!("{ID}.sock")), b"").unwrap();
    let cwd = tmp.path().join("work");
    std::fs::create_dir_all(cwd.join("sub")).unwrap();
    std::fs::write(cwd.join("hello.txt"), b"hello from cwd").unwrap();
    let script = tmp.path().join("fake-tmux.sh");
    let answer = if cwd_recorded {
        format!("printf '%s\\n' '{}'", cwd.display())
    } else {
        "true".to_string()
    };
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\nif [ \"$last\" = \"@kb_cwd\" ]; then {answer}; fi\nexit 0\n"
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let sock_dir = run_dir.join("runner");
    std::fs::create_dir_all(&sock_dir).unwrap();
    let sock = sock_dir.join("runner.sock");
    let cfg = RunnerConfig::test_config(
        run_dir,
        script.to_string_lossy().into_owned(),
        tmp.path().join("conf"),
    );
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
        cwd,
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

type Reader = BufReader<tokio::net::unix::OwnedReadHalf>;
type Writer = tokio::net::unix::OwnedWriteHalf;

async fn open(sock: &Path) -> (Reader, Writer) {
    let stream = UnixStream::connect(sock).await.unwrap();
    let (rd, wr) = stream.into_split();
    (BufReader::new(rd), wr)
}

async fn read_reply(reader: &mut Reader) -> serde_json::Value {
    let mut line = String::new();
    let n = tokio::time::timeout(DEADLINE, reader.read_line(&mut line))
        .await
        .expect("reply deadline")
        .expect("reply read");
    assert!(n > 0, "runner closed without replying");
    serde_json::from_str(line.trim()).expect("reply is JSON")
}

async fn call(reader: &mut Reader, wr: &mut Writer, line: &str) -> serde_json::Value {
    wr.write_all(format!("{line}\n").as_bytes()).await.unwrap();
    read_reply(reader).await
}

/// An `fs_get` that must succeed: the header, then exactly `len` bytes.
async fn get_ok(reader: &mut Reader, wr: &mut Writer, line: &str) -> (serde_json::Value, Vec<u8>) {
    let reply = call(reader, wr, line).await;
    assert_eq!(reply["status"], "ok", "reply: {reply}");
    let len = reply["data"]["len"].as_u64().unwrap() as usize;
    let mut body = vec![0u8; len];
    tokio::time::timeout(DEADLINE, reader.read_exact(&mut body))
        .await
        .expect("body deadline")
        .expect("body read");
    (reply["data"].clone(), body)
}

fn ls_line(root: &str, path: &str) -> String {
    format!("{{\"v\":1,\"op\":\"fs_ls\",\"id\":\"{ID}\",\"root\":\"{root}\",\"path\":\"{path}\"}}")
}

fn get_line(root: &str, path: &str, offset: u64, len: u64) -> String {
    format!(
        "{{\"v\":1,\"op\":\"fs_get\",\"id\":\"{ID}\",\"root\":\"{root}\",\"path\":\"{path}\",\"offset\":{offset},\"len\":{len}}}"
    )
}

#[tokio::test]
async fn the_up_root_lists_and_reads_over_the_socket_with_exact_framing() {
    let served = serve_runner(true).await;
    let up = up_dir(&served.sessions, ID);
    std::fs::create_dir_all(&up).unwrap();
    let data = filler(3 * MIB);
    std::fs::write(up.join("0123456789ab-data.bin"), &data).unwrap();
    let (mut r, mut w) = open(&served.sock).await;

    let ls = call(
        &mut r,
        &mut w,
        &format!("{{\"v\":1,\"op\":\"fs_ls\",\"id\":\"{ID}\",\"root\":\"up\",\"path\":\"\"}}"),
    )
    .await;
    assert_eq!(ls["status"], "ok", "reply: {ls}");
    assert_eq!(ls["data"]["root"], "up");
    assert_eq!(ls["data"]["entries"][0]["name"], "0123456789ab-data.bin");
    assert_eq!(ls["data"]["entries"][0]["bytes"], data.len());
    assert_eq!(ls["data"]["truncated"], false);

    // Two reads on the SAME connection: the framing leaves nothing behind.
    let (head, body) = get_ok(
        &mut r,
        &mut w,
        &get_line("up", "0123456789ab-data.bin", 0, 1024),
    )
    .await;
    assert_eq!(head["size"], data.len());
    assert_eq!(head["len"], 1024);
    assert_eq!(head["name"], "0123456789ab-data.bin");
    assert_eq!(body, data[..1024]);
    let (_, body2) = get_ok(
        &mut r,
        &mut w,
        &get_line("up", "0123456789ab-data.bin", 2048, 4096),
    )
    .await;
    assert_eq!(body2, data[2048..2048 + 4096]);
    // An absolute path under the up root is accepted too.
    let abs = up.join("0123456789ab-data.bin");
    let (_, body3) = get_ok(&mut r, &mut w, &get_line("up", abs.to_str().unwrap(), 0, 8)).await;
    assert_eq!(body3, data[..8]);
    served.stop().await;
}

#[tokio::test]
async fn the_cwd_root_is_the_launch_recorded_directory() {
    let served = serve_runner(true).await;
    let (mut r, mut w) = open(&served.sock).await;
    let ls = call(&mut r, &mut w, &ls_line("cwd", "")).await;
    assert_eq!(ls["status"], "ok", "reply: {ls}");
    let names: Vec<&str> = ls["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["sub", "hello.txt"]);
    let abs = served.cwd.join("hello.txt");
    let (head, body) = get_ok(
        &mut r,
        &mut w,
        &get_line("cwd", abs.to_str().unwrap(), 0, 1024),
    )
    .await;
    assert_eq!(body, b"hello from cwd");
    assert_eq!(head["name"], "hello.txt");
    // The up area is a different root: the same absolute path is outside it.
    let r2 = call(&mut r, &mut w, &get_line("up", abs.to_str().unwrap(), 0, 8)).await;
    assert_eq!(r2["code"], "outside-root", "reply: {r2}");
    served.stop().await;
}

#[tokio::test]
async fn a_session_with_no_recorded_cwd_has_no_cwd_root() {
    let served = serve_runner(false).await;
    let (mut r, mut w) = open(&served.sock).await;
    let ls = call(&mut r, &mut w, &ls_line("cwd", "")).await;
    assert_eq!(ls["code"], "not-found", "reply: {ls}");
    served.stop().await;
}

#[tokio::test]
async fn refusals_are_err_envelopes_with_no_bytes_and_the_connection_stays_in_step() {
    let served = serve_runner(true).await;
    std::os::unix::fs::symlink("/etc/passwd", served.cwd.join("pw")).unwrap();
    std::fs::File::create(served.cwd.join("over.bin"))
        .unwrap()
        .set_len(FILE_MAX_BYTES + 1)
        .unwrap();
    let (mut r, mut w) = open(&served.sock).await;
    for (line, want) in [
        (get_line("cwd", "../x", 0, 8), "outside-root"),
        (get_line("cwd", "/etc/passwd", 0, 8), "outside-root"),
        (get_line("cwd", "pw", 0, 8), "not-a-file"),
        (get_line("cwd", "sub", 0, 8), "not-a-file"),
        (get_line("cwd", "missing", 0, 8), "not-found"),
        (get_line("cwd", "over.bin", 0, 8), "too-large"),
        (get_line("cwd", "hello.txt", 99, 8), "bad-request"),
        (
            get_line("cwd", "hello.txt", 0, CHUNK_MAX_BYTES + 1),
            "too-large",
        ),
        (get_line("home", "hello.txt", 0, 8), "bad-request"),
        (get_line("", "hello.txt", 0, 8), "bad-request"),
    ] {
        let reply = call(&mut r, &mut w, &line).await;
        assert_eq!(reply["status"], "err", "{line} -> {reply}");
        assert_eq!(reply["code"], want, "{line} -> {reply}");
    }
    // After ten refusals the next request is still answered in step.
    let (_, body) = get_ok(&mut r, &mut w, &get_line("cwd", "hello.txt", 0, 5)).await;
    assert_eq!(body, b"hello");
    // Bad id and an unknown session are refused before any path is built.
    let bad = call(
        &mut r,
        &mut w,
        "{\"v\":1,\"op\":\"fs_get\",\"id\":\"../x\",\"root\":\"up\",\"path\":\"a\",\"offset\":0,\"len\":1}",
    )
    .await;
    assert_eq!(bad["code"], "invalid-id", "reply: {bad}");
    let gone = call(
        &mut r,
        &mut w,
        "{\"v\":1,\"op\":\"fs_ls\",\"id\":\"ffff0000\",\"root\":\"up\",\"path\":\"\"}",
    )
    .await;
    assert_eq!(gone["code"], "not-found", "reply: {gone}");
    // An unknown field is a bad request, never silently dropped.
    let extra = call(
        &mut r,
        &mut w,
        &format!("{{\"v\":1,\"op\":\"fs_ls\",\"id\":\"{ID}\",\"root\":\"up\",\"path\":\"\",\"deep\":true}}"),
    )
    .await;
    assert_eq!(extra["code"], "bad-request", "reply: {extra}");
    served.stop().await;
}

#[tokio::test]
async fn a_fifo_named_over_the_socket_is_refused_without_stalling_the_runner() {
    let served = serve_runner(true).await;
    mkfifo(&served.cwd.join("pipe"));
    let (mut r, mut w) = open(&served.sock).await;
    let reply = call(&mut r, &mut w, &get_line("cwd", "pipe", 0, 8)).await;
    assert_eq!(reply["code"], "not-a-file", "reply: {reply}");
    let (_, body) = get_ok(&mut r, &mut w, &get_line("cwd", "hello.txt", 0, 5)).await;
    assert_eq!(body, b"hello");
    served.stop().await;
}
