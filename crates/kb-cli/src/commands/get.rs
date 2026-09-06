//! `kb get <id> [--kb] [--format json|md|html] [--no-record]` —
//! single-artifact lookup via the daemon. v0.4 D2 —
//! Claude-Code-friendly retrieval verb that pairs with `kb search`
//! (`kb search` → pick id → `kb get <id>`).
//!
//! GC-F1: like `kb cat`/`kb read`, a successful get records a
//! `history` "open" row tagged `source: "cli"` (default on;
//! `--no-record` opts out). `get` is purely daemon-backed — there is
//! no offline path — so recording needs no `--record` opt-in
//! counterpart; the write is best-effort and never fails the read.

use anyhow::{anyhow, Context, Result};

pub async fn run(
    id: &str,
    kb: Option<&str>,
    format: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
    no_record: bool,
) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/');
    let kb = kb.ok_or_else(|| anyhow!("--kb is required"))?;
    match format {
        "json" => fetch_metadata_json(base, kb, id, bearer).await?,
        "md" => fetch_metadata_md(base, kb, id, bearer).await?,
        "html" => fetch_html(base, kb, id, bearer).await?,
        other => {
            return Err(anyhow!(
                "unknown --format {other:?}; expected one of: json, md, html"
            ))
        }
    }

    if !no_record {
        // Best-effort: the default-on path shouldn't fail `get` over a
        // telemetry write. `--no-record` skips this branch entirely.
        if let Err(e) = crate::http::record_open_cli(base, kb, id, bearer).await {
            eprintln!("kb get: warning: history record failed: {e}");
        }
    }
    Ok(())
}

fn client(bearer: Option<&str>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10));
    if let Some(token) = bearer {
        let mut headers = reqwest::header::HeaderMap::new();
        let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .context("bearer token contained an invalid header byte")?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
        builder = builder.default_headers(headers);
    }
    builder.build().context("reqwest client")
}

async fn fetch_metadata_json(base: &str, kb: &str, id: &str, bearer: Option<&str>) -> Result<()> {
    let url = format!(
        "{base}/api/kb/{}/docs/{}",
        crate::http::encode_path_segment(kb),
        crate::http::encode_path_segment(id),
    );
    let resp = client(bearer)?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    let body: serde_json::Value = resp.json().await?;
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

async fn fetch_metadata_md(base: &str, kb: &str, id: &str, bearer: Option<&str>) -> Result<()> {
    let url = format!(
        "{base}/api/kb/{}/docs/{}",
        crate::http::encode_path_segment(kb),
        crate::http::encode_path_segment(id),
    );
    let body: serde_json::Value = client(bearer)?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;
    let title = body["title"].as_str().unwrap_or("(untitled)");
    let path = body["path"].as_str().unwrap_or("?");
    let category = body["kb_category"].as_str().unwrap_or("-");
    println!("# {title}\n\n- id: `{id}`\n- kb: `{kb}`\n- path: `{path}`\n- category: {category}\n");
    Ok(())
}

async fn fetch_html(base: &str, kb: &str, id: &str, bearer: Option<&str>) -> Result<()> {
    let url = format!(
        "{base}/api/kb/{}/artifact/{}",
        crate::http::encode_path_segment(kb),
        crate::http::encode_path_segment(id),
    );
    let body = client(bearer)?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .text()
        .await?;
    print!("{body}");
    Ok(())
}
