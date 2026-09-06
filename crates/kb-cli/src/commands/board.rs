//! `kb board <list> [--kb] [--json]` / `kb board set <list> --file <path|->`
//! — Boards v1 (W2.4): the JSON Canvas `.canvas` geometry sidecar behind
//! `GET`/`PUT /api/kb/{kb}/boards/{list_id}/canvas`. The daemon stores
//! whatever parses (`routes::boards`, "store what parses" — the format
//! itself IS the interop contract), so this CLI is deliberately thin: it
//! never models JSON Canvas as a Rust type, it just resolves `<list>` to
//! `(kb, list_id)` the same way `kb list show` does, then GETs/PUTs the
//! raw JSON body straight through.
//!
//! Geometry is a CORPUS SIDECAR (`<source_root>/boards/<list_id>.canvas`),
//! not part of the kb-list/1 document — `kb list export`/`import` never
//! touch it; this is the only CLI surface that reads or writes it.
//!
//! `<list>` addressing duplicates `commands::list`'s `resolve_list` (not
//! reused directly — that helper is private to `list.rs`, which this
//! phase doesn't own) rather than promoting it to `pub(crate)`.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::Read;

use crate::http::{
    client_with_timeout_and_bearer, encode_path_segment, resolve_default_kb, send_json,
};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

fn base_url(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

fn canvas_url(daemon: Option<&str>, kb: &str, list_id: &str) -> String {
    format!(
        "{}/api/kb/{}/boards/{}/canvas",
        base_url(daemon),
        encode_path_segment(kb),
        encode_path_segment(list_id),
    )
}

/// Resolve `<list>` to `(kb, list_id)`: exact `l_…` id first, else a
/// case-insensitive title match within the (resolved) kb. Mirrors
/// `commands::list::resolve_list` — see the module doc for why this
/// isn't a shared function instead.
async fn resolve_list_ref(
    kb: Option<&str>,
    list: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<(String, String)> {
    let resolved_kb = resolve_default_kb(kb, daemon, bearer).await?;
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let url = format!("{}/api/lists?include_archived=true", base_url(daemon));
    let body = send_json(client.get(url), "list index").await?;
    let empty = Vec::new();
    let rows = body["lists"].as_array().unwrap_or(&empty);
    let in_kb: Vec<&Value> = rows
        .iter()
        .filter(|l| l["kb"].as_str() == Some(resolved_kb.as_str()))
        .collect();
    if let Some(l) = in_kb.iter().find(|l| l["id"].as_str() == Some(list)) {
        let id = l["id"].as_str().context("list row missing id")?.to_string();
        return Ok((resolved_kb, id));
    }
    let matched: Vec<&&Value> = in_kb
        .iter()
        .filter(|l| {
            l["title"]
                .as_str()
                .is_some_and(|t| t.eq_ignore_ascii_case(list))
        })
        .collect();
    match matched.len() {
        1 => {
            let id = matched[0]["id"]
                .as_str()
                .context("list row missing id")?
                .to_string();
            Ok((resolved_kb, id))
        }
        0 => {
            let mut msg =
                format!("no reading list {list:?} in kb {resolved_kb}; existing lists:\n");
            if in_kb.is_empty() {
                msg.push_str("  (none — `kb list create <title>`)\n");
            }
            for l in &in_kb {
                msg.push_str(&format!(
                    "  {}  {}\n",
                    l["id"].as_str().unwrap_or("?"),
                    l["title"].as_str().unwrap_or("?")
                ));
            }
            bail!(msg)
        }
        _ => {
            let mut msg = format!("{list:?} matched {} lists; pick by id:\n", matched.len());
            for l in matched {
                msg.push_str(&format!(
                    "  {}  {}\n",
                    l["id"].as_str().unwrap_or("?"),
                    l["title"].as_str().unwrap_or("?")
                ));
            }
            bail!(msg)
        }
    }
}

/// Manual parser for the `Show` catch-all's raw tokens (`BoardAction::
/// Show(Vec<String>)` in `main.rs`) — clap's typed per-field parsing
/// doesn't reach an `external_subcommand` variant, so `--kb`/`--json`
/// are hand-scanned here. Exactly one non-flag token is expected (the
/// list ref); `--daemon` rides along for parity with every other verb's
/// dev-pointed-at-a-non-default-port flag.
pub fn parse_show_args(raw: &[String]) -> Result<(String, Option<String>, bool, Option<String>)> {
    let mut list: Option<String> = None;
    let mut kb: Option<String> = None;
    let mut json = false;
    let mut daemon: Option<String> = None;
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--kb" => {
                i += 1;
                kb = Some(raw.get(i).cloned().context("--kb requires a value")?);
            }
            "--json" => json = true,
            "--daemon" => {
                i += 1;
                daemon = Some(raw.get(i).cloned().context("--daemon requires a value")?);
            }
            other if list.is_none() => list = Some(other.to_string()),
            other => bail!("kb board: unexpected argument {other:?}"),
        }
        i += 1;
    }
    let list = list.context("kb board: missing <list-id-or-title>")?;
    Ok((list, kb, json, daemon))
}

/// `kb board <list-id-or-title> [--kb] [--json]` — print the board's
/// JSON Canvas doc (pretty by default; `--json` prints it compact, for
/// piping). A list with no board yet prints the empty default the GET
/// route itself serves (`{"nodes":[],"edges":[]}`) — there's nothing to
/// create until the first `kb board set` / SPA drag.
pub async fn show(
    list: &str,
    kb: Option<&str>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (resolved_kb, list_id) = resolve_list_ref(kb, list, daemon, bearer).await?;
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let canvas = send_json(
        client.get(canvas_url(daemon, &resolved_kb, &list_id)),
        "board show",
    )
    .await?;
    if json_out {
        println!("{canvas}");
    } else {
        println!("{}", serde_json::to_string_pretty(&canvas)?);
    }
    Ok(())
}

/// `kb board set <list> --file <path|->` — replace the board's canvas
/// wholesale. `--file -` reads the JSON Canvas doc from stdin (the
/// heredoc shape every other `kb list import -` style verb uses).
/// Geometry is a corpus sidecar, not part of the kb-list/1 export/import
/// document — this never touches list membership/order.
pub async fn set(
    list: &str,
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
    let (resolved_kb, list_id) = resolve_list_ref(kb, list, daemon, bearer).await?;
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let canvas = send_json(
        client
            .put(canvas_url(daemon, &resolved_kb, &list_id))
            .header("Content-Type", "application/json")
            .body(body),
        "board set",
    )
    .await?;
    let nodes = canvas["nodes"].as_array().map(Vec::len).unwrap_or(0);
    let edges = canvas["edges"].as_array().map(Vec::len).unwrap_or(0);
    println!("board {list_id} set — {nodes} node(s), {edges} edge(s)");
    Ok(())
}
