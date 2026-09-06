//! `kb similar <id> [--kb] [--limit] [--json]` — true (embedding-space)
//! nearest neighbors for one artifact. Pulls from `GET
//! /api/kb/{kb}/atlas/similar/{id}?limit=N` (W2.3a). Distinct from `kb
//! related` (the `kind='link'` wikilink/hub graph) — this is vector-space
//! cosine similarity, not the link graph or text-query recall.

use crate::http::{client_with_timeout_and_bearer, encode_path_segment};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;

pub async fn run(
    id: &str,
    kb: Option<&str>,
    limit: Option<u32>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    let kb = kb.ok_or_else(|| anyhow!("--kb is required"))?;
    let mut url = format!(
        "{base}/api/kb/{}/atlas/similar/{}",
        encode_path_segment(kb),
        encode_path_segment(id),
    );
    if let Some(limit) = limit {
        url.push_str(&format!("?limit={limit}"));
    }

    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    print_human(id, &body);
    Ok(())
}

fn print_human(id: &str, body: &Value) {
    if let Some(reason) = body["reason"].as_str() {
        println!("similar  [{id}]  no neighbors — {reason}");
        return;
    }
    let neighbors = body["neighbors"].as_array().cloned().unwrap_or_default();
    if neighbors.is_empty() {
        println!("similar  [{id}]  no neighbors found");
        return;
    }
    println!(
        "similar  [{id}]  {} neighbor{}",
        neighbors.len(),
        if neighbors.len() == 1 { "" } else { "s" }
    );
    for n in &neighbors {
        let cosine = n["cosine"].as_f64().unwrap_or(0.0);
        let title = n["title"].as_str().unwrap_or("?");
        let nid = n["id"].as_str().unwrap_or("?");
        println!("  {cosine:>6.3}  {title}  ({nid})");
    }
}
