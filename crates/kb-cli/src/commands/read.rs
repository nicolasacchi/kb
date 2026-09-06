//! `kb read <id> [--kb] [--offline] [--daemon URL] [--no-record|--record]`
//! — open an artifact in the system browser.
//!
//! GC-B5 (roadmap G17) ports the `search`/`download` daemon-first ladder
//! here, same as `cat`. In daemon mode the served SPA permalink
//! (`/a/{kb}/{source_relative}`, from `GET /api/kb/{kb}/docs/{id}`) is
//! opened rather than the local file path — the daemon's state dir may
//! not even be on this host (docker / shared-daemon topology). `--offline`
//! forces the previous local-path behaviour (unchanged); `--daemon URL`
//! forces HTTP. See
//! `docs/research/kb-cat-read-daemon-resolution-defect-2026-07.html`.
//!
//! Recording follows `cat`: a daemon-backed read records a `history`
//! "open" row tagged `source: "cli"` by default (`--no-record` opts out);
//! `--record` opts a local-fs-only read into recording too, erroring if no
//! daemon is reachable to store it.
//!
//! `KB_OPENER` env var overrides the spawned opener (`xdg-open`) —
//! a test-only seam so integration tests can point it at a recorder
//! script instead of actually launching a browser.

use super::{load_config_or_default, resolve_config_path};
use crate::http;
use anyhow::{anyhow, Context, Result};
use kb_core::paths::KbPaths;
use kb_core::storage::lance::Storage;
use kb_core::types::KbName;
use std::path::PathBuf;
use std::process::Command;

/// `KB_OPENER` overrides the platform opener — test-only seam so the
/// integration suite can point this at a harmless recorder script instead
/// of actually spawning a browser.
fn opener() -> String {
    if let Ok(over) = std::env::var("KB_OPENER") {
        if !over.is_empty() {
            return over;
        }
    }
    "xdg-open".to_string()
}

fn open_url_or_path(target: &str) -> Result<()> {
    let opener = opener();
    let status = Command::new(&opener).arg(target).status()?;
    if !status.success() {
        return Err(anyhow!("{opener} returned exit code {status}"));
    }
    Ok(())
}

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
            return read_via_daemon(&url, id, kb, bearer, !no_record).await;
        }
        if let Some(forced) = daemon {
            return Err(anyhow!("daemon at {forced} not reachable"));
        }
        eprintln!("daemon not reachable — falling back to offline lance read");
    }

    if record {
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

    read_offline(config_path, id, kb).await
}

async fn read_via_daemon(
    base: &str,
    id: &str,
    kb: Option<&str>,
    bearer: Option<&str>,
    record: bool,
) -> Result<()> {
    let kb_name = http::resolve_default_kb(kb, Some(base), bearer).await?;
    let base = base.trim_end_matches('/');
    let meta_url = format!(
        "{base}/api/kb/{}/docs/{}",
        http::encode_path_segment(&kb_name),
        http::encode_path_segment(id),
    );
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let resp = client
        .get(&meta_url)
        .send()
        .await
        .with_context(|| format!("GET {meta_url}"))?;
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
    let body: serde_json::Value = resp.json().await?;
    let source_relative = body
        .get("source_relative")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("daemon response for {id} missing source_relative"))?;
    // Encode each path segment individually — `encode_path_segment` would
    // otherwise escape the `/`s inside a subfolder path.
    let encoded_rel = source_relative
        .split('/')
        .map(http::encode_path_segment)
        .collect::<Vec<_>>()
        .join("/");
    let open_url = format!(
        "{base}/a/{}/{encoded_rel}",
        http::encode_path_segment(&kb_name),
    );

    open_url_or_path(&open_url)?;

    if record {
        if let Err(e) = http::record_open_cli(base, &kb_name, id, bearer).await {
            eprintln!("kb read: warning: history record failed: {e}");
        }
    }
    Ok(())
}

async fn read_offline(config_path: Option<&PathBuf>, id: &str, kb: Option<&str>) -> Result<()> {
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
    let row = storage.get_by_id(id).await?.ok_or_else(|| {
        anyhow!(
            "artifact {id} not found in LOCAL lance state for kb {kb_name} \
             ({}); the daemon wasn't consulted — pass --daemon or check kb.toml",
            paths.kb_lance(&kb_name).display()
        )
    })?;

    open_url_or_path(&row.path)
}
