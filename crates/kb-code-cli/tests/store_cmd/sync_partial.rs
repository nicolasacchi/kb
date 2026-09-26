//! `kb-code store sync` — a skipped member is a PARTIAL sync, end to end.
//!
//! # The defect
//!
//! `ReviewStores::plan_for` returns `(plan, problems)`: `members_of`
//! silently DROPS a member whose clone fails `common_dir_of` (a worktree
//! deleted outside kb) or that has left `[[repos]]`, and records it in
//! `problems` instead. `sync_ready` then walks only `plan.members`, so a
//! `problems` member's refs are neither imported nor connectivity-verified
//! — yet the pass still returns `state: "ready"`. The CLI's partial
//! signal read `report["member_errors"]` alone, so
//! `kb-code store sync --repo R --json` reported `degraded: false` and
//! exited 0 for a store that had silently never seeded one of its
//! worktrees.
//!
//! `member_errors` and `member_problems` are SIBLINGS off the same
//! `members_of` call; both mean a member was skipped. The fix widens the
//! signal to both (and prints them in the human branch).
//!
//! # What each test pins
//!
//! ```text
//! let partial = ["member_errors", "member_problems"]
//!     .iter()
//!     .any(|k| report[*k].as_array().is_some_and(|a| !a.is_empty()));
//! ```
//!
//! `member_problems_alone_exits_7` pins the `"member_problems"` arm of
//! that `any` plus the `EXIT_PARTIAL` exit: before the fix the CLI read
//! `member_errors` only, so a problems-only report exited 0.
//! `member_problems_are_printed_in_the_human_branch` pins the
//! `for p in report["member_problems"]` line (absent before the fix, so
//! the operator never saw which worktree was skipped).
//! `member_errors_alone_still_exit_7` is the CONTROL for the sibling arm
//! (it passed before the fix too, and must keep passing — the fix
//! widens the signal, it does not move it). `a_clean_pass_exits_0` is the
//! control against a `partial` that is unconditionally true.

use super::stub::StubDaemon;
use assert_cmd::Command;
use serde_json::{json, Value};
use std::time::Duration;

// `envelope` is a private module of the `kb-code` BINARY (`mod envelope;`
// in `src/main.rs:188`) and this crate has no `lib.rs`, so its constants
// are not nameable from an integration test. Values copied from
// `crates/kb-code-cli/src/envelope.rs`:
//   `pub const EXIT_OK: i32 = 0;`       (line 49)
//   `pub const EXIT_PARTIAL: i32 = 7;` (line 76)
const EXIT_OK: i32 = 0;
const EXIT_PARTIAL: i32 = 7;

const REPO: &str = "widgets";

/// One `SyncReport` as `store_sync_route` serializes it into
/// `body["report"]` (`registry::SyncReport` + the `BaseFetch`
/// `#[serde(tag = "state")]` shape the CLI reads `report.base.state`).
fn sync_answer(member_errors: &[&str], member_problems: &[&str]) -> String {
    json!({
        "schema": "kbc-store-sync/1",
        "repo": REPO,
        "action": "synced",
        "report": {
            "members": [
                {"repo_id": 3, "heads": 2, "review_refs": 5, "conflicts": []},
            ],
            "member_errors": member_errors,
            "member_problems": member_problems,
            "base": {"state": "skipped", "code": "offline"},
            "objects_missing": [],
        },
    })
    .to_string()
}

fn stub(body: String) -> StubDaemon {
    StubDaemon::json(&body, 1, Duration::from_secs(20))
}

fn kb() -> Command {
    Command::cargo_bin("kb-code").expect("kb-code binary")
}

fn sync(d: &StubDaemon, as_json: bool) -> assert_cmd::assert::Assert {
    let mut args = vec!["store", "sync", "--repo", REPO, "--daemon", d.url.as_str()];
    if as_json {
        args.push("--json");
    }
    kb().args(&args).assert()
}

fn envelope(stdout: &[u8]) -> Value {
    serde_json::from_slice(stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not the D20 envelope ({e}): {}",
            String::from_utf8_lossy(stdout)
        )
    })
}

#[test]
fn member_problems_alone_exits_7() {
    // The bug's exact shape: `member_errors` EMPTY, `member_problems`
    // carrying the worktree whose `common_dir_of` failed. Before the fix
    // this exited 0 with `degraded: false`.
    let d = stub(sync_answer(&[], &["work-7: no common git dir"]));
    let a = sync(&d, true).code(EXIT_PARTIAL);
    let env = envelope(&a.get_output().stdout);
    assert_eq!(env["schema"], "kbc-store-sync/1");
    assert_eq!(env["degraded"], true);
    // The daemon's own verdict is unchanged and must be reported as-is:
    // the pass still said `ready`, and that is exactly what makes the
    // degraded flag load-bearing.
    assert_eq!(env["data"]["report"]["member_errors"].as_array().unwrap().len(), 0);
    assert_eq!(
        env["data"]["report"]["member_problems"][0],
        "work-7: no common git dir"
    );
}

#[test]
fn member_problems_are_printed_in_the_human_branch() {
    // The operator has to be able to SEE which worktree was skipped —
    // before the fix the human branch printed neither list.
    let d = stub(sync_answer(&[], &["work-7: no common git dir"]));
    let a = sync(&d, false).code(EXIT_PARTIAL);
    let stdout = String::from_utf8_lossy(&a.get_output().stdout).into_owned();
    assert!(
        stdout.contains("member problem: work-7: no common git dir"),
        "the skipped worktree must be named on stdout, got: {stdout}"
    );
    assert!(
        stdout.contains("synced"),
        "the pass itself is still reported, got: {stdout}"
    );
}

#[test]
fn member_errors_alone_still_exit_7() {
    // CONTROL for the sibling arm: an `import_member` failure was already
    // partial before the fix and must stay so — the fix widens the signal
    // to `member_problems`, it does not move it off `member_errors`.
    let d = stub(sync_answer(&["work-3: fetch timed out"], &[]));
    let a = sync(&d, true).code(EXIT_PARTIAL);
    assert_eq!(envelope(&a.get_output().stdout)["degraded"], true);
}

#[test]
fn a_clean_pass_exits_0() {
    // CONTROL: both lists empty is a clean sync — the `any()` must not be
    // "always partial", or every sync would exit 7 and an agent could no
    // longer tell a healthy store from a degraded one.
    let d = stub(sync_answer(&[], &[]));
    let a = sync(&d, true).code(EXIT_OK);
    let env = envelope(&a.get_output().stdout);
    assert_eq!(env["degraded"], false);
    assert_eq!(env["data"]["report"]["base"]["state"], "skipped");
}
