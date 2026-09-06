//! `kb-code-annotations.sh` (D4) — proves the injection, the same bar
//! `kb-code-why.sh` was held to in W5.3: boot a REAL daemon over a temp
//! repo, create real `flag-for-agent` annotations through the CLI (the
//! same path an operator's `kb-code annotate ... --intent flag-for-agent`
//! would take), then run the hook script as an actual subprocess piping a
//! realistic PreToolUse/SessionStart JSON envelope into its stdin —
//! exactly how Claude Code itself invokes it. Mirrors `tests/annotations.rs`'s
//! `boot()`/fixture-repo conventions and
//! `plugins/kb-code/hooks/tests/test-kb-code-why.sh`'s own env-isolated
//! subprocess-invocation approach, just against a real daemon (not a
//! stdlib-http-server mock) since the D4 brief calls for proving the CLI's
//! own `GET /api/annotations/open` wiring end to end, not just the hook's
//! shell logic in isolation.

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
    std::fs::write(dir.join("lib.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
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

/// The hook is BEST-EFFORT by contract: any slow/failed curl makes it print
/// nothing and exit 0 (never block the agent — see the script's own doc).
/// That makes "assert it injected" race daemon readiness, which is what
/// flaked 6 hook tests under the loaded 2026-08-02 gate. Establish the
/// PRECONDITION here — both endpoints the hook calls answer 200 — so the
/// single hook invocation under test is measuring the hook, not the boot.
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

/// Create a `flag-for-agent` (or any other intent) annotation via the REAL
/// CLI create form — the same path an operator's reader-side "flag for
/// agent" action ultimately drives. Returns the created id.
fn create_annotation(
    daemon: &str,
    repo: &str,
    path_line: &str,
    body: &str,
    intent: &str,
) -> String {
    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate", path_line, "-m", body, "--intent", intent, "--daemon", daemon, "--repo",
            repo, "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let created: Value = serde_json::from_slice(&out).expect("valid JSON create response");
    created["id"].as_str().unwrap().to_string()
}

fn reply_to(daemon: &str, id: &str, repo: &str, path: &str, body: &str) {
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate", "reply", id, "-m", body, "--repo", repo, "--path", path, "--daemon", daemon,
        ])
        .assert()
        .success();
}

fn hook_script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/kb-code/hooks/kb-code-annotations.sh")
}

/// Run `kb-code-annotations.sh` as a REAL subprocess — exactly how Claude
/// Code itself invokes a hook: the JSON envelope arrives over stdin, the
/// script's own stdout is its ENTIRE contract (either one
/// `hookSpecificOutput` JSON object, or nothing at all). `xdg_cache_home`
/// is a fresh temp dir per test so the per-session dedup file (and the
/// repos cache shared with kb-code-why.sh) never leak across tests.
/// Mirrors `test-kb-code-why.sh`'s own `run_hook_env` helper, just from
/// Rust against a real daemon.
fn run_hook(
    payload: &Value,
    daemon: &str,
    xdg_cache_home: &Path,
    session_start: bool,
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let script = hook_script_path();
    assert!(
        script.is_file(),
        "hook script missing at {}",
        script.display()
    );
    let mut cmd = StdCommand::new("bash");
    cmd.arg(&script);
    if session_start {
        cmd.arg("--session-start");
    }
    cmd.env("KB_CODE_DAEMON_URL", daemon);
    cmd.env("XDG_CACHE_HOME", xdg_cache_home);
    // Prod default is 1.5s (a hook must never block the agent); a loaded
    // parallel test box can push daemon responses past that, which reads
    // as "hook returned nothing" here (6 tests flaked, 2026-08-02).
    cmd.env("KB_CODE_HOOK_MAX_TIME", "20");
    cmd.env_remove("KB_CODE_ANNOTATIONS_HOOK");
    cmd.env_remove("HTTP_PROXY");
    cmd.env_remove("HTTPS_PROXY");
    cmd.env_remove("http_proxy");
    cmd.env_remove("https_proxy");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn bash kb-code-annotations.sh");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .expect("write hook stdin");
    child.wait_with_output().expect("wait for hook subprocess")
}

