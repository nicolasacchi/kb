//! `kb daemon doctor` integration tests. Exercises the human + JSON
//! output paths against a dead endpoint (no live daemon needed — the
//! reachability check fails, the verdict goes UNHEALTHY, exit code 1).

use assert_cmd::Command;
use predicates::str::contains;
use std::io::{Read, Write};

/// Pick a port that's almost certainly not bound — high range, well
/// above default ephemeral ranges. Doctor's identity probe will fail
/// against this, exercising the FAIL → UNHEALTHY path.
fn dead_endpoint() -> String {
    "http://127.0.0.1:55555".to_string()
}

/// Minimal stub daemon on a loopback ephemeral port: answers just
/// enough of the doctor's probe surface (identity, kbs, stats, events)
/// to reach the stats-derived checks. The stats body carries a kb with
/// the given nonzero `decode_skips` (GC-B2 silent row drops).
fn spawn_stub_daemon(decode_skips: u64) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            std::thread::spawn(move || {
                // Read request headers (GETs only — no body).
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            req.extend_from_slice(&buf[..n]);
                            if req.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }
                let head = String::from_utf8_lossy(&req);
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                if path.starts_with("/api/events") {
                    // One SSE frame, then close — enough for the
                    // event-bus poke to see a live frame.
                    let _ = s.write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\nevent: metrics.tick\ndata: {}\n\n",
                    );
                    let _ = s.flush();
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    return;
                }
                let body: String = if path.starts_with("/api/identity") {
                    r#"{"name":"stub","version":"0.0.0","host":"test"}"#.into()
                } else if path.starts_with("/api/kbs") {
                    // Empty on purpose: skips the per-kb embedder probe
                    // (its 20s budget) while stats below still reports
                    // per-kb decode_skips.
                    "[]".into()
                } else if path.starts_with("/api/stats") {
                    format!(
                        r#"{{"total_docs":1,"kbs":[{{"name":"k1","decode_skips":{decode_skips}}}]}}"#
                    )
                } else {
                    "{}".into()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
            });
        }
    });
    format!("http://{addr}")
}

#[test]
fn doctor_against_dead_endpoint_exits_unhealthy() {
    Command::cargo_bin("kb")
        .unwrap()
        .args(["daemon", "doctor", "--endpoint", &dead_endpoint()])
        .assert()
        .failure() // exit code != 0
        .stdout(contains("UNHEALTHY"))
        .stdout(contains("identity"))
        .stdout(contains("unreachable"));
}

#[test]
fn doctor_watch_with_json_is_rejected() {
    // 4: --watch and --json don't compose — scripted callers want
    // one-shot output, --watch is interactive-only. The CLI errors
    // out before doing any HTTP work.
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "daemon",
            "doctor",
            "--endpoint",
            &dead_endpoint(),
            "--json",
            "--watch",
            "1",
        ])
        .assert()
        .failure()
        .stderr(contains("mutually exclusive"));
}

#[test]
fn doctor_json_output_against_dead_endpoint_is_valid_json() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["daemon", "doctor", "--endpoint", &dead_endpoint(), "--json"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value =
        serde_json::from_slice(&out).expect("doctor --json should emit a valid JSON object");
    assert_eq!(parsed["verdict"], "unhealthy");
    assert_eq!(parsed["endpoint"], dead_endpoint());
    let checks = parsed["checks"].as_array().expect("checks array");
    assert!(!checks.is_empty(), "checks array should not be empty");
    // First check is always identity (the doctor's reachability probe).
    assert_eq!(checks[0]["name"], "identity");
    assert_eq!(checks[0]["status"], "fail");
}

#[test]
fn doctor_warns_on_nonzero_decode_skips() {
    // GC-B2: decode_skips counts rows silently dropped from
    // search/list results (typed-decode failures). Nonzero must
    // surface as a warn-level check, not stay buried in /api/stats.
    let endpoint = spawn_stub_daemon(7);
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["daemon", "doctor", "--endpoint", &endpoint, "--json"])
        .assert()
        .success() // warns → DEGRADED, exit 0 (only UNHEALTHY exits 1)
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value =
        serde_json::from_slice(&out).expect("doctor --json should emit a valid JSON object");
    let checks = parsed["checks"].as_array().expect("checks array");
    let skips = checks
        .iter()
        .find(|c| c["name"] == "decode-skips")
        .expect("decode-skips check present when /api/stats responds");
    assert_eq!(skips["status"], "warn");
    let detail = skips["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("7 rows silently dropped") && detail.contains("kb reindex"),
        "warn detail should carry the count + the reindex hint, got: {detail}"
    );
}
