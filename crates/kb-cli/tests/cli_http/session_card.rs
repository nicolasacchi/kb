//! `kb sessions card` against an in-process daemon (the `slate.rs` boot
//! shape).
//!
//! What this is FOR: the CLI↔wire contract A3 owns — test 14. `kb
//! sessions card <sid>` prints the daemon's `text` byte-for-byte (the
//! CLI NEVER renders card lanes of its own; the slate-digest precedent),
//! `--json` carries the same `text`, a missing session is the not-found
//! error, and no daemon is a clear error. The card's content is pinned
//! by kb-server's `http_extra/session_card.rs`; nothing here re-asserts
//! lane arithmetic.
use crate::common;

use assert_cmd::Command;
use kb_core::paths::KbPaths;
use kb_core::storage::sqlite::{Db, SessionRow};
use kb_core::types::KbName;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

struct Fixture {
    _tmp: tempfile::TempDir,
    url: String,
    cache: PathBuf,
    paths: KbPaths,
}

async fn boot() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let daemon_name = format!(
        "cli-session-card-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map = BTreeMap::new();
    kb_map.insert(KbName::new("smoke").unwrap(), common::kb_section(source));
    let cfg = common::base_config(&daemon_name, kb_map);
    let paths = KbPaths::rooted_at(tmp.path(), &daemon_name);
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths.clone())
        .await
        .expect("serve");
    let url = format!("http://{addr}");
    {
        let client = reqwest::Client::new();
        let probe = url.clone();
        common::poll_until("daemon /api/identity", move || {
            let client = client.clone();
            let probe = probe.clone();
            async move {
                client
                    .get(format!("{probe}/api/identity"))
                    .send()
                    .await
                    .ok()?
                    .status()
                    .is_success()
                    .then_some(())
            }
        })
        .await;
    }
    Fixture {
        _tmp: tmp,
        url,
        cache,
        paths,
    }
}

impl Fixture {
    fn cmd(&self, args: &[&str]) -> Command {
        let mut full = vec!["sessions"];
        full.extend_from_slice(args);
        full.extend_from_slice(&["--daemon", &self.url]);
        let mut cmd = Command::cargo_bin("kb").unwrap();
        cmd.env("KB_TEST_HTTP_TIMEOUT_SECS", common::http_timeout_secs())
            .env("XDG_CACHE_HOME", &self.cache)
            .env_remove("KB_SESSION_ID")
            .env_remove("KB_HARNESS")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("GROK_SESSION_ID")
            .args(&full);
        cmd
    }

