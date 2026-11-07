//! `kb-termd`: the kb-term runner (runner.sock 0600, one private tmux
//! holder per session).

use std::sync::Arc;

use tokio::sync::broadcast;

use kb_term::runner::config::SupervisorKind;
use kb_term::runner::server::{bind_runner_socket, serve};
use kb_term::runner::{adopt_anchors, check_tmux_version, ensure_layout, Runner, RunnerError};

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() {
    init_tracing();
    let cfg = kb_term::runner::RunnerConfig::from_cli();
    if let Err(e) = run(cfg).await {
        // Error codes are part of the interface (e.g. `tmux-version`).
        eprintln!("kb-termd: {e}");
        std::process::exit(1);
    }
}

async fn run(cfg: kb_term::runner::RunnerConfig) -> Result<(), RunnerError> {
    match cfg.supervisor {
        SupervisorKind::Direct => run_mode(cfg, Runner::direct).await,
        SupervisorKind::Systemd => run_mode(cfg, Runner::systemd).await,
    }
}

async fn run_mode<S: kb_term::runner::supervisor::Supervisor>(
    cfg: kb_term::runner::RunnerConfig,
    build: fn(kb_term::runner::RunnerConfig) -> Runner<S>,
) -> Result<(), RunnerError> {
    ensure_layout(&cfg).await?;
    let runner = Arc::new(build(cfg));
    check_tmux_version(&runner.tmux, &runner.cfg.expected_tmux).await?;
    adopt_anchors(&runner).await?;
    let sock_path = runner.cfg.run_dir.join("runner").join("runner.sock");
    let listener = bind_runner_socket(&sock_path)
        .map_err(|e| RunnerError::Internal(format!("bind runner.sock: {e}")))?;
    let (tx, rx) = broadcast::channel::<()>(1);
    // SIGTERM/SIGINT: stop accepting, close anchor masters, remove
    // runner.sock, leave every holder alone.
    tokio::spawn(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        let mut int = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .expect("SIGINT handler");
        tokio::select! {
            _ = term.recv() => { let _ = tx.send(()); }
            _ = int.recv() => { let _ = tx.send(()); }
        }
    });
    serve(runner, listener, sock_path, rx)
        .await
        .map_err(|e| RunnerError::Internal(format!("serve: {e}")))?;
    Ok(())
}
