//! End-to-end tests driving the real axum router + a real `LspClient`
//! against the fake LSP fixture (`src/bin/fake_lsp.rs`, spawned via
//! `env!("CARGO_BIN_EXE_kb-lip-fake-lsp")` — no real language server runs
//! in CI, per design-lip.md Phase L1). Covers: handshake, hover round-trip
//! with byte<->UTF-16 position conversion, the blob-hash guard's refusal
//! path, the references cap, restart-after-crash (degrade then recover),
//! `/lip/diagnostics`'s publishDiagnostics cache (wait path, cached
//! path, empty-after-timeout, blob-mismatch — design-addendum-2.md §D),
//! and its LSP 3.17 PULL mode (PRR-L5 — happy path, unchanged-serves-cache,
//! push fallback when pull isn't advertised, blob guard, indexing gate).

use kb_lip::config::{Config, RestartBackoff};
use kb_lip::server;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn fake_lsp_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kb-lip-fake-lsp"))
}

async fn boot(
    workspace_root: &Path,
    restart_backoff: RestartBackoff,
    diagnostics_wait_ms: u64,
) -> (Arc<kb_lip::Supervisor>, SocketAddr) {
    let config = Config {
        lang_ids: vec!["ruby".to_string()],
        command: vec![fake_lsp_path().to_string_lossy().into_owned()],
        workspace_root: workspace_root.to_path_buf(),
        port: 0,
        initialization_options: None,
        restart_backoff,
        diagnostics_wait_ms,
    };
    let (addr, supervisor, _serve_task) =
        server::serve(config, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .expect("bind + serve");
    (supervisor, addr)
}

/// `boot` with defaults for tests that don't touch `/lip/diagnostics` —
/// keeps their wait budget small (300ms, well under the 2000ms cap) so an
/// incidental wait path never slows the rest of the suite down.
async fn boot_default(workspace_root: &Path) -> (Arc<kb_lip::Supervisor>, SocketAddr) {
    boot(workspace_root, RestartBackoff::default(), 300).await
}

fn write_fixture(dir: &Path, rel: &str, content: &str) -> PathBuf {
    let abs = dir.join(rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, content).unwrap();
    abs
}

async fn post_json(addr: SocketAddr, route: &str, body: Value) -> Value {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}{route}"))
        .json(&body)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        200,
        "lip/1 never answers a refusal with a 5xx"
    );
    resp.json().await.expect("json body")
}

/// Polls `GET /lip/identity` until `indexing` reads `want` or `timeout`
/// elapses. Needed because a fake-LSP's `$/progress` notifications travel
/// over the child's stdio pipe and are processed by kb-lip's reader task
/// asynchronously — there is no synchronous guarantee that they've landed
/// by the time `server::serve()`/`boot()` returns, so a bare single check
/// right after boot would be racy.
async fn poll_until_indexing(addr: SocketAddr, want: bool, timeout: std::time::Duration) -> Value {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let id = get_json(addr, "/lip/identity").await;
        if id["indexing"] == json!(want) || tokio::time::Instant::now() >= deadline {
            return id;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

async fn get_json(addr: SocketAddr, route: &str) -> Value {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}{route}"))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    resp.json().await.expect("json body")
}

// ---------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------

#[tokio::test]
async fn handshake_completes_and_identity_reports_the_fake_server() {
    let dir = tempfile::tempdir().unwrap();
    let (_sup, addr) = boot_default(dir.path()).await;

    let id = get_json(addr, "/lip/identity").await;
    assert_eq!(id["protocol"], json!("lip/1"));
    assert_eq!(id["lip_major"], json!(1));
    assert_eq!(id["langs"], json!(["ruby"]));
    assert_eq!(id["healthy"], json!(true));
    assert_eq!(id["server_name"], json!("fake-lsp"));
    assert_eq!(id["server_version"], json!("0.1.0-test"));
    assert!(
        id["pid"].is_number(),
        "identity should surface the LSP child's pid: {id}"
    );
    assert!(id["uptime_secs"].is_number());
}

// ---------------------------------------------------------------------
// Hover round-trip + position conversion (byte offset <-> UTF-16)
// ---------------------------------------------------------------------

#[tokio::test]
async fn hover_round_trip_converts_positions_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/greeting.rb";
    // Same multibyte fixture as position.rs's unit tests: a 4-byte astral
    // char (surrogate pair in UTF-16) is what actually exercises the
    // byte<->UTF-16 conversion rather than a byte-count coincidence.
    let content = "héllo 😀 wörld\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = boot_default(dir.path()).await;

    let line = content.lines().next().unwrap();
    let emoji_byte = line.find('\u{1F600}').unwrap();
    let after_emoji_byte = emoji_byte + '\u{1F600}'.len_utf8();
    let expected_utf16 = kb_lip::position::byte_col_to_utf16(line, after_emoji_byte);

    let body = post_json(
        addr,
        "/lip/hover",
        json!({"path": rel, "blob_sha": sha, "line": 1, "col": after_emoji_byte}),
    )
    .await;

    assert!(body.get("refused").is_none(), "unexpected refusal: {body}");
    assert_eq!(body["verified_blob_sha"], json!(sha));

    // The fake LSP echoes the LSP position it received (0-based line,
    // UTF-16 character) into the hover markdown — this is the assertion
    // that the ADAPTER converted our byte offset to UTF-16 correctly
    // before ever sending the request.
    let contents = body["results"][0]["contents"].as_str().unwrap();
    assert_eq!(
        contents,
        format!("hover at line=0 character={expected_utf16}")
    );

    // The fake LSP's range is {start: same position, end: character+5} on
    // the SAME (0-based) line — the adapter must convert that back into
    // lip/1's 1-based line / byte column shape.
    assert_eq!(body["results"][0]["range"]["line"], json!(1));
    assert_eq!(body["results"][0]["range"]["col"], json!(after_emoji_byte));
    let expected_end_byte = kb_lip::position::utf16_col_to_byte(line, expected_utf16 + 5) as u64;
    assert_eq!(
        body["results"][0]["range"]["end_col"],
        json!(expected_end_byte)
    );
}

