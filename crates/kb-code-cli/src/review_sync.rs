//! RS-U10b — `kb-code review sync` and `kb-code review status` (README
//! §13), the two verbs that replace the morning skill's hand-written
//! provisioning (§10): "does a review exist? is the PR merged? has the head
//! moved? was it rebased?" is ONE daemon operation now.
//!
//! Both keep the RS-U10a contract (`crate::review_agent`'s module doc):
//! ONE versioned envelope on stdout under `--json`, diagnostics on stderr,
//! typed errors with `code`/`hint`/`next`, and the shipped exit-code table —
//! plus `7` (partial) when `sync --open` synced some PRs and failed others.
//!
//! `sync` runs as a daemon JOB (`POST /api/reviews/sync?async=1`, polled
//! through the same `GET /api/reviews/jobs/{id}` start-pr uses —
//! `crate::poll_review_job`), so a cold fetch is never cut by a client
//! timeout: `--wait[=SECS]` bounds only how long THIS process polls
//! (default 600 s for one PR, 3600 s for `--open`), and `--wait=0` hands
//! back the job id. Re-running the same sync attaches to a running job.

use crate::envelope::{self, NextArgv};
use crate::review_agent::{self, argv, AgentError, DEFAULT_DAEMON};
use clap::Args;
use serde_json::{json, Value};
use std::time::Duration;

pub use kb_code_server::review_sync::{STATUS_SCHEMA, SYNC_OPEN_SCHEMA, SYNC_SCHEMA};

/// `POST /api/reviews/sync` (loopback-only, JSON body — not a
/// `RouteContract`, like `POST /api/reviews/pr`).
pub const SYNC_PATH: &str = "/api/reviews/sync";
pub const JOB_SCHEMA: &str = "kbc-review-job/1";

/// Poll budget when `--wait` is omitted: one PR / the whole `--open` loop.
const DEFAULT_WAIT_ONE: u64 = 600;
const DEFAULT_WAIT_OPEN: u64 = 3600;
/// Per-request backstop (the daemon answers the POST at once with a job).
const CLIENT_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Args, Debug)]
pub struct SyncArgs {
    /// The configured repo (a `[[repos]]` name).
    #[arg(long)]
    pub repo: String,
    /// Sync ONE PR.
    #[arg(long = "pr", conflicts_with = "open", required_unless_present = "open")]
    pub pr: Option<u32>,
    /// Sync EVERY open PR of the repo (the morning loop).
    #[arg(long)]
    pub open: bool,
    /// With `--open`: also sync PRs merged since DATE — `YYYY-MM-DD`
    /// (meaning 00:00 UTC that day) or an RFC 3339 timestamp with its own
    /// offset (`2026-09-24T08:00:00+02:00`). (Checked by [`check_args`]: clap's `requires` is always
    /// satisfied by a `SetTrue` flag's implicit `false` default.)
    #[arg(long = "merged-since", value_name = "DATE")]
    pub merged_since: Option<String>,
    /// Title for a review this sync creates (default: the PR title).
    #[arg(long, conflicts_with = "open")]
    pub title: Option<String>,
    /// `--base` grammar for a review this sync CREATES (an existing
    /// review's base moves with `review retrack`).
    #[arg(long = "base", value_name = "SPEC", conflicts_with = "open")]
    pub base: Option<String>,
    /// Compute what a sync would do; fetch and write nothing.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    /// Reopen a CLOSED review whose PR is open again (after a successful
    /// capture). Without it sync never reopens a review: it reports
    /// `review-closed-pr-open` and captures nothing.
    #[arg(long)]
    pub reopen: bool,
    /// How long to poll the daemon job, in seconds (`--wait` alone = 600;
    /// omitted = 600 for one PR, 3600 for `--open`). `--wait=0` returns
    /// the job id at once.
    #[arg(
        long,
        value_name = "SECS",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "600"
    )]
    pub wait: Option<u64>,
    /// Run `gh auth token` and send it as the api credential
    /// (loopback-only, never persisted, never printed).
    #[arg(long = "gh-token-from-cli")]
    pub gh_token_from_cli: bool,
    #[arg(long, default_value = DEFAULT_DAEMON)]
    pub daemon: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// `<id>` or `pr:<N>`.
    pub target: String,
    /// Fetch the PR head (+ tracked base) into the review store first
    /// (loopback-only; the user clone is never written).
    #[arg(long)]
    pub fetch: bool,
    /// Disambiguates `pr:<N>` when several repos have one.
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long, default_value = DEFAULT_DAEMON)]
    pub daemon: String,
    #[arg(long)]
    pub json: bool,
}

// --- request builders ----------------------------------------------------------

/// Flag combinations clap cannot express. Pure.
pub fn check_args(a: &SyncArgs) -> Result<(), AgentError> {
    if a.merged_since.is_some() && !a.open {
        return Err(
            AgentError::usage("--merged-since applies only with --open").with_next(vec![argv(&[
                "kb-code", "review", "sync", "--repo", &a.repo, "--open", "--json",
            ])]),
        );
    }
    Ok(())
}

