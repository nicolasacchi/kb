//! A1 — `plugins/kb-memory/hooks/kb-capture-codex.sh` stamps `job_ulid`
//! on its adapter-meta line from `$KB_JOB_ULID` (validated shape
//! `^[A-Za-z0-9_-]{1,64}$`, omitted when unset/invalid). The extractor
//! already turns any adapter-meta `job_ulid` into a
//! `session_research` row `kind="grok_job"`; this test pins that the
//! codex adapter's stamp reaches it. Synthetic fixture only
//! (`tests/fixtures/codex-rollout-job.jsonl`).

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
    repo_root().join("plugins/kb-memory/hooks/kb-capture-codex.sh")
}

fn fixture_path() -> PathBuf {
    repo_root().join("plugins/kb-memory/hooks/tests/fixtures/codex-rollout-job.jsonl")
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

fn run_capture(sessions_out: &Path, tmp: &Path, job_ulid: Option<&str>) -> String {
    let mut cmd = Command::new("bash");
    cmd.arg(script_path())
        .arg(fixture_path())
        .env("KB_SESSIONS_DIR", sessions_out)
        .env("PATH", format!("{}:/usr/bin:/bin", kb_bin_dir().display()))
        .env("KB_CAPTURE_SPOOL", tmp.join("spool"))
        .env("HOME", tmp)
        .env_remove("KB_BIN_DIR")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("KB_CACHE_DIR")
        .env_remove("KB_CAPTURE_LOCKS");
    match job_ulid {
        Some(v) => cmd.env("KB_JOB_ULID", v),
        None => cmd.env_remove("KB_JOB_ULID"),
    };
    let out = cmd.output().expect("run kb-capture-codex.sh");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let html = std::fs::read_to_string(only_capture_html(sessions_out)).unwrap();
    kb_core::sessions::recover_jsonl_from_capture(&html).expect("a <pre> transcript")
}

#[test]
fn codex_adapter_stamps_job_ulid_from_env() {
    const JOB_ULID: &str = "01FIXTUREJOBULID0000000A";
    let tmp = tempfile::tempdir().unwrap();
    let sessions_out = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_out).unwrap();
    let jsonl = run_capture(&sessions_out, tmp.path(), Some(JOB_ULID));
    let recs: Vec<Value> = jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(!recs.is_empty(), "{jsonl}");
    assert_eq!(recs[0]["type"], "adapter-meta");
    assert_eq!(recs[0]["job_ulid"], JOB_ULID, "{jsonl}");
    let act = kb_core::sessions::parse_session_activity(&jsonl);
    assert!(
        act.research
            .iter()
            .any(|r| r.kind == "grok_job" && r.query == JOB_ULID),
        "grok_job research row must carry the ulid: {:?}",
        act.research
    );
    // Absent case: without KB_JOB_ULID the key is omitted entirely.
    let sessions_out2 = tmp.path().join("sessions2");
    std::fs::create_dir_all(&sessions_out2).unwrap();
    let jsonl2 = run_capture(&sessions_out2, tmp.path(), None);
    let recs2: Vec<Value> = jsonl2
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(!recs2.is_empty(), "{jsonl2}");
    assert_eq!(recs2[0]["type"], "adapter-meta");
    assert!(
        recs2[0].get("job_ulid").is_none(),
        "job_ulid must be omitted when KB_JOB_ULID is unset: {jsonl2}"
    );
}
