//! `kb slate` end-to-end against an in-process daemon (the `desk.rs`
//! boot shape).
//!
//! What these are FOR: the CLI↔wire contract SL3 owns — the exit-code
//! ladder (§9: 3 for BOTH refusals, 2 for not-found), the `--anyway`
//! escape, the displaced/nudge feedback, the cursor/topic markers, and the
//! ONE parity assertion that keeps the CLI from ever growing a second
//! renderer: `kb slate open`'s stdout is byte-identical to the server's
//! `DigestResponse.text`. The projection arithmetic itself is pinned by
//! kb-core's goldens and the routes by kb-server's; nothing here re-asserts
//! either.
use crate::common;

use assert_cmd::Command;
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

struct Fixture {
    _tmp: tempfile::TempDir,
    url: String,
    cache: PathBuf,
    slug: String,
}

async fn boot(slug: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let daemon_name = format!(
        "cli-slate-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map = BTreeMap::new();
    kb_map.insert(KbName::new("smoke").unwrap(), common::kb_section(source));
    let cfg = common::base_config(&daemon_name, kb_map);
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(600)).await;
    Fixture {
        _tmp: tmp,
        url: format!("http://{addr}"),
        cache,
        slug: slug.to_string(),
    }
}

impl Fixture {
    /// `kb slate <args…> --slate <slug> --daemon <url>` with a private
    /// cache dir, so the cursor/topic markers never touch the operator's
    /// real `~/.cache/kb` and no ambient session id leaks in.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut full = vec!["slate"];
        full.extend_from_slice(args);
        full.extend_from_slice(&["--slate", &self.slug, "--daemon", &self.url]);
        let mut cmd = Command::cargo_bin("kb").unwrap();
        cmd.env("KB_TEST_HTTP_TIMEOUT_SECS", common::http_timeout_secs())
            .env("XDG_CACHE_HOME", &self.cache)
            .env_remove("KB_SESSION_ID")
            .env_remove("KB_HARNESS")
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

    fn ok_json(&self, args: &[&str]) -> Value {
        let mut a = args.to_vec();
        a.push("--json");
        serde_json::from_str(&self.ok(&a)).expect("json stdout")
    }