// ---------------------------------------------------------------------
// Blob-hash guard refusal
// ---------------------------------------------------------------------

#[tokio::test]
async fn blob_guard_refuses_when_file_changed_since_the_caller_hashed_it() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let original = "original content\n";
    write_fixture(dir.path(), rel, original);
    let stale_sha = kb_lip::blob::git_blob_sha1(original.as_bytes());

    let (_sup, addr) = boot_default(dir.path()).await;

    // Mutate the file AFTER the caller computed its hash but BEFORE
    // sending the request — the adapter must refuse rather than answer
    // against stale content.
    let mutated = "mutated content, completely different bytes\n";
    write_fixture(dir.path(), rel, mutated);
    let current_sha = kb_lip::blob::git_blob_sha1(mutated.as_bytes());
    assert_ne!(stale_sha, current_sha);

    let body = post_json(
        addr,
        "/lip/hover",
        json!({"path": rel, "blob_sha": stale_sha, "line": 1, "col": 0}),
    )
    .await;

    assert_eq!(body["refused"], json!("blob_mismatch"));
    assert_eq!(body["have"], json!(current_sha));
    assert_eq!(body["want"], json!(stale_sha));
    // A refusal must never carry a verified_blob_sha/results — it's a
    // distinct shape, not a degraded success.
    assert!(body.get("results").is_none());
}

// ---------------------------------------------------------------------
// References cap
// ---------------------------------------------------------------------

// Guards process-global env var mutation (`FAKE_LSP_REF_COUNT` here;
// `FAKE_LSP_DIAG_SUPPRESS`/`FAKE_LSP_DIAG_DELAY_MS` further down),
// mirroring kb-core's own `embed_ipc::tests::ENV_LOCK` precedent for
// exactly the same hazard: env vars are process-wide, and cargo test runs
// `#[tokio::test]` functions on separate OS threads by default. A
// `tokio::sync::Mutex` (not `std::sync::Mutex`) because the guard is held
// across a `boot().await` below — an async-aware lock is required there
// (clippy::await_holding_lock).
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn references_are_capped_at_200() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("FAKE_LSP_REF_COUNT", "250");
        let booted = boot_default(dir.path()).await;
        std::env::remove_var("FAKE_LSP_REF_COUNT");
        booted
    };

    let body = post_json(
        addr,
        "/lip/references",
        json!({"path": rel, "blob_sha": sha, "line": 1, "col": 0}),
    )
    .await;

    assert!(body.get("refused").is_none(), "unexpected refusal: {body}");
    let results = body["results"].as_array().expect("results array");
    assert_eq!(
        results.len(),
        kb_lip::http::REFERENCES_CAP,
        "the fake server offered 250 references; lip/1 must cap at {}",
        kb_lip::http::REFERENCES_CAP
    );
}

// ---------------------------------------------------------------------
// LocationLink[] definition replies (PRR-L4 regression pin)
// ---------------------------------------------------------------------

#[tokio::test]
async fn definition_translates_a_location_link_reply_end_to_end() {
    // Real-world regression pin (PRR-L3 smoke → PRR-L4 fix): ruby-lsp
    // 0.26.11 replies to textDocument/definition with LocationLink[]
    // unconditionally, even though kb-lip never advertises
    // textDocument.definition.linkSupport. Before the fix this made
    // /lip/definition silently return `results: []` against a fully
    // healthy provider with a real answer available.
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("FAKE_LSP_DEFINITION_LOCATION_LINK", "1");
        let booted = boot_default(dir.path()).await;
        std::env::remove_var("FAKE_LSP_DEFINITION_LOCATION_LINK");
        booted
    };

    let body = post_json(
        addr,
        "/lip/definition",
        json!({"path": rel, "blob_sha": sha, "line": 1, "col": 0}),
    )
    .await;

    assert!(body.get("refused").is_none(), "unexpected refusal: {body}");
    let results = body["results"].as_array().expect("results array");
    assert_eq!(
        results.len(),
        1,
        "a LocationLink[] reply must not be silently dropped: {body}"
    );
    assert_eq!(results[0]["path"], json!(rel));
    // The fixture's targetSelectionRange is {0,0}-{0,3}; targetRange is
    // wider ({0,0}-{2,3}) — the narrower selection range must win.
    assert_eq!(results[0]["line"], json!(1));
    assert_eq!(results[0]["col"], json!(0));
    assert_eq!(results[0]["end_line"], json!(1));
    assert_eq!(results[0]["end_col"], json!(3));
}

