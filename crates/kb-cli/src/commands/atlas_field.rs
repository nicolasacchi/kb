//! `kb atlas field [--kb] [--json]` / `kb atlas field set --file <path|->` /
//! `kb atlas field diff [--limit N] [--json]` — the dual-field atlas's
//! operator half (W3 F-b): the JSON Canvas `.canvas` sidecar behind
//! `GET`/`PUT /api/kb/{kb}/atlas/field` plus the machine-vs-operator
//! displacement score behind `GET /api/kb/{kb}/atlas/field/disagreement`.
//! Modelled on `commands::board` (the sidecar CLI precedent): the daemon
//! stores whatever parses ("store what parses" — the JSON Canvas format
//! itself IS the interop contract), so `field`/`field set` are deliberately
//! thin, never modelling JSON Canvas as a Rust type and just passing the
//! raw JSON body straight through.
//!
//! Unlike a board, the field has no `<list>` to resolve — it's ONE sidecar
//! per kb (`atlas/operator.canvas`), so every verb here only needs `--kb`
//! (defaulted via `resolve_default_kb` the same way `kb atlas points`/
//! `labels`/etc. do) — no id/title lookup round-trip. Unlike
//! `commands::board`, there's no bare-positional/`external_subcommand`
//! parsing here either: `main.rs`'s `action: Option<AtlasFieldAction>`
//! plain-clap-parses `--kb`/`--json`/`--daemon` directly (see that
//! variant's doc comment for why the board's catch-all trick isn't needed).

use anyhow::{Context, Result};
use serde_json::Value;
use std::io::Read;

use crate::http::{
    client_with_timeout_and_bearer, encode_path_segment, resolve_default_kb, send_json,
};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";
const DEFAULT_DIFF_LIMIT: usize = 20;

fn base_url(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

fn field_url(daemon: Option<&str>, kb: &str) -> String {
    format!(
        "{}/api/kb/{}/atlas/field",
        base_url(daemon),
        encode_path_segment(kb),
    )
}

fn disagreement_url(daemon: Option<&str>, kb: &str) -> String {
    format!(
        "{}/api/kb/{}/atlas/field/disagreement",
        base_url(daemon),
        encode_path_segment(kb),
    )
}

/// `kb atlas field [--kb] [--json]` — print the operator field's raw JSON
/// Canvas doc (pretty by default; `--json` prints it compact). A kb with
/// no field yet prints the empty default the GET route itself serves
/// (`{"nodes":[],"edges":[]}`) — nothing to create until the first
/// `kb atlas field set` / SPA drag.
pub async fn show(
    kb: Option<&str>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let resolved_kb = resolve_default_kb(kb, daemon, bearer).await?;
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let canvas = send_json(
        client.get(field_url(daemon, &resolved_kb)),
        "atlas field show",
    )
    .await?;
    if json_out {
        println!("{canvas}");
    } else {
        println!("{}", serde_json::to_string_pretty(&canvas)?);
    }
    Ok(())
}

/// `kb atlas field set --file <path|-> [--kb]` — replace the operator
/// field wholesale. `--file -` reads the JSON Canvas doc from stdin.
/// Geometry is a corpus sidecar — this never touches any reading list or
/// the index.
pub async fn set(
    file: &str,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let body = if file == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("read canvas JSON from stdin")?;
        s
    } else {
        std::fs::read_to_string(file).with_context(|| format!("read {file}"))?
    };
    let resolved_kb = resolve_default_kb(kb, daemon, bearer).await?;
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let canvas = send_json(
        client
            .put(field_url(daemon, &resolved_kb))
            .header("Content-Type", "application/json")
            .body(body),
        "atlas field set",
    )
    .await?;
    let nodes = canvas["nodes"].as_array().map(Vec::len).unwrap_or(0);
    let edges = canvas["edges"].as_array().map(Vec::len).unwrap_or(0);
    println!("atlas field [{resolved_kb}] set — {nodes} node(s), {edges} edge(s)");
    Ok(())
}

/// `kb atlas field diff [--kb] [--limit N] [--json]` — the top-N
/// most-disagreeing artifacts between the machine layout and the operator
/// field (`GET /api/kb/{kb}/atlas/field/disagreement`), Procrustes-aligned
/// server-side (same fit `kb atlas show`'s alignment uses). `--json` prints
/// the full (server-sorted, un-truncated) response; the human view slices
/// to `--limit` (default 20).
pub async fn diff(
    kb: Option<&str>,
    limit: Option<usize>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let resolved_kb = resolve_default_kb(kb, daemon, bearer).await?;
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let url = disagreement_url(daemon, &resolved_kb);
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    print_diff_human(&resolved_kb, &body, limit.unwrap_or(DEFAULT_DIFF_LIMIT));
    Ok(())
}

fn print_diff_human(kb: &str, body: &Value, limit: usize) {
    let rows = body["disagreements"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let matched = body["matched"].as_u64().unwrap_or(0);
    let placements = body["operator_placements"].as_u64().unwrap_or(0);

    if placements == 0 {
        println!("atlas field diff  [{kb}]  the operator field is empty — nothing placed yet");
        println!("  run `kb atlas field set --file <path|-> ` (or place artifacts in the SPA)");
        return;
    }
    if matched == 0 {
        println!(
            "atlas field diff  [{kb}]  {placements} artifact(s) placed, but none join the \
             machine layout"
        );
        println!("  (their ids don't match any doc's source-relative path, or the kb hasn't");
        println!("  run an atlas recompute yet — see `kb atlas recompute --kb {kb}`)");
        return;
    }

    println!(
        "atlas field diff  [{kb}]  {matched} matched artifact(s) — top {} by disagreement",
        rows.len().min(limit)
    );
    for row in rows.iter().take(limit) {
        let id = row["id"].as_str().unwrap_or("?");
        let distance = row["distance"].as_f64().unwrap_or(0.0);
        let mx = row["machine_x"].as_f64().unwrap_or(0.0);
        let my = row["machine_y"].as_f64().unwrap_or(0.0);
        let ox = row["operator_x"].as_f64().unwrap_or(0.0);
        let oy = row["operator_y"].as_f64().unwrap_or(0.0);
        println!(
            "  {distance:.4}  {id}  machine ({mx:.3}, {my:.3}) vs operator ({ox:.3}, {oy:.3})"
        );
    }
}
