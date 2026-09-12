//! V72-J1 — `comments/1`: the four `GET /api/comments*` reads, the drift
//! oracle over a real git fixture, and the `GET /api/todos` subsumption.
//!
//! Boots a daemon the same way `todos_route.rs` does, over a synthetic
//! in-repo fixture with PINNED commit dates (`GIT_AUTHOR_DATE`/
//! `GIT_COMMITTER_DATE`) so the drift arithmetic is exact rather than
//! "some number of days".

use kb_code_server::config::{CommentsSection, KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// c1 — everything's first commit.
const T1: i64 = 1_700_000_000;
/// c2 — fourteen days later; only `order.rb`'s BODY moves.
const T2: i64 = T1 + 14 * 86_400;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_commit(dir: &Path, msg: &str, at: i64) {
    let date = format!("{at} +0000");
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-q", "-m", msg])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `note.rb` and `order.rb` start life identical in shape: a magic
/// comment, a doc block, and a three-line method. Only `order.rb` is
/// touched again in c2.
const RB_V1: &str = "\
# frozen_string_literal: true

# Returns the order total in cents.
def total
  1
end
";

const RB_V2: &str = "\
# frozen_string_literal: true

# Returns the order total in cents.
def total
  2
end
";

const WIDGET_RB: &str = "\
# rubocop:disable Metrics/AbcSize
# TODO(on: date('2020-01-01'), to: 'owner@example.com') drop the shim
# TODO: drop this FIXME shim
class Widget
end
# rubocop:enable Metrics/AbcSize -- the block above is the whole method
";

const SETTINGS_YML: &str = "\
# TODO: yaml markers were invisible to the pre-comments/1 index
key: 1
";

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(dir.join("app/models")).unwrap();
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(dir.join("app/models/order.rb"), RB_V1).unwrap();
    std::fs::write(dir.join("app/models/note.rb"), RB_V1).unwrap();
    std::fs::write(dir.join("app/models/widget.rb"), WIDGET_RB).unwrap();
    std::fs::write(dir.join("config/settings.yml"), SETTINGS_YML).unwrap();
    git(dir, &["add", "-A"]);
    git_commit(dir, "c1", T1);

    // c2 touches ONLY the body of `order.rb#total` — its doc block is
    // byte-identical, which is exactly the shape the oracle reports.
    std::fs::write(dir.join("app/models/order.rb"), RB_V2).unwrap();
    git(dir, &["add", "-A"]);
    git_commit(dir, "c2", T2);
    tmp
}

fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: Some("http://127.0.0.1:0".to_string()),
        token_file: None,
        public_url: None,
    }
}

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot(path: &Path, comments: CommentsSection) -> Boot {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "r".to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: disabled_kb_daemon(),
        comments,
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    Boot {
        tmp,
        base: format!("http://{addr}"),
        task,
    }
}

/// Wait until every fixture file's comment rows have landed. Waiting on
/// `file_count` alone races the per-file `replace_comments` (the `files`
/// row lands before its derived rows) — the same flake `todos_route.rs`
/// records for the table this one replaces.
async fn wait_for_indexed(base: &str, expected_comments: u64) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(resp) = client
            .get(format!("{base}/api/comments/summary"))
            .query(&[("repo", "r")])
            .send()
            .await
        {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if body["total"].as_u64().unwrap_or(0) >= expected_comments {
                    return;
                }
            }
        }
        assert!(Instant::now() < deadline, "indexing timed out");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn get(base: &str, path: &str, query: &[(&str, &str)]) -> (u16, serde_json::Value) {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}{path}"))
        .query(query)
        .send()
        .await
        .expect("request");
    let status = resp.status().as_u16();
    let body = resp.json::<serde_json::Value>().await.unwrap_or_default();
    (status, body)
}

fn rows(body: &serde_json::Value) -> Vec<&serde_json::Value> {
    body["comments"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn find_row<'a>(body: &'a serde_json::Value, path: &str, line: i64) -> &'a serde_json::Value {
    rows(body)
        .into_iter()
        .find(|c| c["path"] == path && c["line_start"] == line)
        .unwrap_or_else(|| panic!("no row at {path}:{line} in {body:#?}"))
}