// ---------------------------------------------------------------------
// LSP child cwd (PRR-L4 regression pin)
// ---------------------------------------------------------------------

#[tokio::test]
async fn lsp_child_is_spawned_with_workspace_root_as_its_cwd() {
    // Real-world regression pin (PRR-L3 smoke → PRR-L4 fix): LspClient::
    // start used to spawn the child with NO .current_dir() call, so it
    // inherited kb-lip's own process cwd rather than workspace_root —
    // breaking ruby-lsp's Bundler/Gemfile detection when kb-lip is
    // started from anywhere other than the target repo.
    let dir = tempfile::tempdir().unwrap();
    let cwd_file = dir.path().join("observed-cwd.txt");

    let config = Config {
        lang_ids: vec!["ruby".to_string()],
        command: vec![fake_lsp_path().to_string_lossy().into_owned()],
        workspace_root: dir.path().to_path_buf(),
        port: 0,
        initialization_options: None,
        restart_backoff: RestartBackoff::default(),
        diagnostics_wait_ms: 300,
    };

    {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("FAKE_LSP_CWD_FILE", &cwd_file);
        let served = server::serve(config, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .expect("bind + serve");
        std::env::remove_var("FAKE_LSP_CWD_FILE");
        // Keep the supervisor (and its child) alive until we're done
        // reading the file the child wrote — dropping it would kill the
        // child (kill_on_drop) but the file is already written by then
        // regardless (the handshake completed before serve() returned).
        drop(served);
    }

    let observed = std::fs::read_to_string(&cwd_file)
        .expect("fake-lsp should have written its cwd on startup before replying to initialize");
    let expected = std::fs::canonicalize(dir.path()).unwrap();
    let observed_path = std::fs::canonicalize(observed.trim()).unwrap();
    assert_eq!(
        observed_path, expected,
        "the LSP child must be spawned with workspace_root as its cwd, not kb-lip's own"
    );
}

// ---------------------------------------------------------------------
// Indexing readiness gate (PRR-L4 regression pin)
// ---------------------------------------------------------------------

#[tokio::test]
async fn hover_refuses_with_indexing_while_a_progress_token_is_still_open() {
    // Real-world regression pin (PRR-L3 smoke → PRR-L4 fix): querying
    // ruby-lsp while its cold-boot workspace index is running (a
    // `$/progress` cycle with no `end` yet) used to silently return a
    // null hover result. kb-lip must now refuse honestly instead.
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("FAKE_LSP_PROGRESS_TOKEN", "fake-index");
        std::env::set_var("FAKE_LSP_PROGRESS_NO_END", "1");
        let booted = boot_default(dir.path()).await;
        std::env::remove_var("FAKE_LSP_PROGRESS_TOKEN");
        std::env::remove_var("FAKE_LSP_PROGRESS_NO_END");
        booted
    };

    let id = poll_until_indexing(addr, true, std::time::Duration::from_secs(2)).await;
    assert_eq!(
        id["indexing"],
        json!(true),
        "identity must surface the open progress token: {id}"
    );

    let body = post_json(
        addr,
        "/lip/hover",
        json!({"path": rel, "blob_sha": sha, "line": 1, "col": 0}),
    )
    .await;
    assert_eq!(
        body["refused"],
        json!("indexing"),
        "expected an honest indexing refusal, not a silent empty result: {body}"
    );
}

#[tokio::test]
async fn hover_proceeds_normally_once_the_progress_cycle_has_ended() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("FAKE_LSP_PROGRESS_TOKEN", "fake-index");
        std::env::remove_var("FAKE_LSP_PROGRESS_NO_END");
        let booted = boot_default(dir.path()).await;
        std::env::remove_var("FAKE_LSP_PROGRESS_TOKEN");
        booted
    };

    // begin+end are sent back-to-back with no delay by the fixture, so
    // this is expected to already read false by the time it's checked —
    // poll_until_indexing is used defensively (see its doc comment)
    // rather than a bare single check.
    let id = poll_until_indexing(addr, false, std::time::Duration::from_secs(2)).await;
    assert_eq!(
        id["indexing"],
        json!(false),
        "the progress cycle already completed (begin+end): {id}"
    );
    // Extra safety margin: the identity round trip above almost certainly
    // already outlasted the begin->end burst, but sleep briefly anyway so
    // the hover call below can never race a transient begin-before-end
    // window (see poll_until_indexing's doc comment for why this can't be
    // observed via the identity check alone).
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let body = post_json(
        addr,
        "/lip/hover",
        json!({"path": rel, "blob_sha": sha, "line": 1, "col": 0}),
    )
    .await;
    assert!(
        body.get("refused").is_none(),
        "a completed progress cycle must not block requests: {body}"
    );
}

// ---------------------------------------------------------------------
// Restart-after-crash: degrade then recover
// ---------------------------------------------------------------------

