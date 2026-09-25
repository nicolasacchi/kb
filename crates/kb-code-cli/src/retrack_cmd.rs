//! RS-U7 — `kb-code review retrack` (README §10 step 4/§12, D17/D20):
//!
//! * `kb-code review retrack <ID|pr:N> [--base SPEC] [--dry-run] [--repo R]`
//!   — one review. Address resolution reuses [`crate::review_agent::
//!   resolve`] (the SAME `<id>`/`pr:<N>` grammar `find`/`diff`/`log`/`cat`
//!   already share), so `pr:15790` works with no `--repo` when exactly one
//!   repo has a review bound to it.
//! * `kb-code review retrack --all [--repo R] [--pinned|--legacy]
//!   --dry-run|--yes` — every review in scope. `--yes` is required to
//!   apply (bare `--all` with neither flag is a dry run — never a silent
//!   write). Applies ONLY `stale-pin` rows (README D17); `custom` rows are
//!   always left for a human, `equivalent` rows need nothing.
//!
//! Both print the daemon's `kbc-review-retrack{,-all}/1` body verbatim as
//! `--json`'s `data` (RS-U10a's `envelope::print_ok`), and a one-line
//! human summary otherwise. Errors go through [`crate::review_agent::
//! AgentError`] — the SAME typed `{code, message, hint, next}` shape and
//! exit-code table (2 usage · 3 conflict · 4 refused · 5 unreachable · 6
//! upstream · 8 not-found) every other RS-U10a verb uses, so a caller
//! scripting against `find`/`diff`/`retrack` never has to special-case one
//! of them.

use anyhow::Result;
use clap::Args;
use serde_json::Value;
use std::time::Duration;

use crate::envelope;
use crate::review_agent::{self, AgentError};

const READ_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Args, Debug)]
pub struct RetrackArgs {
    /// `<id>` or `pr:<N>` — required unless `--all`.
    id: Option<String>,
    /// Classify + retrack every eligible review in scope, instead of one.
    #[arg(long)]
    all: bool,
    /// Narrows address resolution (single form) or the scan (`--all`).
    #[arg(long)]
    repo: Option<String>,
    /// The `--base` grammar (README §6). Omitted, or `auto`, re-runs the
    /// resolution chain. Single form only.
    #[arg(long = "base")]
    base: Option<String>,
    /// Report the class without writing anything.
    #[arg(long = "dry-run")]
    dry_run: bool,
    /// `--all` only: apply to every `stale-pin` row. Required to write —
    /// there is no prompt.
    #[arg(long)]
    yes: bool,
    /// `--all` only: scan every review whose base is a `pin` (the
    /// default when neither `--pinned` nor `--legacy` is given).
    #[arg(long)]
    pinned: bool,
    /// `--all` only: narrow to rows the V0045 migration never touched
    /// (`base_set_by = legacy`) — a subset of `--pinned`.
    #[arg(long)]
    legacy: bool,
    #[arg(long, default_value = "http://127.0.0.1:4747")]
    daemon: String,
    #[arg(long)]
    json: bool,
}

async fn client() -> Result<reqwest::Client, AgentError> {
    crate::client_builder()
        .timeout(READ_TIMEOUT)
        .build()
        .map_err(|e| AgentError::new("client", e.to_string(), envelope::EXIT_GENERIC))
}

async fn post(daemon: &str, path: &str, body: &Value) -> Result<Value, AgentError> {
    let c = client().await?;
    let url = format!("{}{path}", daemon.trim_end_matches('/'));
    let resp = c.post(&url).json(body).send().await.map_err(|e| {
        let exit = if e.is_connect() || e.is_timeout() || e.status().is_none() {
            envelope::EXIT_UNREACHABLE
        } else {
            envelope::EXIT_GENERIC
        };
        AgentError::new("unreachable", format!("POST {url}: {e}"), exit)
            .with_hint(format!("is kb-code-server running at {daemon}?"))
    })?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let body = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(text.trim())
            .unwrap_or_else(|_| serde_json::json!({ "error": text.trim() }))
    };
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(AgentError::from_http(status, &body, "review retrack"))
    }
}

pub async fn run(a: RetrackArgs) -> Result<()> {
    let result = if a.all {
        run_all(&a).await
    } else {
        run_one(&a).await
    };
    match result {
        Ok(()) => Ok(()),
        Err(e) => e.emit(a.json),
    }
}

