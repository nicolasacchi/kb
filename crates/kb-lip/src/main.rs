//! `kb-lip --config <path>` — see `Cli`/`Config` for the full surface.

use clap::Parser;
use kb_lip::config::Config;
use kb_lip::server;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "kb-lip",
    about = "lip/1 — a generic LSP-to-HTTP adapter (design-lip.md, Track L1)"
)]
struct Cli {
    /// TOML config: lang_ids, command, workspace_root, port, [restart_backoff].
    #[arg(long)]
    config: PathBuf,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let config = match Config::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kb-lip: {e}");
            std::process::exit(1);
        }
    };

    let bind_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), config.port);
    let (addr, supervisor, task) = match server::serve(config, bind_addr).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("kb-lip: bind {bind_addr}: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!(%addr, "kb-lip listening (loopback-only lip/1 adapter)");

    shutdown_signal().await;
    tracing::info!("kb-lip shutting down (SIGTERM/Ctrl-C received)");
    // Stop the child LSP gracefully (shutdown/exit, then SIGTERM if it
    // doesn't exit on its own) before the process itself goes away.
    supervisor.shutdown().await;
    task.abort();
}

/// Resolves on SIGINT or SIGTERM (or, on non-unix, just Ctrl-C) — mirrors
/// kb-server's own `shutdown_signal` (crates/kb-server/src/lib.rs).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
