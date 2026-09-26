//! `kb-code store sync` — a base fetch skipped by a CREDENTIAL failure is
//! an UPSTREAM failure, end to end.
//!
//! # The defect
//!
//! `ReviewStores::sync_ready` cannot fetch the base with a credential it
//! does not have, so a `resolve_credential` error becomes
//! `BaseFetch::Skipped { code: e.class().slug() }` — the same shape a
//! genuinely-offline or base-less remote produces. The CLI read only
//!
//! ```text
//! let upstream = base_state == "failed";
//! ```
//!
//! and `member_errors` / `member_problems` were the only other source of
//! `degraded`. So a `gh` account that is not the pinned/recorded one — D12
//! calls that an ERROR, not a warning — produced `degraded: false` and
//! **exit 0**.
//!
//! That also made the two routes disagree about one condition: the CAPTURE
//! path already reports `credential-account-mismatch` as an envelope warning
//! AND in `base_status.code`. Same failure, visible on one route and silent
//! on the other.
//!
//! # What each test pins
//!
//! `a_credential_skip_exits_6` — `skipped` / `credential-account-mismatch`
//! is upstream: exit 6, `degraded: true`.
//! `an_offline_skip_still_exits_0` is the CONTROL that stops this being
//! "any skip is an error": `store sync --offline` is a supported,
//! successful operation, so `skipped` / `offline` must keep exiting 0 with
//! `degraded: false`. `a_base_less_remote_still_exits_0` is the same
//! control for a code that names no `FailureClass` at all
//! (`no-base-remote`), and `a_failed_base_still_exits_6` keeps the
//! pre-existing `failed` arm.
//!
//! The class set itself is `FailureClass::is_auth`, unit-tested in
//! `review_store::classify`; what is pinned HERE is that the CLI reads it.
//!
//! Filter example: `cargo test -p kb-code-cli --test store_cmd`

use super::stub::StubDaemon;
use assert_cmd::Command;
use serde_json::{json, Value};
use std::time::Duration;

// `envelope` is a private module of the `kb-code` BINARY (`mod envelope;`
// in `src/main.rs`) and this crate has no `lib.rs`, so its constants are
// not nameable from an integration test. Values copied from
// `crates/kb-code-cli/src/envelope.rs`:
//   `pub const EXIT_OK: i32 = 0;`       (line 49)
//   `pub const EXIT_UPSTREAM: i32 = 6;` (line 73)
const EXIT_OK: i32 = 0;
const EXIT_UPSTREAM: i32 = 6;

const REPO: &str = "widgets";

/// One `SyncReport` whose members all imported cleanly, so the ONLY thing
/// that can move the exit code is `base`.
fn sync_answer(base: Value) -> String {
    json!({
        "schema": "kbc-store-sync/1",
        "repo": REPO,
        "action": "synced",
        "report": {
            "members": [
                {"repo_id": 3, "heads": 2, "review_refs": 5, "conflicts": []},
            ],
            "member_errors": [],
            "member_problems": [],
            "base": base,
            "objects_missing": [],
        },
    })
    .to_string()
}

fn stub(body: String) -> StubDaemon {
    StubDaemon::json(&body, 1, Duration::from_secs(20))
}

fn sync(d: &StubDaemon) -> assert_cmd::assert::Assert {
    Command::cargo_bin("kb-code")
        .expect("kb-code binary")
        .args([
            "store",
            "sync",
            "--repo",
            REPO,
            "--daemon",
            d.url.as_str(),
            "--json",
        ])
        .assert()
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
fn a_credential_skip_exits_6() {
    // What the daemon writes when `resolve_credential` failed D12-style:
    // a `skipped` base whose code is the credential class. Before the fix
    // this exited 0 with `degraded: false` — the same wrong-account
    // answer the capture path reports as a visible failure.
    let d = stub(sync_answer(json!({
        "state": "skipped",
        "code": "credential-account-mismatch",
    })));
    let a = sync(&d).code(EXIT_UPSTREAM);
    let env = envelope(&a.get_output().stdout);
    assert_eq!(env["degraded"], true);
    // The daemon's own verdict is still reported verbatim — the CLI
    // classifies the skip, it does not rewrite the pass.
    assert_eq!(env["data"]["report"]["base"]["state"], "skipped");
    assert_eq!(
        env["data"]["report"]["base"]["code"],
        "credential-account-mismatch"
    );
}

/// The other half, and the one that matters for not over-correcting: a
/// base fetch that was skipped because there was nothing to fetch is a
/// SUCCESSFUL sync. `--offline` is a supported flag.
#[test]
fn an_offline_skip_still_exits_0() {
    let d = stub(sync_answer(json!({"state": "skipped", "code": "offline"})));
    let a = sync(&d).code(EXIT_OK);
    assert_eq!(envelope(&a.get_output().stdout)["degraded"], false);
}

/// A code that names no [`FailureClass`] at all — the repo has no base
/// remote — is a structural skip, not a failure. Guards the other
/// direction of the same over-correction.
#[test]
fn a_base_less_remote_still_exits_0() {
    let d = stub(sync_answer(json!({
        "state": "skipped",
        "code": "no-base-remote",
    })));
    let a = sync(&d).code(EXIT_OK);
    assert_eq!(envelope(&a.get_output().stdout)["degraded"], false);
}

/// CONTROL for the pre-existing arm: a base fetch that was ATTEMPTED and
/// failed was already upstream, and must stay so.
#[test]
fn a_failed_base_still_exits_6() {
    let d = stub(sync_answer(json!({
        "state": "failed",
        "code": "vanished",
        "detail": "refs/heads/main: not our ref",
    })));
    let a = sync(&d).code(EXIT_UPSTREAM);
    assert_eq!(envelope(&a.get_output().stdout)["degraded"], true);
}