#[tokio::test]
async fn restart_after_kill_degrades_then_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    // A generous backoff window so the "immediately after kill" request is
    // unambiguously still inside it (no reliance on sub-millisecond test
    // timing), and the "after the window" request is unambiguously past
    // it — regardless of how loaded the CI box is.
    let backoff = RestartBackoff {
        initial_ms: 1000,
        max_ms: 5000,
        multiplier: 2.0,
    };
    let (_sup, addr) = boot(dir.path(), backoff, 300).await;

    // Confirm it's alive first.
    let body = post_json(
        addr,
        "/lip/hover",
        json!({"path": rel, "blob_sha": sha, "line": 1, "col": 0}),
    )
    .await;
    assert!(
        body.get("refused").is_none(),
        "expected a healthy start: {body}"
    );

    let id = get_json(addr, "/lip/identity").await;
    let pid = id["pid"].as_u64().expect("pid present while healthy") as i32;

    // SAFETY: sending SIGKILL to a pid we just read off our own adapter's
    // identity response, in a test we control end-to-end.
    let killed = unsafe { libc::kill(pid, libc::SIGKILL) };
    assert_eq!(
        killed,
        0,
        "kill({pid}, SIGKILL) failed: {}",
        std::io::Error::last_os_error()
    );

    // Give the reader task a moment to observe EOF on the child's stdout
    // and flip `alive` to false (near-instant, but not synchronous with
    // the kill syscall).
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Well within the 1000ms backoff window: degraded, no restart attempt.
    let body = post_json(
        addr,
        "/lip/hover",
        json!({"path": rel, "blob_sha": sha, "line": 1, "col": 0}),
    )
    .await;
    assert_eq!(
        body["refused"],
        json!("server_down"),
        "expected a degraded response right after the kill: {body}"
    );

    // Past the backoff window: the next request triggers a respawn and
    // succeeds.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let body = post_json(
        addr,
        "/lip/hover",
        json!({"path": rel, "blob_sha": sha, "line": 1, "col": 0}),
    )
    .await;
    assert!(
        body.get("refused").is_none(),
        "expected recovery after the backoff window elapsed: {body}"
    );
    assert_eq!(body["verified_blob_sha"], json!(sha));

    let id_after = get_json(addr, "/lip/identity").await;
    assert_eq!(id_after["healthy"], json!(true));
    assert_ne!(
        id_after["pid"], id["pid"],
        "the respawned child must have a different pid"
    );
}

// ---------------------------------------------------------------------
// /lip/diagnostics — publishDiagnostics cache (design-addendum-2.md §D)
// ---------------------------------------------------------------------

#[tokio::test]
async fn identity_reports_the_six_endpoint_capabilities_array() {
    let dir = tempfile::tempdir().unwrap();
    let (_sup, addr) = boot_default(dir.path()).await;
    let id = get_json(addr, "/lip/identity").await;
    assert_eq!(
        id["capabilities"],
        json!([
            "hover",
            "definition",
            "references",
            "symbols",
            "diagnostics",
            "code-actions"
        ])
    );
}

#[tokio::test]
async fn diagnostics_wait_path_catches_a_delayed_publish() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let body = {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("FAKE_LSP_DIAG_SUPPRESS");
        std::env::set_var("FAKE_LSP_DIAG_DELAY_MS", "100");
        // A generous 1000ms budget so a 100ms delayed publish is
        // unambiguously caught, not raced against the timeout.
        let (_sup, addr) = boot(dir.path(), RestartBackoff::default(), 1000).await;

        let start = tokio::time::Instant::now();
        let resp_body = post_json(
            addr,
            "/lip/diagnostics",
            json!({"path": rel, "blob_sha": sha}),
        )
        .await;
        let elapsed = start.elapsed();
        std::env::remove_var("FAKE_LSP_DIAG_DELAY_MS");

        assert!(
            elapsed >= std::time::Duration::from_millis(80),
            "expected the wait to actually catch the ~100ms-delayed publish \
             (too fast means it returned before the publish arrived): {elapsed:?}"
        );
        resp_body
    };

    assert!(body.get("refused").is_none(), "unexpected refusal: {body}");
    assert!(body["verified_blob_sha"].is_string());
    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["message"], json!("fake diagnostic"));
    assert_eq!(results[0]["severity"], json!(1));
    assert_eq!(results[0]["code"], json!("FAKE001"));
    assert_eq!(results[0]["line"], json!(1));
    assert_eq!(results[0]["col"], json!(0));
    assert_eq!(results[0]["end_col"], json!(3));
}

#[tokio::test]
async fn diagnostics_cached_path_is_fast_on_a_second_query() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("FAKE_LSP_DIAG_DELAY_MS");
        std::env::remove_var("FAKE_LSP_DIAG_SUPPRESS");
        boot(dir.path(), RestartBackoff::default(), 1000).await
    };

    // First query: a fresh open, so it waits for the (immediate) publish.
    let first = post_json(
        addr,
        "/lip/diagnostics",
        json!({"path": rel, "blob_sha": sha}),
    )
    .await;
    assert!(
        first.get("refused").is_none(),
        "unexpected refusal: {first}"
    );
    assert_eq!(first["results"].as_array().unwrap().len(), 1);

    // Second query: same path + unchanged content — `ensure_doc_open`
    // reuses the already-open doc (no fresh didOpen), so the diagnostics
    // cache is already clean and must answer immediately rather than
    // re-waiting the full 1000ms budget.
    let start = tokio::time::Instant::now();
    let second = post_json(
        addr,
        "/lip/diagnostics",
        json!({"path": rel, "blob_sha": sha}),
    )
    .await;
    let elapsed = start.elapsed();
    assert!(
        second.get("refused").is_none(),
        "unexpected refusal: {second}"
    );
    assert_eq!(second["results"], first["results"]);
    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "the cached path must not re-wait: took {elapsed:?}"
    );
}

