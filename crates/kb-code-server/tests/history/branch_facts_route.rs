//! V75-M3 (D15) — end-to-end HTTP tests for `branch-facts/1`, the conflict
//! radar, favourites and "compare with common base".
//!
//! Modelled on `stacks_route.rs`: a real daemon on a random port, a real
//! git fixture, pure git reads (no `wait_for_indexed` — none of these
//! routes touches the symbol index).
//!
//! The fixture is built ONCE per test (each gets its own tempdir) by
//! [`facts_fixture`], which is deliberately rich: a default branch, an
//! upstream-tracking branch, a two-layer stack, an agent-trailer branch, a
//! human branch carrying ONLY a `Co-authored-by:` trailer (the D18
//! never-label case), a squash-merged branch, an ancestry-merged branch, a
//! deliberately old branch, and a pair that conflicts.

use crate::common::git;
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

/// The address `[branches] agent_emails` defaults to — the `likely` rung.
const AGENT_EMAIL: &str = "noreply@anthropic.com";

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "human@example.com"]);
    git(dir, &["config", "user.name", "Human"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

/// Commit with a FIXED author/committer date, so the age distribution the
/// stale rule takes a percentile of is deterministic.
fn commit_at(dir: &Path, file: &str, contents: &str, msg: &str, date: &str) {
    std::fs::write(dir.join(file), contents).unwrap();
    git(dir, &["add", "-A"]);
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-q", "-m", msg])
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .expect("git commit runs");
    assert!(
        out.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit_as(dir: &Path, file: &str, contents: &str, msg: &str, date: &str, email: &str) {
    std::fs::write(dir.join(file), contents).unwrap();
    git(dir, &["add", "-A"]);
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-q", "-m", msg])
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_AUTHOR_NAME", "Agent")
        .output()
        .expect("git commit runs");
    assert!(
        out.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

const RECENT: &str = "2026-09-01T12:00:00 +0000";
const OLDER: &str = "2026-06-01T12:00:00 +0000";
const ANCIENT: &str = "2024-01-01T12:00:00 +0000";

/// The synthetic repo every test here reads. Everything is invented; no
/// path, name or address comes from any real checkout.
fn facts_fixture(dir: &Path) {
    init_repo(dir);
    commit_at(dir, "base.txt", "base\n", "base", RECENT);

    // A two-layer stack: stack/a on main, stack/b on stack/a.
    git(dir, &["checkout", "-q", "-b", "stack/a"]);
    commit_at(dir, "a.txt", "a\n", "layer a", RECENT);
    git(dir, &["checkout", "-q", "-b", "stack/b"]);
    commit_at(dir, "b.txt", "b\n", "layer b", RECENT);

    // An AGENT branch: the tip carries a `Kb-Session:` trailer (exact).
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["checkout", "-q", "-b", "agent/work"]);
    commit_at(
        dir,
        "agent.txt",
        "agent\n",
        "agent work\n\nKb-Session: sess-fixture-1\n",
        RECENT,
    );

    // A branch whose AUTHOR EMAIL is an agent address (likely).
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["checkout", "-q", "-b", "agent/email"]);
    commit_as(dir, "email.txt", "e\n", "by email", RECENT, AGENT_EMAIL);

    // The D18 NEVER case: a HUMAN-authored commit whose only trailer is
    // `Co-authored-by:` naming an agent.
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["checkout", "-q", "-b", "human/assisted"]);
    commit_at(
        dir,
        "human.txt",
        "h\n",
        &format!("human work\n\nCo-authored-by: Claude <{AGENT_EMAIL}>\n"),
        RECENT,
    );

    // A branch that conflicts with main on base.txt.
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["checkout", "-q", "-b", "conflicting"]);
    commit_at(dir, "base.txt", "theirs\n", "conflicting edit", RECENT);

    // A SQUASH-merged branch: its commit is cherry-picked onto main, so
    // ancestry says "1 ahead" and only patch-id sees the merge.
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["checkout", "-q", "-b", "squashed"]);
    commit_at(dir, "s.txt", "s\n", "squashed work", RECENT);
    let sha = rev_parse(dir, "squashed");
    git(dir, &["checkout", "-q", "main"]);
    // NOT `-q`: `git cherry-pick` has no quiet flag (unlike `commit`/
    // `checkout`), and passing one fails with a usage message.
    git(dir, &["cherry-pick", &sha]);

    // main's OWN edit to base.txt, which is what makes `conflicting`
    // actually conflict: a branch that changed a file the target never
    // touched merges cleanly, so the radar fixture needs BOTH sides to
    // have moved the same line since the fork.
    commit_at(dir, "base.txt", "ours\n", "main edits base.txt", RECENT);

    // An ANCESTRY-merged branch: it points at a commit main already has.
    git(dir, &["branch", "merged/ff", "main"]);

    // An ANCIENT branch, for the stale distribution.
    git(dir, &["checkout", "-q", "-b", "old/forgotten"]);
    commit_at(dir, "old.txt", "o\n", "long ago", ANCIENT);

    // A moderately old one, so the distribution has spread.
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["checkout", "-q", "-b", "older/thing"]);
    commit_at(dir, "older.txt", "x\n", "a while ago", OLDER);

    git(dir, &["checkout", "-q", "main"]);
}

