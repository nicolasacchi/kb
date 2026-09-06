//! Standalone daemon binary. Reads `kb.toml` from the path supplied via
//! `--config <path>` (defaults to `~/.config/kb/kb.toml`), then delegates
//! to `kb_server::serve`.
//!
//! kb-cli's `kb daemon` subcommand calls `kb_server::serve` directly
//! in-process — this binary exists for systemd-style standalone launches.

use anyhow::{Context, Result};
use kb_core::config::KbConfig;
use kb_core::paths::KbPaths;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut config_path: Option<std::path::PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                config_path = args.next().map(std::path::PathBuf::from);
            }
            "--help" | "-h" => {
                eprintln!("kb-server [--config PATH]\n");
                eprintln!("Default config: ~/.config/kb/kb.toml");
                return Ok(());
            }
            other => {
                anyhow::bail!("unknown arg: {other}");
            }
        }
    }

    let cfg_paths = KbPaths::new("default").context("XDG paths")?;
    let resolved = config_path.unwrap_or_else(|| cfg_paths.config_file());
    let config = if resolved.exists() {
        KbConfig::load(&resolved).with_context(|| format!("load config {}", resolved.display()))?
    } else {
        tracing::warn!(path = %resolved.display(), "config not found — using defaults");
        KbConfig::default()
    };

    // CE — state paths derive from the daemon name (the config file itself
    // is shared across daemons). Pin them for serve_loop, which re-reads
    // `resolved` on each in-process restart (PUT /api/config).
    let paths =
        KbPaths::new(config.daemon.name.as_deref().unwrap_or("default")).context("XDG paths")?;

    // L1 — layered tracing: stderr keeps the exact pre-L1 defaults
    // (RUST_LOG-overridable, so docker logs are unchanged) and an ndjson
    // daily file lands in `<state>/log/` (default `info`, override via
    // KB_LOG_FILE_LEVEL). The guard must stay alive for the process
    // lifetime or buffered file logs are dropped on exit.
    let _log_guard =
        kb_core::tracing_init::init(&paths, STDERR_DEFAULT_FILTER).context("init tracing")?;

    // PF-B1 — build-stamp injection. This standalone bin deliberately does
    // NOT link `kb-buildstamp` (a package dep would also attach to the
    // kb-server LIB — cargo can't scope deps to one target — and the
    // per-commit re-stamp would drag the lib and everything downstream
    // back into every commit's rebuild). It reads the plain env at compile
    // time instead (the kb-code-server precedent): Docker/release builds
    // inject real values, dev builds get "unknown"/"0.0.0-dev" — which the
    // SPA drift guard treats as "no stamp, no-op". `kb daemon` (kb-cli)
    // injects the real git-probed stamp.
    let version = option_env!("KB_BUILD_VERSION").unwrap_or("0.0.0-dev");
    kb_server::set_build_stamp(
        version.strip_prefix('v').unwrap_or(version),
        option_env!("KB_BUILD_SHA").unwrap_or("unknown"),
    );

    kb_server::serve_loop(resolved, paths).await
}

// lance 4.0.0 emits a paired `_score`/`_distance` auto-projection
// deprecation WARN on every FTS + vector search. lancedb 0.27.2 doesn't
// expose a builder to opt into the future behavior. Pin the noisy logger
// to ERROR so the daemon log stays useful.
const STDERR_DEFAULT_FILTER: &str = "info,kb=debug,lance::dataset::scanner=error";
