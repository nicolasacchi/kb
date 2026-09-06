//! `kb related <id> [--depth N]` — print outbound link graph for an
//! artifact. Pulls from `/api/kb/{kb}/graph/{id}?depth=N` (the v0.3
//! F2 cross-artifact extension). Output is an ASCII tree suitable
//! for piping into `claude code -- ...`.

use anyhow::{anyhow, Context, Result};

pub async fn run(
    id: &str,
    kb: Option<&str>,
    depth: u32,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    let kb = kb.ok_or_else(|| anyhow!("--kb is required"))?;
    let url = format!(
        "{base}/api/kb/{}/graph/{}?depth={depth}",
        crate::http::encode_path_segment(kb),
        crate::http::encode_path_segment(id),
    );

    let mut builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10));
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
    print_ascii(&resp);
    Ok(())
}

fn print_ascii(graph: &serde_json::Value) {
    let id = graph["artifact_id"].as_str().unwrap_or("?");
    println!("{id}");
    let edges = graph["edges"].as_array().cloned().unwrap_or_default();
    if edges.is_empty() {
        println!("  (no outbound links)");
        return;
    }
    let last = edges.len() - 1;
    for (i, e) in edges.iter().enumerate() {
        let connector = if i == last { " └ " } else { " ├ " };
        let from = e["from"].as_str().unwrap_or("?");
        let to = e["to"].as_str().unwrap_or("?");
        let kind = e["kind"].as_str().unwrap_or("?");
        let depth = e["depth"].as_u64().unwrap_or(0);
        println!("{connector}{from} → {to} (kind={kind}, depth={depth})");
    }
}
