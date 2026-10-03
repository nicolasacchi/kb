//! v0.44 F9 — `kb-code review queue [--repo R] [--state open|closed|all]
//! [--json]`: what is waiting FOR THE AGENT (`GET /api/reviews/agent-queue`,
//! computed by `kb_code_server::review_queue`; this verb adds no
//! interpretation). Lane 1 is an open `question` thread whose latest voice is
//! not an agent, lane 2 a disputed finding with no agent reply since the
//! dispute, lane 3 an agreed/fix-later finding with no suggestion or author
//! hunk yet, lane 4 a PR whose head moved past the latest patchset, lane 5 an
//! open `flag-for-agent` thread. Each row carries the argv to run next.
//! `--watch` polls and prints only rows that appear (seed-then-diff). A work queue, not a score
//! and not a dispatcher: the daemon launches nothing.

use crate::envelope::{self, NextArgv};
use crate::review_agent::{self, AgentError, DEFAULT_DAEMON};
use clap::Args;
use serde_json::Value;
use std::collections::HashSet;
use std::time::Duration;

pub use kb_code_server::review_queue::QUEUE_SCHEMA;

#[derive(Args, Debug)]
pub struct QueueArgs {
    /// One configured repo (default: every repo).
    #[arg(long)]
    pub repo: Option<String>,
    /// `open` (default), `closed` or `all`.
    #[arg(long, default_value = "open", value_name = "open|closed|all")]
    pub state: String,
    #[arg(long, default_value = DEFAULT_DAEMON)]
    pub daemon: String,
    #[arg(long)]
    pub json: bool,
    /// Keep polling and print only rows that newly appear (the first poll is
    /// printed in full as the seed). One NDJSON object per new row with
    /// `--json`. A row that clears and later returns is new again.
    #[arg(long)]
    pub watch: bool,
    /// Seconds between polls with `--watch`.
    #[arg(long, default_value_t = 30, value_name = "SECS", requires = "watch")]
    pub interval: u64,
}

/// `GET /api/reviews/agent-queue` (walked by main.rs's dead-surface test).
pub fn agent_queue_request(
    repo: Option<&str>,
    state: &str,
) -> (&'static str, Vec<(&'static str, String)>) {
    let mut q = vec![("state", state.to_string())];
    if let Some(r) = repo {
        q.push(("repo", r.to_string()));
    }
    (kb_code_server::review_queue::AGENT_QUEUE_ROUTE.path, q)
}