#[tokio::test]
async fn diagnostics_times_out_to_an_empty_result_when_nothing_is_ever_published() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let body = {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("FAKE_LSP_DIAG_DELAY_MS");
        std::env::set_var("FAKE_LSP_DIAG_SUPPRESS", "1");
        // A small budget so this test stays fast — it deliberately always
        // times out.
        let (_sup, addr) = boot(dir.path(), RestartBackoff::default(), 150).await;

        let start = tokio::time::Instant::now();
        let resp_body = post_json(
            addr,
            "/lip/diagnostics",
            json!({"path": rel, "blob_sha": sha}),
        )
        .await;
        let elapsed = start.elapsed();
        std::env::remove_var("FAKE_LSP_DIAG_SUPPRESS");

        assert!(
            elapsed >= std::time::Duration::from_millis(150),
            "expected the full wait budget to elapse before giving up: {elapsed:?}"
        );
        resp_body
    };

    assert!(
        body.get("refused").is_none(),
        "empty-after-wait must be a valid success, never a refusal: {body}"
    );
    assert_eq!(body["results"], json!([]));
    assert!(body["verified_blob_sha"].is_string());
}

#[tokio::test]
async fn diagnostics_blob_guard_refuses_when_file_changed_since_the_caller_hashed_it() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let original = "original content\n";
    write_fixture(dir.path(), rel, original);
    let stale_sha = kb_lip::blob::git_blob_sha1(original.as_bytes());

    let (_sup, addr) = boot_default(dir.path()).await;

    // Mutate AFTER the caller computed its hash but BEFORE sending the
    // request — same TOCTOU shape as the hover blob-guard test, now
    // exercised against `/lip/diagnostics`'s own guard pipeline.
    let mutated = "mutated content, completely different bytes\n";
    write_fixture(dir.path(), rel, mutated);
    let current_sha = kb_lip::blob::git_blob_sha1(mutated.as_bytes());
    assert_ne!(stale_sha, current_sha);

    let body = post_json(
        addr,
        "/lip/diagnostics",
        json!({"path": rel, "blob_sha": stale_sha}),
    )
    .await;

    assert_eq!(body["refused"], json!("blob_mismatch"));
    assert_eq!(body["have"], json!(current_sha));
    assert_eq!(body["want"], json!(stale_sha));
    assert!(body.get("results").is_none());
}

// ---------------------------------------------------------------------
// /lip/diagnostics — LSP 3.17 PULL mode (PRR-L5, design-addendum-2.md §D
// amendment): closes the PRR-L4 finding that ruby-lsp 0.26.11 advertises
// ONLY `diagnosticProvider` (pull) and never pushes `publishDiagnostics`,
// so a push-cache-only pipeline returned `results: []` forever against it
// regardless of the file's actual lint state.
// ---------------------------------------------------------------------

#[tokio::test]
async fn identity_reports_push_mode_by_default_when_pull_is_not_advertised() {
    let dir = tempfile::tempdir().unwrap();
    let (_sup, addr) = boot_default(dir.path()).await;
    let id = get_json(addr, "/lip/identity").await;
    assert_eq!(
        id["diagnostics_mode"],
        json!("push"),
        "the fake fixture never advertises diagnosticProvider unless FAKE_LSP_DIAG_PULL=1: {id}"
    );
}

#[tokio::test]
async fn pull_diagnostics_happy_path_translates_the_full_report() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("FAKE_LSP_DIAG_PULL", "1");
        let booted = boot_default(dir.path()).await;
        std::env::remove_var("FAKE_LSP_DIAG_PULL");
        booted
    };

    let id = get_json(addr, "/lip/identity").await;
    assert_eq!(
        id["diagnostics_mode"],
        json!("pull"),
        "identity must surface the advertised diagnosticProvider: {id}"
    );

    let body = post_json(
        addr,
        "/lip/diagnostics",
        json!({"path": rel, "blob_sha": sha}),
    )
    .await;
    assert!(body.get("refused").is_none(), "unexpected refusal: {body}");
    assert!(body["verified_blob_sha"].is_string());
    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["message"], json!("fake diagnostic"));
    assert_eq!(results[0]["severity"], json!(1));
    assert_eq!(results[0]["code"], json!("FAKE001"));
    assert_eq!(results[0]["line"], json!(1));
    assert_eq!(results[0]["col"], json!(0));
    assert_eq!(results[0]["end_col"], json!(3));
}