/// The sync body (never carries a token — that is added by the caller).
pub fn sync_payload(a: &SyncArgs) -> Value {
    let mut p = json!({ "repo": a.repo, "dry_run": a.dry_run, "reopen": a.reopen });
    match a.pr {
        Some(n) => p["pr_number"] = json!(n),
        None => p["open"] = json!(true),
    }
    if let Some(d) = &a.merged_since {
        p["merged_since"] = json!(d);
    }
    if let Some(t) = &a.title {
        p["title"] = json!(t);
    }
    if let Some(b) = &a.base {
        p["base_ref"] = json!(b);
    }
    p
}

/// `GET /api/reviews/{id}/status[?fetch=1]` (walked by main.rs's
/// dead-surface test).
pub fn review_status_request(fetch: bool) -> (&'static str, Vec<(&'static str, String)>) {
    let q = if fetch {
        vec![("fetch", "1".to_string())]
    } else {
        vec![]
    };
    (kb_code_server::review_sync::REVIEW_STATUS_ROUTE.path, q)
}

/// The argv that re-runs this sync (attaches to a running job).
pub fn rerun_argv(a: &SyncArgs) -> NextArgv {
    let mut v = argv(&["kb-code", "review", "sync", "--repo", &a.repo]);
    match a.pr {
        Some(n) => v.extend(["--pr".to_string(), n.to_string()]),
        None => v.push("--open".into()),
    }
    if let Some(d) = &a.merged_since {
        v.push(format!("--merged-since={d}"));
    }
    if a.dry_run {
        v.push("--dry-run".into());
    }
    if a.reopen {
        v.push("--reopen".into());
    }
    v.push("--json".into());
    v
}

// --- envelopes (pure) ------------------------------------------------------------

