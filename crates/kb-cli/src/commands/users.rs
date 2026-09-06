//! `kb users` — list configured ∪ observed users (v0.34 Z1 / kb-users/1).
//!
//! Hits `GET /api/users`. Human form is a four-column table (name, display,
//! configured, observed); `--output json` echoes the raw response.

use crate::http::client_with_timeout_and_bearer;
use anyhow::{anyhow, Result};
use serde_json::Value;

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

fn base_url(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

/// `kb users [--daemon URL] [--output json]`.
pub async fn run(daemon: Option<&str>, bearer: Option<&str>, output: Option<&str>) -> Result<()> {
    let base = base_url(daemon);
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let url = format!("{base}/api/users");
    let resp =
        client.get(&url).send().await.map_err(|e| {
            anyhow!("daemon not reachable at {base} ({e}); start it with `kb daemon`")
        })?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    let body: Value = resp.json().await?;

    if output == Some("json") {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let users = body
        .get("users")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if users.is_empty() {
        println!("(no users configured or observed)");
        return Ok(());
    }

    println!(
        "{:<20} {:<20} {:<11} OBSERVED",
        "NAME", "DISPLAY", "CONFIGURED"
    );
    for u in &users {
        let name = u.get("name").and_then(Value::as_str).unwrap_or("");
        let display = u.get("display").and_then(Value::as_str).unwrap_or("-");
        let configured = if u
            .get("configured")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "yes"
        } else {
            "no"
        };
        let observed = if u.get("observed").and_then(Value::as_bool).unwrap_or(false) {
            "yes"
        } else {
            "no"
        };
        println!("{name:<20} {display:<20} {configured:<11} {observed}");
    }
    Ok(())
}
