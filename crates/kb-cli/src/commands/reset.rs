//! `kb reset --kb <NAME> [--yes] [--all]` — wipe a kb's index state
//! (Lance dataset + SQLite index db) so the next daemon start
//! reindexes from scratch. Comments under `<state>/<kb>/.review/`
//! are preserved by default; pass `--all` to drop them too.
//!
//! The daemon must be stopped first (best-effort detected via a probe
//! to the configured listen address). The command refuses to run when
//! it sees the daemon up, unless `--yes --force` is given.

use super::{load_config_or_default, resolve_config_path};
use anyhow::{anyhow, bail, Result};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

pub fn run(
    config_path: Option<&PathBuf>,
    kb: &str,
    yes: bool,
    all: bool,
    force: bool,
) -> Result<()> {
    let kb_name = KbName::new(kb).map_err(|e| anyhow!("invalid kb {kb:?}: {e}"))?;
    let cfg_path = resolve_config_path(config_path)?;
    let cfg = load_config_or_default(&cfg_path)?;
    let paths = KbPaths::new(cfg.daemon.name.as_deref().unwrap_or("default"))?;

    let kb_state = paths.kb_state(&kb_name);
    if !kb_state.exists() {
        println!("nothing to reset: {} does not exist", kb_state.display());
        return Ok(());
    }

    if daemon_appears_running(&cfg.server.addr) && !force {
        bail!(
            "daemon at {} appears to be running. Stop it before resetting (or pass --force to override; doing so on a live daemon will corrupt its open file handles).",
            cfg.server.addr
        );
    }

    let lance = paths.kb_lance(&kb_name);
    let sqlite = paths.kb_sqlite(&kb_name);
    let review = paths.kb_review_dir(&kb_name);

    let mut targets: Vec<PathBuf> = Vec::new();
    if lance.exists() {
        targets.push(lance.clone());
    }
    // SQLite WAL + shm sit alongside the db file.
    for suffix in ["", "-shm", "-wal"] {
        let p = if suffix.is_empty() {
            sqlite.clone()
        } else {
            PathBuf::from(format!("{}{suffix}", sqlite.display()))
        };
        if p.exists() {
            targets.push(p);
        }
    }
    if all && review.exists() {
        targets.push(review.clone());
    }

    if targets.is_empty() {
        println!("nothing to reset: state already empty");
        return Ok(());
    }

    println!("will remove:");
    for t in &targets {
        println!("  {}", t.display());
    }
    if !all && review.exists() {
        println!("preserving:");
        println!("  {} (pass --all to drop comments too)", review.display());
    }

    if !yes {
        eprint!("\nproceed? [y/N] ");
        io::stderr().flush().ok();
        let mut input = String::new();
        io::stdin().lock().read_line(&mut input).ok();
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("aborted.");
            return Ok(());
        }
    }

    for t in &targets {
        if t.is_dir() {
            std::fs::remove_dir_all(t)
                .map_err(|e| anyhow!("remove_dir_all {}: {e}", t.display()))?;
        } else {
            std::fs::remove_file(t).map_err(|e| anyhow!("remove_file {}: {e}", t.display()))?;
        }
    }

    println!("\nreset done. start the daemon to re-index from zero:");
    println!("  kb daemon --config {}", cfg_path.display());
    Ok(())
}

/// Quick TCP probe — if something is bound to `addr`, the daemon is
/// (very likely) up. False positives are possible if another process
/// holds the port; the caller can override with --force.
///
/// K9: uses `to_socket_addrs` so `localhost:4000` (hostname, not raw
/// IP) parses. Pre-fix `SocketAddr::parse` rejected hostname forms,
/// silently returning false — `kb reset` would happily wipe lance/
/// sqlite under a live daemon.
fn daemon_appears_running(addr: &str) -> bool {
    use std::net::ToSocketAddrs;
    let socket_addrs = match addr.to_socket_addrs() {
        Ok(it) => it.collect::<Vec<_>>(),
        Err(_) => return false,
    };
    socket_addrs
        .into_iter()
        .any(|sa| std::net::TcpStream::connect_timeout(&sa, Duration::from_millis(200)).is_ok())
}
