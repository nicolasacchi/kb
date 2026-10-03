//! v0.44 F9 — `kb-code review queue [--repo R] [--state open|closed|all]
//! [--json]`: what is waiting FOR THE AGENT (`GET /api/reviews/agent-queue`,
//! computed by `kb_code_server::review_queue`; this verb adds no
//! interpretation). Lane 1 is an open `question` thread whose latest voice is
//! not an agent, lane 2 a disputed finding with no agent reply since the
//! dispute. Each row carries the argv to run next. A work queue, not a score
//! and not a dispatcher: the daemon launches nothing.

use crate::envelope::{self, NextArgv};
use crate::review_agent::{self, AgentError, DEFAULT_DAEMON};
use clap::Args;
use serde_json::Value;

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

/// One line per row. Pure.
pub fn queue_lines(body: &Value) -> Vec<String> {
    let rows = body["rows"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return vec!["nothing is waiting for the agent".to_string()];
    }
    rows.iter()
        .map(|r| {
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
        })
        .collect()
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
        return Ok(());
    }
    for l in queue_lines(&body) {
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
