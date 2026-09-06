//! `kb whoami` — print the daemon-resolved identity for this CLI request
//! (v0.34 Z1 / kb-users/1).
//!
//! Hits `GET /api/identity` with the same bearer the other verbs send
//! (`read_bearer()` → `~/.config/kb/token`). Human form is
//! `user (source)`; `--output json` echoes the raw response.

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

/// `kb whoami [--daemon URL] [--output json]`.
pub async fn run(daemon: Option<&str>, bearer: Option<&str>, output: Option<&str>) -> Result<()> {
    let base = base_url(daemon);
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let url = format!("{base}/api/identity");
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

    let user = body
        .get("user")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let source = body
        .get("identity_source")
        .and_then(Value::as_str)
        .unwrap_or("?");
    println!("{user} ({source})");
    Ok(())
}
