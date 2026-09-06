//! W5/R8 — golden mapping test for `plugins/kb-memory/hooks/kb-capture-grok.sh`,
//! the Grok Build (via grokclaude) capture adapter. Filesystem-only, no
//! daemon: shells the bash script over a tiny CHECKED-IN, entirely synthetic
//! fixture pair (`plugins/kb-memory/hooks/tests/fixtures/{grok-session,
//! grok-job}/` — hand-authored to mirror the real `~/.grok/sessions/*`
//! schema's key shapes, per W0's rule that real transcript content is NEVER
//! copied into the repo) and asserts on the resulting kb capture envelope.
//!
//! This is the "simplest maintainable form" the milestone brief asked for:
//! reuses `cargo test`'s existing integration-test machinery (no bats/shunit2
//! dependency, runs in CI exactly like every other kb-cli integration test),
//! and exercises the adapter exactly as a caller would — as an external
//! process, not a Rust unit under test — since the thing actually shipped is
//! the shell script, not a Rust module.

use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE_SESSION_UUID: &str = "01900000-0000-7000-8000-000000000001";
const FIXTURE_CWD: &str = "/tmp/kb-grok-fixture-cwd";
const FIXTURE_JOB_ULID: &str = "FIXTURE0JOBULID000000001A";

fn repo_root() -> PathBuf {
    // crates/kb-cli -> repo root
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn script_path() -> PathBuf {
    repo_root().join("plugins/kb-memory/hooks/kb-capture-grok.sh")
}

fn fixtures_dir() -> PathBuf {
    repo_root().join("plugins/kb-memory/hooks/tests/fixtures")
}

/// jq's `@uri` percent-encoding — mirrors the script's own `url_encode()`
/// (verified live to match Grok's on-disk directory naming).
fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Copy the checked-in `grok-session` fixture (chat_history.jsonl,
/// events.jsonl, summary.json) into `<root>/<url-encoded cwd>/<uuid>/`, the
/// on-disk shape `resolve_session_dir` expects under `GROK_SESSIONS_ROOT`.
fn seed_session_dir(root: &Path, cwd: &str, uuid: &str) -> PathBuf {
    let dir = root.join(url_encode(cwd)).join(uuid);
    std::fs::create_dir_all(&dir).unwrap();
    for f in ["chat_history.jsonl", "events.jsonl", "summary.json"] {
        std::fs::copy(fixtures_dir().join("grok-session").join(f), dir.join(f)).unwrap();
    }
    dir
}

/// Bin dir of the just-built `kb` binary, for tests that want it on PATH
/// (the preferred `kb sessions capture` write path).
fn kb_bin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kb"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// PATH with ONLY the standard system tool dirs — no `kb` binary reachable
/// — to force the script's bash hand-rolled-HTML fallback path.
fn bare_path() -> String {
    "/usr/bin:/bin".to_string()
}

/// PATH with the freshly built `kb` prepended — the preferred write path.
fn path_with_kb() -> String {
    format!("{}:/usr/bin:/bin", kb_bin_dir().display())
}

struct Run {
    status: std::process::ExitStatus,
    stderr: String,
}

fn run_script(args: &[&str], kb_sessions_dir: &Path, grok_root: &Path, path: &str) -> Run {
    let out = Command::new("bash")
        .arg(script_path())
        .args(args)
        .env("KB_SESSIONS_DIR", kb_sessions_dir)
        .env("GROK_SESSIONS_ROOT", grok_root)
        .env("PATH", path)
        .env_remove("GROKCLAUDE_FAKE")
        .output()
        .expect("run kb-capture-grok.sh");
    Run {
        status: out.status,
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
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

/// The golden mapping test, run through the PREFERRED `kb sessions capture`
/// write path (kb on PATH) — the shape production actually uses.
#[test]
fn session_dir_mode_captures_the_expected_envelope_via_kb_sessions_capture() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_out = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_out).unwrap();
    let grok_root = tmp.path().join("grok-sessions");
    let session_dir = seed_session_dir(&grok_root, FIXTURE_CWD, FIXTURE_SESSION_UUID);

    let run = run_script(
        &[
            "--session-dir",
            session_dir.to_str().unwrap(),
            "--cwd",
            FIXTURE_CWD,
            "--job-ulid",
            FIXTURE_JOB_ULID,
        ],
        &sessions_out,
        &grok_root,
        &path_with_kb(),
    );
    assert!(run.status.success(), "stderr: {}", run.stderr);
    assert!(run.stderr.contains("action=captured"), "{}", run.stderr);

    let html_path = only_capture_html(&sessions_out);
    let html = std::fs::read_to_string(&html_path).unwrap();
    assert!(html.contains(r#"<meta name="kb-category" content="memory-session">"#));
    assert!(html.contains(&format!(
        r#"<meta name="kb-session" content="{FIXTURE_SESSION_UUID}">"#
    )));

    // Recover the synthesized JSONL through the SAME round-trip the daemon
    // uses, and pin the mapping shape.
    let recovered = kb_core::sessions::recover_jsonl_from_capture(&html).unwrap();
    let lines: Vec<serde_json::Value> = recovered
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    // adapter-meta first, sessionId on every line (id ladder rung 1).
    assert_eq!(lines[0]["type"], "adapter-meta");
    assert_eq!(lines[0]["harness"], "grok");
    assert_eq!(lines[0]["driver"], "grokclaude");
    assert_eq!(lines[0]["job_ulid"], FIXTURE_JOB_ULID);
    for l in &lines {
        assert_eq!(l["sessionId"], FIXTURE_SESSION_UUID, "{l:?}");
    }

    // Fixture has: system(1) + user(2) + reasoning(3, folded/dropped) +
    // assistant(3) + tool_result(2) => 1 adapter-meta + 1 system-marker +
    // 2 user + 3 assistant + 2 tool_result = 9 lines total.
    assert_eq!(lines.len(), 9, "{lines:#?}");

    // The system-prompt line collapses to an isMeta marker, body dropped.
    let system_marker = &lines[1];
    assert_eq!(system_marker["type"], "user");
    assert_eq!(system_marker["isMeta"], true);
    let marker_text = system_marker["message"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(marker_text.contains("grok system prompt"), "{marker_text}");
    assert!(!marker_text.contains("Grok 4.5"), "body must be dropped");

    // The <user_info> envelope line is flagged isMeta (grok-specific tag,
    // not in kb-core's Claude-shaped wrapper list).
    let user_info_line = &lines[2];
    assert_eq!(user_info_line["type"], "user");
    assert_eq!(user_info_line["isMeta"], true);
    assert!(user_info_line["message"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with("<user_info>"));

    // The real prompt is NOT flagged meta.
    let real_prompt = &lines[3];
    assert_eq!(real_prompt["type"], "user");
    assert!(real_prompt["isMeta"].is_null() || real_prompt["isMeta"] == false);
    assert_eq!(
        real_prompt["message"]["content"][0]["text"],
        "Add a --dry-run flag to the fixture export command."
    );

    // First assistant turn: reasoning folded into a leading `thinking`
    // block, text block, and the read_file→Read tool mapping.
    let a1 = &lines[4];
    assert_eq!(a1["type"], "assistant");
    let content = a1["message"]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(
        content[0]["thinking"],
        "I will look at the CLI module first."
    );
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[1]["text"], "Let me check the CLI module.");
    assert_eq!(content[2]["type"], "tool_use");
    assert_eq!(content[2]["name"], "Read");
    assert_eq!(
        content[2]["input"]["file_path"],
        "/tmp/kb-grok-fixture-cwd/src/cli.rs"
    );
    assert_eq!(content[2]["input"]["offset"], 1);
    assert_eq!(content[2]["input"]["limit"], 40);

    // tool_result pairing.
    let r1 = &lines[5];
    assert_eq!(r1["type"], "user");
    let rc = &r1["message"]["content"][0];
    assert_eq!(rc["type"], "tool_result");
    assert_eq!(rc["tool_use_id"], "call-fixture-1");
    assert!(rc["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("pub struct Cli"));

    // Second assistant turn: search_replace -> Edit mapping.
    let a2 = &lines[6];
    let content2 = a2["message"]["content"].as_array().unwrap();
    let edit = content2.iter().find(|b| b["type"] == "tool_use").unwrap();
    assert_eq!(edit["name"], "Edit");
    assert_eq!(
        edit["input"]["file_path"],
        "/tmp/kb-grok-fixture-cwd/src/cli.rs"
    );
    assert_eq!(edit["input"]["old_string"], "pub struct ExportCmd {}");

    // Final assistant turn: pure text, no tool_calls.
    let a3 = &lines[8];
    assert_eq!(a3["type"], "assistant");
    let content3 = a3["message"]["content"].as_array().unwrap();
    assert_eq!(content3.len(), 2, "{content3:#?}"); // thinking + text, no tools
    assert_eq!(
        content3[1]["text"],
        "Done: --dry-run now short-circuits the export before any writes."
    );

    // Timestamps: loop-joined lines resolve to the events.jsonl loop_started
    // stamps (NOT the session-wide created_at fallback).
    assert_eq!(a1["timestamp"], "2026-01-01T00:00:02.000Z");
    let a2_ts = a2["timestamp"].as_str().unwrap();
    assert_eq!(a2_ts, "2026-01-01T00:02:00.000Z");

    // Re-running against the SAME sid reuses the filename (#11 multi-capture).
    let run2 = run_script(
        &[
            "--session-dir",
            session_dir.to_str().unwrap(),
            "--cwd",
            FIXTURE_CWD,
        ],
        &sessions_out,
        &grok_root,
        &path_with_kb(),
    );
    assert!(run2.status.success(), "stderr: {}", run2.stderr);
    let html_path2 = only_capture_html(&sessions_out);
    assert_eq!(html_path, html_path2, "must reuse the same capture file");
}

/// The bash hand-rolled-HTML fallback path (no `kb` binary reachable) must
/// produce an equivalent envelope — same meta tags, same session id — via
/// its own independent write path.
#[test]
fn session_dir_mode_falls_back_to_hand_rolled_html_without_kb_on_path() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_out = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_out).unwrap();
    let grok_root = tmp.path().join("grok-sessions");
    let session_dir = seed_session_dir(&grok_root, FIXTURE_CWD, FIXTURE_SESSION_UUID);

    let run = run_script(
        &["--session-dir", session_dir.to_str().unwrap()],
        &sessions_out,
        &grok_root,
        &bare_path(),
    );
    assert!(run.status.success(), "stderr: {}", run.stderr);
    assert!(run.stderr.contains("action=captured"), "{}", run.stderr);

    let html_path = only_capture_html(&sessions_out);
    let html = std::fs::read_to_string(&html_path).unwrap();
    assert!(html.contains(r#"<meta name="kb-category" content="memory-session">"#));
    assert!(html.contains(r#"<meta name="kb-harness" content="grok">"#));
    assert!(html.contains(&format!(
        r#"<meta name="kb-session" content="{FIXTURE_SESSION_UUID}">"#
    )));
}

/// `--job-dir` mode: resolves `meta.json.grok_session_id` + `cwd` to the
/// session dir under `GROK_SESSIONS_ROOT`, captures, and (with
/// `--with-report`) also writes the linked report artifact (D5).
#[test]
fn job_dir_mode_resolves_via_cwd_and_writes_the_linked_report() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_out = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_out).unwrap();
    let grok_root = tmp.path().join("grok-sessions");
    seed_session_dir(&grok_root, FIXTURE_CWD, FIXTURE_SESSION_UUID);

    let job_dir = fixtures_dir().join("grok-job");
    let run = run_script(
        &["--with-report", job_dir.to_str().unwrap()],
        &sessions_out,
        &grok_root,
        &path_with_kb(),
    );
    assert!(run.status.success(), "stderr: {}", run.stderr);
    assert!(run.stderr.contains("action=captured"), "{}", run.stderr);

    let _ = only_capture_html(&sessions_out); // one session capture landed

    let report_path = sessions_out.join(format!("grok-report-{FIXTURE_JOB_ULID}.html"));
    assert!(report_path.exists(), "report artifact must be written");
    let report_html = std::fs::read_to_string(&report_path).unwrap();
    assert!(report_html.contains(r#"<meta name="kb-category" content="reference">"#));
    assert!(report_html.contains(r#"<meta name="kb-decay" content="fast">"#));
    assert!(report_html.contains(&format!(
        r#"<meta name="kb-session" content="{FIXTURE_SESSION_UUID}">"#
    )));
    assert!(report_html.contains(&format!("job:{FIXTURE_JOB_ULID}")));
    assert!(report_html.contains("fixture --dry-run flag") || report_html.contains("Report:"));
    assert!(report_html.contains("ExportCmd had no dry-run flag"));
}

/// `--dry-run` writes nothing (session, nor report), regardless of mode.
#[test]
fn dry_run_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_out = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_out).unwrap();
    let grok_root = tmp.path().join("grok-sessions");
    seed_session_dir(&grok_root, FIXTURE_CWD, FIXTURE_SESSION_UUID);

    let job_dir = fixtures_dir().join("grok-job");
    let run = run_script(
        &["--dry-run", "--with-report", job_dir.to_str().unwrap()],
        &sessions_out,
        &grok_root,
        &path_with_kb(),
    );
    assert!(run.status.success(), "stderr: {}", run.stderr);
    assert!(run.stderr.contains("would-capture"), "{}", run.stderr);
    let entries: Vec<_> = std::fs::read_dir(&sessions_out).unwrap().collect();
    assert!(
        entries.is_empty(),
        "dry-run must write nothing: {entries:?}"
    );
}

/// A `grok_session_id` starting with `fake-` (GROKCLAUDE_FAKE test runs) is
/// skipped unconditionally — never synthesizes noise into the corpus.
#[test]
fn fake_run_guard_skips_via_grok_session_id_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_out = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_out).unwrap();
    let grok_root = tmp.path().join("grok-sessions");
    std::fs::create_dir_all(&grok_root).unwrap();

    let job_dir = tmp.path().join("fake-job");
    std::fs::create_dir_all(&job_dir).unwrap();
    std::fs::write(
        job_dir.join("meta.json"),
        r#"{"id":"FAKEJOB0000000000000000AB","type":"research","grok_session_id":"fake-session-0001"}"#,
    )
    .unwrap();

    let run = run_script(
        &[job_dir.to_str().unwrap()],
        &sessions_out,
        &grok_root,
        &path_with_kb(),
    );
    assert!(run.status.success(), "stderr: {}", run.stderr);
    let entries: Vec<_> = std::fs::read_dir(&sessions_out).unwrap().collect();
    assert!(
        entries.is_empty(),
        "fake run must write nothing: {entries:?}"
    );
}

/// The `GROKCLAUDE_FAKE` env-var guard (defense in depth for the live
/// post-run trigger) short-circuits before ANY resolution work, even for a
/// real, resolvable session.
#[test]
fn grokclaude_fake_env_guard_skips_unconditionally() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_out = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_out).unwrap();
    let grok_root = tmp.path().join("grok-sessions");
    let session_dir = seed_session_dir(&grok_root, FIXTURE_CWD, FIXTURE_SESSION_UUID);

    let out = Command::new("bash")
        .arg(script_path())
        .args(["--session-dir", session_dir.to_str().unwrap()])
        .env("KB_SESSIONS_DIR", &sessions_out)
        .env("GROK_SESSIONS_ROOT", &grok_root)
        .env("PATH", path_with_kb())
        .env("GROKCLAUDE_FAKE", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    let entries: Vec<_> = std::fs::read_dir(&sessions_out).unwrap().collect();
    assert!(entries.is_empty());
}

/// The at-source substance gate (<1 user or <1 assistant line): a session
/// dir whose chat_history.jsonl never got past the system prompt (e.g. the
/// worker errored before any real exchange) is skipped, not captured as a
/// husk.
#[test]
fn substance_gate_skips_a_transcript_with_no_real_exchange() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_out = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_out).unwrap();
    let grok_root = tmp.path().join("grok-sessions");
    let thin_uuid = "01900000-0000-7000-8000-00000000thin";
    let dir = grok_root.join(url_encode(FIXTURE_CWD)).join(thin_uuid);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("chat_history.jsonl"),
        r#"{"type":"system","content":"fixture-only system prompt"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("summary.json"),
        r#"{"info":{"id":"thin","cwd":"/tmp/kb-grok-fixture-cwd"},"created_at":"2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();

    let run = run_script(
        &["--session-dir", dir.to_str().unwrap()],
        &sessions_out,
        &grok_root,
        &path_with_kb(),
    );
    assert!(run.status.success(), "stderr: {}", run.stderr);
    assert!(run.stderr.contains("no-substance"), "{}", run.stderr);
    let entries: Vec<_> = std::fs::read_dir(&sessions_out).unwrap().collect();
    assert!(entries.is_empty());
}

/// `--job-ulid` on the emitted adapter-meta line + the CHILD-side
/// `grok_job` research row (kb-core `sessions.rs`) round-trip end to end:
/// the daemon's own `parse_session_activity` recovers BOTH `harness` and
/// the `grok_job` row from the ADAPTER'S OWN OUTPUT (not a hand-written
/// fixture) — the two halves (this adapter, and item E's kb-core parse
/// rule) actually agree on the wire shape.
#[test]
fn adapter_output_round_trips_through_parse_session_activity() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_out = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_out).unwrap();
    let grok_root = tmp.path().join("grok-sessions");
    let session_dir = seed_session_dir(&grok_root, FIXTURE_CWD, FIXTURE_SESSION_UUID);

    let run = run_script(
        &[
            "--session-dir",
            session_dir.to_str().unwrap(),
            "--cwd",
            FIXTURE_CWD,
            "--job-ulid",
            FIXTURE_JOB_ULID,
        ],
        &sessions_out,
        &grok_root,
        &path_with_kb(),
    );
    assert!(run.status.success(), "stderr: {}", run.stderr);
    let html_path = only_capture_html(&sessions_out);
    let html = std::fs::read_to_string(&html_path).unwrap();
    let recovered = kb_core::sessions::recover_jsonl_from_capture(&html).unwrap();

    let activity = kb_core::sessions::parse_session_activity(&recovered);
    assert_eq!(activity.harness.as_deref(), Some("grok"));
    assert_eq!(activity.session_id.as_deref(), Some(FIXTURE_SESSION_UUID));
    let grok_job_rows: Vec<_> = activity
        .research
        .iter()
        .filter(|r| r.kind == "grok_job")
        .collect();
    assert_eq!(grok_job_rows.len(), 1, "{:?}", activity.research);
    assert_eq!(grok_job_rows[0].query, FIXTURE_JOB_ULID);
}