fn rev_parse(dir: &Path, spec: &str) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", spec])
        .output()
        .expect("git rev-parse runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: Some("http://127.0.0.1:0".to_string()),
        token_file: None,
        public_url: None,
    }
}

async fn boot_with_repo(name: &str, path: &Path) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: disabled_kb_daemon(),
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"))
}

async fn facts(base: &str, query: &[(&str, &str)]) -> serde_json::Value {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/branches/facts"))
        .query(query)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "facts route must answer 200");
    resp.json().await.unwrap()
}

fn row<'a>(body: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    body["rows"]
        .as_array()
        .expect("rows is an array")
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no row named {name:?} in {body}"))
}

fn names(body: &serde_json::Value) -> Vec<String> {
    body["rows"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r["name"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_row_carries_a_classed_base_and_the_ladder_rides_the_wire() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    facts_fixture(&dir);
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;

    let body = facts(&base, &[("repo", "fixture"), ("limit", "100")]).await;
    assert_eq!(body["schema"], "branch-facts/1");
    assert_eq!(body["default"], "main");
    assert_eq!(
        body["rules"]["base_ladder"],
        serde_json::json!(["upstream", "fork-point", "merge-base", "unknown"]),
        "the ladder is on the wire so no client hardcodes it"
    );
    let rows = body["rows"].as_array().unwrap();
    assert!(rows.len() >= 8, "the fixture has many branches: {body}");
    for r in rows {
        let class = r["base"]["class"].as_str().unwrap();
        assert!(
            ["upstream", "fork-point", "merge-base", "unknown"].contains(&class),
            "{} has an unclassed base: {class}",
            r["name"]
        );
        // The base is never silently defaulted: a CLASSED base names a
        // ref, an unknown one names none AND reports no ahead/behind.
        if class == "unknown" {
            assert!(r["base"]["ref"].is_null());
            assert!(r["ahead"].is_null(), "an unknown base measures nothing");
        } else {
            assert!(r["base"]["ref"].is_string());
        }
    }
    // The default branch is its own base at zero, measured not guessed.
    let main = row(&body, "main");
    assert_eq!(main["ahead"], 0);
    assert_eq!(main["behind"], 0);
    assert_eq!(main["base"]["ref"], "main");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_provenance_is_trailer_exact_email_likely_and_never_a_humans_commit() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    facts_fixture(&dir);
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;

    let body = facts(&base, &[("repo", "fixture"), ("limit", "100")]).await;
    let trailer = row(&body, "agent/work");
    assert_eq!(trailer["agent"]["class"], "exact");
    assert_eq!(trailer["agent"]["via"], "kb-session-trailer");
    assert_eq!(trailer["agent"]["session_id"], "sess-fixture-1");

    let by_email = row(&body, "agent/email");
    assert_eq!(by_email["agent"]["class"], "likely");
    assert_eq!(by_email["agent"]["via"], "author-email");

    // D18's hard rule. `human/assisted` carries `Co-authored-by:` naming
    // the very address the `likely` rung matches on — and is still `none`,
    // because a co-author trailer is the shape a HUMAN commit takes.
    let human = row(&body, "human/assisted");
    assert_eq!(
        human["agent"]["class"], "none",
        "a Co-authored-by: trailer alone must never label a human's commit agent"
    );
    assert_eq!(human["agent"]["via"], "no-evidence");

    // And the rule is STATED, not just implemented.
    assert!(
        body["rules"]["agent"]["never"]
            .as_str()
            .unwrap()
            .contains("Co-authored-by"),
        "the never-rule must ride the response"
    );

    // `view=agent` membership follows the same ladder.
    let agents = facts(&base, &[("repo", "fixture"), ("view", "agent")]).await;
    let got = names(&agents);
    assert!(got.contains(&"agent/work".to_string()));
    assert!(got.contains(&"agent/email".to_string()));
    assert!(!got.contains(&"human/assisted".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_and_stale_partition_the_set_and_the_rule_is_stated() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    facts_fixture(&dir);
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;

    let all = facts(&base, &[("repo", "fixture"), ("limit", "100")]).await;
    let total = all["rows"].as_array().unwrap().len();
    let active = facts(
        &base,
        &[("repo", "fixture"), ("view", "active"), ("limit", "100")],
    )
    .await;
    let stale = facts(
        &base,
        &[("repo", "fixture"), ("view", "stale"), ("limit", "100")],
    )
    .await;
    assert_eq!(
        active["rows"].as_array().unwrap().len() + stale["rows"].as_array().unwrap().len(),
        total,
        "active and stale must PARTITION the enumeration"
    );

    let rule = &all["rules"]["stale"];
    assert!(rule["applied"].as_bool().unwrap(), "{rule}");
    assert!(
        rule["rule"].as_str().unwrap().contains("75th percentile"),
        "the rule text must say what it measured: {rule}"
    );
    assert!(rule["threshold_age_secs"].as_i64().unwrap() > 0);
    // The ANCIENT branch is the one the distribution should catch.
    assert!(
        names(&stale).contains(&"old/forgotten".to_string()),
        "the oldest fixture branch must be stale: {}",
        stale["rows"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_degrades_honestly_when_there_is_no_distribution() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    commit_at(&dir, "a.txt", "a\n", "base", ANCIENT);
    git(&dir, &["branch", "solo"]);
    let (_tmp, base) = boot_with_repo("tiny", &dir).await;

    let body = facts(&base, &[("repo", "tiny")]).await;
    let rule = &body["rules"]["stale"];
    assert!(!rule["applied"].as_bool().unwrap());
    assert!(rule["threshold_age_secs"].is_null());
    assert!(
        rule["degraded_reason"]
            .as_str()
            .unwrap()
            .contains("percentile"),
        "{rule}"
    );
    // ...and NOTHING is called stale on the strength of two branches.
    for r in body["rows"].as_array().unwrap() {
        assert_eq!(r["stale"], false, "{r}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_review_holds_a_branch_out_of_stale_however_old_it_is() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    facts_fixture(&dir);
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();

    // Baseline: the ancient branch IS stale.
    let before = facts(
        &base,
        &[("repo", "fixture"), ("view", "stale"), ("limit", "100")],
    )
    .await;
    assert!(names(&before).contains(&"old/forgotten".to_string()));

    // Open a review on it (loopback — the test daemon binds 127.0.0.1).
    let resp = client
        .post(format!("{base}/api/reviews"))
        .header("x-kbc-request", "1")
        .json(&serde_json::json!({
            "repo": "fixture",
            "head_ref": "old/forgotten",
            "base_ref": "main",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{:?}", resp.text().await);

    let after = facts(
        &base,
        &[("repo", "fixture"), ("view", "stale"), ("limit", "100")],
    )
    .await;
    assert!(
        !names(&after).contains(&"old/forgotten".to_string()),
        "a branch with an OPEN review is never stale: {}",
        after["rows"]
    );
    // And it is now in `review`, naming the review.
    let review_view = facts(&base, &[("repo", "fixture"), ("view", "review")]).await;
    let r = row(&review_view, "old/forgotten");
    assert_eq!(r["reviews"].as_array().unwrap().len(), 1);
    assert!(r["reviews"][0]["id"].as_i64().unwrap() > 0);
    assert!(r["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["code"] == "review-open"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merged_names_its_witness_and_patch_id_catches_the_squash() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    facts_fixture(&dir);
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;

    let merged = facts(
        &base,
        &[("repo", "fixture"), ("view", "merged"), ("limit", "100")],
    )
    .await;
    let got = names(&merged);

    // Ancestry: `merged/ff` points at a commit main already has.
    assert!(got.contains(&"merged/ff".to_string()), "{}", merged["rows"]);
    let ff = row(&merged, "merged/ff");
    assert_eq!(ff["merged"]["kind"], "ancestry");
    assert_eq!(ff["merged"]["into"], "main");

    // Patch-id: `squashed`'s commit was cherry-picked onto main, so
    // ancestry CANNOT see it — this is the GitHub weakness D15 names.
    assert!(
        got.contains(&"squashed".to_string()),
        "patch-id must catch the squash: {}",
        merged["rows"]
    );
    let sq = row(&merged, "squashed");
    assert_eq!(sq["merged"]["kind"], "patch-id");
    assert_eq!(sq["merged"]["equivalent"], 1);
    assert!(
        sq["ahead"].as_u64().unwrap() >= 1,
        "still ahead by ancestry"
    );

    // The budget is stated.
    let rule = &merged["rules"]["merged"];
    assert!(rule["patch_id_probed"].as_u64().unwrap() >= 1);
    assert!(rule["patch_id_cap"].as_u64().unwrap() > 0);

    // And WITHOUT the probe, the squash is honestly absent rather than
    // wrongly reported — ancestry alone cannot see it.
    let ancestry_only = facts(&base, &[("repo", "fixture"), ("limit", "100")]).await;
    assert!(row(&ancestry_only, "squashed")["merged"].is_null());
    assert_eq!(ancestry_only["rules"]["merged"]["patch_id_probed"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_kbcq_atoms_narrow_the_listing_and_a_bad_one_is_a_diagnostic() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    facts_fixture(&dir);
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;

    let by_branch = facts(&base, &[("repo", "fixture"), ("q", "branch:stack/")]).await;
    let got = names(&by_branch);
    assert!(got.contains(&"stack/a".to_string()) && got.contains(&"stack/b".to_string()));
    assert!(!got.contains(&"main".to_string()));

    let by_author = facts(&base, &[("repo", "fixture"), ("q", "by:noreply@")]).await;
    assert_eq!(names(&by_author), vec!["agent/email".to_string()]);

    let by_agent = facts(&base, &[("repo", "fixture"), ("q", "agent:exact")]).await;
    assert_eq!(names(&by_agent), vec!["agent/work".to_string()]);

    // `touches:` narrows AND captions its cap. Only the two stack layers'
    // diffs against their own base contain `a.txt`.
    let touching = facts(&base, &[("repo", "fixture"), ("q", "touches:a.txt")]).await;
    let mut got = names(&touching);
    got.sort();
    assert_eq!(
        got,
        vec!["stack/a".to_string(), "stack/b".to_string()],
        "{}",
        touching["rows"]
    );
    let t = &touching["rules"]["touches"];
    assert_eq!(t["path"], "a.txt");
    assert!(t["cap"].as_u64().unwrap() > 0);
    assert!(t["scanned"].as_u64().unwrap() > 0);

    // A closed-vocabulary typo is a DIAGNOSTIC and an ordinary word, never
    // a 400 and never a silent empty page.
    let typo = facts(&base, &[("repo", "fixture"), ("q", "agent:maybe")]).await;
    assert!(
        !typo["diagnostics"].as_array().unwrap().is_empty(),
        "{}",
        typo["diagnostics"]
    );
    assert_eq!(typo["normalized"], "agent:maybe");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prefix_folding_favourites_and_the_view_selector_agree_with_the_rows() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    facts_fixture(&dir);
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();

    let body = facts(&base, &[("repo", "fixture"), ("limit", "100")]).await;
    let prefixes = body["prefixes"].as_array().unwrap();
    let agent_prefix = prefixes
        .iter()
        .find(|p| p["prefix"] == "agent/")
        .unwrap_or_else(|| panic!("no agent/ prefix in {prefixes:?}"));
    assert_eq!(agent_prefix["count"], 2);

    // Selecting the prefix returns exactly that many rows.
    let folded = facts(&base, &[("repo", "fixture"), ("prefix", "agent/")]).await;
    assert_eq!(folded["total"], 2);

    // The view counts agree with what each view actually returns...
    for view in [
        "current", "mine", "agent", "review", "active", "stale", "all",
    ] {
        let page = facts(
            &base,
            &[("repo", "fixture"), ("view", view), ("limit", "100")],
        )
        .await;
        assert_eq!(
            body["view_counts"][view], page["total"],
            "view_counts[{view}] disagrees with the view's own total"
        );
    }
    // ...except `merged`, which is the ONE count that legitimately can
    // differ: this request ran no patch-id probe, so its `merged` count is
    // ancestry-only, while `?view=merged` probes and finds the squash. The
    // response SAYS this rather than leaving a caller to discover it.
    let merged = facts(
        &base,
        &[("repo", "fixture"), ("view", "merged"), ("limit", "100")],
    )
    .await;
    assert!(
        body["view_counts"]["merged"].as_u64().unwrap() < merged["total"].as_u64().unwrap(),
        "the fixture's squash must be invisible to an ancestry-only count"
    );
    assert!(
        body["rules"]["view_counts_note"]
            .as_str()
            .unwrap()
            .contains("ANCESTRY"),
        "the caveat must ride the response"
    );

    // Favourites: star, filter, unstar — idempotent in both directions.
    let star = |on: bool| {
        let client = client.clone();
        let base = base.clone();
        async move {
            client
                .post(format!("{base}/api/branches/favourites"))
                .header("x-kbc-request", "1")
                .json(&serde_json::json!({
                    "repo": "fixture", "ref": "refs/heads/stack/a", "on": on
                }))
                .send()
                .await
                .unwrap()
        }
    };
    let r: serde_json::Value = star(true).await.json().await.unwrap();
    assert_eq!(r["changed"], true);
    let r: serde_json::Value = star(true).await.json().await.unwrap();
    assert_eq!(r["changed"], false, "starring twice is an honest no-op");

    let fav = facts(&base, &[("repo", "fixture"), ("fav", "1")]).await;
    assert_eq!(names(&fav), vec!["stack/a".to_string()]);
    assert!(row(&fav, "stack/a")["favourite"].as_bool().unwrap());

    let listed: serde_json::Value = client
        .get(format!("{base}/api/branches/favourites"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        listed["favourites"],
        serde_json::json!(["refs/heads/stack/a"])
    );

    let r: serde_json::Value = star(false).await.json().await.unwrap();
    assert_eq!(r["changed"], true);
    let fav = facts(&base, &[("repo", "fixture"), ("fav", "1")]).await;
    assert!(fav["rows"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mistyped_view_is_a_400_naming_the_closed_set() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    facts_fixture(&dir);
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;

    let resp = reqwest::Client::new()
        .get(format!("{base}/api/branches/facts"))
        .query(&[("repo", "fixture"), ("view", "recent")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("current|mine"),
        "{body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_base_cache_keys_on_the_ref_tip_and_the_default_tip() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    facts_fixture(&dir);
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;

    let first = facts(&base, &[("repo", "fixture"), ("limit", "100")]).await;
    let misses_1 = first["rules"]["base_cache_misses"].as_u64().unwrap();
    assert!(misses_1 > 0, "a cold cache misses everything");

    let second = facts(&base, &[("repo", "fixture"), ("limit", "100")]).await;
    assert!(
        second["rules"]["base_cache_hits"].as_u64().unwrap() > 0,
        "the identical second request must hit"
    );
    assert_eq!(
        second["rules"]["base_cache_misses"].as_u64().unwrap(),
        misses_1,
        "nothing new should have missed"
    );

    // Move a ref: the key changes, so the row misses again — invalidation
    // by construction rather than by a pass someone could forget.
    git(&dir, &["checkout", "-q", "stack/a"]);
    commit_at(&dir, "a2.txt", "a2\n", "moved", RECENT);
    git(&dir, &["checkout", "-q", "main"]);
    let third = facts(&base, &[("repo", "fixture"), ("limit", "100")]).await;
    assert!(
        third["rules"]["base_cache_misses"].as_u64().unwrap() > misses_1,
        "a moved ref must miss"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_conflict_radar_reports_kinds_and_hunks_and_leaves_no_scratch_dir() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    facts_fixture(&dir);
    let (state_tmp, base) = boot_with_repo("fixture", &dir).await;

    let objects_before = loose_object_count(&dir);
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/branches/conflicts"))
        .query(&[("repo", "fixture"), ("against", "main"), ("limit", "20")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["schema"], "branch-conflicts/1");
    assert_eq!(body["against"], "main");

    let rows = body["rows"].as_array().unwrap();
    let conflicting = rows
        .iter()
        .find(|r| r["branch"] == "conflicting")
        .unwrap_or_else(|| panic!("no `conflicting` row in {body}"));
    assert_eq!(conflicting["clean"], false);
    let paths = conflicting["conflicts"].as_array().unwrap();
    let base_txt = paths
        .iter()
        .find(|c| c["path"] == "base.txt")
        .unwrap_or_else(|| panic!("base.txt not reported: {paths:?}"));
    assert_eq!(base_txt["kind"], "both-modified");
    assert_eq!(base_txt["stages"], serde_json::json!([1, 2, 3]));
    assert_eq!(
        base_txt["hunks"], 1,
        "a content conflict's hunks are COUNTED from the merged blob"
    );

    // A branch that only adds a file is clean against main.
    let clean = rows
        .iter()
        .find(|r| r["branch"] == "agent/work")
        .unwrap_or_else(|| panic!("no `agent/work` row in {body}"));
    assert_eq!(clean["clean"], true);

    // The budget is on the wire and in the caption.
    assert!(body["budget"]["computed"].as_u64().unwrap() >= 1);
    assert!(body["budget"]["candidates"].as_u64().unwrap() >= 1);
    assert!(body["caption"].as_str().unwrap().contains("computed"));

    // SEC-15 — the browsed repo's ODB is untouched...
    assert_eq!(
        loose_object_count(&dir),
        objects_before,
        "the radar must write no objects into the browsed repo"
    );
    // ...and every per-pair scratch dir is gone. The scratch ROOT is found
    // by WALKING the daemon's state tree rather than by assembling a path:
    // a hardcoded path that stopped matching would make this assertion
    // pass vacuously, which is the one failure mode a cleanup test must
    // not have.
    let scratch = find_scratch_root(state_tmp.path())
        .unwrap_or_else(|| panic!("no scratch root under {}", state_tmp.path().display()));
    let leftovers: Vec<std::path::PathBuf> = std::fs::read_dir(&scratch)
        .expect("the scratch root is readable")
        .flatten()
        .map(|e| e.path())
        .collect();
    assert!(
        leftovers.is_empty(),
        "the Drop guard must remove every scratch dir: {leftovers:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_radar_narrows_with_the_same_kbcq_atoms_the_listing_uses() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    facts_fixture(&dir);
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;

    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/branches/conflicts"))
        .query(&[
            ("repo", "fixture"),
            ("against", "main"),
            ("q", "branch:conflicting"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = body["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["branch"], "conflicting");
    assert_eq!(body["budget"]["candidates"], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compare_with_common_base_starts_a_review_against_the_classed_base() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    facts_fixture(&dir);
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/branches/review"))
        .header("x-kbc-request", "1")
        .json(&serde_json::json!({
            "repo": "fixture", "ref": "stack/b", "base": "auto"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "branch-review/1");
    assert_eq!(body["three_dot"], true);
    let class = body["base"]["class"].as_str().unwrap();
    assert!(
        ["fork-point", "merge-base", "upstream"].contains(&class),
        "the base must be CLASSED, got {class}"
    );
    // `stack/b` sits atop `stack/a`: reviewing it against `main` would
    // drown the reviewer in `stack/a`'s diff, so the stack's per-level
    // base wins and the response SAYS which decision made it.
    assert_eq!(body["base"]["ref"], "stack/a");
    assert_eq!(body["base_source"], "stack");
    assert!(body["review"]["id"].as_i64().unwrap() > 0);
    assert_eq!(body["review"]["head_ref"], "stack/b");
    assert_eq!(body["review"]["base_ref"], body["base"]["ref"]);

    // An explicit base is used verbatim and reported as UNCLASSED, because
    // this daemon did not detect it.
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/branches/review"))
        .header("x-kbc-request", "1")
        .json(&serde_json::json!({
            "repo": "fixture", "ref": "stack/a", "base": "main"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["base"]["class"], "unknown");
    assert_eq!(body["base_source"], "explicit");
    assert_eq!(body["base"]["ref"], "main");
}

/// The daemon's scratch-ODB root, found by walking rather than by
/// assembling a path — see the caller.
fn find_scratch_root(root: &Path) -> Option<std::path::PathBuf> {
    if root.file_name().map(|n| n == "scratch").unwrap_or(false) {
        return Some(root.to_path_buf());
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if let Some(found) = find_scratch_root(&entry.path()) {
                return Some(found);
            }
        }
    }
    None
}

/// `git count-objects -v`'s `count:` line — the loose-object total SEC-15
/// says a merge-tree lane must not move (the same probe
/// `merge_check`'s own tests use).
fn loose_object_count(dir: &Path) -> u64 {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["count-objects", "-v"])
        .output()
        .expect("git count-objects runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("count: "))
        .and_then(|n| n.trim().parse().ok())
        .expect("count-objects reports a count")
}