// Expected rows across the fixture:
//   order.rb      1 directive (the magic comment), 1 doc
//   note.rb       1 directive, 1 doc
//   widget.rb     2 directives, 2 annotations
//   settings.yml  1 annotation
const EXPECTED_TOTAL: u64 = 9;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn comments_index_classifies_states_and_subsumes_the_todo_index() {
    let repo = fixture_repo();
    let b = boot(repo.path(), CommentsSection::default()).await;
    wait_for_indexed(&b.base, EXPECTED_TOTAL).await;

    // --- the doc lane + the drift oracle --------------------------------
    let (status, docs) = get(&b.base, "/api/comments", &[("repo", "r"), ("kind", "doc")]).await;
    assert_eq!(status, 200);
    assert_eq!(docs["total"].as_u64(), Some(2), "{docs:#?}");
    // The blame budget covered both files, and the response says so.
    assert_eq!(docs["blame"]["files_blamed"].as_u64(), Some(2));
    assert_eq!(docs["blame"]["exhausted"].as_bool(), Some(false));

    let order = find_row(&docs, "app/models/order.rb", 3);
    assert_eq!(order["state"]["state"], "drifted", "{order:#?}");
    assert_eq!(
        order["state"]["age_days"].as_i64(),
        Some(14),
        "c2 landed exactly 14 days after c1: {order:#?}"
    );
    assert!(order["state"]["code_commit"].is_string());
    assert!(order["state"]["doc_commit"].is_string());
    assert_ne!(order["state"]["code_commit"], order["state"]["doc_commit"]);
    assert_eq!(order["symbol"]["name"], "total");

    let note = find_row(&docs, "app/models/note.rb", 3);
    assert_eq!(
        note["state"]["state"], "fresh",
        "a file untouched since its doc landed: {note:#?}"
    );

    // --- the state filter -----------------------------------------------
    let (_, drifted) = get(
        &b.base,
        "/api/comments",
        &[("repo", "r"), ("state", "drifted")],
    )
    .await;
    assert_eq!(drifted["total"].as_u64(), Some(1), "{drifted:#?}");
    assert_eq!(rows(&drifted)[0]["path"], "app/models/order.rb");

    let (_, aged) = get(
        &b.base,
        "/api/comments",
        &[("repo", "r"), ("state", "aged")],
    )
    .await;
    assert_eq!(aged["total"].as_u64(), Some(1), "{aged:#?}");
    let aged_row = rows(&aged)[0];
    assert_eq!(aged_row["keyword"], "TODO");
    assert_eq!(aged_row["state"]["on_date"], "2020-01-01");
    assert_eq!(aged_row["fields"]["to"], "owner@example.com");

    let (_, unreasoned) = get(
        &b.base,
        "/api/comments",
        &[("repo", "r"), ("state", "unreasoned")],
    )
    .await;
    assert_eq!(unreasoned["total"].as_u64(), Some(1), "{unreasoned:#?}");
    assert_eq!(rows(&unreasoned)[0]["directive"]["tool"], "rubocop");
    assert_eq!(rows(&unreasoned)[0]["line_start"], 1);

    // A `rubocop:enable` with a `--` reason is NOT unreasoned, and a magic
    // comment has nothing to justify at all.
    let (_, directives) = get(
        &b.base,
        "/api/comments",
        &[("repo", "r"), ("kind", "directive")],
    )
    .await;
    let magic = find_row(&directives, "app/models/order.rb", 1);
    assert_eq!(magic["directive"]["tool"], "ruby-magic");
    assert!(
        magic["directive"]["has_reason"].is_null(),
        "a magic comment is never unreasoned: {magic:#?}"
    );

    // --- an unknown vocabulary value is a 400 that NAMES the vocabulary --
    let (status, err) = get(&b.base, "/api/comments", &[("repo", "r"), ("kind", "nope")]).await;
    assert_eq!(status, 400);
    assert!(
        err["error"]
            .as_str()
            .unwrap_or("")
            .contains("commented_code"),
        "{err:#?}"
    );
    let (status, err) = get(
        &b.base,
        "/api/comments",
        &[("repo", "r"), ("state", "stale")],
    )
    .await;
    assert_eq!(status, 400);
    assert!(err["error"].as_str().unwrap_or("").contains("unreasoned"));

    // --- the per-file feed ----------------------------------------------
    let (status, file) = get(
        &b.base,
        "/api/comments/file",
        &[("repo", "r"), ("path", "app/models/widget.rb")],
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(file["total"].as_u64(), Some(4), "{file:#?}");
    assert!(file["blob_sha"].is_string());
    let kinds: Vec<&str> = rows(&file)
        .iter()
        .filter_map(|c| c["kind"].as_str())
        .collect();
    assert_eq!(
        kinds,
        vec!["directive", "annotation", "annotation", "directive"],
        "line order, and two adjacent annotations stay two rows"
    );

    // A path with no rows is an EMPTY-WITH-REASON read, not a 404.
    let (status, empty) = get(
        &b.base,
        "/api/comments/file",
        &[("repo", "r"), ("path", "app/models/nope.rb")],
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(empty["total"].as_u64(), Some(0));
    assert!(!empty["notes"].as_array().unwrap().is_empty(), "{empty:#?}");

    // --- the summary ----------------------------------------------------
    let (status, summary) = get(&b.base, "/api/comments/summary", &[("repo", "r")]).await;
    assert_eq!(status, 200);
    assert_eq!(summary["total"].as_u64(), Some(EXPECTED_TOTAL));
    assert_eq!(summary["by_kind"]["doc"].as_i64(), Some(2));
    assert_eq!(summary["by_kind"]["directive"].as_i64(), Some(4));
    assert_eq!(summary["by_kind"]["annotation"].as_i64(), Some(3));
    assert_eq!(summary["by_keyword"]["TODO"].as_i64(), Some(3));
    assert_eq!(summary["by_state"]["aged"].as_i64(), Some(1));
    assert_eq!(summary["by_state"]["unreasoned"].as_i64(), Some(1));
    // The blame-derived lanes are ABSENT with a reason, not a number over
    // a silently-tiny sample.
    let excluded: Vec<&str> = summary["state_basis"]["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(excluded, vec!["fresh", "drifted", "unknown"]);
    assert!(summary["by_state"]["drifted"].is_null());

    // --- the effective keyword set --------------------------------------
    let (status, kw) = get(&b.base, "/api/comments/keywords", &[]).await;
    assert_eq!(status, 200);
    assert_eq!(kw["source"], "default");
    let set: Vec<&str> = kw["keywords"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    for want in [
        "TODO", "FIXME", "OPTIMIZE", "HACK", "REVIEW", "NOTE", "XXX", "BUG",
    ] {
        assert!(set.contains(&want), "{want} missing from {set:?}");
    }

    // --- `GET /api/todos` is the same rows, filtered ---------------------
    let (status, todos) = get(&b.base, "/api/todos", &[("repo", "r")]).await;
    assert_eq!(status, 200);
    let items = todos["items"].as_array().unwrap();
    assert_eq!(todos["total"].as_u64(), Some(3), "{todos:#?}");
    assert_eq!(todos["truncated"], false);
    // Byte-compatible row shape: exactly path/line/marker/text.
    let first = &items[0];
    let keys: Vec<&String> = first.as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["line", "marker", "path", "text"], "{first:#?}");
    // Ordered by path then line.
    let paths: Vec<&str> = items.iter().filter_map(|i| i["path"].as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "app/models/widget.rb",
            "app/models/widget.rb",
            "config/settings.yml"
        ]
    );
    // Documented row-set change 2: the LEFTMOST keyword on a two-marker
    // line (the deleted `find_todo_marker` reported `FIXME` here).
    let two_marker = items.iter().find(|i| i["line"] == 3).unwrap();
    assert_eq!(two_marker["marker"], "TODO");
    assert_eq!(two_marker["text"], ": drop this FIXME shim");
    // The smart_todo parenthetical stays in `text` verbatim — `fields` is
    // a parsed view of it on `/api/comments`, never a rewrite here.
    let smart = items.iter().find(|i| i["line"] == 2).unwrap();
    assert_eq!(
        smart["text"],
        "(on: date('2020-01-01'), to: 'owner@example.com') drop the shim"
    );
    // Documented row-set change 1: YAML comments are now scanned.
    assert_eq!(items[2]["marker"], "TODO");

    // The family filter holds: an `OPTIMIZE`/`REVIEW`/`NOTE` hit would
    // never reach this route, and an unknown marker is simply empty.
    let (_, none) = get(
        &b.base,
        "/api/todos",
        &[("repo", "r"), ("marker", "OPTIMIZE")],
    )
    .await;
    assert_eq!(none["total"].as_u64(), Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_keyword_override_replaces_the_default_set_and_narrows_the_todo_view() {
    let repo = fixture_repo();
    let b = boot(
        repo.path(),
        CommentsSection {
            keywords: vec!["DEBT".to_string()],
        },
    )
    .await;
    // The three TODO lines stop being annotations: two of them sit
    // directly above `class Widget` so they fall through to `doc`, and the
    // YAML one (no definitions in that grammar) falls through to `prose`.
    // Eight blocks instead of nine — the two widget annotations merge into
    // one doc block, which is what "adjacent same-kind lines form one
    // block" means once they stop being annotations.
    wait_for_indexed(&b.base, 8).await;

    let (_, kw) = get(&b.base, "/api/comments/keywords", &[]).await;
    assert_eq!(kw["source"], "config");
    assert_eq!(kw["keywords"].as_array().unwrap().len(), 1);

    let (_, annotations) = get(
        &b.base,
        "/api/comments",
        &[("repo", "r"), ("kind", "annotation")],
    )
    .await;
    assert_eq!(annotations["total"].as_u64(), Some(0), "{annotations:#?}");

    // One scanner: narrowing the vocabulary narrows `GET /api/todos` too,
    // which is the honest consequence of the subsumption.
    let (_, todos) = get(&b.base, "/api/todos", &[("repo", "r")]).await;
    assert_eq!(todos["total"].as_u64(), Some(0), "{todos:#?}");
}
