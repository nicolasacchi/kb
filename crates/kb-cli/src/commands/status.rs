//! `kb status` — observability snapshot.
//!
//! Prefers the daemon's `GET /api/stats` when reachable (with bearer), so a
//! host CLI pointed at a dockerised / remote daemon lists the live kbs even
//! when this machine has no `kb.toml`. Falls back to reading each kb's
//! `index.db` from local config when the daemon is down.
//!
//! `--watch N` re-runs the sweep every N seconds with an ANSI clear
//! between renders. Same convention as `kb daemon doctor --watch`;
//! mutually exclusive with `--json` (scripted callers want one-shot).

use super::{load_config_or_default, resolve_config_path};
use anyhow::{anyhow, Result};
use kb_core::paths::KbPaths;
use kb_core::storage::sqlite::Db;
use serde_json::json;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

/// R1 — per-kb snapshot fetched from `/api/stats`. Keyed by kb name.
/// Empty when the daemon is unreachable; render falls back to sqlite-only.
type StatsMap = HashMap<String, serde_json::Value>;

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

fn daemon_base(explicit: Option<&str>) -> String {
    explicit
        .map(str::to_string)
        .or_else(|| {
            std::env::var("KB_DAEMON_URL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_DAEMON.to_string())
        .trim_end_matches('/')
        .to_string()
}

pub async fn run(
    config_path: Option<&PathBuf>,
    json_out: bool,
    watch_secs: Option<u64>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let cfg_path = resolve_config_path(config_path)?;
    let cfg = load_config_or_default(&cfg_path)?;
    let paths = KbPaths::new(cfg.daemon.name.as_deref().unwrap_or("default"))?;
    let base = daemon_base(daemon);

    if let Some(secs) = watch_secs {
        if json_out {
            return Err(anyhow!(
                "--watch and --json are mutually exclusive — --json is one-shot"
            ));
        }
        let interval = std::time::Duration::from_secs(secs.max(1));
        loop {
            print!("\x1b[2J\x1b[H");
            let cfg = load_config_or_default(&cfg_path)?;
            let paths = KbPaths::new(cfg.daemon.name.as_deref().unwrap_or("default"))?;
            let (stats, reachable) = fetch_stats(&base, bearer).await;
            print_human(&cfg, &paths, &cfg_path, &base, reachable, &stats);
            println!(
                "\n  (watch · refresh {}s · Ctrl+C to exit)",
                interval.as_secs()
            );
            tokio::time::sleep(interval).await;
        }
    }

    let (stats, reachable) = fetch_stats(&base, bearer).await;
    if json_out {
        return run_json(&cfg, &paths, &cfg_path, &base, reachable, &stats);
    }
    print_human(&cfg, &paths, &cfg_path, &base, reachable, &stats);
    Ok(())
}

fn kb_names(cfg: &kb_core::config::KbConfig, stats: &StatsMap) -> Vec<String> {
    let mut names = BTreeSet::new();
    for k in cfg.kb.keys() {
        names.insert(k.as_str().to_string());
    }
    for k in stats.keys() {
        names.insert(k.clone());
    }
    names.into_iter().collect()
}

fn print_human(
    cfg: &kb_core::config::KbConfig,
    paths: &KbPaths,
    cfg_path: &std::path::Path,
    daemon_url: &str,
    daemon_reachable: bool,
    stats: &StatsMap,
) {
    println!("daemon name: {}", paths.daemon_name);
    println!("state dir:   {}", paths.state.display());
    println!("config:      {}", cfg_path.display());
    if daemon_reachable {
        println!("daemon:      {daemon_url}");
    }

    let names = kb_names(cfg, stats);
    if names.is_empty() {
        println!("\n(no kbs configured — `kb add <path>` to register one)");
        return;
    }

    println!("\nkbs:");
    for kb_name in &names {
        println!("  {kb_name}");
        let sqlite_path = cfg
            .kb
            .keys()
            .find(|k| k.as_str() == kb_name)
            .cloned()
            .or_else(|| kb_core::types::KbName::new(kb_name.clone()).ok())
            .map(|k| paths.kb_sqlite(&k));

        let printed_sqlite = sqlite_path
            .as_ref()
            .map(|p| print_sqlite_block(p))
            .unwrap_or(false);
        if let Some(rec) = stats.get(kb_name) {
            if !printed_sqlite {
                let docs = rec.get("doc_count").and_then(|v| v.as_u64()).unwrap_or(0);
                let errs = rec.get("open_errors").and_then(|v| v.as_u64()).unwrap_or(0);
                println!("    docs: {docs}  open errors: {errs}");
            }
            println!("    {}", format_reconcile_line(rec));
        } else if !printed_sqlite {
            println!("    (not yet indexed — index.db absent)");
        }
    }
}

/// Returns true if sqlite details were printed.
fn print_sqlite_block(sqlite_path: &std::path::Path) -> bool {
    if !sqlite_path.exists() {
        return false;
    }
    let db = match Db::open(sqlite_path) {
        Ok(db) => db,
        Err(e) => {
            println!("    (open failed: {e})");
            return true;
        }
    };
    let sources = db.list_sources().unwrap_or_default();
    let errors = db.list_open_errors().unwrap_or_default();
    for src in &sources {
        let last = db
            .last_run_for_source(&kb_core::ids::SourceSlug::from_path(&src.path))
            .ok()
            .flatten();
        let last_ts = last
            .as_ref()
            .and_then(|r| r.finished_at_unix)
            .map(format_unix)
            .unwrap_or_else(|| "(no completed run)".to_string());
        let last_ok = last.as_ref().map(|r| r.ok_count).unwrap_or(0);
        let last_err = last.as_ref().map(|r| r.err_count).unwrap_or(0);
        let paused_marker = if src.paused { " [paused]" } else { "" };
        println!(
            "    source: {} → {}{paused_marker}",
            src.raw_slug,
            src.path.display()
        );
        println!("      last run: {last_ts}  (ok={last_ok}  err={last_err})");
    }
    if !errors.is_empty() {
        println!("    open errors: {}", errors.len());
        for err in errors.iter().take(5) {
            println!(
                "      [{}]  {}  {}",
                err.kind,
                err.path.display(),
                err.message.lines().next().unwrap_or(&err.message)
            );
        }
        if errors.len() > 5 {
            println!("      ... and {} more", errors.len() - 5);
        }
    }
    true
}

/// Try `/api/stats` with bearer. Returns (map, reachable).
async fn fetch_stats(base: &str, bearer: Option<&str>) -> (StatsMap, bool) {
    let client = match crate::http::client_with_timeout_and_bearer(5, bearer) {
        Ok(c) => c,
        Err(_) => return (StatsMap::new(), false),
    };
    let url = format!("{base}/api/stats");
    let resp = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return (StatsMap::new(), false),
    };
    let body: serde_json::Value = match resp.json().await {
        Ok(b) => b,
        Err(_) => return (StatsMap::new(), false),
    };
    let mut out = StatsMap::new();
    if let Some(kbs) = body.get("kbs").and_then(|v| v.as_array()) {
        for kb in kbs {
            if let Some(name) = kb.get("name").and_then(|v| v.as_str()) {
                out.insert(name.to_string(), kb.clone());
            }
        }
    }
    (out, true)
}

/// Format one kb's reconcile line: timestamp + (+files −deletes) +
/// staleness warning when `last_reconcile_at` is older than 5× the
/// configured `reconcile_secs`.
fn format_reconcile_line(kb: &serde_json::Value) -> String {
    let secs = kb
        .get("reconcile_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if secs == 0 {
        return "reconcile: disabled (reconcile_secs=0)".to_string();
    }
    let at = kb.get("last_reconcile_at").and_then(|v| v.as_i64());
    let files = kb
        .get("last_reconcile_files")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let deletes = kb
        .get("last_reconcile_deletes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let dur = kb
        .get("last_reconcile_duration_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    match at {
        None => format!("reconcile: pending (interval {secs}s)"),
        Some(t) => {
            let stale = is_stale(t, secs);
            let marker = if stale { " STALE" } else { "" };
            format!(
                "reconcile: {} (files={} deletes={} {}ms interval={}s){marker}",
                format_unix(t),
                files,
                deletes,
                dur,
                secs,
            )
        }
    }
}

/// `last_reconcile_at` is stale when it's older than 5× the configured
/// interval. Indicates the reconciler hasn't run on schedule (daemon
/// hung, crashed task, or a too-slow pass).
fn is_stale(last_reconcile_at: i64, secs: u64) -> bool {
    let now = chrono::Utc::now().timestamp();
    now - last_reconcile_at > (secs as i64) * 5
}

fn run_json(
    cfg: &kb_core::config::KbConfig,
    paths: &KbPaths,
    cfg_path: &std::path::Path,
    daemon_url: &str,
    daemon_reachable: bool,
    stats: &StatsMap,
) -> Result<()> {
    let names = kb_names(cfg, stats);
    let mut kbs = Vec::new();
    for kb_name in &names {
        let sqlite_path = cfg
            .kb
            .keys()
            .find(|k| k.as_str() == kb_name)
            .map(|k| paths.kb_sqlite(k));
        let mut kb_obj = match sqlite_path {
            Some(p) if p.exists() => sqlite_json(&p, kb_name),
            Some(_) => json!({
                "name": kb_name,
                "indexed": false,
            }),
            None => json!({
                "name": kb_name,
                "indexed": stats.contains_key(kb_name),
            }),
        };
        if let Some(rec) = stats.get(kb_name) {
            kb_obj["indexed"] = json!(true);
            for key in [
                "doc_count",
                "open_errors",
                "last_index_at",
                "last_reconcile_at",
                "last_reconcile_files",
                "last_reconcile_deletes",
                "last_reconcile_duration_ms",
                "reconcile_secs",
                "decode_skips",
            ] {
                if let Some(v) = rec.get(key) {
                    // Don't overwrite a sqlite-sourced open_errors *array*
                    // with the daemon's numeric count.
                    if key == "open_errors"
                        && kb_obj.get(key).map(|x| x.is_array()).unwrap_or(false)
                    {
                        kb_obj["open_error_count"] = v.clone();
                        continue;
                    }
                    kb_obj[key] = v.clone();
                }
            }
        }
        kbs.push(kb_obj);
    }
    let value = json!({
        "daemon_name": paths.daemon_name,
        "state_dir": paths.state,
        "config_path": cfg_path,
        "daemon_url": daemon_url,
        "daemon_reachable": daemon_reachable,
        "kbs": kbs,
    });
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn sqlite_json(sqlite_path: &std::path::Path, kb_name: &str) -> serde_json::Value {
    let db = match Db::open(sqlite_path) {
        Ok(db) => db,
        Err(e) => {
            return json!({
                "name": kb_name,
                "open_error": e.to_string(),
            });
        }
    };
    let sources: Vec<_> = db
        .list_sources()
        .unwrap_or_default()
        .into_iter()
        .map(|src| {
            let last = db
                .last_run_for_source(&kb_core::ids::SourceSlug::from_path(&src.path))
                .ok()
                .flatten();
            json!({
                "slug": src.raw_slug,
                "path": src.path,
                "paused": src.paused,
                "last_run_finished_at_unix": last.as_ref().and_then(|r| r.finished_at_unix),
                "last_run_ok_count": last.as_ref().map(|r| r.ok_count),
                "last_run_err_count": last.as_ref().map(|r| r.err_count),
            })
        })
        .collect();
    let errors: Vec<_> = db
        .list_open_errors()
        .unwrap_or_default()
        .into_iter()
        .map(|e| {
            json!({
                "id": e.id,
                "kind": e.kind,
                "path": e.path,
                "message": e.message,
            })
        })
        .collect();
    json!({
        "name": kb_name,
        "indexed": true,
        "sources": sources,
        "open_errors": errors,
    })
}

fn format_unix(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_else(|| ts.to_string())
}