    fn cmd_no_daemon(&self, args: &[&str]) -> Command {
        let mut full = vec!["sessions"];
        full.extend_from_slice(args);
        full.extend_from_slice(&["--daemon", "http://127.0.0.1:1"]);
        let mut cmd = Command::cargo_bin("kb").unwrap();
        cmd.env("KB_TEST_HTTP_TIMEOUT_SECS", common::http_timeout_secs())
            .env("XDG_CACHE_HOME", &self.cache)
            .env_remove("KB_SESSION_ID")
            .env_remove("KB_HARNESS")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("GROK_SESSION_ID")
            .args(&full);
        cmd
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self
            .cmd(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(out).unwrap()
    }

    fn seed(&self) {
        let mut db = Db::open(&self.paths.kb_sqlite(&KbName::new("smoke").unwrap())).unwrap();
        db.sessions_upsert(&SessionRow {
            artifact_id: "art-cli-1".into(),
            session_id: "cli-1".into(),
            started_at: 1_000,
            ended_at: 1_600,
            message_count: 42,
            first_user_prompt: Some("synthetic prompt".into()),
            source_relative: "sessions/session-1000-cli-1.html".into(),
            title: Some("synthetic title".into()),
            cwd: Some("/work/alpha".into()),
            git_branch: Some("main".into()),
            files_read_count: 3,
            files_edited_count: 2,
            token_total: 12_345,
            tool_calls: 9,
            model: Some("synthetic-model".into()),
            error_count: 1,
            subagent_count: 1,
            subagent_tokens: 5_000,
            subagent_tool_calls: 4,
            subagent_files_edited: 1,
            subagent_launched_unstatted: 0,
            project_key: Some("-work-alpha".into()),
            repo_root: Some("/work/alpha".into()),
            harness: "claude".into(),
            cc_version: None,
            last_assistant_text: Some("synthetic closing prose".into()),
            all_cwds: None,
            commit_count: 0,
            user_turns: 2,
            active_secs: 321,
            substance: None,
            segment_of: None,
            segment_idx: None,
            evidence_json: None,
            parent_session_id: None,
            root_session_id: None,
            project_slug: None,
            continued_from_harness: None,
            continued_from_session_id: None,
        })
        .unwrap();
    }

    async fn daemon_card(&self) -> Value {
        reqwest::get(format!("{}/api/sessions/cli-1/card", self.url))
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

/// Test 14 — `kb sessions card` prints the daemon `text` verbatim;
/// `--json` carries the same `text`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kb_sessions_card_prints_the_daemon_text_verbatim() {
    let fx = boot().await;
    fx.seed();
    let card = fx.daemon_card().await;
    let daemon_text = card["text"].as_str().unwrap().to_string();
    assert!(!daemon_text.is_empty());
    assert!(daemon_text.contains("terminal: unavailable (terminal disabled)"));

    let stdout = fx.ok(&["card", "cli-1"]);
    assert_eq!(
        stdout, daemon_text,
        "the CLI must print the daemon `text` byte-for-byte, never re-render it"
    );

    let json_stdout = fx.ok(&["card", "cli-1", "--json"]);
    let as_json: Value = serde_json::from_str(&json_stdout).expect("json stdout");
    assert_eq!(as_json["text"], card["text"]);
    assert_eq!(as_json["schema"], "kb-session-card/1");
    assert_eq!(as_json["session_id"], "cli-1");
}

/// A missing session is the not-found error other `kb sessions` verbs
/// use — a failure, not an empty card.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kb_sessions_card_missing_session_fails() {
    let fx = boot().await;
    fx.cmd(&["card", "ghost-404"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("session ghost-404 not found"));
}

/// `kb sessions card` with no daemon is a clear daemon-not-found error
/// (brief S4). Without this pin, replacing `require_daemon` with a
/// fallback URL would break the contract while staying green. Points at
/// the repo-convention dead port so it never probes the default daemon.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kb_sessions_card_no_daemon_is_a_clear_error() {
    let fx = boot().await;
    fx.cmd_no_daemon(&["card", "cli-1"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("daemon not reachable"));
}

/// FUWEBOPEN — a session the registry knows from a beat but with no
/// capture yet is a live-only card: the CLI prints the daemon's text
/// verbatim, which says "capture: pending" and prints no capture-backed
/// lane as a fact; `--json` carries the marker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kb_sessions_card_renders_a_live_only_card_honestly() {
    let fx = boot().await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/sessions/beat", fx.url))
        .json(&serde_json::json!({
            "v": 1,
            "session_id": "live-cli-1",
            "harness": "claude",
            "event": "start",
            "at": chrono::Utc::now().to_rfc3339(),
            "cwd": "/work/alpha",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    let stdout = fx.ok(&["card", "live-cli-1"]);
    assert!(stdout.contains("capture: pending"), "{stdout}");
    assert!(stdout.contains("session live-cli-1"), "{stdout}");
    assert!(!stdout.contains("/work/alpha"), "{stdout}");
    for fact in ["files:", "outcome:", "commits:", "decisions:", "started:"] {
        assert!(
            !stdout.contains(fact),
            "{fact} printed as a fact on a live-only card: {stdout}"
        );
    }
    let json_stdout = fx.ok(&["card", "live-cli-1", "--json"]);
    let as_json: Value = serde_json::from_str(&json_stdout).expect("json stdout");
    assert_eq!(as_json["capture_pending"], true);
    // The live lane prints "<N>s ago" from the daemon's wall clock at each
    // request, so two separate invocations can straddle a second boundary.
    // Normalise ONLY that token (asserting it is present and well-formed in
    // both) and compare everything else byte-for-byte.
    let json_text = as_json["text"].as_str().expect("text is a string");
    assert_eq!(
        mask_age(json_text),
        mask_age(&stdout),
        "the CLI must print the daemon `text` verbatim (age token aside)"
    );
}

/// Replace the single `<digits>s ago` token of a live lane with `<age>`,
/// panicking if it is absent, malformed or not unique.
fn mask_age(text: &str) -> String {
    let mut out = String::new();
    let mut found = 0;
    for line in text.split_inclusive('\n') {
        match line.find("s ago") {
            Some(end) => {
                let head = &line[..end];
                let digits = head
                    .chars()
                    .rev()
                    .take_while(|c| c.is_ascii_digit())
                    .count();
                assert!(digits > 0, "malformed age token in {line:?}");
                out.push_str(&head[..head.len() - digits]);
                out.push_str("<age>");
                out.push_str(&line[end + 1..]);
                found += 1;
            }
            None => out.push_str(line),
        }
    }
    assert_eq!(
        found, 1,
        "expected exactly one `<N>s ago` token in {text:?}"
    );
    out
}
