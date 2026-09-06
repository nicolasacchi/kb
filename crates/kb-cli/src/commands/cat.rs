//! `kb cat <id> [--kb] [--offline] [--daemon URL] [--no-record|--record]`
//! — dump an artifact's HTML to stdout.
//!
//! GC-B5 (roadmap G17) ports the `search`/`download` daemon-first ladder
//! here: try the daemon first (`GET /api/kb/{kb}/artifact/{id}`, the same
//! route `kb get --format html` and `kb download` use), fall back to a
//! read-only local lance open if the daemon isn't reachable. `--offline`
//! forces the local path; `--daemon URL` forces HTTP. Before this fix,
//! `cat` was local-state-only and silently couldn't see anything served
//! by a daemon with its own (e.g. docker-bind-mounted) state dir — see
//! `docs/research/kb-cat-read-daemon-resolution-defect-2026-07.html`.
//!
//! A daemon-backed read also records a `history` "open" row tagged
//! `source: "cli"` (default on; `--no-record` opts out) — agent reads
//! were previously invisible to read-state. `--record` opts a
//! local-fs-only read INTO recording too, but requires a reachable daemon
//! to store it in; with none reachable it errors rather than silently
//! doing nothing.

use super::{load_config_or_default, resolve_config_path};
use crate::http;
use anyhow::{anyhow, Context, Result};
use kb_core::paths::KbPaths;
use kb_core::storage::lance::Storage;
use kb_core::types::KbName;
use std::io::Write;
use std::path::PathBuf;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    config_path: Option<&PathBuf>,
    id: &str,
    kb: Option<&str>,
    offline: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
    no_record: bool,
    record: bool,
) -> Result<()> {
    if !offline {
        if let Some(url) = http::detect_daemon(daemon, bearer).await {
            return cat_via_daemon(&url, id, kb, bearer, !no_record).await;
        }
        if let Some(forced) = daemon {
            return Err(anyhow!("daemon at {forced} not reachable"));
        }
        eprintln!("daemon not reachable — falling back to offline lance read");
    }

    if record {
        // `--record` on an offline-served read: the bytes come from local
        // lance, but recording still needs a daemon to POST to. Try once
        // more (an explicit `--offline` skips the ladder above entirely)
        // and error politely rather than silently dropping the request.
        match http::detect_daemon(daemon, bearer).await {
            Some(url) => {
                let kb_name = http::resolve_default_kb(kb, Some(url.as_str()), bearer).await?;
                http::record_open_cli(&url, &kb_name, id, bearer)
                    .await
                    .map_err(|e| anyhow!("--record: failed to store history entry: {e}"))?;
            }
            None => {
                return Err(anyhow!(
                    "--record requires a reachable daemon to store the history entry, \
                     and none was found; drop --record or start the daemon"
                ));
            }
        }
    }

    cat_offline(config_path, id, kb).await
}

async fn cat_via_daemon(
    base: &str,
    id: &str,
    kb: Option<&str>,
    bearer: Option<&str>,
    record: bool,
) -> Result<()> {
    let kb_name = http::resolve_default_kb(kb, Some(base), bearer).await?;
    let bytes = fetch_artifact_bytes(base, &kb_name, id, bearer).await?;
    std::io::stdout().write_all(&bytes)?;

    if record {
        // Best-effort: the default-on path shouldn't fail `cat` over a
        // telemetry write. `--no-record` skips this branch entirely.
        if let Err(e) = http::record_open_cli(base, &kb_name, id, bearer).await {
            eprintln!("kb cat: warning: history record failed: {e}");
        }
    }
    Ok(())
}

/// GET `/api/kb/{kb}/artifact/{id}`'s raw bytes — split out of
/// `cat_via_daemon` (CT-B4) so `kb memory expand` can fetch a highlight's
/// origin artifact the SAME way `kb cat` does (same route, same error
/// shape), rather than a second HTTP call site drifting from this one.
/// `cat_via_daemon`'s own behaviour is unchanged — this is exactly what it
/// did inline before the split.
pub(crate) async fn fetch_artifact_bytes(
    base: &str,
    kb_name: &str,
    id: &str,
    bearer: Option<&str>,
) -> Result<bytes::Bytes> {
    let url = format!(
        "{}/api/kb/{}/artifact/{}",
        base.trim_end_matches('/'),
        http::encode_path_segment(kb_name),
        http::encode_path_segment(id),
    );
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_string))
            .unwrap_or(text);
        return Err(anyhow!(
            "artifact {id} not found in kb {kb_name} via daemon {base}: HTTP {status} — {detail}"
        ));
    }
    Ok(resp.bytes().await?)
}

async fn cat_offline(config_path: Option<&PathBuf>, id: &str, kb: Option<&str>) -> Result<()> {
    let cfg_path = resolve_config_path(config_path)?;
    let cfg = load_config_or_default(&cfg_path)?;

    if cfg.kb.is_empty() {
        return Err(anyhow!(
            "no kb configured in {} and no daemon was reachable — pass --kb and start \
             the daemon (it resolves ids over HTTP), or point --config at a kb.toml \
             with a local lance state dir",
            cfg_path.display()
        ));
    }

    let kb_name = match kb {
        Some(k) => KbName::new(k).map_err(|e| anyhow!("invalid kb {k:?}: {e}"))?,
        None if cfg.kb.len() == 1 => cfg.kb.keys().next().unwrap().clone(),
        None => {
            let names: Vec<String> = cfg.kb.keys().map(|k| k.to_string()).collect();
            return Err(anyhow!(
                "must specify --kb when multiple kbs are configured locally ({})",
                names.join(", ")
            ));
        }
    };

    let paths = KbPaths::new(cfg.daemon.name.as_deref().unwrap_or("default"))?;
    let config_dim = cfg
        .kb
        .get(&kb_name)
        .and_then(|s| s.embedding_model.as_deref())
        .and_then(kb_core::embed::model_info)
        .map(|m| m.dim as i32);
    let storage = Storage::open(&paths.kb_lance(&kb_name), config_dim).await?;

    // Path-based ids are hex hashes — poor BM25 tokens — so resolve the
    // row directly rather than searching for the id and post-filtering.
    let row = storage.get_by_id(id).await?.ok_or_else(|| {
        anyhow!(
            "artifact {id} not found in LOCAL lance state for kb {kb_name} \
             ({}); the daemon wasn't consulted — pass --daemon or check kb.toml",
            paths.kb_lance(&kb_name).display()
        )
    })?;

    let bytes = std::fs::read(&row.path)?;
    std::io::stdout().write_all(&bytes)?;
    Ok(())
}
