//! `kb reindex [--kb NAME] [--daemon URL] [--json] [--re-embed]` — force the
//! daemon to re-walk a kb's source folder and re-emit `watch.modify` for
//! every HTML file (`force=true`, bypassing the indexer's content-hash dedup
//! gate). The fast path the user reaches for when the SPA / popover is
//! missing files — usually because inotify dropped events under a
//! burst and the next reconciler tick hasn't run yet.
//!
//! Embeddings are reused unless `--re-embed` is set, which POSTs
//! `?re_embed=true`. Without the flag the request has no query and no body.
//!
//! Synchronous: POST returns when the work is queued. Stream progress
//! with `kb push --filter index.complete`.

use anyhow::{Context, Result};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

pub async fn run(
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
    re_embed: bool,
) -> Result<()> {
    let resolved_kb = crate::http::resolve_default_kb(kb, daemon, bearer).await?;
    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let url = format!(
        "{base}/api/kb/{}/reindex",
        crate::http::encode_path_segment(&resolved_kb),
    );
    let client = crate::http::client_with_timeout_and_bearer(15, bearer)?;
    let mut req = client.post(&url);
    // Only the flag sends the hatch. An absent query keeps daemon reuse.
    if re_embed {
        req = req.query(&[("re_embed", "true")]);
    }
    let resp = req.send().await.with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("reindex {url}: HTTP {status} — {body}");
    }
    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "kb": resolved_kb,
            })
        );
    } else {
        eprintln!("✓ reindex queued for kb {resolved_kb}");
        eprintln!("  watch progress with: kb push --filter artifact.indexed");
    }
    Ok(())
}