#[tokio::test]
async fn pull_diagnostics_unchanged_report_serves_the_previous_full_result() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("FAKE_LSP_DIAG_PULL", "1");
        let booted = boot_default(dir.path()).await;
        std::env::remove_var("FAKE_LSP_DIAG_PULL");
        booted
    };

    // First pull: no previousResultId sent (fresh doc), so the fixture
    // answers "full" with FAKE_PULL_RESULT_ID.
    let first = post_json(
        addr,
        "/lip/diagnostics",
        json!({"path": rel, "blob_sha": sha}),
    )
    .await;
    assert!(
        first.get("refused").is_none(),
        "unexpected refusal: {first}"
    );
    assert_eq!(first["results"].as_array().unwrap().len(), 1);

    // Second pull: same open doc — kb-lip must now send the cached
    // resultId as previousResultId, and the fixture replies "unchanged"
    // (no items). The client must serve the items it cached from the
    // FIRST full report rather than translating an empty/absent list.
    let second = post_json(
        addr,
        "/lip/diagnostics",
        json!({"path": rel, "blob_sha": sha}),
    )
    .await;
    assert!(
        second.get("refused").is_none(),
        "unexpected refusal: {second}"
    );
    assert_eq!(
        second["results"], first["results"],
        "an 'unchanged' pull report must be served from the client's own \
         cache of the last full report, not lost: {second}"
    );
}

#[tokio::test]
async fn push_fallback_diagnostics_still_works_when_pull_is_not_advertised() {
    // Regression pin: PRR-L5 introduces the pull branch inside
    // guarded_diagnostics — this pins that the pre-existing push-cache
    // path (design-addendum-2.md §D) is completely unaffected when the
    // server never advertises diagnosticProvider (the fake fixture's
    // default, no FAKE_LSP_DIAG_PULL set).
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("FAKE_LSP_DIAG_PULL");
        std::env::remove_var("FAKE_LSP_DIAG_SUPPRESS");
        std::env::remove_var("FAKE_LSP_DIAG_DELAY_MS");
        boot(dir.path(), RestartBackoff::default(), 1000).await
    };

    let id = get_json(addr, "/lip/identity").await;
    assert_eq!(id["diagnostics_mode"], json!("push"));

    let body = post_json(
        addr,
        "/lip/diagnostics",
        json!({"path": rel, "blob_sha": sha}),
    )
    .await;
    assert!(body.get("refused").is_none(), "unexpected refusal: {body}");
    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["message"], json!("fake diagnostic"));
}

#[tokio::test]
async fn pull_diagnostics_blob_guard_refuses_when_file_changed_since_the_caller_hashed_it() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let original = "original content\n";
    write_fixture(dir.path(), rel, original);
    let stale_sha = kb_lip::blob::git_blob_sha1(original.as_bytes());

    let (_sup, addr) = {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("FAKE_LSP_DIAG_PULL", "1");
        let booted = boot_default(dir.path()).await;
        std::env::remove_var("FAKE_LSP_DIAG_PULL");
        booted
    };

    // Mutate AFTER the caller computed its hash but BEFORE sending the
    // request — same TOCTOU shape as the push-mode blob-guard test, now
    // exercised against the pull branch of guarded_diagnostics.
    let mutated = "mutated content, completely different bytes\n";
    write_fixture(dir.path(), rel, mutated);
    let current_sha = kb_lip::blob::git_blob_sha1(mutated.as_bytes());
    assert_ne!(stale_sha, current_sha);

    let body = post_json(
        addr,
        "/lip/diagnostics",
        json!({"path": rel, "blob_sha": stale_sha}),
    )
    .await;

    assert_eq!(body["refused"], json!("blob_mismatch"));
    assert_eq!(body["have"], json!(current_sha));
    assert_eq!(body["want"], json!(stale_sha));
    assert!(body.get("results").is_none());
}

#[tokio::test]
async fn pull_diagnostics_refuses_with_indexing_while_a_progress_token_is_still_open() {
    // Same indexing-gate regression shape as hover's own pin
    // (`hover_refuses_with_indexing_while_a_progress_token_is_still_open`),
    // now exercised against the pull branch: the indexing check in
    // guarded_diagnostics runs BEFORE ensure_doc_open/pull_diagnostics, so
    // it must refuse identically regardless of diagnostics mode.
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("FAKE_LSP_DIAG_PULL", "1");
        std::env::set_var("FAKE_LSP_PROGRESS_TOKEN", "fake-index");
        std::env::set_var("FAKE_LSP_PROGRESS_NO_END", "1");
        let booted = boot_default(dir.path()).await;
        std::env::remove_var("FAKE_LSP_DIAG_PULL");
        std::env::remove_var("FAKE_LSP_PROGRESS_TOKEN");
        std::env::remove_var("FAKE_LSP_PROGRESS_NO_END");
        booted
    };

    let id = poll_until_indexing(addr, true, std::time::Duration::from_secs(2)).await;
    assert_eq!(
        id["indexing"],
        json!(true),
        "identity must surface the open progress token: {id}"
    );
    assert_eq!(id["diagnostics_mode"], json!("pull"));

    let body = post_json(
        addr,
        "/lip/diagnostics",
        json!({"path": rel, "blob_sha": sha}),
    )
    .await;
    assert_eq!(
        body["refused"],
        json!("indexing"),
        "expected an honest indexing refusal even in pull mode, not a silent empty result: {body}"
    );
}