/// The top row's own `next` (the queue is already in work order). Pure.
pub fn queue_next(body: &Value) -> Vec<NextArgv> {
    body["rows"]
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["next"].as_array())
        .map(|argvs| {
            argvs
                .iter()
                .filter_map(|a| {
                    a.as_array().map(|parts| {
                        parts
                            .iter()
                            .filter_map(|p| p.as_str().map(str::to_string))
                            .collect::<NextArgv>()
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `review queue --json`'s envelope. An empty queue is an honest empty, not
/// an error. Pure.
pub fn queue_envelope(body: &Value) -> Value {
    let empty = body["rows"].as_array().is_none_or(|r| r.is_empty());
    envelope::ok_value(
        QUEUE_SCHEMA,
        body.clone(),
        Vec::new(),
        false,
        empty.then_some("nothing is waiting for the agent"),
        queue_next(body),
    )
}

/// One row as one line. Pure.
pub fn row_line(r: &Value) -> String {
    format!(
        "{} · review {} · {}{} · from {} · since {}",
        r["lane"].as_str().unwrap_or("?"),
        r["review_id"],
        r["path"]
            .as_str()
            .filter(|p| !p.is_empty())
            .unwrap_or("(review-level)"),
        r["finding_slug"]
            .as_str()
            .map(|s| format!(" [{s}]"))
            .unwrap_or_default(),
        r["from"].as_str().unwrap_or("?"),
        r["waiting_since"],
    )
}

/// One line per row. Pure.
pub fn queue_lines(body: &Value) -> Vec<String> {
    let rows = body["rows"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return vec!["nothing is waiting for the agent".to_string()];
    }
    rows.iter().map(row_line).collect()
}

/// A row's identity for the watch diff: the lane, the review, the thread and
/// when it began waiting (a fresh human follow-up changes `waiting_since`, so
/// it surfaces again). Pure.
pub fn row_key(r: &Value) -> String {
    format!(
        "{}|{}|{}|{}",
        r["lane"].as_str().unwrap_or(""),
        r["review_id"],
        r["annotation_id"].as_str().unwrap_or(""),
        r["waiting_since"]
    )
}

/// Replace `seen` with the keys now present and return the rows whose key
/// was not in it. Because the set is REPLACED, a row that clears and comes
/// back surfaces again. Pure.
pub fn take_new(seen: &mut HashSet<String>, body: &Value) -> Vec<Value> {
    let rows = body["rows"].as_array().cloned().unwrap_or_default();
    let now: HashSet<String> = rows.iter().map(row_key).collect();
    let fresh = rows
        .into_iter()
        .filter(|r| !seen.contains(&row_key(r)))
        .collect();
    *seen = now;
    fresh
}

pub async fn queue_cmd(a: QueueArgs) -> anyhow::Result<()> {
    let json = a.json;
    match queue_run(&a).await {
        Ok(()) => Ok(()),
        Err(e) => e.emit(json),
    }
}

async fn queue_run(a: &QueueArgs) -> Result<(), AgentError> {
    let (path, q) = agent_queue_request(a.repo.as_deref(), &a.state);
    let body = review_agent::get_ok(&a.daemon, path, &q, "review queue").await?;
    if a.json {
        envelope::print_value(&queue_envelope(&body));
    } else {
        for l in queue_lines(&body) {
            println!("{l}");
        }
    }
    if !a.watch {
        return Ok(());
    }
    let mut seen: HashSet<String> = HashSet::new();
    take_new(&mut seen, &body);
    eprintln!(
        "[kb-code review queue] watching {} (poll every {}s) - Ctrl-C to stop",
        a.daemon, a.interval
    );
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            biased;
            _ = &mut ctrl_c => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(a.interval.max(1))) => {}
        }
        let body = match review_agent::get_ok(&a.daemon, path, &q, "review queue").await {
            Ok(b) => b,
            Err(e) => {
                // A transient daemon error must not end a /loop's watch.
                eprintln!("[kb-code review queue] poll failed: {e}; retrying");
                continue;
            }
        };
        for row in take_new(&mut seen, &body) {
            if a.json {
                println!("{row}");
            } else {
                println!("{}", row_line(&row));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_json::json;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        a: QueueArgs,
    }

    fn body() -> Value {
        json!({
            "schema": QUEUE_SCHEMA,
            "count": 2,
            "rows": [
                {"lane": "question", "review_id": 4, "path": "a.rb", "finding_slug": null,
                 "from": "you", "waiting_since": 100,
                 "next": [["kb-code", "review", "comments", "4", "--json"]]},
                {"lane": "dispute", "review_id": 9, "path": "b.rb", "finding_slug": "f-x",
                 "from": "you", "waiting_since": 200,
                 "next": [["kb-code", "review", "findings", "list", "9", "--json"]]}
            ]
        })
    }

    #[test]
    fn defaults_to_open_and_the_request_names_the_declared_route() {
        let a = Cli::try_parse_from(["queue"]).unwrap().a;
        assert_eq!(a.state, "open");
        let (path, q) = agent_queue_request(Some("w"), "all");
        assert_eq!(path, "/api/reviews/agent-queue");
        assert_eq!(
            q,
            vec![("state", "all".to_string()), ("repo", "w".to_string())]
        );
        assert_eq!(agent_queue_request(None, "open").1.len(), 1);
    }

    #[test]
    fn watch_surfaces_only_new_rows_and_a_returning_row_is_new_again() {
        let row = |ann: &str, since: i64| json!({"lane": "question", "review_id": 1, "annotation_id": ann, "waiting_since": since});
        let mut seen = HashSet::new();
        // the seed pass consumes the snapshot
        assert_eq!(
            take_new(&mut seen, &json!({"rows": [row("a", 1)]})).len(),
            1
        );
        // unchanged: nothing
        assert!(take_new(&mut seen, &json!({"rows": [row("a", 1)]})).is_empty());
        // a new thread, and a follow-up on `a` (new waiting_since): both new
        let fresh = take_new(&mut seen, &json!({"rows": [row("a", 9), row("b", 2)]}));
        assert_eq!(fresh.len(), 2);
        // `a` clears, then returns with the same key: surfaces again
        assert!(take_new(&mut seen, &json!({"rows": [row("b", 2)]})).is_empty());
        assert_eq!(
            take_new(&mut seen, &json!({"rows": [row("b", 2), row("a", 9)]})).len(),
            1
        );
    }

    #[test]
    fn watch_flags_parse_and_interval_needs_watch() {
        let a = Cli::try_parse_from(["queue", "--watch", "--interval", "5"])
            .unwrap()
            .a;
        assert!(a.watch && a.interval == 5);
        assert!(Cli::try_parse_from(["queue", "--interval", "5"]).is_err());
    }

    #[test]
    fn next_is_the_top_rows_own_command() {
        assert_eq!(
            queue_next(&body()),
            vec![vec!["kb-code", "review", "comments", "4", "--json"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()]
        );
        assert!(queue_next(&json!({"rows": []})).is_empty());
    }

    #[test]
    fn lines_name_lane_review_and_slug_and_an_empty_queue_says_so() {
        let lines = queue_lines(&body());
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("question · review 4 · a.rb"));
        assert!(lines[1].contains("[f-x]"));
        assert_eq!(
            queue_lines(&json!({"rows": []})),
            vec!["nothing is waiting for the agent".to_string()]
        );
        let env = queue_envelope(&json!({"rows": [], "count": 0}));
        assert_eq!(env["schema"], QUEUE_SCHEMA);
        assert_eq!(env["ok"], true);
    }
}
