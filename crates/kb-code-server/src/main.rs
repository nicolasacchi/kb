//! `kb-code-server` bin — W1.2: real daemon boot. Reads `kb-code.toml`
//! from `--config <path>` (default `<config-dir>/kb-code.toml` via
//! `KbPaths::new("kb-code")`, mirroring kb-server's `--config` flag —
//! see `crates/kb-server/src/main.rs`), then delegates to
//! `kb_code_server::serve`.

use anyhow::{Context, Result};
use kb_code_server::config::KbCodeConfig;
use kb_core::paths::KbPaths;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let mut args = std::env::args().skip(1);
    let mut config_path: Option<std::path::PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                config_path = args.next().map(std::path::PathBuf::from);
            }
            "--help" | "-h" => {
                eprintln!("kb-code-server [--config PATH]\n");
                eprintln!("Default config: <config-dir>/kb-code.toml");
                return Ok(());
            }
            other => {
                anyhow::bail!("unknown arg: {other}");
            }
        }
    }

    let paths = KbPaths::new("kb-code").context("resolve kb-code XDG paths")?;
    // `KbPaths::new`'s config/cache roots are the fixed app-name `kb`
    // directory regardless of the daemon-name argument — only `state` is
    // namespaced per daemon (see the `config` module doc comment) — so
    // `kb-code.toml` lives beside kb's own `kb.toml`, distinguished by
    // filename, not directory.
    let resolved = config_path.unwrap_or_else(|| paths.config.join("kb-code.toml"));
    let config = KbCodeConfig::load(&resolved)
        .with_context(|| format!("load config {}", resolved.display()))?;

    tracing::info!(
        version = kb_code_server::version(),
        addr = %config.server.addr,
        repos = config.repos.len(),
        config = %resolved.display(),
        "kb-code-server booting",
    );

    kb_code_server::serve(config).await
}
