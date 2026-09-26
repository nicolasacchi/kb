//! `kb-code store gc` — the exit-status contract, end to end.
//!
//! # The defect
//!
//! `POST /api/repos/R/store/gc` answers HTTP 200 for a REFUSED apply: the
//! daemon's `run_gc_pass` returns an ordinary `GcRunReport` whose `reason`
//! is `"restore-guard"` / `"restore-suspected"` / `"backup-failed"` and
//! whose `applied` is `false` (see `review_store::maint::GcRunReport` — the
//! refusal reasons are the documented variants of that same `reason`
//! field). The `StoreCmd::Gc` arm had no `process::exit` after the round
//! trip at all, so `kb-code store gc --repo R --yes --json` printed an
//! `ok: true`, `degraded: false` envelope with `applied: false` and
//! exited 0 — an agent gating on exit status recorded a refused
//! destructive apply as a successful one.
//!
//! # What each test pins
//!
//! The decision is inline in the arm (there is no pure helper — the only
//! `#[cfg(test)] mod tests` in `src/store_cmd.rs` covers `enc`/`failure`),
//! so it is pinned here by driving the real binary against a stub daemon
//! whose answer is the refused one. Quoting the fixed lines of
//! `src/store_cmd.rs` (post-`c247d52`):
//!
//! ```text
//! let refused = apply
//!     && !applied
//!     && !matches!(report["reason"].as_str(), Some("dry-run" | "nothing-to-do"));
//! ...
//! if refused { std::process::exit(envelope::EXIT_CONFLICT); }
//! if partial { std::process::exit(envelope::EXIT_PARTIAL); }
//! ```
//!
//! `exit_3_conflict_on_a_refused_apply` pins the `refused` predicate AND
//! the `EXIT_CONFLICT` exit; `a_refusal_is_announced_on_stderr` pins the
//! `else if refused` branch (stderr, no success-shaped stdout line);
//! `a_refusal_outranks_a_partial` pins the ORDER of the two exits;
//! `a_partial_apply_exits_7` pins the `EXIT_PARTIAL` exit;
//! `yes_with_dry_run_is_a_usage_error_without_a_round_trip` pins the
//! pre-existing `EXIT_USAGE` contradiction guard so it cannot regress;
//! `benign_no_apply_outcomes_exit_0` is the CONTROL for the other
//! direction — it passes before the fix too, and must keep passing, so
//! the two-entry allowlist (`dry-run`, `nothing-to-do`) and the applied
//! case can never be read as a refusal.

use super::stub::StubDaemon;
use assert_cmd::Command;
use serde_json::{json, Value};
use std::time::Duration;

// `envelope` is `mod envelope;` inside the `kb-code` BINARY
// (`src/main.rs:188`) and this crate has no `lib.rs`, so an integration
// test cannot name its constants. Values are copied from
// `crates/kb-code-cli/src/envelope.rs`:
//   `pub const EXIT_OK: i32 = 0;`        (line 49)
//   `pub const EXIT_USAGE: i32 = 2;`    (line 59)
//   `pub const EXIT_CONFLICT: i32 = 3;` (line 63)
//   `pub const EXIT_PARTIAL: i32 = 7;`  (line 76)
const EXIT_OK: i32 = 0;
const EXIT_USAGE: i32 = 2;
const EXIT_CONFLICT: i32 = 3;
const EXIT_PARTIAL: i32 = 7;

const REPO: &str = "widgets";

/// One `GcRunReport` as the daemon serializes it into `body["report"]`.
fn report(reason: &str, applied: bool, candidates: usize, partial: bool) -> Value {
    json!({
        "candidates": candidates,
        "applied": applied,
        "partial": partial,
        "reason": reason,
        "member_problems": [],
        "detail": "a delete candidate names review 512, above this volume's high-water review id 500",
    })
}

fn gc_answer(reason: &str, applied: bool, candidates: usize, partial: bool) -> String {
    json!({
        "schema": "kbc-store-gc/1",
        "repo": REPO,
        "report": report(reason, applied, candidates, partial),
    })
    .to_string()
}

fn stub(body: String) -> StubDaemon {
    StubDaemon::json(&body, 1, Duration::from_secs(20))
}

fn kb() -> Command {
    Command::cargo_bin("kb-code").expect("kb-code binary")
}

/// The D20 envelope `print_ok` writes to stdout.
fn envelope(stdout: &[u8]) -> Value {
    serde_json::from_slice(stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not the D20 envelope ({e}): {}",
            String::from_utf8_lossy(stdout)
        )
    })
}

#[test]
fn exit_3_conflict_on_a_refused_apply() {
    // Every refusal reason `maint::GcRunReport::reason` can carry with
    // `applied == false`: the operator never acknowledged the restore
    // guard, a delete candidate named a review above the volume's
    // high-water id, or the pre-apply bundle could not be written. All
    // three are HTTP 200 + `applied: false`, and all three must be a
    // non-zero exit for an agent gating on status.
    for reason in ["restore-guard", "restore-suspected", "backup-failed"] {
        let d = stub(gc_answer(reason, false, 3, false));
        let a = kb()
            .args([
                "store", "gc", "--repo", REPO, "--yes", "--json", "--daemon", &d.url,
            ])
            .assert()
            .code(EXIT_CONFLICT);
        let env = envelope(&a.get_output().stdout);
        assert_eq!(env["schema"], "kbc-store-gc/1", "reason {reason}");
        // `partial || refused` — the refused apply is reported degraded
        // even though the daemon's own `partial` is false.
        assert_eq!(env["degraded"], true, "reason {reason}");
        assert_eq!(env["data"]["report"]["applied"], false, "reason {reason}");
        assert_eq!(env["data"]["report"]["reason"], reason);
    }
}

