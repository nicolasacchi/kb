//! v0.44 F9 — `kb-code review since <REF> [--from verdict|psN] [--to psN]
//! [--json]`: what the AUTHOR changed between two patchsets, with base
//! movement taken out (`GET /api/reviews/{id}/since`, computed by
//! `kb_code_server::review_since`; this verb adds no interpretation).
//!
//! Keeps the RS-U10a contract (`crate::review_agent`'s module doc): one
//! versioned envelope on stdout under `--json`, diagnostics on stderr,
//! typed errors with `code`/`hint`/`next`.

use crate::envelope::{self, NextArgv};
use crate::review_agent::{self, argv, AgentError, DEFAULT_DAEMON};
use clap::Args;
use serde_json::Value;

pub use kb_code_server::review_since::SINCE_SCHEMA;

#[derive(Args, Debug)]
pub struct SinceArgs {
    /// `<id>` or `pr:<N>`.
    pub target: String,
    /// Where to start: `verdict` (the patchset the verdict sits on, the
    /// default) or `psN`.
    #[arg(long, default_value = "verdict", value_name = "verdict|psN")]
    pub from: String,
    /// Where to end: `psN` or `latest` (the default).
    #[arg(long, default_value = "latest", value_name = "psN|latest")]
    pub to: String,
    /// Disambiguates `pr:<N>` when several repos have one.
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long, default_value = DEFAULT_DAEMON)]
    pub daemon: String,
    #[arg(long)]
    pub json: bool,
}

/// `GET /api/reviews/{id}/since?from=&to=` (walked by main.rs's
/// dead-surface test).
pub fn review_since_request(from: &str, to: &str) -> (&'static str, Vec<(&'static str, String)>) {
    (
        kb_code_server::review_since::SINCE_ROUTE.path,
        vec![("from", from.to_string()), ("to", to.to_string())],
    )
}

/// The natural follow-ups for one answer. Pure.
pub fn since_next(body: &Value) -> Vec<NextArgv> {
    let id = body["review_id"].as_i64().unwrap_or(0).to_string();
    let mut next = Vec::new();
    if body["rebase_only"] == false {
        let from = body["from"]["ps"].as_u64().unwrap_or(0).to_string();
        let to = body["to"]["ps"].as_u64().unwrap_or(0).to_string();
        next.push(argv(&[
            "kb-code",
            "review",
            "interdiff",
            &id,
            "--from",
            &from,
            "--to",
            &to,
            "--json",
        ]));
    }
    next
}

/// `review since --json`'s envelope. Pure.
pub fn since_envelope(body: &Value, addr: &str) -> Value {
    let next = since_next(body);
    let mut data = body.clone();
    data["ref"] = Value::String(addr.to_string());
    envelope::ok_value(SINCE_SCHEMA, data, Vec::new(), false, None, next)
}

/// The human one-liner plus per-path counts. Pure.
pub fn since_lines(body: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let verdict = if body["from_source"] == "verdict" {
        " (your verdict)"
    } else {
        ""
    };
    let kind = if body["rebase_only"] == true {
        if body["bases"]["moved"] == true {
            "rebase-only, 0 author changes"
        } else {
            "unchanged, 0 author changes"
        }
    } else {
        "AUTHOR CHANGED"
    };
    out.push(format!(
        "review {} ps{}{verdict} -> ps{} · {kind} · {} new / {} gone hunk(s) in {} path(s)",
        body["review_id"],
        body["from"]["ps"],
        body["to"]["ps"],
        body["author_delta"]["new_hunks"],
        body["author_delta"]["gone_hunks"],
        body["author_delta"]["paths_changed"],
    ));
    for p in body["paths"].as_array().into_iter().flatten() {
        if p["new"] == 0 && p["gone"] == 0 {
            continue;
        }
        out.push(format!(
            "  {}  carried {} · new {} · gone {}",
            p["path"].as_str().unwrap_or("?"),
            p["carried"],
            p["new"],
            p["gone"],
        ));
    }
    out
}

