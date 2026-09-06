//! `kb propose` / `kb proposals` — the W2.15b tribal-knowledge proposal
//! inbox. `propose` is the agent-layer submit verb (queues a memory
//! CANDIDATE instead of writing it directly — `kb remember` still writes
//! immediately; use `propose` when a skill wants the human-gate queue
//! instead, e.g. a future /kb-distill-style flow). `proposals
//! list/approve/reject` are the human-side review verbs; approving fires
//! the EXACT `kb remember` write path server-side.

use crate::http;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::io::Read;

/// `kb propose --title T --body <text|-> [opts]` — POST a candidate to the
/// daemon's `.proposals/` queue. `--body -` reads the candidate text from
/// stdin (same `-` sentinel `kb board set --file -`/`kb list import -` use).
#[allow(clippy::too_many_arguments)]
pub async fn propose(
    title: &str,
    body: &str,
    kb: Option<&str>,
    tags: Option<&str>,
    global: bool,
    link: Option<&str>,
    salience: Option<f32>,
    session_id: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let res = propose_inner(
        title, body, kb, tags, global, link, salience, session_id, daemon, bearer,
    )
    .await;
    match res {
        Ok(out) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!(
                    "proposed {}  ({})",
                    out["id"].as_str().unwrap_or("?"),
                    out["title"].as_str().unwrap_or("?")
                );
            }
            Ok(())
        }
        Err(e) => {
            if json {
                emit_json_error(&e, daemon);
            }
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn propose_inner(
    title: &str,
    body: &str,
    kb: Option<&str>,
    tags: Option<&str>,
    global: bool,
    link: Option<&str>,
    salience: Option<f32>,
    session_id: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<Value> {
    let url = require_daemon(daemon, bearer).await?;
    let kb = http::resolve_default_kb(kb, Some(&url), bearer).await?;

    let body_text = if body == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("read proposal body from stdin")?;
        s
    } else {
        body.to_string()
    };

    let tags_vec: Vec<String> = tags
        .map(|t| {
            t.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    // L8-style visibility flags (mirrors `kb remember`): `--link a,b` scopes
    // the eventual memory; `--global` (or the absence of both) is global.
    // clap's `conflicts_with` already rejects passing both.
    let linked_vec: Vec<String> = link
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let _ = global;
    let payload_global = link.is_none();

    let mut payload = serde_json::json!({
        "title": title,
        "body": body_text,
        "tags": tags_vec,
        "global": payload_global,
        "linked_kbs": linked_vec,
    });
    if let Some(s) = salience {
        payload["salience"] = serde_json::json!(s);
    }
    if let Some(sid) = session_id {
        payload["session_id"] = serde_json::json!(sid);
    }

    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let resp = client
        .post(format!(
            "{url}/api/kb/{}/proposals",
            http::encode_path_segment(&kb)
        ))
        .json(&payload)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    Ok(resp.json().await?)
}

/// `kb proposals [list] [--kb NAME] [--json] [--daemon URL]` — the fleet-wide
/// queue (`GET /api/proposals`).
pub async fn list(
    kb_filter: Option<&str>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let full = format!("{url}/api/proposals");
    let client = http::client_with_timeout_and_bearer(5, bearer)?;
    let mut q: Vec<(&str, String)> = Vec::new();
    if let Some(k) = kb_filter {
        q.push(("kb", k.to_string()));
    }
    let body: Value = client
        .get(&full)
        .query(&q)
        .send()
        .await
        .with_context(|| format!("GET {full}"))?
        .error_for_status()
        .with_context(|| format!("GET {full}"))?
        .json()
        .await?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let items = body["items"].as_array().cloned().unwrap_or_default();
    let total = body["total"].as_u64().unwrap_or(0);
    if items.is_empty() {
        println!("(no queued proposals)");
        return Ok(());
    }
    println!("{:<16} {:<10} {:<40} SOURCE", "ID", "KB", "TITLE");
    for it in &items {
        println!(
            "{:<16} {:<10} {:<40} {}",
            truncate(it["id"].as_str().unwrap_or(""), 16),
            truncate(it["kb"].as_str().unwrap_or(""), 10),
            truncate(it["title"].as_str().unwrap_or(""), 40),
            it["source"].as_str().unwrap_or("-"),
        );
    }
    let shown = items.len() as u64;
    if shown < total {
        println!("\n{shown} of {total} proposal(s) — pass --kb to narrow");
    } else {
        println!("\n{total} proposal(s) across the fleet");
    }
    Ok(())
}

/// `kb proposals approve <id> [--kb NAME]` — writes the memory (the EXACT
/// `kb remember` path, server-side) and removes the proposal from the queue.
pub async fn approve(
    id: &str,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json_out: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let kb = http::resolve_default_kb(kb, Some(&url), bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let full = format!(
        "{url}/api/kb/{}/proposals/{}/approve",
        http::encode_path_segment(&kb),
        http::encode_path_segment(id)
    );
    let resp = client.post(&full).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    let out: Value = resp.json().await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "approved {id} → memory {} (kb {kb})",
            out["artifact_id"].as_str().unwrap_or("?")
        );
    }
    Ok(())
}

/// `kb proposals reject <id> [--kb NAME]` — discards the proposal, writes
/// nothing.
pub async fn reject(
    id: &str,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let kb = http::resolve_default_kb(kb, Some(&url), bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let full = format!(
        "{url}/api/kb/{}/proposals/{}/reject",
        http::encode_path_segment(&kb),
        http::encode_path_segment(id)
    );
    let resp = client.post(&full).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    println!("rejected {id} (kb {kb})");
    Ok(())
}

// ---- helpers ---------------------------------------------------------

async fn require_daemon(daemon: Option<&str>, bearer: Option<&str>) -> Result<String> {
    http::detect_daemon(daemon, bearer).await.ok_or_else(|| {
        anyhow!(
            "daemon not reachable{} — start it with `kb daemon`",
            daemon.map(|d| format!(" at {d}")).unwrap_or_default()
        )
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

fn emit_json_error(e: &anyhow::Error, daemon: Option<&str>) {
    let envelope = serde_json::json!({
        "error": e.to_string(),
        "source": daemon.unwrap_or("(auto-detect)"),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&envelope).unwrap_or_default()
    );
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_leaves_short_strings_alone() {
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn truncate_ellipsizes_long_strings() {
        let out = truncate("a very long title indeed", 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }
}