// ---------------------------------------------------------------------
// /lip/code-actions (S2-C, design-s2.md) — driven by the
// FAKE_LSP_CODE_ACTION_MODE fixture scenarios (see src/bin/fake_lsp.rs's
// module doc comment for each mode's exact server-side behavior).
// ---------------------------------------------------------------------

/// Sets `FAKE_LSP_CODE_ACTION_MODE` (and any extra env vars) under
/// `ENV_LOCK`, boots, then clears them — the same guarded-env-mutation
/// shape every other env-var-driven fixture test in this file uses.
async fn boot_with_code_action_mode(
    workspace_root: &Path,
    mode: &str,
    extra: &[(&str, &str)],
) -> (Arc<kb_lip::Supervisor>, SocketAddr) {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("FAKE_LSP_CODE_ACTION_MODE", mode);
    for (k, v) in extra {
        std::env::set_var(k, v);
    }
    let booted = boot_default(workspace_root).await;
    std::env::remove_var("FAKE_LSP_CODE_ACTION_MODE");
    for (k, _) in extra {
        std::env::remove_var(k);
    }
    booted
}

#[tokio::test]
async fn code_actions_refuses_unsupported_when_the_server_has_no_capability() {
    // No FAKE_LSP_CODE_ACTION_MODE set — the fixture's default `initialize`
    // never advertises `codeActionProvider` at all (see fake_lsp.rs's
    // module doc comment), exercising the ordinary capability-absent gate
    // (`crate::lsp::capability_key_for("code-actions")` ==
    // `"codeActionProvider"`) shared with hover/definition/references/
    // symbols.
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    // Boot under ENV_LOCK even though this test sets nothing itself — it
    // must not race a CONCURRENT test's FAKE_LSP_CODE_ACTION_MODE mutation
    // (cargo test runs #[tokio::test] fns on separate OS threads by
    // default) while asserting on the env-var-free default fixture
    // behavior; see ENV_LOCK's own doc comment above.
    let (_sup, addr) = {
        let _guard = ENV_LOCK.lock().await;
        boot_default(dir.path()).await
    };

    let body = post_json(
        addr,
        "/lip/code-actions",
        json!({"path": rel, "blob_sha": sha, "start_line": 1, "start_col": 0}),
    )
    .await;
    assert_eq!(body["refused"], json!("unsupported"));
}

#[tokio::test]
async fn code_actions_literal_edit_is_translated_without_a_resolve_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = boot_with_code_action_mode(dir.path(), "literal", &[]).await;

    let body = post_json(
        addr,
        "/lip/code-actions",
        json!({"path": rel, "blob_sha": sha, "start_line": 1, "start_col": 0}),
    )
    .await;

    assert!(body.get("refused").is_none(), "unexpected refusal: {body}");
    assert_eq!(body["verified_blob_sha"], json!(sha));
    assert_eq!(body["dropped_command_only"], json!(0));
    assert_eq!(body["dropped_unsupported"], json!(0));

    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["kind"], json!("quickfix"));
    assert_eq!(results[0]["is_preferred"], json!(true));
    let title = results[0]["title"].as_str().unwrap();
    assert!(
        title.contains("only=null"),
        "no kinds were sent — context.only must have been omitted (reflected as null): {title}"
    );

    let edits = results[0]["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["path"], json!(rel));
    let file_edits = edits[0]["edits"].as_array().unwrap();
    assert_eq!(file_edits.len(), 1);
    assert_eq!(file_edits[0]["new_text"], json!("// fixed\n"));
    assert_eq!(file_edits[0]["start_line"], json!(1));
    assert_eq!(file_edits[0]["start_col"], json!(0));
}

#[tokio::test]
async fn code_actions_kinds_filter_is_forwarded_to_the_lsp_requests_context_only() {
    // Regression pin for `kinds` -> `CodeActionContext.only` passthrough —
    // the "literal" fixture mode echoes the exact `context.only` it
    // received into the returned title, since there is no other way for
    // an HTTP-only integration test to observe the outgoing LSP request.
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = boot_with_code_action_mode(dir.path(), "literal", &[]).await;

    let body = post_json(
        addr,
        "/lip/code-actions",
        json!({
            "path": rel, "blob_sha": sha,
            "start_line": 1, "start_col": 0,
            "kinds": ["quickfix", "refactor"]
        }),
    )
    .await;

    assert!(body.get("refused").is_none(), "unexpected refusal: {body}");
    let title = body["results"][0]["title"].as_str().unwrap();
    assert!(
        title.contains(r#"only=["quickfix","refactor"]"#),
        "kinds must be forwarded verbatim into CodeActionContext.only: {title}"
    );
}

#[tokio::test]
async fn code_actions_resolves_an_action_that_arrives_without_an_edit() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\nmore\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = boot_with_code_action_mode(dir.path(), "resolve", &[]).await;

    let body = post_json(
        addr,
        "/lip/code-actions",
        json!({"path": rel, "blob_sha": sha, "start_line": 1, "start_col": 0}),
    )
    .await;

    assert!(body.get("refused").is_none(), "unexpected refusal: {body}");
    assert_eq!(body["dropped_command_only"], json!(0));
    assert_eq!(body["dropped_unsupported"], json!(0));
    let results = body["results"].as_array().expect("results array");
    assert_eq!(
        results.len(),
        1,
        "the resolve round trip must have produced a usable edit: {body}"
    );
    assert_eq!(results[0]["title"], json!("Extract variable"));
    let edits = results[0]["edits"][0]["edits"].as_array().unwrap();
    assert_eq!(edits[0]["new_text"], json!("extracted = true\n"));
}