async fn run_one(a: &RetrackArgs) -> Result<(), AgentError> {
    let Some(raw) = a.id.as_deref() else {
        return Err(AgentError::usage(
            "review retrack requires an address (<id> or pr:<N>), or --all",
        )
        .with_next(vec![vec![
            "kb-code".into(),
            "review".into(),
            "retrack".into(),
            "--all".into(),
            "--pinned".into(),
            "--dry-run".into(),
        ]]));
    };
    let resolved = review_agent::resolve(&a.daemon, raw, a.repo.as_deref(), None).await?;
    let body = post(
        &a.daemon,
        &format!("/api/reviews/{}/retrack", resolved.id),
        &serde_json::json!({ "base": a.base, "dry_run": a.dry_run }),
    )
    .await?;
    if a.json {
        envelope::print_ok("kbc-review-retrack/1", &body, vec![], false, None);
        return Ok(());
    }
    print_row(&body, a.dry_run);
    Ok(())
}

async fn run_all(a: &RetrackArgs) -> Result<(), AgentError> {
    if a.id.is_some() {
        return Err(AgentError::usage(
            "review retrack --all takes no address — pass --repo to narrow the scan",
        ));
    }
    if a.dry_run && a.yes {
        return Err(AgentError::usage("pass --dry-run or --yes, not both"));
    }
    let dry_run = !a.yes;
    let body = post(
        &a.daemon,
        "/api/reviews/retrack-bulk",
        &serde_json::json!({
            "repo": a.repo,
            "pinned": a.pinned,
            "legacy": a.legacy,
            "dry_run": dry_run,
        }),
    )
    .await?;
    if a.json {
        let degraded = body["degraded"].as_bool().unwrap_or(false);
        envelope::print_ok("kbc-review-retrack-all/1", &body, vec![], degraded, None);
        if degraded {
            std::process::exit(envelope::EXIT_PARTIAL);
        }
        return Ok(());
    }
    let summary = &body["summary"];
    println!(
        "scanned={}  stale_pin={}  applied={}{}",
        summary["scanned"],
        summary["stale_pin"],
        summary["applied"],
        if dry_run { "  (dry run)" } else { "" },
    );
    for r in body["rows"].as_array().into_iter().flatten() {
        if let Some(err) = r["row_error"].as_str() {
            println!("review {:<5} ERROR {err}", r["id"]);
            continue;
        }
        println!(
            "review {:<5} class={:<10} minted={} base={}",
            r["id"],
            r["class"].as_str().unwrap_or("?"),
            r["minted"],
            r["base"]["merge_base"].as_str().unwrap_or("-"),
        );
    }
    for e in body["repo_errors"].as_array().into_iter().flatten() {
        println!(
            "repo {} ERROR {}",
            e["repo"].as_str().unwrap_or("?"),
            e["error"].as_str().unwrap_or("?"),
        );
    }
    let degraded = body["degraded"].as_bool().unwrap_or(false);
    if degraded {
        std::process::exit(envelope::EXIT_PARTIAL);
    }
    Ok(())
}

fn print_row(body: &Value, dry_run: bool) {
    println!(
        "review {} class={} minted={}{}",
        body["id"],
        body["class"].as_str().unwrap_or("?"),
        body["minted"],
        if dry_run { "  (dry run)" } else { "" },
    );
    let base = &body["base"];
    println!(
        "  base: {} {} · merge-base {}",
        base["mode"].as_str().unwrap_or("-"),
        base["branch"].as_str().unwrap_or(""),
        base["merge_base"]
            .as_str()
            .map(|s| &s[..s.len().min(12)])
            .unwrap_or("-"),
    );
    if let Some(kind) = body["kind"].as_str() {
        println!("  kind: {kind}  ps{}", body["ps_number"]);
    }
    if body["verdict_scope_changed"].as_bool().unwrap_or(false) {
        println!("  verdict_scope_changed: true (findings/verdict stayed on the old patchset)");
    }
    for w in body["warnings"].as_array().into_iter().flatten() {
        println!(
            "  warning ({}): {}",
            w["code"].as_str().unwrap_or("?"),
            w["message"].as_str().unwrap_or("")
        );
    }
}
