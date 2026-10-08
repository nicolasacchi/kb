//! v0.44 F7b — `kb sessions scrub` / `kb sessions rescrub` and the doctor
//! per-lane table, driven as an external process.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

const GH: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";

fn dirty_capture() -> String {
    format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n<meta name=\"kb-harness\" content=\"codex\">\n</head><body>\n<pre>{{\"t\":\"{GH}\"}}\n</pre>\n</body></html>\n"
    )
}

#[test]
fn scrub_filter_redacts_stdin_to_stdout() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["sessions", "scrub"])
        .write_stdin(format!("{{\"a\":\"{GH}\"}}\n"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(!s.contains(GH), "{s}");
    assert!(s.contains("[redacted:github-token]"), "{s}");
}

#[test]
fn rescrub_is_dry_by_default_then_applies_once() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("session-20260301T090000Z-s1.html");
    std::fs::write(&f, dirty_capture()).unwrap();
    let dir = tmp.path().to_str().unwrap();

    Command::cargo_bin("kb")
        .unwrap()
        .args(["sessions", "rescrub", "--dir", dir])
        .assert()
        .success()
        .stdout(contains("dry run").and(contains("1 with unscrubbed")));
    assert!(std::fs::read_to_string(&f).unwrap().contains(GH));

    Command::cargo_bin("kb")
        .unwrap()
        .args(["sessions", "rescrub", "--dir", dir, "--apply"])
        .assert()
        .success()
        .stdout(contains("rewrote 1"));
    assert!(!std::fs::read_to_string(&f).unwrap().contains(GH));

    Command::cargo_bin("kb")
        .unwrap()
        .args(["sessions", "rescrub", "--dir", dir, "--apply", "--json"])
        .assert()
        .success()
        .stdout(contains("\"affected\": 0"));
}

/// v0.49 RI — the second `--apply` over env-secret captures rewrites and
/// reports nothing, and leaves the bytes of the first apply untouched.
#[test]
fn rescrub_apply_twice_second_pass_is_a_noop_for_env_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("session-20260301T090000Z-s1.html");
    std::fs::write(
        &f,
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n</head><body>\n<pre>{\"t\":\"export AWS_SECRET_ACCESS_KEY=abc123def456 ok\"}\n</pre>\n</body></html>\n",
    )
    .unwrap();
    let dir = tmp.path().to_str().unwrap();
    let run = |args: &[&str]| {
        let out = Command::cargo_bin("kb")
            .unwrap()
            .args(["sessions", "rescrub", "--dir", dir, "--json"])
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice::<serde_json::Value>(&out).unwrap()
    };
    let first = run(&["--apply"]);
    assert_eq!(first["rewritten"], 1, "{first}");
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(after.contains("AWS_SECRET_ACCESS_KEY=[masked]"), "{after}");
    for args in [&["--apply"][..], &[][..]] {
        let again = run(args);
        assert_eq!(again["affected"], 0, "{again}");
        assert_eq!(again["rewritten"], 0, "{again}");
        assert_eq!(again["redactions"]["transcript"], 0, "{again}");
    }
    assert_eq!(std::fs::read_to_string(&f).unwrap(), after);
}

#[test]
fn doctor_hooks_prints_the_capture_scrub_lane_table() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("session-20260301T090000Z-s1.html"),
        dirty_capture(),
    )
    .unwrap();
    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_SESSIONS_DIR", &sessions)
        .args([
            "doctor",
            "--hooks",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--daemon",
            "http://127.0.0.1:55556",
        ])
        .assert()
        .stdout(
            contains("capture-scrub")
                .and(contains("sidecar-text"))
                .and(contains("codex")),
        );
}