#[tokio::test]
async fn code_actions_command_only_action_is_dropped_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = boot_with_code_action_mode(dir.path(), "command_only", &[]).await;

    let body = post_json(
        addr,
        "/lip/code-actions",
        json!({"path": rel, "blob_sha": sha, "start_line": 1, "start_col": 0}),
    )
    .await;

    assert!(body.get("refused").is_none(), "unexpected refusal: {body}");
    assert_eq!(body["results"], json!([]));
    assert_eq!(body["dropped_command_only"], json!(1));
    assert_eq!(body["dropped_unsupported"], json!(0));
}

#[tokio::test]
async fn code_actions_translates_a_multi_file_workspace_edit() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let second_rel = "lib/other.rb";
    write_fixture(dir.path(), rel, "content\n");
    let second_abs = write_fixture(dir.path(), second_rel, "other content\n");
    let sha = kb_lip::blob::git_blob_sha1(b"content\n");
    // A COMPLETE uri, built the same way guarded_code_actions itself would
    // — never reconstructed from the fake-lsp child's own OS-reported cwd
    // (see fake_lsp.rs's module doc comment for why).
    let second_uri = kb_lip::lsp::path_to_file_uri(&second_abs);

    let (_sup, addr) = boot_with_code_action_mode(
        dir.path(),
        "multi_file",
        &[("FAKE_LSP_CODE_ACTION_SECOND_URI", second_uri.as_str())],
    )
    .await;

    let body = post_json(
        addr,
        "/lip/code-actions",
        json!({"path": rel, "blob_sha": sha, "start_line": 1, "start_col": 0}),
    )
    .await;

    assert!(body.get("refused").is_none(), "unexpected refusal: {body}");
    assert_eq!(body["dropped_command_only"], json!(0));
    assert_eq!(body["dropped_unsupported"], json!(0));
    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    let edits = results[0]["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 2, "must touch both files: {body}");
    let paths: Vec<&str> = edits.iter().map(|e| e["path"].as_str().unwrap()).collect();
    assert!(paths.contains(&rel));
    assert!(paths.contains(&second_rel));
}

#[tokio::test]
async fn code_actions_drops_an_edit_targeting_a_uri_outside_the_workspace_root() {
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let content = "content\n";
    write_fixture(dir.path(), rel, content);
    let sha = kb_lip::blob::git_blob_sha1(content.as_bytes());

    let (_sup, addr) = boot_with_code_action_mode(dir.path(), "out_of_root", &[]).await;

    let body = post_json(
        addr,
        "/lip/code-actions",
        json!({"path": rel, "blob_sha": sha, "start_line": 1, "start_col": 0}),
    )
    .await;

    assert!(body.get("refused").is_none(), "unexpected refusal: {body}");
    assert_eq!(body["results"], json!([]));
    assert_eq!(body["dropped_command_only"], json!(0));
    assert_eq!(
        body["dropped_unsupported"],
        json!(1),
        "an out-of-workspace-root URI must be dropped as unsupported, never applied: {body}"
    );
}

#[tokio::test]
async fn code_actions_blob_guard_refuses_the_whole_response_when_the_file_changes_mid_resolve() {
    // The hash-post check runs ONCE, after every codeAction/resolve round
    // trip has completed — this pins that a file mutated WHILE a resolve
    // is in flight (not merely before the initial request) still refuses
    // the whole response, discarding whatever the resolve produced.
    let dir = tempfile::tempdir().unwrap();
    let rel = "lib/foo.rb";
    let original = "original content\n";
    write_fixture(dir.path(), rel, original);
    let sha = kb_lip::blob::git_blob_sha1(original.as_bytes());

    let (_sup, addr) = boot_with_code_action_mode(
        dir.path(),
        "blob_mismatch_mid_resolve",
        &[("FAKE_LSP_CODE_ACTION_RESOLVE_DELAY_MS", "150")],
    )
    .await;

    let request_path = dir.path().to_path_buf();
    let req_addr = addr;
    let req_sha = sha.clone();
    let request = tokio::spawn(async move {
        post_json(
            req_addr,
            "/lip/code-actions",
            json!({"path": rel, "blob_sha": req_sha, "start_line": 1, "start_col": 0}),
        )
        .await
    });

    // Give the request enough time to complete its pre-hash read and send
    // textDocument/codeAction + start the (150ms-delayed) codeAction/resolve
    // round trip, then mutate the file well before that delay elapses.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mutated = "mutated content, completely different bytes\n";
    write_fixture(&request_path, rel, mutated);
    let current_sha = kb_lip::blob::git_blob_sha1(mutated.as_bytes());

    let body = request.await.unwrap();
    assert_eq!(body["refused"], json!("blob_mismatch"));
    assert_eq!(body["have"], json!(current_sha));
    assert_eq!(body["want"], json!(sha));
    assert!(body.get("results").is_none());
}
