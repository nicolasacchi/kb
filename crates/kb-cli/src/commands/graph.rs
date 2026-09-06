//! `kb graph <kb> [--top N] [--json]` — the deterministic corpus graph
//! report. Pulls `GET /api/kb/{kb}/graph/report` (GS-track): hubs by
//! in-degree, orphans (never linked ∧ never opened), dead-edge link-rot,
//! and dangling/ambiguous wikilinks re-resolved over Markdown sources.

use anyhow::{Context, Result};

pub async fn run(
    kb: &str,
    top: usize,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    let url = format!(
        "{base}/api/kb/{}/graph/report?top={top}",
        crate::http::encode_path_segment(kb),
    );

    let mut builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(60));
    if let Some(token) = bearer {
        let mut headers = reqwest::header::HeaderMap::new();
        let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .context("invalid bearer header")?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
        builder = builder.default_headers(headers);
    }
    let resp: serde_json::Value = builder
        .build()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }
    print_report(kb, &resp);
    Ok(())
}

fn print_report(kb: &str, r: &serde_json::Value) {
    let n = |k: &str| r[k].as_u64().unwrap_or(0);
    println!("graph report — {kb}");
    println!(
        "  {} docs · {} edges · {} linked · {} never linked · {} opened",
        n("docs"),
        n("edges"),
        n("linked_docs"),
        n("never_linked"),
        n("opened_docs"),
    );
    println!(
        "  max degree in/out {}/{}",
        n("max_in_degree"),
        n("max_out_degree")
    );

    let hubs = r["hubs"].as_array().cloned().unwrap_or_default();
    if !hubs.is_empty() {
        println!("\nhubs (by in-degree):");
        for h in &hubs {
            println!(
                "  {:>3}in {:>3}out  {}  {}",
                h["inbound"].as_u64().unwrap_or(0),
                h["outbound"].as_u64().unwrap_or(0),
                h["id"].as_str().unwrap_or("?"),
                h["title"].as_str().unwrap_or(""),
            );
        }
    }

    let orphans = r["orphans"].as_array().cloned().unwrap_or_default();
    println!("\norphans (never linked ∧ never opened): {}", orphans.len());
    for o in orphans.iter().take(20) {
        println!(
            "  {}  {}",
            o["id"].as_str().unwrap_or("?"),
            o["rel_path"].as_str().unwrap_or(""),
        );
    }
    if orphans.len() > 20 {
        println!("  … {} more (use --json for all)", orphans.len() - 20);
    }

    let dead_dst = r["dead_dst_edges"].as_array().cloned().unwrap_or_default();
    let dead_src = r["dead_src_edges"].as_array().cloned().unwrap_or_default();
    println!(
        "\nlink-rot: {} dead-target edges · {} dead-source edges",
        dead_dst.len(),
        dead_src.len()
    );
    for e in dead_dst.iter().take(10) {
        println!(
            "  {} → {}  (from {})",
            e["src"].as_str().unwrap_or("?"),
            e["dst"].as_str().unwrap_or("?"),
            e["src_rel_path"].as_str().unwrap_or("gone"),
        );
    }
    if dead_dst.len() > 10 {
        println!("  … {} more (use --json for all)", dead_dst.len() - 10);
    }
    if !dead_dst.is_empty() {
        println!("  hint: `kb reindex --kb {kb}` clears rows whose source no longer links there");
    }

    let unresolved = r["unresolved_wikilinks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    println!("\nunresolved wikilinks: {}", unresolved.len());
    for u in unresolved.iter().take(20) {
        println!(
            "  [[{}]] in {}  ({})",
            u["target"].as_str().unwrap_or("?"),
            u["src_rel_path"].as_str().unwrap_or("?"),
            u["state"].as_str().unwrap_or("?"),
        );
    }
    if unresolved.len() > 20 {
        println!("  … {} more (use --json for all)", unresolved.len() - 20);
    }
}
