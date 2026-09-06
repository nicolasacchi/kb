//! Bind + serve: wires [`Config`] → [`Supervisor`] → the [`http::router`]
//! and starts the axum HTTP server in the background. Split out from
//! `main.rs` so integration tests can drive the real router over a real
//! loopback socket on an ephemeral port — the same
//! `serve_on_random_port_with_paths` shape kb-server's own tests use
//! (`crates/kb-server/tests/process/log_level.rs`).

use crate::config::Config;
use crate::http;
use crate::supervisor::Supervisor;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

/// Spawn the initial LSP child, bind `bind_addr` (port `0` picks an
/// ephemeral port — pass that in tests), and start serving lip/1 in the
/// background. Returns the ACTUAL bound address, the [`Supervisor`]
/// handle (for `identity()`/`shutdown()` or driving requests directly in
/// tests without going over HTTP), and the serving task's `JoinHandle`.
pub async fn serve(
    config: Config,
    bind_addr: SocketAddr,
) -> std::io::Result<(SocketAddr, Arc<Supervisor>, tokio::task::JoinHandle<()>)> {
    let supervisor = Arc::new(Supervisor::new(config));
    supervisor.start_initial().await;

    let listener = TcpListener::bind(bind_addr).await?;
    let local_addr = listener.local_addr()?;
    let router = http::router(supervisor.clone());

    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = %e, "kb-lip axum::serve exited with an error");
        }
    });

    Ok((local_addr, supervisor, task))
}