#[test]
fn a_refusal_is_announced_on_stderr_not_stdout() {
    // The human branch's stdout line is success-shaped by construction
    // ("<repo>: gc <reason> — N candidate(s)"), so a refused apply must
    // take the `else if refused` branch and say REFUSED on stderr only.
    let d = stub(gc_answer("restore-suspected", false, 3, false));
    let a = kb()
        .args(["store", "gc", "--repo", REPO, "--yes", "--daemon", &d.url])
        .assert()
        .code(EXIT_CONFLICT);
    let out = a.get_output();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("gc REFUSED") && stderr.contains("restore-suspected"),
        "refusal must be announced on stderr, got: {stderr}"
    );
    // The success-shaped line ("… — 3 candidate(s)", and the detail line
    // that follows it) must NOT have gone to stdout.
    assert!(
        !stdout.contains("candidate(s)"),
        "a refused apply must print no success-shaped stdout line, got: {stdout}"
    );
    assert!(
        !stdout.contains("gc restore-suspected"),
        "a refused apply must print no success-shaped stdout line, got: {stdout}"
    );
}

#[test]
fn a_refusal_outranks_a_partial() {
    // `if refused { exit CONFLICT } if partial { exit PARTIAL }` — a
    // refused apply is not "partially done", it is not done, so the
    // partial pass (which the daemon can also set on the same report)
    // must not win the exit code.
    let d = stub(gc_answer("backup-failed", false, 3, true));
    kb().args([
        "store", "gc", "--repo", REPO, "--yes", "--json", "--daemon", &d.url,
    ])
    .assert()
    .code(EXIT_CONFLICT);
}

#[test]
fn a_partial_apply_exits_7() {
    // The other half of the pair: a report that WAS applied but could not
    // verify every member (`GcRunReport::partial`) is EXIT_PARTIAL, not 0.
    let d = stub(gc_answer("applied", true, 2, true));
    let a = kb()
        .args([
            "store", "gc", "--repo", REPO, "--yes", "--json", "--daemon", &d.url,
        ])
        .assert()
        .code(EXIT_PARTIAL);
    assert_eq!(envelope(&a.get_output().stdout)["degraded"], true);
}

#[test]
fn yes_with_dry_run_is_a_usage_error_without_a_round_trip() {
    // Pre-existing guard, pinned so it cannot regress: the contradiction
    // is the operator's to resolve, and it is resolved WITHOUT a daemon
    // round trip.
    //
    // The stub stays bound for far longer than the CLI process can take
    // to start (it links scip/protobuf/tree-sitter/gix), and `join()`
    // stops it only once the process has exited — so a stray request
    // cannot hide in a closed window: it would be accepted, counted, and
    // answered with the "dry-run" report, which exits 0, not EXIT_USAGE.
    let d = StubDaemon::json(
        &gc_answer("dry-run", false, 3, false),
        1,
        Duration::from_secs(120),
    );
    kb().args([
        "store",
        "gc",
        "--repo",
        REPO,
        "--yes",
        "--dry-run",
        "--json",
        "--daemon",
        &d.url,
    ])
    .assert()
    .code(EXIT_USAGE);
    assert_eq!(d.join(), 0, "a usage error must not reach the daemon");
}

#[test]
fn benign_no_apply_outcomes_exit_0() {
    // The allowlist is exactly two reasons plus the applied case. An
    // implementation that treated every `applied: false` as a refusal
    // would make `--dry-run` and a no-candidate `--yes` unusable; an
    // implementation that treated every `applied: false` as benign is the
    // bug these tests exist to prevent.
    let cases: [(&str, bool, bool, i32); 4] = [
        // reason,            applied, --yes, expected exit
        ("dry-run", false, false, EXIT_OK), // no flag: nothing requested
        ("nothing-to-do", false, true, EXIT_OK), // --yes, nothing to delete
        ("applied", true, true, EXIT_OK),   // --yes, the apply landed
        // The allowlist's `"dry-run"` entry is only reachable as an ANSWER
        // to `--yes` (without `--yes` the `apply && …` guard short-circuits
        // before the reason is read), so this is the one invocation that
        // pins that arm of the `matches!` — a daemon that declines to
        // apply for a reason it calls `dry-run` must not read as a refusal.
        ("dry-run", false, true, EXIT_OK),
    ];
    for (reason, applied, yes, want) in cases {
        let d = stub(gc_answer(reason, applied, 0, false));
        let mut args = vec!["store", "gc", "--repo", REPO, "--json", "--daemon"];
        args.push(&d.url);
        if yes {
            args.push("--yes");
        }
        let a = kb().args(&args).assert().code(want);
        let env = envelope(&a.get_output().stdout);
        assert_eq!(env["degraded"], false, "reason {reason} applied={applied}");
        assert_eq!(env["data"]["report"]["reason"], reason);
    }
}
