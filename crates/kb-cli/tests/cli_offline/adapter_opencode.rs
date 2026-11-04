//! AH6 - a pushed slate delta must not count as a human turn.
//!
//! Shells `plugins/kb-memory/hooks/kb-capture-opencode.sh` over a synthetic
//! `opencode export` and asserts on the recovered transcript: the pushed
//! delta (D26's `client.session.prompt({noReply:true})` stores a USER
//! message) and all-synthetic user messages are kept but marked `isMeta`, so
//! the engine's `user_turns` counts only what a human typed.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn script_path() -> PathBuf {
    repo_root().join("plugins/kb-memory/hooks/kb-capture-opencode.sh")
}

/// Bin dir of the just-built `kb` binary (the preferred write path).
fn kb_bin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kb"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn only_capture_html(dir: &Path) -> PathBuf {
    let mut hits: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("session-") && n.ends_with(".html"))
        })
        .collect();
    assert_eq!(hits.len(), 1, "expected exactly one capture, got {hits:?}");
    hits.pop().unwrap()
}

fn msg(role: &str, created: i64, parts: Vec<Value>) -> Value {
    json!({"info": {"role": role, "time": {"created": created}}, "parts": parts})
}

fn text(t: &str) -> Value {
    json!({"type": "text", "text": t})
}

fn synthetic(t: &str) -> Value {
    json!({"type": "text", "text": t, "synthetic": true})
}

#[test]
fn opencode_adapter_marks_pushed_slate_deltas_and_synthetic_parts_meta() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_out = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_out).unwrap();

    let pushed = format!(
        "{}\nWARN #7 [claude/ab12 3m]  frost tonight",
        kb_core::slate::DELTA_FRAME
    );
    let export = json!({
        "info": {"id": "ses_oc0002", "directory": "/tmp/x", "title": "t",
                 "time": {"created": 1772355600000_i64}},
        "messages": [
            msg("user", 1772355600000, vec![text("prune the north rows")]),
            msg("assistant", 1772355601000, vec![text("on it")]),
            msg("user", 1772355602000, vec![synthetic("Called the Read tool")]),
            msg("user", 1772355603000, vec![text(&pushed)]),
            msg("user", 1772355604000,
                vec![synthetic("Called the Grep tool"), text("and check the gate")]),
        ]
    });
    let export_path = tmp.path().join("export.json");
    std::fs::write(&export_path, serde_json::to_vec(&export).unwrap()).unwrap();

    let out = Command::new("bash")
        .arg(script_path())
        .arg(&export_path)
        .env("KB_SESSIONS_DIR", &sessions_out)
        .env("PATH", format!("{}:/usr/bin:/bin", kb_bin_dir().display()))
        .env("KB_CAPTURE_SPOOL", tmp.path().join("spool"))
        .env("HOME", tmp.path())
        .env_remove("KB_BIN_DIR")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("KB_CACHE_DIR")
        .env_remove("KB_CAPTURE_LOCKS")
        .output()
        .expect("run kb-capture-opencode.sh");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let html = std::fs::read_to_string(only_capture_html(&sessions_out)).unwrap();
    let jsonl = kb_core::sessions::recover_jsonl_from_capture(&html).expect("a <pre> transcript");
    let recs: Vec<Value> = jsonl
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(recs.len(), 6, "{jsonl}");
    assert_eq!(recs[0]["type"], "adapter-meta");

    let users: Vec<&Value> = recs.iter().filter(|r| r["type"] == "user").collect();
    assert_eq!(users.len(), 4, "{jsonl}");
    let meta: Vec<bool> = users
        .iter()
        .map(|r| r["isMeta"].as_bool().expect("isMeta is a boolean"))
        .collect();
    assert_eq!(meta, [false, true, true, false], "{jsonl}");
    assert!(
        users[2]["message"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with(kb_core::slate::DELTA_FRAME),
        "the pushed delta is kept, not dropped"
    );

    let act = kb_core::sessions::parse_session_activity(&jsonl);
    assert_eq!(act.user_turns, 2, "{jsonl}");
    assert_eq!(
        act.first_user_prompt.as_deref(),
        Some("prune the north rows")
    );
}

#[test]
fn opencode_adapter_frame_marker_is_the_delta_frame_first_line() {
    let src = std::fs::read_to_string(script_path()).unwrap();
    let marks: Vec<&str> = src
        .lines()
        .filter(|l| l.starts_with("SLATE_FRAME_LINE='"))
        .collect();
    assert_eq!(marks.len(), 1, "{marks:?}");
    let value = marks[0]
        .strip_prefix("SLATE_FRAME_LINE='")
        .and_then(|v| v.strip_suffix('\''))
        .expect("a single-quoted literal");
    assert_eq!(value, kb_core::slate::DELTA_FRAME.lines().next().unwrap());
}