    async fn digest(&self) -> Value {
        reqwest::get(format!("{}/api/slates/{}", self.url, self.slug))
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

/// §9's whole exit-code ladder plus the `--anyway` escape, in the order a
/// real pair of sessions would hit them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slate_round_trip_open_take_refuse_anyway_mark_drop_history() {
    let f = boot("orchard").await;

    // 1. A slate that has never been posted to is a 404 → exit 2.
    f.cmd(&["open"]).assert().code(2);

    // 2. The first post CREATES the slate (there is no create verb).
    let now = f.ok_json(&["now", "wiring the take ladder", "--session-id", "sess-a"]);
    assert_eq!(now["post"]["kind"], "now");
    assert_eq!(now["post"]["seq"], 1);
    assert_eq!(now["displaced"], serde_json::json!([]));

    // 3. sess-a takes a path.
    let take = f.ok_json(&[
        "take",
        "crates/kb-server",
        "A2 bearer graduation",
        "--session-id",
        "sess-a",
    ]);
    let held = take["post"]["seq"].as_u64().unwrap();
    assert_eq!(take["post"]["subject"], "crates/kb-server");

    // 4. A second LIVE session taking the same subject is exit 3, and the
    //    refusal names the holder (`slate-taken` + the `holder` member).
    let refused = f
        .cmd(&[
            "take",
            "crates/kb-server/src/review_gate.rs",
            "same ground",
            "--session-id",
            "sess-b",
        ])
        .assert()
        .code(3);
    let stderr = String::from_utf8(refused.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("slate-taken"), "{stderr}");
    assert!(stderr.contains("holder: #"), "{stderr}");

    // 4b. `--json` on a refusal passes the whole problem+json through and
    //     still exits 3 — the machine half of the same contract.
    let refused_json = f
        .cmd(&[
            "take",
            "crates/kb-server/src/review_gate.rs",
            "same ground",
            "--session-id",
            "sess-b",
            "--json",
        ])
        .assert()
        .code(3);
    let body: Value =
        serde_json::from_slice(&refused_json.get_output().stdout).expect("problem+json on stdout");
    assert_eq!(body["code"], "slate-taken");
    assert_eq!(body["status"], 409);
    assert_eq!(body["holder"]["seq"], held);

    // 5. `--anyway` posts it CONTESTED instead of refusing.
    let contested = f.ok_json(&[
        "take",
        "crates/kb-server/src/review_gate.rs",
        "same ground",
        "--session-id",
        "sess-b",
        "--anyway",
    ]);
    assert_eq!(contested["post"]["anyway"], true);

    // 6. A mark is a plus-one on someone ELSE's post.
    let marked = f.ok(&["mark", &format!("#{held}"), "--session-id", "sess-b"]);
    assert!(marked.contains(&format!("mark → #{held}")), "{marked}");

    // 7. Dropping a LIVE other session's take needs `--anyway` — the
    //    SECOND exit-3 refusal (`slate-live-author`), which is why a loop
    //    caller can branch on the status alone.
    let live_author = f
        .cmd(&[
            "drop",
            &format!("#{held}"),
            "superseded by the contested one",
            "--session-id",
            "sess-b",
        ])
        .assert()
        .code(3);
    let stderr = String::from_utf8(live_author.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("slate-live-author"), "{stderr}");
    let dropped = f.ok(&[
        "drop",
        &format!("#{held}"),
        "superseded by the contested one",
        "--session-id",
        "sess-b",
        "--anyway",
    ]);
    assert!(
        dropped.starts_with(&format!("#{} drop → #{held} · orchard", held + 3)),
        "{dropped}"
    );

    // 8. `history` is the permanent record of that removal.
    let hist = f.ok(&["history"]);
    assert!(hist.contains(&format!("#{held} take")), "{hist}");
    assert!(hist.contains("dropped by"), "{hist}");
    assert!(hist.contains("superseded by the contested one"), "{hist}");

    // 9. D27 (v0.42): a successful `open` fire-and-forget REPORTS its
    //    session's cursor. `report_cursor` is awaited (with its own 1s
    //    timeout and swallowed errors) before the CLI process exits, so
    //    `GET /api/slates` sees it land: `sessions_served` goes from 0 to
    //    1 for a session that has never posted, only opened.
    f.ok(&["open", "--session-id", "sess-open-only"]);
    let slates: Value = reqwest::get(format!("{}/api/slates", f.url))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = slates
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["slug"] == "orchard")
        .expect("the orchard slate is in the fleet-wide list");
    assert_eq!(row["sessions_served"], 1, "{row}");
}

/// The ONE assertion that keeps a second renderer from ever appearing:
/// `kb slate open` prints the server's `text` byte for byte (module doc
/// point 1 of `commands/slate.rs`; §9 "text is byte-identical to the CLI
/// render"). Runs with NO session id, so neither `?since=` nor `?session=`
/// can make the two reads differ.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_stdout_is_byte_identical_to_the_server_digest_text() {
    let f = boot("parity").await;
    f.ok(&["now", "the milestone line", "--session-id", "sess-a"]);
    f.ok(&[
        "warn",
        "every cargo goes through the flock",
        "--session-id",
        "sess-a",
    ]);
    f.ok(&[
        "found",
        "auth ends at the gate",
        "--ref",
        "path:crates/kb-server/src/review_gate.rs:88",
        "--session-id",
        "sess-a",
    ]);
    f.ok(&[
        "ask",
        "before or after the migrations?",
        "--session-id",
        "sess-b",
    ]);

    let stdout = f.ok(&["open"]);
    let server = f.digest().await;
    assert_eq!(
        stdout,
        server["text"].as_str().unwrap(),
        "the CLI must print the server's text verbatim, trailing newline included"
    );
    // …and the echo line is really there, so the parity is not vacuous.
    assert!(stdout.contains("NOW"), "{stdout}");
    assert!(
        stdout.trim_end().ends_with("the milestone line"),
        "{stdout}"
    );
}

