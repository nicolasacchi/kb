//! `kb history` — list per-kb activity history (v0.34 Z1 / kb-users/1).
//!
//! Hits `GET /api/kb/{kb}/history?limit=&kind=&user=&before=`. Human form
//! is a compact table; `--json` emits the raw `{entries:[…]}` body.
//! Optional `--user` passes the server-side attribution filter.

use crate::http::{client_with_timeout_and_bearer, encode_path_segment, resolve_default_kb};
use anyhow::{anyhow, Result};
use serde_json::Value;

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

fn base_url(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

/// `kb history [--kb NAME] [--kind open|search|comment|all] [--user NAME]
/// [--limit N] [--daemon URL] [--json]`.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    kb: Option<&str>,
    kind: Option<&str>,
    user: Option<&str>,
    limit: Option<u32>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json_out: bool,
) -> Result<()> {
    let kb_name = resolve_default_kb(kb, daemon, bearer).await?;
    let base = base_url(daemon);
    let url = format!("{base}/api/kb/{}/history", encode_path_segment(&kb_name));
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let mut q: Vec<(&str, String)> = Vec::new();
    if let Some(k) = kind {
        q.push(("kind", k.to_string()));
    }
    if let Some(u) = user {
        q.push(("user", u.to_string()));
    }
    if let Some(n) = limit {
        q.push(("limit", n.to_string()));
    }

    let resp =
        client.get(&url).query(&q).send().await.map_err(|e| {
            anyhow!("daemon not reachable at {base} ({e}); start it with `kb daemon`")
        })?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    let body: Value = resp.json().await?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let entries = body
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if entries.is_empty() {
        println!("history  [{kb_name}]  (no entries)");
        return Ok(());
    }

    println!("{:<6} {:<8} {:<14} DETAIL", "ID", "KIND", "STARTED");
    for e in &entries {
        let id = e.get("id").and_then(Value::as_i64).unwrap_or(0);
        let kind = e.get("kind").and_then(Value::as_str).unwrap_or("?");
        let started = e.get("started_at").and_then(Value::as_i64).unwrap_or(0);
        let detail = match kind {
            "search" => e
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            "comment" => format!(
                "{} {}",
                e.get("artifact_id").and_then(Value::as_str).unwrap_or(""),
                e.get("comment_id").and_then(Value::as_str).unwrap_or("")
            ),
            _ => {
                // open (and unknown) — prefer title, fall back to id.
                e.get("title")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .or_else(|| e.get("artifact_id").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string()
            }
        };
        let detail = truncate(&detail, 48);
        println!("{id:<6} {kind:<8} {started:<14} {detail}");
    }
    println!(
        "\n{} entr{}  [{kb_name}]",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" }
    );
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