fn pretooluse_payload(session_id: &str, file_path: &Path) -> Value {
    json!({
        "session_id": session_id,
        "tool_name": "Edit",
        "tool_input": {
            "file_path": file_path.to_string_lossy(),
            "old_string": "fn a",
            "new_string": "fn a2",
        },
    })
}

fn session_start_payload(session_id: &str, cwd: &Path) -> Value {
    json!({
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
    })
}

// =====================================================================
// PreToolUse — per-edited-file injection.
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn injects_open_flag_for_agent_annotation_with_reply_and_resolve_commands() {
    let repo_tmp = fixture_repo();
    let (_tmp, daemon, task) = boot(repo_tmp.path(), "fixture").await;

    let id = create_annotation(
        &daemon,
        "fixture",
        "lib.rs:2",
        "this loop is O(n^2), please fix before merging",
        "flag-for-agent",
    );

    let xdg = tempfile::tempdir().unwrap();
    let file_path = std::fs::canonicalize(repo_tmp.path().join("lib.rs")).unwrap();
    let payload = pretooluse_payload("sess-inject", &file_path);
    let out = run_hook(&payload, &daemon, xdg.path(), false, &[]);

    assert!(
        out.status.success(),
        "hook must always exit 0, got {:?}; stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    assert!(
        !stdout.trim().is_empty(),
        "expected an injection, got nothing"
    );

    let body: Value = serde_json::from_str(&stdout).expect("hook stdout must be valid JSON");
    assert_eq!(body["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    let ctx = body["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext must be a string");
    assert!(
        ctx.contains("this loop is O(n^2), please fix before merging"),
        "ctx must contain the annotation body, got: {ctx}"
    );
    assert!(
        ctx.contains(&format!(
            "kb-code annotate reply {id} -m \"<what you did>\" --repo fixture --path lib.rs"
        )),
        "ctx must contain the reply command, got: {ctx}"
    );
    assert!(
        ctx.contains(&format!("kb-code annotate resolve {id}")),
        "ctx must contain the resolve command, got: {ctx}"
    );
    assert!(
        ctx.contains("lib.rs:2"),
        "ctx must name the repo-relative path + line, got: {ctx}"
    );

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_invocation_for_the_same_fully_shown_file_is_silent() {
    let repo_tmp = fixture_repo();
    let (_tmp, daemon, task) = boot(repo_tmp.path(), "fixture").await;

    create_annotation(
        &daemon,
        "fixture",
        "lib.rs:2",
        "only one flag here",
        "flag-for-agent",
    );

    let xdg = tempfile::tempdir().unwrap();
    let file_path = std::fs::canonicalize(repo_tmp.path().join("lib.rs")).unwrap();
    let payload = pretooluse_payload("sess-dedup", &file_path);

    let first = run_hook(&payload, &daemon, xdg.path(), false, &[]);
    assert!(first.status.success());
    assert!(
        !String::from_utf8_lossy(&first.stdout).trim().is_empty(),
        "first call should inject"
    );

    let second = run_hook(&payload, &daemon, xdg.path(), false, &[]);
    assert!(second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stdout).trim().is_empty(),
        "second call for a file with nothing new to show must be silent, got: {}",
        String::from_utf8_lossy(&second.stdout)
    );

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caps_at_two_per_file_and_discloses_the_rest_on_a_later_edit() {
    let repo_tmp = fixture_repo();
    let (_tmp, daemon, task) = boot(repo_tmp.path(), "fixture").await;

    create_annotation(&daemon, "fixture", "lib.rs:1", "flag one", "flag-for-agent");
    create_annotation(&daemon, "fixture", "lib.rs:2", "flag two", "flag-for-agent");
    create_annotation(
        &daemon,
        "fixture",
        "lib.rs:3",
        "flag three",
        "flag-for-agent",
    );

    let xdg = tempfile::tempdir().unwrap();
    let file_path = std::fs::canonicalize(repo_tmp.path().join("lib.rs")).unwrap();
    let payload = pretooluse_payload("sess-cap", &file_path);

    let first = run_hook(&payload, &daemon, xdg.path(), false, &[]);
    assert!(first.status.success());
    let ctx1 = String::from_utf8(first.stdout).unwrap();
    let shown_in_first = ["flag one", "flag two", "flag three"]
        .iter()
        .filter(|f| ctx1.contains(**f))
        .count();
    assert_eq!(
        shown_in_first, 2,
        "at most 2 flags per file per call, got: {ctx1}"
    );

    // A later edit of the SAME file in the SAME session discloses the
    // still-unseen 3rd flag — the per-file cap throttles a single call, it
    // never permanently hides an annotation nobody has replied to.
    let second = run_hook(&payload, &daemon, xdg.path(), false, &[]);
    assert!(second.status.success());
    let ctx2 = String::from_utf8(second.stdout).unwrap();
    assert!(
        !ctx2.trim().is_empty(),
        "the 3rd, still-unseen flag must surface on a later edit"
    );
    let shown_total: std::collections::HashSet<&str> = ["flag one", "flag two", "flag three"]
        .into_iter()
        .filter(|f| ctx1.contains(f) || ctx2.contains(f))
        .collect();
    assert_eq!(
        shown_total.len(),
        3,
        "across both calls every flag must eventually surface exactly once; \
         call1={ctx1}\ncall2={ctx2}"
    );

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_flag_for_agent_intent_is_surfaced_not_other_intents() {
    let repo_tmp = fixture_repo();
    let (_tmp, daemon, task) = boot(repo_tmp.path(), "fixture").await;

    create_annotation(
        &daemon,
        "fixture",
        "lib.rs:1",
        "just a todo, not a flag",
        "todo",
    );
    create_annotation(
        &daemon,
        "fixture",
        "lib.rs:2",
        "an actual operator flag",
        "flag-for-agent",
    );

    let xdg = tempfile::tempdir().unwrap();
    let file_path = std::fs::canonicalize(repo_tmp.path().join("lib.rs")).unwrap();
    let payload = pretooluse_payload("sess-intent", &file_path);
    let out = run_hook(&payload, &daemon, xdg.path(), false, &[]);
    assert!(out.status.success());
    let ctx = String::from_utf8(out.stdout).unwrap();
    assert!(ctx.contains("an actual operator flag"));
    assert!(!ctx.contains("just a todo, not a flag"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reply_count_is_surfaced_when_the_thread_has_replies() {
    let repo_tmp = fixture_repo();
    let (_tmp, daemon, task) = boot(repo_tmp.path(), "fixture").await;

    let id = create_annotation(
        &daemon,
        "fixture",
        "lib.rs:2",
        "please address this",
        "flag-for-agent",
    );
    reply_to(
        &daemon,
        &id,
        "fixture",
        "lib.rs",
        "already looked, need more time",
    );

    let xdg = tempfile::tempdir().unwrap();
    let file_path = std::fs::canonicalize(repo_tmp.path().join("lib.rs")).unwrap();
    let payload = pretooluse_payload("sess-replies", &file_path);
    let out = run_hook(&payload, &daemon, xdg.path(), false, &[]);
    assert!(out.status.success());
    let ctx = String::from_utf8(out.stdout).unwrap();
    assert!(
        ctx.contains("thread has 1 replies")
            && ctx.contains("kb-code annotations lib.rs --repo fixture"),
        "expected a reply-count nudge, got: {ctx}"
    );

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_switch_silences_even_with_an_open_flag() {
    let repo_tmp = fixture_repo();
    let (_tmp, daemon, task) = boot(repo_tmp.path(), "fixture").await;
    create_annotation(
        &daemon,
        "fixture",
        "lib.rs:2",
        "should never show",
        "flag-for-agent",
    );

    let xdg = tempfile::tempdir().unwrap();
    let file_path = std::fs::canonicalize(repo_tmp.path().join("lib.rs")).unwrap();
    let payload = pretooluse_payload("sess-kill", &file_path);
    let out = run_hook(
        &payload,
        &daemon,
        xdg.path(),
        false,
        &[("KB_CODE_ANNOTATIONS_HOOK", "off")],
    );
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "kill switch must silence the hook entirely"
    );
    assert!(
        !xdg.path()
            .join("kb-code/annotations-hook-seen-sess-kill")
            .exists(),
        "kill switch must not write any state"
    );

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_down_fails_open() {
    let repo_tmp = fixture_repo();
    // No daemon booted at all — the hook must still exit 0 with no output.
    let xdg = tempfile::tempdir().unwrap();
    let file_path = std::fs::canonicalize(repo_tmp.path().join("lib.rs")).unwrap();
    let payload = pretooluse_payload("sess-down", &file_path);
    let out = run_hook(&payload, "http://127.0.0.1:1", xdg.path(), false, &[]);
    assert!(
        out.status.success(),
        "a down daemon must still exit 0, got {:?}",
        out.status.code()
    );
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_edit_write_tool_is_silent() {
    let repo_tmp = fixture_repo();
    let (_tmp, daemon, task) = boot(repo_tmp.path(), "fixture").await;
    create_annotation(
        &daemon,
        "fixture",
        "lib.rs:2",
        "should never show",
        "flag-for-agent",
    );

    let xdg = tempfile::tempdir().unwrap();
    let file_path = std::fs::canonicalize(repo_tmp.path().join("lib.rs")).unwrap();
    let mut payload = pretooluse_payload("sess-bash", &file_path);
    payload["tool_name"] = json!("Bash");
    let out = run_hook(&payload, &daemon, xdg.path(), false, &[]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());

    task.abort();
}

// =====================================================================
// SessionStart — one summary line, no per-annotation detail.
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_start_summarizes_open_flags_in_the_repo() {
    let repo_tmp = fixture_repo();
    let (_tmp, daemon, task) = boot(repo_tmp.path(), "fixture").await;
    create_annotation(&daemon, "fixture", "lib.rs:1", "flag one", "flag-for-agent");
    create_annotation(&daemon, "fixture", "lib.rs:2", "flag two", "flag-for-agent");

    let xdg = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(repo_tmp.path()).unwrap();
    let payload = session_start_payload("sess-start-1", &cwd);
    let out = run_hook(&payload, &daemon, xdg.path(), true, &[]);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(!stdout.trim().is_empty(), "expected a summary line");

    let body: Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(body["hookSpecificOutput"]["hookEventName"], "SessionStart");
    let ctx = body["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.contains("2 operator flag(s)"), "got: {ctx}");
    assert!(ctx.contains("fixture"), "got: {ctx}");
    assert!(
        ctx.contains("kb-code annotations open --repo fixture --intent flag-for-agent"),
        "got: {ctx}"
    );
    // No per-annotation dump — neither flag body should appear individually.
    assert!(!ctx.contains("flag one"));
    assert!(!ctx.contains("flag two"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_start_silent_when_no_open_flags_exist() {
    let repo_tmp = fixture_repo();
    let (_tmp, daemon, task) = boot(repo_tmp.path(), "fixture").await;

    let xdg = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(repo_tmp.path()).unwrap();
    let payload = session_start_payload("sess-start-2", &cwd);
    let out = run_hook(&payload, &daemon, xdg.path(), true, &[]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_start_matches_a_subdirectory_cwd_via_longest_prefix() {
    let repo_tmp = fixture_repo();
    std::fs::create_dir_all(repo_tmp.path().join("src")).unwrap();
    let (_tmp, daemon, task) = boot(repo_tmp.path(), "fixture").await;
    create_annotation(&daemon, "fixture", "lib.rs:1", "flag one", "flag-for-agent");

    let xdg = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(repo_tmp.path().join("src")).unwrap();
    let payload = session_start_payload("sess-start-3", &cwd);
    let out = run_hook(&payload, &daemon, xdg.path(), true, &[]);
    assert!(out.status.success());
    let ctx = String::from_utf8(out.stdout).unwrap();
    assert!(ctx.contains("1 operator flag(s)"), "got: {ctx}");

    task.abort();
}