/// `open` seeds the cursor in EVERY mode; `delta` reads it when `--since`
/// is absent and advances it; an explicit `--since` never touches the file
/// (rules matrix "Cursor": the cursor is client state, and no READ writes
/// on the server side).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cursor_is_seeded_by_open_advanced_by_delta_and_untouched_by_explicit_since() {
    let f = boot("cursors").await;
    f.ok(&["now", "first", "--session-id", "sess-a"]);
    let marker = f.cache.join("kb/slate-cursor-sess-c");
    assert!(!marker.exists());

    f.ok(&["open", "--hybrid", "--session-id", "sess-c"]);
    assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "1");

    // A post from ANOTHER session, then a delta with no --since: the
    // cursor is the since, and it advances to the new head.
    f.ok(&["idea", "worth trying the pool", "--session-id", "sess-a"]);
    let text = f.ok(&["delta", "--session-id", "sess-c"]);
    assert!(text.contains("worth trying the pool"), "{text}");
    assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "2");

    // An explicit --since is the caller's own bookkeeping: the file stays.
    f.ok(&["delta", "--since", "0", "--session-id", "sess-c"]);
    assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "2");
}

/// `open --topic T` DECLARES the session topic; later posting verbs pick
/// it up, and a read is never silently narrowed by it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_topic_declares_the_session_topic_for_later_posts() {
    let f = boot("topics").await;
    f.ok(&["now", "seed", "--session-id", "sess-a"]);
    f.ok(&["open", "--topic", "v7", "--session-id", "sess-t"]);
    assert_eq!(
        std::fs::read_to_string(f.cache.join("kb/slate-topic-sess-t"))
            .unwrap()
            .trim(),
        "v7"
    );
    let posted = f.ok_json(&["idea", "the lane budget", "--session-id", "sess-t"]);
    assert_eq!(posted["post"]["topic"], "v7");
    // An explicit --topic still wins over the marker.
    let posted = f.ok_json(&[
        "idea",
        "elsewhere",
        "--session-id",
        "sess-t",
        "--topic",
        "perf",
    ]);
    assert_eq!(posted["post"]["topic"], "perf");
}

/// `edit` is client-side sugar: it reads #n and copies kind/topic/subject/
/// refs unless overridden, then posts with `supersedes` (§9).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edit_copies_the_targets_fields_and_supersedes_it() {
    let f = boot("edits").await;
    let found = f.ok_json(&[
        "found",
        "auth ends at the gate",
        "--ref",
        "path:crates/kb-server/src/review_gate.rs:88",
        "--topic",
        "v7",
        "--session-id",
        "sess-a",
    ]);
    let seq = found["post"]["seq"].as_u64().unwrap();
    let edited = f.ok_json(&[
        "edit",
        &format!("#{seq}"),
        "auth ends at review_gate, not the handler",
        "--session-id",
        "sess-a",
    ]);
    assert_eq!(edited["post"]["kind"], "found");
    assert_eq!(edited["post"]["topic"], "v7");
    assert_eq!(edited["post"]["supersedes"], seq);
    assert_eq!(
        edited["post"]["refs"],
        serde_json::json!(["path:crates/kb-server/src/review_gate.rs:88"]),
        "a found's ref must survive the edit or the post would 400"
    );
}

/// The SL3a hook contract: the wake hook reads `.text` and `.head_seq` off
/// `open --hybrid … --json`, and the recall hook reads the same two off
/// `delta --since … --json`. If either key moves, both hooks silently show
/// nothing — so this pins them explicitly rather than relying on the human
/// path's parity test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hook_json_carries_text_and_head_seq_for_open_and_delta() {
    let f = boot("hooks").await;
    f.ok(&["now", "the milestone line", "--session-id", "sess-a"]);

    let wake = f.ok_json(&[
        "open",
        "--hybrid",
        "--budget",
        "2000",
        "--session-id",
        "sess-hook",
    ]);
    assert!(wake["text"]
        .as_str()
        .is_some_and(|t| t.contains("the milestone line")));
    assert_eq!(wake["head_seq"], 1);

    f.ok(&["idea", "worth trying the pool", "--session-id", "sess-a"]);
    let delta = f.ok_json(&[
        "delta",
        "--since",
        "1",
        "--budget",
        "1500",
        "--session-id",
        "sess-hook",
    ]);
    assert!(delta["text"]
        .as_str()
        .is_some_and(|t| t.contains("worth trying the pool")));
    assert_eq!(delta["head_seq"], 2);
    assert_eq!(delta["truncated"], false);
}
