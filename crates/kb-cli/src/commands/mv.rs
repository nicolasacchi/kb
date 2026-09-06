//! `kb mv <target> <new-path> [--kb NAME] [--json] [--daemon URL]`
//!
//! Move/rename an artifact (or an entire indexed folder prefix) via the
//! daemon's relocate routes:
//!
//! - Resolve `<target>` through `GET /api/kb/{kb}/lookup` (12-hex id,
//!   source-rel, unique filename).
//! - If that misses but `<target>` names an indexed folder prefix
//!   (`/docs?folder=` returns rows), treat as folder rename →
//!   `POST /folders/rename`.
//! - Ambiguity (artifact AND folder both match) → error; a trailing `/`
//!   forces folder mode.
//!
//! Prints `moved <old_rel> -> <new_rel> (id <old_id> -> <new_id>)`; folder
//! mode prints one line per item plus a summary count.

use anyhow::{Context, Result};
use serde_json::Value;

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

pub async fn run(
    target: &str,
    new_path: &str,
    kb: Option<&str>,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let resolved_kb = crate::http::resolve_default_kb(kb, daemon, bearer).await?;
    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let force_folder = target.ends_with('/');
    let target_trim = target.trim_end_matches('/');
    let new_trim = new_path.trim_end_matches('/');

    if force_folder {
        return run_folder(base, &resolved_kb, target_trim, new_trim, bearer, json).await;
    }

    let lookup = crate::http::get_lookup(daemon, &resolved_kb, target_trim, bearer).await?;
    let kind = lookup
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    match kind {
        "exact" | "unique_suffix" => {
            let id = lookup
                .get("id")
                .and_then(|v| v.as_str())
                .context("lookup response missing `id`")?;
            // Ambiguity check: same path is also a folder prefix.
            if folder_has_docs(base, &resolved_kb, target_trim, bearer).await? {
                anyhow::bail!(
                    "{target_trim:?} matches both an artifact and a folder prefix in {resolved_kb}; \
                     pass a trailing slash to force folder rename, or use the 12-hex id for the artifact"
                );
            }
            run_doc(base, &resolved_kb, id, new_trim, bearer, json).await
        }
        "ambiguous" => {
            let candidates = lookup
                .get("candidates")
                .and_then(|v| v.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            let mut msg = format!(
                "{target_trim:?} matched {} artifacts in {resolved_kb}; pick one:\n",
                candidates.len()
            );
            for c in candidates {
                let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let rel = c
                    .get("source_relative")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                msg.push_str(&format!("  {id}  {rel}\n"));
            }
            anyhow::bail!(msg)
        }
        "not_found" => {
            if folder_has_docs(base, &resolved_kb, target_trim, bearer).await? {
                return run_folder(base, &resolved_kb, target_trim, new_trim, bearer, json).await;
            }
            anyhow::bail!("no match: {target_trim:?} (kb={resolved_kb})")
        }
        other => anyhow::bail!("lookup returned unknown kind {other:?}: {lookup}"),
    }
}

async fn run_doc(
    base: &str,
    kb: &str,
    id: &str,
    new_path: &str,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = format!(
        "{base}/api/kb/{}/docs/{}/move",
        crate::http::encode_path_segment(kb),
        crate::http::encode_path_segment(id),
    );
    let client = crate::http::client_with_timeout_and_bearer(30, bearer)?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "to": new_path }))
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(serde_json::json!({}));
    if !status.is_success() {
        let detail = body
            .get("detail")
            .and_then(|d| d.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| body.to_string());
        anyhow::bail!("move failed (HTTP {status}): {detail}");
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        println!(
            "moved {} -> {} (id {} -> {})",
            body["old_source_rel"].as_str().unwrap_or("?"),
            body["new_source_rel"].as_str().unwrap_or("?"),
            body["old_id"].as_str().unwrap_or("?"),
            body["new_id"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

async fn run_folder(
    base: &str,
    kb: &str,
    from: &str,
    to: &str,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = format!(
        "{base}/api/kb/{}/folders/rename",
        crate::http::encode_path_segment(kb),
    );
    let client = crate::http::client_with_timeout_and_bearer(60, bearer)?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "from": from, "to": to }))
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(serde_json::json!({}));
    if !status.is_success() {
        let detail = body
            .get("detail")
            .and_then(|d| d.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| body.to_string());
        anyhow::bail!("folder rename failed (HTTP {status}): {detail}");
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let moved = body
        .get("moved")
        .and_then(|m| m.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    for item in moved {
        println!(
            "moved {} -> {} (id {} -> {})",
            item["old_source_rel"].as_str().unwrap_or("?"),
            item["new_source_rel"].as_str().unwrap_or("?"),
            item["old_id"].as_str().unwrap_or("?"),
            item["new_id"].as_str().unwrap_or("?"),
        );
    }
    println!(
        "renamed folder {from} -> {to} ({n} artifacts)",
        n = moved.len()
    );
    Ok(())
}

/// True when `/docs?folder=` returns at least one doc under `folder`.
async fn folder_has_docs(base: &str, kb: &str, folder: &str, bearer: Option<&str>) -> Result<bool> {
    let url = format!(
        "{base}/api/kb/{}/docs",
        crate::http::encode_path_segment(kb),
    );
    let client = crate::http::client_with_timeout_and_bearer(10, bearer)?;
    let resp = client
        .get(&url)
        .query(&[("folder", folder), ("envelope", "1"), ("limit", "1")])
        .send()
        .await
        .with_context(|| format!("GET {url}?folder={folder}"))?;
    if !resp.status().is_success() {
        return Ok(false);
    }
    let body: Value = resp.json().await.unwrap_or(serde_json::json!({}));
    // Envelope mode: {docs, total, …}; legacy bare array also accepted.
    if let Some(total) = body.get("total").and_then(|t| t.as_u64()) {
        return Ok(total > 0);
    }
    if let Some(docs) = body.get("docs").and_then(|d| d.as_array()) {
        return Ok(!docs.is_empty());
    }
    if let Some(arr) = body.as_array() {
        return Ok(!arr.is_empty());
    }
    Ok(false)
}