/// The daemon's `warnings[]` (`{code, message}`) as `"code: message"`.
pub fn warning_strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|ws| {
            ws.iter()
                .filter_map(|w| match (w["code"].as_str(), w["message"].as_str()) {
                    (Some(c), Some(m)) => Some(format!("{c}: {m}")),
                    (Some(c), None) => Some(c.to_string()),
                    _ => w.as_str().map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The natural follow-up for one sync answer.
pub fn sync_next(item: &Value) -> Vec<NextArgv> {
    let repo_pr = (item["repo"].as_str(), item["pr_number"].as_u64());
    let has_warning = |code: &str| {
        item["warnings"]
            .as_array()
            .is_some_and(|ws| ws.iter().any(|w| w["code"] == code))
    };
    // A closed review whose PR is open again: the same sync, with --reopen.
    if has_warning("review-closed-pr-open") {
        if let (Some(r), Some(n)) = repo_pr {
            return vec![argv(&[
                "kb-code",
                "review",
                "sync",
                "--repo",
                r,
                "--pr",
                &n.to_string(),
                "--reopen",
                "--json",
            ])];
        }
    }
    let Some(id) = item["review_id"].as_i64() else {
        // A dry run of a review that does not exist yet (a closed PR that
        // gets no review has no follow-up).
        if item["reason"] != "created" {
            return vec![];
        }
        return match repo_pr {
            (Some(r), Some(n)) => vec![argv(&[
                "kb-code",
                "review",
                "sync",
                "--repo",
                r,
                "--pr",
                &n.to_string(),
                "--json",
            ])],
            _ => vec![],
        };
    };
    let id = id.to_string();
    if item["dry_run"] == true {
        let reopen = has_warning("would-reopen");
        if let (Some(r), Some(n)) = repo_pr {
            if item["minted"] == true || reopen {
                let n = n.to_string();
                let mut v = argv(&["kb-code", "review", "sync", "--repo", r, "--pr", &n]);
                if reopen {
                    v.push("--reopen".into());
                }
                v.push("--json".into());
                return vec![v];
            }
        }
        return vec![argv(&["kb-code", "review", "status", &id, "--json"])];
    }
    match item["reason"].as_str() {
        Some("merged-final") | Some("unchanged") => {
            vec![argv(&["kb-code", "review", "verify", &id, "--json"])]
        }
        _ => vec![
            argv(&["kb-code", "review", "diff", &id, "--stat", "--json"]),
            argv(&["kb-code", "review", "verify", &id, "--json"]),
        ],
    }
}

/// `review sync --pr N --json`'s envelope.
pub fn sync_envelope(result: &Value) -> Value {
    envelope::ok_value(
        SYNC_SCHEMA,
        result,
        warning_strings(&result["warnings"]),
        false,
        None,
        sync_next(result),
    )
}

/// `review sync --open --json`'s envelope and whether it was PARTIAL (any
/// item failed → `degraded: true`, exit 7).
pub fn sync_open_envelope(result: &Value) -> (Value, bool) {
    let items = result["items"].as_array().cloned().unwrap_or_default();
    let repo = result["repo"].as_str().unwrap_or("<repo>");
    let mut warnings = Vec::new();
    let mut next: Vec<NextArgv> = Vec::new();
    let mut failed = 0usize;
    for it in &items {
        let n = it["pr_number"].as_u64().unwrap_or(0).to_string();
        if it["ok"] == false {
            failed += 1;
            warnings.push(format!(
                "pr #{n}: {}: {}",
                it["error"]["code"].as_str().unwrap_or("error"),
                it["error"]["message"].as_str().unwrap_or("sync failed")
            ));
            next.push(argv(&[
                "kb-code", "review", "sync", "--repo", repo, "--pr", &n, "--json",
            ]));
        } else if it["minted"] == true && it["dry_run"] != true {
            if let Some(id) = it["review_id"].as_i64() {
                next.push(argv(&[
                    "kb-code",
                    "review",
                    "diff",
                    &id.to_string(),
                    "--stat",
                    "--json",
                ]));
            }
        }
    }
    if result["truncated"] == true {
        warnings.push(
            "truncated: the forge listed more PRs than one sync reads (300 per list); the rest were NOT synced — re-running reads the same first 300, so sync the others with --pr".into(),
        );
    }
    let partial = failed > 0;
    (
        envelope::ok_value(
            SYNC_OPEN_SCHEMA,
            result,
            warnings,
            partial,
            items.is_empty().then_some("no-prs"),
            next,
        ),
        partial,
    )
}

/// The natural follow-up for a status answer: the head moved → sync; the
/// verdict is missing or stale → verify; else → the diff.
pub fn status_next(body: &Value) -> Vec<NextArgv> {
    let id = body["review_id"].as_i64().unwrap_or(0).to_string();
    let mut next = Vec::new();
    if body["head_moved"] == true {
        if let (Some(r), Some(n)) = (body["repo"].as_str(), body["pr_number"].as_u64()) {
            next.push(argv(&[
                "kb-code",
                "review",
                "sync",
                "--repo",
                r,
                "--pr",
                &n.to_string(),
                "--json",
            ]));
        }
    } else {
        next.push(argv(&[
            "kb-code", "review", "diff", &id, "--stat", "--json",
        ]));
    }
    if body["verdict_stale"] == true || body["verdict"]["state"].is_null() {
        next.push(argv(&["kb-code", "review", "verify", &id, "--json"]));
    }
    next
}

/// `review status --json`'s envelope (`data.next` repeats the top-level
/// `next`, as `verify` does).
pub fn status_envelope(body: &Value, addr: &str) -> Value {
    let next = status_next(body);
    let mut data = body.clone();
    data["ref"] = json!(addr);
    data["next"] = json!(next);
    envelope::ok_value(
        STATUS_SCHEMA,
        data,
        warning_strings(&body["warnings"]),
        false,
        None,
        next,
    )
}

/// A failed sync job as a typed error. The daemon's status rides
/// `error_status`; a forge/fetch failure is exit 6 (upstream).
pub fn job_error(terminal: &Value, rerun: NextArgv) -> AgentError {
    let status = terminal["error_status"]
        .as_u64()
        .and_then(|s| u16::try_from(s).ok())
        .unwrap_or(500);
    let body = json!({ "error": terminal["error"], "type": terminal["error_type"] });
    let mut e = AgentError::from_http(status, &body, "review sync");
    if is_upstream(status, &e.code, &e.message) {
        e.exit = envelope::EXIT_UPSTREAM;
    }
    if e.next.is_empty() {
        e.next = vec![rerun];
    }
    e
}

/// Is this refusal the forge's (network, auth, missing PR ref)?
pub fn is_upstream(status: u16, code: &str, message: &str) -> bool {
    status == 502
        || code.ends_with("forge-unavailable")
        || code.ends_with("pr-fetch-failed")
        || message.starts_with("PR fetch failed")
}

// --- transport ----------------------------------------------------------------

fn transport_error(e: &anyhow::Error, daemon: &str) -> AgentError {
    let exit = envelope::exit_code_for(e);
    AgentError::new("unreachable", format!("{e:#}"), exit)
        .with_hint(format!("is kb-code-server running at {daemon}?"))
        .with_next(vec![argv(&["kb-code", "identity", "--daemon", daemon])])
}

fn gh_token() -> Result<String, AgentError> {
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .map_err(|e| {
            AgentError::new(
                "gh-unavailable",
                format!("run `gh auth token`: {e}"),
                envelope::EXIT_GENERIC,
            )
        })?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || token.is_empty() {
        return Err(AgentError::new(
            "gh-unavailable",
            "`gh auth token` returned no token",
            envelope::EXIT_GENERIC,
        )
        .with_hint("log in with `gh auth login`, or drop --gh-token-from-cli"));
    }
    Ok(token)
}

// --- review sync ----------------------------------------------------------------

pub async fn sync_cmd(a: SyncArgs) -> anyhow::Result<()> {
    let json = a.json;
    match sync_run(&a).await {
        Ok(0) => Ok(()),
        Ok(code) => std::process::exit(code),
        Err(e) => e.emit(json),
    }
}

async fn sync_run(a: &SyncArgs) -> Result<i32, AgentError> {
    check_args(a)?;
    let mut payload = sync_payload(a);
    if a.gh_token_from_cli {
        payload["gh_token"] = json!(gh_token()?);
    }
    let client = crate::client_builder()
        .timeout(CLIENT_TIMEOUT)
        .build()
        .map_err(|e| AgentError::new("client", e.to_string(), envelope::EXIT_GENERIC))?;
    let rerun = rerun_argv(a);
    let (status, body) =
        crate::post_json_query_raw(&client, &a.daemon, SYNC_PATH, &[("async", "1")], &payload)
            .await
            .map_err(|e| transport_error(&e, &a.daemon))?;
    let result = if status == reqwest::StatusCode::ACCEPTED {
        let job_id = body["job_id"].as_str().map(str::to_string).ok_or_else(|| {
            AgentError::new(
                "daemon-error",
                format!("202 without a job_id: {body}"),
                envelope::EXIT_GENERIC,
            )
        })?;
        let default = if a.open {
            DEFAULT_WAIT_OPEN
        } else {
            DEFAULT_WAIT_ONE
        };
        let budget = Duration::from_secs(a.wait.unwrap_or(default));
        if budget.is_zero() {
            let v = envelope::ok_value(
                JOB_SCHEMA,
                json!({
                    "job_id": job_id,
                    "kind": "sync",
                    "status": "running",
                    "attached": body["attached"],
                    "poll": kb_code_server::review_jobs::REVIEW_JOB_ROUTE
                        .path
                        .replace("{id}", &job_id),
                }),
                vec![],
                false,
                None,
                vec![rerun],
            );
            if a.json {
                envelope::print_value(&v);
            } else {
                println!("sync: job {job_id} running (re-run with --wait to attach)");
            }
            return Ok(0);
        }
        let terminal = crate::poll_review_job(&client, &a.daemon, &job_id, budget, a.json, "sync")
            .await
            .map_err(|e| transport_error(&e, &a.daemon))?;
        let Some(t) = terminal else {
            return Err(AgentError::new(
                "job-running",
                format!(
                    "sync job {job_id} still running after {} s — the daemon is still working",
                    budget.as_secs()
                ),
                envelope::EXIT_CONFLICT,
            )
            .with_hint("re-run the same sync: it attaches to the running job")
            .with_next(vec![rerun]));
        };
        if t["status"] != "done" {
            return Err(job_error(&t, rerun));
        }
        t["result"].clone()
    } else if status.is_success() {
        body
    } else {
        let mut e = AgentError::from_http(status.as_u16(), &body, "review sync");
        if is_upstream(status.as_u16(), &e.code, &e.message) {
            e.exit = envelope::EXIT_UPSTREAM;
        }
        if e.next.is_empty() {
            e.next = vec![rerun];
        }
        return Err(e);
    };

    if a.open {
        let (env, partial) = sync_open_envelope(&result);
        if a.json {
            envelope::print_value(&env);
        } else {
            for it in result["items"].as_array().into_iter().flatten() {
                print_sync_line(it);
            }
            println!(
                "{} PR(s), {} failed",
                result["count"].as_u64().unwrap_or(0),
                result["failed"].as_u64().unwrap_or(0)
            );
        }
        return Ok(if partial { envelope::EXIT_PARTIAL } else { 0 });
    }
    review_agent::eprint_base_line(&result);
    if a.json {
        envelope::print_value(&sync_envelope(&result));
    } else {
        print_sync_line(&result);
        for w in warning_strings(&result["warnings"]) {
            eprintln!("warning: {w}");
        }
    }
    Ok(0)
}

fn print_sync_line(it: &Value) {
    let n = it["pr_number"].as_u64().unwrap_or(0);
    if it["ok"] == false {
        println!(
            "PR #{n}: FAILED ({}): {}",
            it["error"]["code"].as_str().unwrap_or("error"),
            it["error"]["message"].as_str().unwrap_or("")
        );
        return;
    }
    let files = match (
        it["files_count"].as_u64(),
        it["forge"]["changed_files"].as_u64(),
    ) {
        (Some(f), Some(g)) => format!("{f} files (GitHub {g})"),
        (Some(f), None) => format!("{f} files"),
        _ => "files ?".to_string(),
    };
    println!(
        "PR #{n}: review {} ps{} · {}{}{} · {files}",
        it["review_id"],
        it["ps"],
        it["reason"].as_str().unwrap_or("?"),
        if it["minted"] == true {
            " (minted)"
        } else {
            ""
        },
        if it["dry_run"] == true {
            " [dry run]"
        } else {
            ""
        },
    );
}

// --- review status ----------------------------------------------------------------

pub async fn status_cmd(a: StatusArgs) -> anyhow::Result<()> {
    let json = a.json;
    match status_run(&a).await {
        Ok(()) => Ok(()),
        Err(e) => e.emit(json),
    }
}

async fn status_run(a: &StatusArgs) -> Result<(), AgentError> {
    let res = review_agent::resolve(&a.daemon, &a.target, a.repo.as_deref(), None).await?;
    if res.ps.is_some() {
        return Err(AgentError::usage(
            "review status is about the whole review — address it as <id> or pr:<N>, not a patchset",
        ));
    }
    let (tpl, q) = review_status_request(a.fetch);
    let body = review_agent::get_ok(
        &a.daemon,
        &review_agent::fill_id(tpl, res.id),
        &q,
        "review status",
    )
    .await?;
    if a.json {
        envelope::print_value(&status_envelope(&body, &a.target));
        return Ok(());
    }
    let moved = match body["head_moved"].as_bool() {
        Some(true) => "HEAD MOVED",
        Some(false) => "head current",
        None => "head unknown",
    };
    println!(
        "review {} ps{} · {moved} · base {} ({}) · files {} (GitHub {}) · verdict {}{} · {} open finding(s)",
        body["review_id"],
        body["latest_ps"],
        body["base"]["branch"].as_str().unwrap_or("?"),
        body["base"]["state"].as_str().unwrap_or("?"),
        body["drift"]["files_count"],
        body["drift"]["forge_changed_files"],
        body["verdict"]["state"].as_str().unwrap_or("none"),
        if body["verdict_stale"] == true { " (stale)" } else { "" },
        body["open_findings"],
    );
    for w in warning_strings(&body["warnings"]) {
        eprintln!("warning: {w}");
    }
    for n in status_next(&body) {
        eprintln!("try: {}", n.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser, Debug)]
    struct SyncCli {
        #[command(flatten)]
        a: SyncArgs,
    }

    #[derive(Parser, Debug)]
    struct StatusCli {
        #[command(flatten)]
        a: StatusArgs,
    }

    fn parse(args: &[&str]) -> Result<SyncArgs, clap::Error> {
        let mut v = vec!["sync"];
        v.extend_from_slice(args);
        SyncCli::try_parse_from(v).map(|c| c.a)
    }

    // --- a small schema checker ---------------------------------------------

    #[derive(Clone, Copy)]
    enum K {
        Str,
        Int,
        Bool,
        Arr,
        Obj,
        StrOrNull,
        IntOrNull,
        BoolOrNull,
        ObjOrNull,
    }

    fn kind_ok(v: &Value, k: K) -> bool {
        match k {
            K::Str => v.is_string(),
            K::Int => v.is_i64() || v.is_u64(),
            K::Bool => v.is_boolean(),
            K::Arr => v.is_array(),
            K::Obj => v.is_object(),
            K::StrOrNull => v.is_string() || v.is_null(),
            K::IntOrNull => v.is_i64() || v.is_u64() || v.is_null(),
            K::BoolOrNull => v.is_boolean() || v.is_null(),
            K::ObjOrNull => v.is_object() || v.is_null(),
        }
    }

    fn assert_shape(v: &Value, spec: &[(&str, K)]) {
        for (ptr, k) in spec {
            let got = v
                .pointer(ptr)
                .unwrap_or_else(|| panic!("missing {ptr} in {v:#}"));
            assert!(kind_ok(got, *k), "{ptr} has the wrong type in {v:#}");
        }
    }

    const ENVELOPE: &[(&str, K)] = &[
        ("/schema", K::Str),
        ("/ok", K::Bool),
        ("/data", K::Obj),
        ("/warnings", K::Arr),
        ("/degraded", K::Bool),
        ("/empty_reason", K::StrOrNull),
        ("/next", K::Arr),
    ];

    /// `kbc-review-sync/1` — the documented data shape (README §13 row).
    const SYNC_DATA: &[(&str, K)] = &[
        ("/data/schema", K::Str),
        ("/data/repo", K::Str),
        ("/data/pr_number", K::Int),
        ("/data/review_id", K::IntOrNull),
        ("/data/review_state", K::StrOrNull),
        ("/data/created", K::Bool),
        ("/data/ps", K::IntOrNull),
        ("/data/minted", K::Bool),
        ("/data/reason", K::Str),
        ("/data/kind", K::StrOrNull),
        ("/data/dry_run", K::Bool),
        ("/data/base", K::ObjOrNull),
        ("/data/head_sha", K::StrOrNull),
        ("/data/files_count", K::IntOrNull),
        ("/data/files_equal", K::BoolOrNull),
        ("/data/forge", K::Obj),
        ("/data/forge/available", K::Bool),
        ("/data/forge/state", K::StrOrNull),
        ("/data/forge/changed_files", K::IntOrNull),
        ("/data/forge/title", K::StrOrNull),
        ("/data/forge/base_ref", K::StrOrNull),
        ("/data/verdict", K::ObjOrNull),
        ("/data/warnings", K::Arr),
    ];

    fn assert_next_is_argv(v: &Value) {
        for n in v["next"].as_array().unwrap() {
            let a = n.as_array().expect("each next is an argv array");
            assert!(!a.is_empty());
            assert!(a.iter().all(Value::is_string));
            assert_eq!(a[0], "kb-code");
        }
    }

    fn sync_body(reason: &str, minted: bool, dry_run: bool) -> Value {
        json!({
            "schema": SYNC_SCHEMA,
            "repo": "widgets",
            "pr_number": 7,
            "review_id": 12,
            "review_state": "open",
            "created": reason == "created",
            "ps": 3,
            "minted": minted,
            "reason": reason,
            "kind": "rebase",
            "dry_run": dry_run,
            "base": {
                "mode": "track", "branch": "main", "set_by": "auto",
                "source": "forge-api", "state": "ok",
                "merge_base": "a".repeat(40),
                "fetched_at": 1, "last_fetch": "fetched", "fetched_via": "local"
            },
            "head_sha": "b".repeat(40),
            "files_count": 4,
            "files_equal": true,
            "forge": {
                "available": true, "state": "open", "changed_files": 4,
                "title": "Add à widget", "base_ref": "main",
                "head_sha": "b".repeat(40), "merged_at": null, "unavailable": null
            },
            "verdict": { "state": null, "ps": null },
            "warnings": [{ "code": "retargeted", "message": "the PR now targets main" }],
        })
    }

    #[test]
    fn sync_flags_parse_and_refuse_bad_combinations() {
        let a = parse(&["--repo", "widgets", "--pr", "7", "--json"]).unwrap();
        assert_eq!((a.pr, a.open, a.wait), (Some(7), false, None));
        let a = parse(&["--repo", "w", "--open", "--merged-since", "2026-09-24"]).unwrap();
        assert!(a.open && a.pr.is_none());
        let a = parse(&["--repo", "w", "--pr", "7", "--wait"]).unwrap();
        assert_eq!(a.wait, Some(600));
        let a = parse(&["--repo", "w", "--pr", "7", "--wait=0"]).unwrap();
        assert_eq!(a.wait, Some(0));
        let bads: Vec<Vec<&str>> = vec![
            vec!["--repo", "w"],
            vec!["--repo", "w", "--pr", "7", "--open"],
            vec!["--repo", "w", "--open", "--base", "main"],
            vec!["--repo", "w", "--pr", "-3"],
        ];
        for bad in &bads {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        // `--merged-since` without `--open` parses, then refuses typed.
        let a = parse(&["--repo", "w", "--pr", "7", "--merged-since", "2026-09-24"]).unwrap();
        let e = check_args(&a).unwrap_err();
        assert_eq!(e.exit, envelope::EXIT_USAGE);
        assert!(!e.next.is_empty());
        let a = parse(&["--repo", "w", "--open", "--merged-since", "2026-09-24"]).unwrap();
        assert!(check_args(&a).is_ok());
        let s = StatusCli::try_parse_from(["status", "pr:7", "--fetch", "--json"]).unwrap();
        assert!(s.a.fetch && s.a.json);
        assert_eq!(s.a.target, "pr:7");
    }

    #[test]
    fn the_payload_and_rerun_carry_no_token_and_round_trip_the_target() {
        let a = parse(&[
            "--repo",
            "widgets",
            "--pr",
            "7",
            "--base",
            "track:main",
            "--title",
            "T",
            "--gh-token-from-cli",
        ])
        .unwrap();
        let p = sync_payload(&a);
        assert_eq!(p["pr_number"], 7);
        assert_eq!(p["base_ref"], "track:main");
        assert_eq!(p["title"], "T");
        assert!(p.get("gh_token").is_none());
        assert!(p.get("open").is_none());
        assert_eq!(
            rerun_argv(&a),
            argv(&["kb-code", "review", "sync", "--repo", "widgets", "--pr", "7", "--json"])
        );
        let o = parse(&["--repo", "w", "--open", "--merged-since", "2026-09-24"]).unwrap();
        let p = sync_payload(&o);
        assert_eq!(
            (p["open"].clone(), p["merged_since"].clone()),
            (json!(true), json!("2026-09-24"))
        );
        assert!(rerun_argv(&o).contains(&"--merged-since=2026-09-24".to_string()));
    }

    #[test]
    fn the_status_request_names_the_declared_route() {
        let (path, q) = review_status_request(false);
        assert_eq!(path, "/api/reviews/{id}/status");
        assert!(q.is_empty());
        let (_, q) = review_status_request(true);
        assert_eq!(q, vec![("fetch", "1".to_string())]);
    }

    #[test]
    fn sync_envelope_schema_and_next() {
        for (reason, minted) in [
            ("created", true),
            ("head-moved", true),
            ("base-moved", true),
            ("retargeted", true),
            ("unchanged", false),
            ("merged-final", false),
        ] {
            let env = sync_envelope(&sync_body(reason, minted, false));
            assert_shape(&env, ENVELOPE);
            assert_shape(&env, SYNC_DATA);
            assert_eq!(env["schema"], SYNC_SCHEMA);
            assert_eq!(env["data"]["reason"], reason);
            assert_next_is_argv(&env);
            let first = &env["next"][0];
            match reason {
                "unchanged" | "merged-final" => assert_eq!(first[2], "verify"),
                _ => assert_eq!(first[2], "diff"),
            }
            assert_eq!(
                env["warnings"][0], "retargeted: the PR now targets main",
                "daemon warnings pass through as code: message"
            );
        }
        // A dry run that WOULD mint suggests the real sync.
        let env = sync_envelope(&sync_body("head-moved", true, true));
        assert_eq!(env["next"][0][2], "sync");
        assert!(!env["next"][0]
            .as_array()
            .unwrap()
            .contains(&json!("--dry-run")));
        // A dry run of a review that does not exist yet.
        let mut none = sync_body("created", true, true);
        none["review_id"] = Value::Null;
        none["ps"] = Value::Null;
        let env = sync_envelope(&none);
        assert_shape(&env, SYNC_DATA);
        assert_eq!(env["next"][0][2], "sync");
    }

    #[test]
    fn closed_reviews_suggest_the_explicit_reopen() {
        let mut v = sync_body("unchanged", false, false);
        v["review_state"] = json!("closed");
        v["warnings"] = json!([{ "code": "review-closed-pr-open", "message": "…" }]);
        let env = sync_envelope(&v);
        assert_shape(&env, SYNC_DATA);
        assert_eq!(
            env["next"][0],
            json!([
                "kb-code", "review", "sync", "--repo", "widgets", "--pr", "7", "--reopen", "--json"
            ])
        );
        // A dry run that would reopen suggests the real sync WITH --reopen.
        let mut d = sync_body("unchanged", false, true);
        d["warnings"] = json!([{ "code": "would-reopen", "message": "…" }]);
        let env = sync_envelope(&d);
        assert_eq!(env["next"][0][2], "sync");
        assert!(env["next"][0]
            .as_array()
            .unwrap()
            .contains(&json!("--reopen")));
        // A closed-unmerged PR that gets no review has no follow-up.
        let mut c = sync_body("unchanged", false, false);
        c["review_id"] = Value::Null;
        c["ps"] = Value::Null;
        c["warnings"] = json!([{ "code": "pr-closed", "message": "…" }]);
        let env = sync_envelope(&c);
        assert_shape(&env, SYNC_DATA);
        assert!(env["next"].as_array().unwrap().is_empty());
        // --reopen rides the payload and the rerun argv.
        let a = parse(&["--repo", "widgets", "--pr", "7", "--reopen"]).unwrap();
        assert_eq!(sync_payload(&a)["reopen"], true);
        assert!(rerun_argv(&a).contains(&"--reopen".to_string()));
    }

    #[test]
    fn a_job_conflict_is_a_typed_conflict() {
        let body = json!({ "error": "a sync job … is already running with a different request (job_0123456789ab)", "type": "urn:kb:errors:job-conflict", "job_id": "job_0123456789ab" });
        let e = AgentError::from_http(409, &body, "review sync");
        assert_eq!(e.exit, envelope::EXIT_CONFLICT);
        assert_eq!(e.code, "urn:kb:errors:job-conflict");
    }

    #[test]
    fn sync_open_is_partial_when_any_item_failed() {
        let ok = {
            let mut v = sync_body("head-moved", true, false);
            v["ok"] = json!(true);
            v
        };
        let bad = json!({
            "ok": false, "pr_number": 9, "listed_as": "open",
            "error": { "code": "urn:kb:errors:bad-request", "message": "PR fetch failed: no ref", "status": 400 }
        });
        let result = json!({
            "schema": SYNC_OPEN_SCHEMA, "repo": "widgets", "merged_since": null,
            "dry_run": false, "count": 2, "failed": 1, "truncated": false,
            "items": [ok, bad],
        });
        let (env, partial) = sync_open_envelope(&result);
        assert!(partial);
        assert_shape(&env, ENVELOPE);
        assert_shape(
            &env,
            &[
                ("/data/schema", K::Str),
                ("/data/repo", K::Str),
                ("/data/count", K::Int),
                ("/data/failed", K::Int),
                ("/data/truncated", K::Bool),
                ("/data/items", K::Arr),
                ("/data/items/1/ok", K::Bool),
                ("/data/items/1/pr_number", K::Int),
                ("/data/items/1/error/code", K::Str),
                ("/data/items/1/error/message", K::Str),
                ("/data/items/1/error/status", K::Int),
            ],
        );
        assert_eq!(env["degraded"], true);
        assert!(env["warnings"][0]
            .as_str()
            .unwrap()
            .starts_with("pr #9: urn:kb:errors:bad-request"));
        assert_next_is_argv(&env);
        let nexts: Vec<Vec<String>> = serde_json::from_value(env["next"].clone()).unwrap();
        assert!(nexts.contains(&argv(&[
            "kb-code", "review", "sync", "--repo", "widgets", "--pr", "9", "--json"
        ])));
        assert!(nexts.iter().any(|n| n[2] == "diff"));

        let empty = json!({ "schema": SYNC_OPEN_SCHEMA, "repo": "w", "count": 0, "failed": 0, "truncated": false, "items": [] });
        let (env, partial) = sync_open_envelope(&empty);
        assert!(!partial);
        assert_eq!(env["empty_reason"], "no-prs");
        assert_eq!(env["degraded"], false);
    }

    fn status_body(head_moved: Value, verdict_ps: Value, latest: i64) -> Value {
        let verdict_state = if verdict_ps.is_null() {
            Value::Null
        } else {
            json!("approve")
        };
        json!({
            "schema": STATUS_SCHEMA,
            "review_id": 12, "repo": "widgets", "state": "open", "pr_number": 7,
            "head_moved": head_moved,
            "remote_head": "b".repeat(40), "remote_head_source": "forge-api",
            "fetched": false,
            "latest_ps": latest, "latest_tip": "a".repeat(40),
            "latest_merge_base": "c".repeat(40), "pr_head_sha": "a".repeat(40),
            "base": { "mode": "track", "branch": "main", "set_by": "auto", "source": "forge-api",
                      "state": "ok", "merge_base": "c".repeat(40), "fetched_at": null,
                      "last_fetch": null, "fetched_via": null },
            "drift": { "files_count": 4, "forge_changed_files": 4, "equal": true },
            "verdict": { "state": verdict_state, "ps": verdict_ps },
            "verdict_stale": verdict_ps.as_i64().is_some_and(|v| v != latest),
            "findings_total": 3, "open_findings": 1,
            "forge": { "available": true, "state": "open", "changed_files": 4, "title": "T",
                       "base_ref": "main", "head_sha": "b".repeat(40), "merged_at": null, "unavailable": null },
            "warnings": [],
        })
    }

    const STATUS_DATA: &[(&str, K)] = &[
        ("/data/schema", K::Str),
        ("/data/review_id", K::Int),
        ("/data/head_moved", K::BoolOrNull),
        ("/data/remote_head", K::StrOrNull),
        ("/data/remote_head_source", K::StrOrNull),
        ("/data/latest_ps", K::IntOrNull),
        ("/data/latest_tip", K::StrOrNull),
        ("/data/base", K::Obj),
        ("/data/base/state", K::StrOrNull),
        ("/data/drift/files_count", K::IntOrNull),
        ("/data/drift/forge_changed_files", K::IntOrNull),
        ("/data/verdict_stale", K::Bool),
        ("/data/open_findings", K::Int),
        ("/data/next", K::Arr),
    ];

    #[test]
    fn status_envelope_schema_and_next() {
        let env = status_envelope(&status_body(json!(true), json!(2), 3), "pr:7");
        assert_shape(&env, ENVELOPE);
        assert_shape(&env, STATUS_DATA);
        assert_next_is_argv(&env);
        assert_eq!(env["schema"], STATUS_SCHEMA);
        assert_eq!(env["data"]["ref"], "pr:7");
        assert_eq!(env["next"][0][2], "sync", "a moved head → sync first");
        assert_eq!(env["next"][1][2], "verify", "a stale verdict → verify");
        assert_eq!(env["data"]["next"], env["next"]);

        let env = status_envelope(&status_body(json!(false), json!(3), 3), "12");
        assert_eq!(env["next"][0][2], "diff");
        assert_eq!(env["next"].as_array().unwrap().len(), 1);

        let env = status_envelope(&status_body(Value::Null, Value::Null, 3), "12");
        assert_shape(&env, STATUS_DATA);
        assert_eq!(env["next"][0][2], "diff");
        assert_eq!(env["next"][1][2], "verify", "no verdict → verify");
    }

    #[test]
    fn job_failures_map_onto_the_exit_table() {
        let rerun = argv(&[
            "kb-code", "review", "sync", "--repo", "w", "--pr", "7", "--json",
        ]);
        let fetch = json!({ "status": "failed", "error": "PR fetch failed: couldn't find remote ref", "error_type": null, "error_status": 400 });
        let e = job_error(&fetch, rerun.clone());
        assert_eq!(e.exit, envelope::EXIT_UPSTREAM);
        assert_eq!(e.next, vec![rerun.clone()]);
        let v = e.to_value();
        assert!(v["error"]["code"]
            .as_str()
            .unwrap()
            .starts_with("urn:kb:errors:"));
        assert!(v["error"]["next"].as_array().is_some_and(|n| !n.is_empty()));

        let forge = json!({ "status": "failed", "error": "no GitHub origin", "error_type": "urn:kb:errors:forge-unavailable", "error_status": 502 });
        let e = job_error(&forge, rerun.clone());
        assert_eq!(e.exit, envelope::EXIT_UPSTREAM);
        assert_eq!(e.code, "urn:kb:errors:forge-unavailable");

        let closed = json!({ "status": "failed", "error": "review 3 is closed", "error_type": "urn:kb:errors:review-closed", "error_status": 409 });
        assert_eq!(
            job_error(&closed, rerun.clone()).exit,
            envelope::EXIT_CONFLICT
        );

        let missing = json!({ "status": "failed", "error": "unknown repo: nope", "error_type": null, "error_status": 404 });
        assert_eq!(
            job_error(&missing, rerun.clone()).exit,
            envelope::EXIT_NOT_FOUND
        );

        let bad = json!({ "status": "failed", "error": "base-unresolved", "error_type": "urn:kb:errors:base-unresolved", "error_status": 400 });
        assert_eq!(job_error(&bad, rerun).exit, envelope::EXIT_USAGE);
    }
}
