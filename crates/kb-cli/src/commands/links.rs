//! `kb links {suggest,apply}` — CT-F3 unlinked mentions: "the graph you
//! wrote is half the graph you meant."
//!
//! `suggest` pulls `GET /api/kb/{kb}/links/suggest` — the DERIVED queue of
//! docs whose prose names another artifact's exact title or unique basename
//! with no `kind="link"` edge to show for it. Nothing is stored; the row is
//! recomputed on every read.
//!
//! `apply` is the human half: `POST /api/kb/{kb}/links/apply` re-derives the
//! mention and splices a real `[[wikilink]]` into the Markdown source. A
//! source that can't carry one (an HTML artifact, a memory body — invariant
//! #29) is reported by `suggest` with its reason and REFUSED by `apply`,
//! loudly, with the tree untouched.

use anyhow::{Context, Result};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

fn base_of(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

/// `kb links suggest [--kb NAME] [--limit N] [--json]`.
pub async fn suggest(
    kb: Option<&str>,
    limit: Option<u32>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let kb = crate::http::resolve_default_kb(kb, daemon, bearer).await?;
    let base = base_of(daemon);
    let mut url = format!(
        "{base}/api/kb/{}/links/suggest",
        crate::http::encode_path_segment(&kb)
    );
    if let Some(n) = limit {
        url.push_str(&format!("?limit={n}"));
    }
    // The report re-reads every Markdown source and projects the HTML
    // bodies, so it is an on-demand scan, not a keystroke query — give it
    // the same headroom `kb graph` gets.
    let client = crate::http::client_with_timeout_and_bearer(60, bearer)?;
    let body = crate::http::send_json(client.get(&url), &format!("GET {url}")).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    print_queue(&kb, &body);
    Ok(())
}

fn print_queue(kb: &str, r: &serde_json::Value) {
    let rows = r["suggestions"].as_array().cloned().unwrap_or_default();
    println!(
        "unlinked mentions — kb `{kb}` · {} docs scanned · names shorter than {} chars are skipped",
        r["scanned"].as_u64().unwrap_or(0),
        r["min_length"].as_u64().unwrap_or(0),
    );
    if rows.is_empty() {
        println!("\nnothing unlinked — every mention this corpus can see is already an edge.");
        return;
    }
    let (yes, no): (Vec<_>, Vec<_>) = rows
        .iter()
        .partition(|s| s["applicable"].as_bool().unwrap_or(false));

    if !yes.is_empty() {
        println!("\napplicable ({}) — apply authors the wikilink:", yes.len());
        for s in &yes {
            print_row(kb, s, true);
        }
    }
    if !no.is_empty() {
        println!(
            "\nnot applicable ({}) — reported, never rewritten:",
            no.len()
        );
        for s in &no {
            print_row(kb, s, false);
        }
    }
}

fn print_row(kb: &str, s: &serde_json::Value, applicable: bool) {
    let f = |side: &str, key: &str| s[side][key].as_str().unwrap_or("?");
    let target = s["target"].as_str().unwrap_or("?");
    let matched = s["matched"].as_str().unwrap_or("?");
    let kind = s["match_kind"].as_str().unwrap_or("?");
    let link = if matched == target {
        format!("[[{target}]]")
    } else {
        format!("[[{target}|{matched}]]")
    };
    let (src_id, dst_id) = (f("src", "id"), f("dst", "id"));
    println!(
        "\n  {src_id}  {}\n    → {dst_id}  {}  ({})",
        f("src", "source_relative"),
        f("dst", "title"),
        f("dst", "source_relative"),
    );
    println!("    {matched:?} ({kind} match) ⇒ {link}");
    if applicable {
        println!("    kb links apply {src_id} {dst_id} --kb {kb}");
    } else if let Some(note) = s["note"].as_str() {
        println!("    {note}");
    }
}

/// `kb links apply <src> <dst> [--kb NAME] [--json]`. `<src>`/`<dst>` are
/// each a 12-hex id, a source-relative path, or a unique filename — the
/// same target grammar every other artifact verb takes.
pub async fn apply(
    kb: Option<&str>,
    src: &str,
    dst: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let kb = crate::http::resolve_default_kb(kb, daemon, bearer).await?;
    let (_, src_id) = crate::http::resolve_artifact_target(Some(&kb), src, daemon, bearer)
        .await
        .with_context(|| format!("resolving source {src:?}"))?;
    let (_, dst_id) = crate::http::resolve_artifact_target(Some(&kb), dst, daemon, bearer)
        .await
        .with_context(|| format!("resolving destination {dst:?}"))?;
    let base = base_of(daemon);
    let url = format!(
        "{base}/api/kb/{}/links/apply",
        crate::http::encode_path_segment(&kb)
    );
    let client = crate::http::client_with_timeout_and_bearer(60, bearer)?;
    let req = client
        .post(&url)
        .json(&serde_json::json!({ "src": src_id, "dst": dst_id }));
    let body = crate::http::send_json(req, &format!("POST {url}")).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "linked {} → {}\n  {} now carries {}",
        body["src"]["title"].as_str().unwrap_or(&src_id),
        body["dst"]["title"].as_str().unwrap_or(&dst_id),
        body["source_relative"].as_str().unwrap_or("?"),
        body["wikilink"].as_str().unwrap_or("?"),
    );
    Ok(())
}
