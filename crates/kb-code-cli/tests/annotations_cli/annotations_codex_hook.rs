//! `kb-code-annotations-codex.sh` (V71-X1, recon `cli-agent-surface.md`
//! open question 9) — proves the Codex `apply_patch` adapter against a
//! REAL daemon, mirroring `annotations_hook.rs`'s own boot/fixture-repo
//! conventions (that file's doc explains why: proving the CLI's
//! `GET /api/annotations/open` wiring end to end, not just the adapter's
//! own shell logic against a mock). The adapter itself only owns the
//! apply_patch-envelope-to-per-file-delegate translation — see the
//! script's own header — so these tests focus on THAT translation
//! (single file, multi-file merge, non-apply_patch silence, daemon-down
//! fail-open) rather than re-proving kb-code-annotations.sh's own
//! cap/dedupe/intent-filter behavior, which `annotations_hook.rs` already
//! covers.

use crate::common::git;
use assert_cmd::Command;
use kb_code_server::config::{KbCodeConfig, RepoEntry};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    std::fs::write(dir.join("other.rs"), "fn c() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

async fn boot(
    repo_dir: &Path,
    repo_name: &str,
) -> (
    tempfile::TempDir,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(repo_dir).unwrap(),
        }],
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    let base = format!("http://{addr}");
    wait_for_hook_endpoints(&base, repo_name).await;
    (tmp, base, task)
}

/// Same precondition wait `annotations_hook.rs::wait_for_hook_endpoints`
/// establishes, for the same reason: the hook is fail-open by contract, so
/// "assert it injected" must not race daemon readiness.
async fn wait_for_hook_endpoints(base: &str, repo: &str) {
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let repos_ok = client
            .get(format!("{base}/api/repos"))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        let open_ok = client
            .get(format!("{base}/api/annotations/open"))
            .query(&[("repo", repo), ("intent", "flag-for-agent")])
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if repos_ok && open_ok {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "daemon did not answer /api/repos + /api/annotations/open within the deadline"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

fn create_annotation(daemon: &str, repo: &str, path_line: &str, body: &str) -> String {
    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate",
            path_line,
            "-m",
            body,
            "--intent",
            "flag-for-agent",
            "--daemon",
            daemon,
            "--repo",
            repo,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let created: Value = serde_json::from_slice(&out).expect("valid JSON create response");
    created["id"].as_str().unwrap().to_string()
}

fn hook_script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/kb-code/hooks/kb-code-annotations-codex.sh")
}

/// Run `kb-code-annotations-codex.sh` as a REAL subprocess with a synthetic
/// Codex `apply_patch` PreToolUse payload — mirrors
/// `annotations_hook.rs::run_hook`, just building the apply_patch envelope
/// shape instead of the Claude-Code `tool_input.file_path` one.
fn run_codex_hook(
    session_id: &str,
    command_text: &str,
    daemon: &str,
    xdg_cache_home: &Path,
) -> std::process::Output {
    let script = hook_script_path();
    assert!(
        script.is_file(),
        "hook script missing at {}",
        script.display()
    );
    let payload = json!({
        "session_id": session_id,
        "tool_name": "apply_patch",
        "tool_input": { "command": command_text },
    });
    let mut cmd = StdCommand::new("bash");
    cmd.arg(&script);
    cmd.env("KB_CODE_DAEMON_URL", daemon);
    cmd.env("XDG_CACHE_HOME", xdg_cache_home);
    // Same loaded-parallel-box headroom `annotations_hook.rs::run_hook` uses.
    cmd.env("KB_CODE_HOOK_MAX_TIME", "20");
    cmd.env_remove("KB_CODE_ANNOTATIONS_HOOK");
    cmd.env_remove("HTTP_PROXY");
    cmd.env_remove("HTTPS_PROXY");
    cmd.env_remove("http_proxy");
    cmd.env_remove("https_proxy");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .expect("spawn bash kb-code-annotations-codex.sh");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .expect("write hook stdin");
    child.wait_with_output().expect("wait for hook subprocess")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_file_apply_patch_injects_the_open_flag() {
    let repo_tmp = fixture_repo();
    let (_tmp, daemon, task) = boot(repo_tmp.path(), "fixture").await;
    create_annotation(&daemon, "fixture", "lib.rs:1", "please fix before merging");

    let lib_abs = std::fs::canonicalize(repo_tmp.path().join("lib.rs")).unwrap();
    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-fn a() {{}}\n+fn a2() {{}}\n*** End Patch",
        lib_abs.display()
    );
    let xdg = tempfile::tempdir().unwrap();
    let out = run_codex_hook("sess-codex-A", &patch, &daemon, xdg.path());
    assert!(
        out.status.success(),
        "hook must always exit 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(!stdout.trim().is_empty(), "expected an injection");

    let body: Value = serde_json::from_str(&stdout).expect("hook stdout must be valid JSON");
    assert_eq!(body["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    let ctx = body["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext must be a string");
    assert!(
        ctx.contains("please fix before merging"),
        "ctx must contain the flag body, got: {ctx}"
    );

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_file_apply_patch_merges_every_touched_files_flags() {
    let repo_tmp = fixture_repo();
    let (_tmp, daemon, task) = boot(repo_tmp.path(), "fixture").await;
    create_annotation(&daemon, "fixture", "lib.rs:1", "flag on lib");
    create_annotation(&daemon, "fixture", "other.rs:1", "flag on other");

    let lib_abs = std::fs::canonicalize(repo_tmp.path().join("lib.rs")).unwrap();
    let other_abs = std::fs::canonicalize(repo_tmp.path().join("other.rs")).unwrap();
    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-fn a() {{}}\n+fn a2() {{}}\n*** Update File: {}\n@@\n-fn c() {{}}\n+fn c2() {{}}\n*** End Patch",
        lib_abs.display(),
        other_abs.display()
    );
    let xdg = tempfile::tempdir().unwrap();
    let out = run_codex_hook("sess-codex-B", &patch, &daemon, xdg.path());
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let body: Value = serde_json::from_str(&stdout).expect("hook stdout must be valid JSON");
    let ctx = body["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext must be a string");
    assert!(ctx.contains("flag on lib"), "got: {ctx}");
    assert!(ctx.contains("flag on other"), "got: {ctx}");

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_apply_patch_tool_name_is_silent() {
    let repo_tmp = fixture_repo();
    let (_tmp, daemon, task) = boot(repo_tmp.path(), "fixture").await;
    create_annotation(&daemon, "fixture", "lib.rs:1", "should never show");

    let xdg = tempfile::tempdir().unwrap();
    let script = hook_script_path();
    let payload = json!({
        "session_id": "sess-codex-C",
        "tool_name": "shell",
        "tool_input": { "command": "echo hi" },
    });
    let mut cmd = StdCommand::new("bash");
    cmd.arg(&script);
    cmd.env("KB_CODE_DAEMON_URL", &daemon);
    cmd.env("XDG_CACHE_HOME", xdg.path());
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());

    task.abort();
}

#[tokio::test]
async fn daemon_down_fails_open() {
    let repo_tmp = fixture_repo();
    let lib_abs = std::fs::canonicalize(repo_tmp.path().join("lib.rs")).unwrap();
    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-fn a() {{}}\n+fn a2() {{}}\n*** End Patch",
        lib_abs.display()
    );
    let xdg = tempfile::tempdir().unwrap();
    let out = run_codex_hook("sess-codex-D", &patch, "http://127.0.0.1:1", xdg.path());
    assert!(
        out.status.success(),
        "a down daemon must still exit 0, got {:?}",
        out.status.code()
    );
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
}
