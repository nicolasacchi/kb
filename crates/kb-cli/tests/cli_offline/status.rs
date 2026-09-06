//! `kb status` against a daemon HTTP `/api/stats` snapshot.
//!
//! The verb used to list kbs only from the local kb.toml + sqlite, so a
//! host CLI pointed at a dockerised (or otherwise remote) daemon printed
//! "(no kbs configured)" even when `/api/stats` had a full fleet. These
//! tests pin the daemon-first listing + bearer auth that fix that.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use std::io::{Read, Write};
use std::path::Path;

fn isolated(mut cmd: Command, tmp: &Path) -> Command {
    cmd.env("KB_STATE_DIR", tmp.join("state"))
        .env("KB_CONFIG_DIR", tmp.join("config"))
        .env("KB_CACHE_DIR", tmp.join("cache"));
    cmd
}

/// Stub daemon: answers `/api/stats` (and `/api/identity`) with a two-kb
/// snapshot. If `require_bearer` is set, unauthenticated requests 401.
fn spawn_stats_stub(require_bearer: Option<&str>) -> String {
    let required = require_bearer.map(str::to_string);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let required = required.clone();
            std::thread::spawn(move || {
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
                if let Some(token) = required.as_deref() {
                    let head_lc = head.to_ascii_lowercase();
                    let want = format!("authorization: bearer {token}");
                    if !head_lc.contains(&want) {
                        let body = r#"{"detail":"authentication required"}"#;
                        let resp = format!(
                            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = s.write_all(resp.as_bytes());
                        return;
                    }
                }
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body: &str = if path.starts_with("/api/stats") {
                    r#"{"daemon":{"name":"default"},"total_docs":430,"total_open_errors":2,"kbs":[{"name":"memory","doc_count":406,"open_errors":0,"last_index_at":1787933218,"last_reconcile_at":1787953474,"last_reconcile_files":406,"last_reconcile_deletes":0,"last_reconcile_duration_ms":24,"reconcile_secs":3600,"decode_skips":0},{"name":"sessions","doc_count":24,"open_errors":2,"last_index_at":1787935936,"last_reconcile_at":1787953475,"last_reconcile_files":24,"last_reconcile_deletes":0,"last_reconcile_duration_ms":549,"reconcile_secs":3600,"decode_skips":0}]}"#
                } else if path.starts_with("/api/identity") {
                    r#"{"name":"default","version":"0.0.0","host":"test"}"#
                } else {
                    "{}"
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
fn status_help_lists_daemon_flag() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["status", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("--daemon"),
        "--daemon missing from `kb status --help`: {text}"
    );
}

#[test]
fn status_lists_daemon_kbs_when_local_config_empty() {
    // Production change that would make this fail: going back to iterating
    // only `cfg.kb` (empty when there's no host kb.toml) and ignoring
    // `/api/stats`. That's the ci-host "no kbs configured" bug.
    let tmp = tempfile::tempdir().unwrap();
    let endpoint = spawn_stats_stub(None);
    let out = isolated(Command::cargo_bin("kb").unwrap(), tmp.path())
        .args(["status", "--daemon", &endpoint, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value =
        serde_json::from_slice(&out).expect("status --json should emit JSON");
    let kbs = parsed["kbs"].as_array().expect("kbs array");
    let names: Vec<&str> = kbs.iter().filter_map(|k| k["name"].as_str()).collect();
    assert_eq!(names, ["memory", "sessions"]);
    assert_eq!(kbs[0]["doc_count"], 406);
    assert_eq!(kbs[1]["open_errors"], 2);
}

#[test]
fn status_human_does_not_say_no_kbs_when_daemon_has_them() {
    let tmp = tempfile::tempdir().unwrap();
    let endpoint = spawn_stats_stub(None);
    isolated(Command::cargo_bin("kb").unwrap(), tmp.path())
        .args(["status", "--daemon", &endpoint])
        .assert()
        .success()
        .stdout(contains("memory"))
        .stdout(contains("sessions"))
        .stdout(contains("docs: 406"))
        .stdout(contains("no kbs configured").not());
}

#[test]
fn status_sends_bearer_against_auth_on_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let token = "test-token-for-status";
    std::fs::create_dir_all(tmp.path().join("config")).unwrap();
    std::fs::write(tmp.path().join("config").join("token"), token).unwrap();
    let endpoint = spawn_stats_stub(Some(token));
    let out = isolated(Command::cargo_bin("kb").unwrap(), tmp.path())
        .args(["status", "--daemon", &endpoint, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(parsed["kbs"].as_array().unwrap().len(), 2);
}
