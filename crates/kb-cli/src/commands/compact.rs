//! `kb compact [--kb NAME] [--daemon URL] [--json]` — operator-triggered
//! lance maintenance pass. Calls `POST /api/kb/{kb}/compact`, which
//! runs `Table::optimize(OptimizeAction::All)` through the storage
//! actor: merges small data fragments, rebuilds indices over new
//! rows, and prunes manifest versions old enough to drop.
//!
//! Reach for this when search latency has crept up on a kb that's
//! been reindexed many times — every reindex commits N fragments and
//! N manifest versions, so a few hundred docs across a few reindex
//! cycles can shred the dataset into thousands of fragments. The
//! daemon's startup heuristic catches this automatically on restart;
//! the manual lever is here for when you don't want to bounce the
//! daemon.
//!
//! Synchronous: the HTTP call blocks until lance finishes the
//! compaction + index optimize + version prune pass. Increase the
//! client timeout (`--timeout-seconds` via reqwest default is plenty
//! for tens of thousands of fragments; we set it generous here).

use anyhow::{Context, Result};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

pub async fn run(
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let resolved_kb = crate::http::resolve_default_kb(kb, daemon, bearer).await?;
    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let url = format!(
        "{base}/api/kb/{}/compact",
        crate::http::encode_path_segment(&resolved_kb),
    );
    // Generous timeout — a badly fragmented dataset (thousands of small
    // fragments) can take minutes to compact. 600s lets the worst case
    // through; healthy kbs return in well under a second.
    let client = crate::http::client_with_timeout_and_bearer(600, bearer)?;
    let resp = client
        .post(&url)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("compact {url}: HTTP {status} — {body}");
    }
    let body: serde_json::Value = resp.json().await.context("decode compact response")?;
    if json {
        println!("{body}");
        return Ok(());
    }
    let ms = body.get("ms").and_then(|v| v.as_u64()).unwrap_or(0);
    let stats = body
        .get("stats")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let frag_removed = stats
        .get("fragments_removed")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let frag_added = stats
        .get("fragments_added")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let files_removed = stats
        .get("files_removed")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let old_versions = stats
        .get("old_versions_removed")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let bytes_pruned = stats
        .get("bytes_pruned")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    eprintln!("✓ compact kb {resolved_kb} in {ms} ms");
    eprintln!("  fragments: {frag_removed} → {frag_added} (files removed: {files_removed})");
    eprintln!("  versions pruned: {old_versions} ({bytes_pruned} bytes freed)");
    Ok(())
}