pub async fn since_cmd(a: SinceArgs) -> anyhow::Result<()> {
    let json = a.json;
    match since_run(&a).await {
        Ok(()) => Ok(()),
        Err(e) => e.emit(json),
    }
}

async fn since_run(a: &SinceArgs) -> Result<(), AgentError> {
    review_agent::reject_patchset_address(&a.target, "since")?;
    let res = review_agent::resolve(&a.daemon, &a.target, a.repo.as_deref(), None).await?;
    if res.ps.is_some() {
        return Err(AgentError::usage(
            "review since compares two patchsets — address the review as <id> or pr:<N> and \
             pass --from/--to",
        ));
    }
    let (tpl, q) = review_since_request(&a.from, &a.to);
    let body = review_agent::get_ok(
        &a.daemon,
        &review_agent::fill_id(tpl, res.id),
        &q,
        "review since",
    )
    .await?;
    if a.json {
        envelope::print_value(&since_envelope(&body, &a.target));
        return Ok(());
    }
    for l in since_lines(&body) {
        println!("{l}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_json::json;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        a: SinceArgs,
    }

    fn body(rebase_only: bool, moved: bool) -> Value {
        json!({
            "schema": SINCE_SCHEMA,
            "review_id": 12,
            "from": {"ps": 2}, "to": {"ps": 4},
            "from_source": "verdict",
            "paths": [
                {"path": "a.txt", "carried": 1, "new": if rebase_only {0} else {1}, "gone": 0},
                {"path": "b.txt", "carried": 3, "new": 0, "gone": 0}
            ],
            "author_delta": {"new_hunks": if rebase_only {0} else {1}, "gone_hunks": 0,
                              "paths_changed": if rebase_only {0} else {1}},
            "rebase_only": rebase_only,
            "bases": {"from": "x", "to": "y", "moved": moved},
        })
    }

    #[test]
    fn defaults_are_verdict_to_latest() {
        let a = Cli::try_parse_from(["since", "12"]).unwrap().a;
        assert_eq!((a.from.as_str(), a.to.as_str()), ("verdict", "latest"));
        let a = Cli::try_parse_from(["since", "pr:7", "--from", "ps2", "--to", "ps4", "--json"])
            .unwrap()
            .a;
        assert_eq!(
            (a.from.as_str(), a.to.as_str(), a.json),
            ("ps2", "ps4", true)
        );
    }

    #[test]
    fn the_request_names_the_declared_route() {
        let (path, q) = review_since_request("verdict", "latest");
        assert_eq!(path, "/api/reviews/{id}/since");
        assert_eq!(
            q,
            vec![
                ("from", "verdict".to_string()),
                ("to", "latest".to_string())
            ]
        );
    }

    #[test]
    fn a_rebase_only_answer_says_so_and_offers_no_follow_up() {
        let b = body(true, true);
        let lines = since_lines(&b);
        assert!(
            lines[0].contains("rebase-only, 0 author changes"),
            "{lines:?}"
        );
        assert!(lines[0].contains("(your verdict)"));
        assert_eq!(lines.len(), 1, "carried-only paths are not listed");
        assert!(since_next(&b).is_empty());
        let env = since_envelope(&b, "12");
        assert_eq!(env["schema"], SINCE_SCHEMA);
        assert_eq!(env["data"]["ref"], "12");
    }

    #[test]
    fn an_author_change_lists_the_path_and_points_at_the_interdiff() {
        let b = body(false, true);
        let lines = since_lines(&b);
        assert!(lines[0].contains("AUTHOR CHANGED"));
        assert!(lines[1].contains("a.txt") && lines[1].contains("new 1"));
        assert_eq!(
            since_next(&b),
            vec![argv(&[
                "kb-code",
                "review",
                "interdiff",
                "12",
                "--from",
                "2",
                "--to",
                "4",
                "--json"
            ])]
        );
    }
}
