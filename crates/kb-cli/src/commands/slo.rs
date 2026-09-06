//! `kb slo status|snapshot|log` — CT-F5 corpus-health SLOs.
//!
//! Three thin verbs over `routes/slo.rs`; every definition, every threshold
//! comparison and every `unknown` decision lives server-side in
//! `kb_core::slo`, so the CLI and the SPA can never disagree about what a
//! number means.
//!
//! **SURFACED, NEVER ENFORCED — including the exit code.** `kb slo status`
//! exits 0 on a `warn`. It is deliberately NOT a check command: making a
//! missed target fail a shell would put this number on a CI gate's critical
//! path, and an SLO that can block something stops being an honest
//! measurement and becomes a thing people tune to stay green. If an operator
//! wants an alert they can build one from `--json`; kb will not ship one.

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::http;

/// `kb slo status [--kb NAME] [--json]`.
pub async fn status(
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let (base, kb_name, client) = connect(kb, daemon, bearer).await?;
    let url = format!("{base}/api/kb/{}/slo", http::encode_path_segment(&kb_name));
    let body: Value = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    print_report(&body);
    Ok(())
}

/// `kb slo snapshot [--kb NAME] [--json]` — append one run to the append-only
/// log. Every run lands, including one identical to the last: a flat line is
/// itself the reading.
pub async fn snapshot(
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let (base, kb_name, client) = connect(kb, daemon, bearer).await?;
    let url = format!(
        "{base}/api/kb/{}/slo/snapshot",
        http::encode_path_segment(&kb_name)
    );
    let body: Value = client
        .post(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let appended = body["appended"].as_u64().unwrap_or(0);
    let at = body["taken_at_unix"].as_i64().unwrap_or(0);
    println!("snapshot appended — {appended} indicator row(s) at {at}\n");
    print_report(&body["report"]);
    Ok(())
}

/// `kb slo log [--kb NAME] [--limit N] [--json]` — newest-first page over the
/// append-only log. Without a reader the log would be a write-only table, so
/// this verb ships with the writer, not after it.
pub async fn log(
    kb: Option<&str>,
    limit: u32,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let (base, kb_name, client) = connect(kb, daemon, bearer).await?;
    let url = format!(
        "{base}/api/kb/{}/slo/snapshots?limit={limit}",
        http::encode_path_segment(&kb_name)
    );
    let body: Value = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let rows = body["rows"].as_array().cloned().unwrap_or_default();
    println!("slo log — {kb_name} ({} row(s), newest first)", rows.len());
    if rows.is_empty() {
        println!("  (empty — nothing has run `kb slo snapshot` against this corpus yet)");
        return Ok(());
    }
    for r in &rows {
        println!(
            "  {:>12}  {:<26} {:>10}  target {:>8}  {}",
            r["taken_at_unix"].as_i64().unwrap_or(0),
            r["indicator"].as_str().unwrap_or("?"),
            fmt_opt_num(r.get("value")),
            fmt_opt_num(r.get("target")),
            r["status"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

/// Resolve the daemon + the kb once, shared by all three verbs.
async fn connect(
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<(String, String, reqwest::Client)> {
    let base = http::detect_daemon(daemon, bearer).await.ok_or_else(|| {
        anyhow!(
            "daemon not reachable{} — start it with `kb daemon`",
            daemon.map(|d| format!(" at {d}")).unwrap_or_default()
        )
    })?;
    let kb_name = http::resolve_default_kb(kb, daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(30, bearer)?;
    Ok((base, kb_name, client))
}

/// `null` renders as `—`, never as `0`: an unmeasured indicator and a
/// measured zero are different facts, and the whole `unknown` status exists to
/// keep them apart.
fn fmt_opt_num(v: Option<&Value>) -> String {
    match v.and_then(|v| v.as_f64()) {
        Some(n) => format!("{n}"),
        None => "—".to_string(),
    }
}

fn print_report(r: &Value) {
    let kb = r["kb"].as_str().unwrap_or("?");
    let warn = r["warn_count"].as_u64().unwrap_or(0);
    println!("corpus-health SLOs — {kb}   ({warn} warn)");
    println!("  surfaced, never enforced: nothing changes behaviour on a miss.\n");
    for i in r["indicators"].as_array().cloned().unwrap_or_default() {
        let unit = i["unit"].as_str().unwrap_or("");
        let value = match i["value"].as_f64() {
            Some(v) => format!("{v}{}", unit_suffix(unit)),
            None => "—".to_string(),
        };
        let target = match i["target"].as_f64() {
            Some(t) => {
                let dir = if i["direction"].as_str() == Some("higher_is_better") {
                    "min"
                } else {
                    "max"
                };
                format!("{dir} {t}{}", unit_suffix(unit))
            }
            None => "no target".to_string(),
        };
        println!(
            "  [{:^7}] {:<30} {:>10}   ({target})",
            i["status"].as_str().unwrap_or("?"),
            i["label"].as_str().unwrap_or("?"),
            value,
        );
        if let Some(d) = i["detail"].as_str() {
            println!("            {d}");
        }
    }
}

fn unit_suffix(unit: &str) -> &'static str {
    match unit {
        "percent" => "%",
        "hours" => "h",
        _ => "",
    }
}
