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
