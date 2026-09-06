//! `kb pause [--kb NAME]` / `kb resume [--kb NAME]` — flip a kb source's
//! paused flag via `POST /api/kb/{kb}/sources/{src}/{pause|resume}` (X3;
//! the CLI mirror of the SPA's pause toggle).
//!
//! D6 (v0.24 behavior change): `paused` is now ENFORCED at the ingest gate
//! — a paused source genuinely stops indexing (watcher, reconcile walk,
//! reindex nudges) until resumed, and can go stale by design. `kb status`
//! surfaces the `[paused]` marker per source.
//!
//! v0.0.1 has one source per kb, so the slug is auto-resolved from
//! `GET /sources` rather than taken as an argument.

use anyhow::{Context, Result};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

pub async fn set_paused(
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    paused: bool,
    json: bool,
) -> Result<()> {
    let resolved_kb = crate::http::resolve_default_kb(kb, daemon, bearer).await?;
    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let kb_seg = crate::http::encode_path_segment(&resolved_kb);
    let client = crate::http::client_with_timeout_and_bearer(15, bearer)?;

    // One source per kb today — resolve its slug from the daemon.
    let sources = crate::http::send_json(
        client.get(format!("{base}/api/kb/{kb_seg}/sources")),
        "list sources",
    )
    .await?;
    let slug = sources
        .as_array()
        .and_then(|a| a.first())
        .and_then(|s| s["slug"].as_str())
        .context("kb has no sources")?
        .to_string();

    let verb = if paused { "pause" } else { "resume" };
    let url = format!(
        "{base}/api/kb/{kb_seg}/sources/{}/{verb}",
        crate::http::encode_path_segment(&slug),
    );
    let body = crate::http::send_json(client.post(&url), verb).await?;
    if json {
        println!("{body}");
        return Ok(());
    }
    if paused {
        eprintln!("✓ paused source {slug} in kb {resolved_kb} — ingest gated until `kb resume`");
    } else {
        eprintln!("✓ resumed source {slug} in kb {resolved_kb} — ingest re-enabled");
    }
    Ok(())
}
