//! The documented exit-code table, end to end, for the verbs that go through
//! the `*_raw` HTTP helpers (`review compose`, `suggest apply`, ...).
//!
//! Those helpers return `(StatusCode, Value)` and drop the `reqwest::Error`,
//! so before v044-X1 every 400/403/404/409 they saw exited 1 while
//! `docs/kb-code.md` and the kb-review-work skill promised 2/4/8/3. The skill
//! tells a remote agent that `review compose` "404s with an empty body and
//! exits 8"; `compose_empty_404_exits_8` is that sentence, executed.
//!
//! Exit values are copied from `src/envelope.rs` (the binary has no lib
//! target an integration test could import).

mod stub;

use assert_cmd::Command;
use stub::StatusStub;

const EXIT_USAGE: i32 = 2;
const EXIT_CONFLICT: i32 = 3;
const EXIT_REFUSED: i32 = 4;
const EXIT_NOT_FOUND: i32 = 8;
const EXIT_GENERIC: i32 = 1;

fn kb() -> Command {
    Command::cargo_bin("kb-code").expect("kb-code binary")
}

fn compose_exit(status: u16, body: &str) -> i32 {
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("body.json");
    std::fs::write(&payload, r#"{"summary":"s"}"#).unwrap();
    let d = StatusStub::new(status, body);
    let a = kb()
        .args(["review", "compose", "12", "--from-file"])
        .arg(&payload)
        .args(["--daemon", &d.url])
        .assert();
    a.get_output().status.code().expect("exited normally")
}

#[test]
fn compose_empty_404_exits_8() {
    // A loopback-only route answers an off-host caller with a bodiless 404.
    assert_eq!(compose_exit(404, ""), EXIT_NOT_FOUND);
}

#[test]
fn compose_maps_every_documented_status() {
    for (status, want) in [
        (400, EXIT_USAGE),
        (403, EXIT_REFUSED),
        (404, EXIT_NOT_FOUND),
        (409, EXIT_CONFLICT),
        (500, EXIT_GENERIC),
    ] {
        assert_eq!(
            compose_exit(status, r#"{"error":"nope"}"#),
            want,
            "HTTP {status}"
        );
    }
}

#[test]
fn suggest_apply_drift_409_exits_3() {
    let d = StatusStub::new(409, r#"{"error":"drift","expected":"a\n","found":"b\n"}"#);
    kb().args(["suggest", "apply", "ann_x", "--daemon", &d.url])
        .assert()
        .code(EXIT_CONFLICT);
}

/// v0.44 F5 - a LOOPBACK-ONLY route's bodiless 404 from a daemon that
/// classifies this caller as non-loopback is `needs-daemon-host` (exit 4),
/// not a not-found (8). The stub plays that daemon via `/api/identity`.
#[test]
fn bodiless_404_from_a_daemon_that_calls_us_non_loopback_exits_4_needs_daemon_host() {
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("body.json");
    std::fs::write(&payload, r#"{"summary":"s"}"#).unwrap();
    let d = StatusStub::with_identity(
        404,
        "",
        Some(r#"{"name":"kb-code","caller_loopback":false}"#),
    );
    let out = kb()
        .args(["review", "compose", "12", "--from-file"])
        .arg(&payload)
        .args(["--daemon", &d.url])
        .assert()
        .code(EXIT_REFUSED);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(stderr.contains("needs-daemon-host"), "{stderr}");
}

/// ... and when the daemon says the caller IS loopback (a real not-found
/// behind the gate), or does not know the field (older daemon), the
/// documented 404 -> 8 mapping stands.
#[test]
fn bodiless_404_from_a_loopback_or_older_daemon_keeps_exit_8() {
    for identity in [
        r#"{"name":"kb-code","caller_loopback":true}"#,
        r#"{"name":"kb-code"}"#,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("body.json");
        std::fs::write(&payload, r#"{"summary":"s"}"#).unwrap();
        let d = StatusStub::with_identity(404, "", Some(identity));
        kb().args(["review", "compose", "12", "--from-file"])
            .arg(&payload)
            .args(["--daemon", &d.url])
            .assert()
            .code(EXIT_NOT_FOUND);
    }
}

/// The constants above are COPIES (the binary has no lib target). Parse
/// `src/envelope.rs` and fail on any drift, so the copies cannot silently
/// diverge from the table the docs and the agent skill promise.
#[test]
fn the_copied_exit_constants_match_envelope_rs() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/envelope.rs"),
    )
    .expect("read src/envelope.rs");
    let parse = |name: &str| -> i32 {
        let needle = format!("pub const {name}: i32 = ");
        let rest = src
            .split(&needle)
            .nth(1)
            .unwrap_or_else(|| panic!("{name} not found in envelope.rs"));
        rest.split(';').next().unwrap().trim().parse().unwrap()
    };
    for (name, copy) in [
        ("EXIT_USAGE", EXIT_USAGE),
        ("EXIT_CONFLICT", EXIT_CONFLICT),
        ("EXIT_REFUSED", EXIT_REFUSED),
        ("EXIT_NOT_FOUND", EXIT_NOT_FOUND),
        ("EXIT_GENERIC", EXIT_GENERIC),
    ] {
        assert_eq!(parse(name), copy, "{name} drifted from src/envelope.rs");
    }
}
