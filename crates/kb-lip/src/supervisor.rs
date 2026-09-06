//! Crash → restart with capped exponential backoff ("Restart-on-crash
//! with backoff, capped; health degraded → identity says so.",
//! design-lip.md Phase L1). One [`Supervisor`] per adapter process, one
//! child at a time behind it.
//!
//! Model: `client()` hands out a live [`crate::lsp::LspClient`] if one
//! exists and is alive; if the current client died (either detected here,
//! or reported by a caller whose request against it failed transport-
//! level via [`Supervisor::report_failure`]), the NEXT call attempts a
//! fresh spawn — but only once the configured backoff window since the
//! last spawn ATTEMPT (successful or not) has elapsed. Between attempts,
//! every endpoint answers `refused: "server_down"` — "while down,
//! endpoints answer degraded".

use crate::config::Config;
use crate::lsp::LspClient;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

struct State {
    client: Option<Arc<LspClient>>,
    down_since: Option<Instant>,
    last_restart_attempt: Option<Instant>,
    /// Consecutive spawn attempts since the last successful one (reset to
    /// 0 on success) — the exponent for [`crate::config::RestartBackoff`].
    restart_attempts: u32,
}

pub struct Supervisor {
    config: Config,
    state: Mutex<State>,
    start_time: Instant,
}

/// Snapshot for `GET /lip/identity`.
#[derive(Debug, Clone)]
pub struct Identity {
    pub langs: Vec<String>,
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    pub workspace_root: String,
    pub pid: Option<u32>,
    pub uptime_secs: u64,
    pub healthy: bool,
    /// Does the LSP child currently have outstanding `$/progress` work
    /// (e.g. a cold-boot workspace index)? See
    /// [`crate::lsp::LspClient::is_indexing`] for the confirmed real-world
    /// finding this surfaces + its documented known gap. Always `false`
    /// while `!healthy` (no client to ask).
    pub indexing: bool,
    /// `"pull"|"push"|"none"` — the LSP 3.17 diagnostics mode the child
    /// advertised at `initialize` (see
    /// [`crate::lsp::ServerInfo::diagnostics_mode`], PRR-L5). `"none"`
    /// while `!healthy` (no client to ask, mirrors `indexing`'s own
    /// always-`false` convention in that case).
    pub diagnostics_mode: String,
}

impl Supervisor {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            state: Mutex::new(State {
                client: None,
                down_since: None,
                last_restart_attempt: None,
                restart_attempts: 0,
            }),
            start_time: Instant::now(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn workspace_root(&self) -> &Path {
        &self.config.workspace_root
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    /// Attempt the first spawn at boot. Best-effort: a failure here just
    /// leaves the supervisor down; the first HTTP request retries per the
    /// normal backoff-gated path in [`Supervisor::client`].
    pub async fn start_initial(&self) {
        let mut state = self.state.lock().await;
        self.try_spawn(&mut state).await;
    }

    async fn try_spawn(&self, state: &mut State) {
        state.last_restart_attempt = Some(Instant::now());
        match LspClient::start(&self.config).await {
            Ok(client) => {
                tracing::info!(
                    server = ?client.server_info.name,
                    version = ?client.server_info.version,
                    "lsp child spawned and initialized"
                );
                state.client = Some(Arc::new(client));
                state.down_since = None;
                state.restart_attempts = 0;
            }
            Err(e) => {
                tracing::warn!(error = %e, "lsp spawn/handshake failed");
                if state.down_since.is_none() {
                    state.down_since = Some(Instant::now());
                }
                state.restart_attempts = state.restart_attempts.saturating_add(1);
            }
        }
    }

    fn backoff_elapsed(&self, state: &State) -> bool {
        match state.last_restart_attempt {
            None => true,
            Some(last) => {
                let attempt_idx = state.restart_attempts.saturating_sub(1);
                let delay = self.config.restart_backoff.delay_ms(attempt_idx);
                last.elapsed() >= Duration::from_millis(delay)
            }
        }
    }

    /// A live client, or `None` if currently down (caller answers
    /// `refused: "server_down"`). May attempt a backoff-gated restart as
    /// a side effect.
    pub async fn client(&self) -> Option<Arc<LspClient>> {
        let mut state = self.state.lock().await;
        if let Some(c) = state.client.clone() {
            if c.is_alive() {
                return Some(c);
            }
            state.client = None;
            if state.down_since.is_none() {
                state.down_since = Some(Instant::now());
            }
        }
        if state.client.is_none() && self.backoff_elapsed(&state) {
            self.try_spawn(&mut state).await;
        }
        state.client.clone()
    }

    /// A caller's request against `client` failed at the transport level
    /// (broken pipe / channel closed, as opposed to a normal JSON-RPC
    /// error response). Marks it dead so the next [`Supervisor::client`]
    /// call attempts a restart, UNLESS a concurrent caller already
    /// replaced it (pointer-equality guard — never clobber a fresher
    /// client with a stale failure report).
    pub async fn report_failure(&self, client: &Arc<LspClient>) {
        let mut state = self.state.lock().await;
        if let Some(current) = &state.client {
            if Arc::ptr_eq(current, client) {
                state.client = None;
                if state.down_since.is_none() {
                    state.down_since = Some(Instant::now());
                }
            }
        }
    }

    pub async fn identity(&self) -> Identity {
        let state = self.state.lock().await;
        let (server_name, server_version, pid, healthy, indexing, diagnostics_mode) =
            match &state.client {
                Some(c) if c.is_alive() => (
                    c.server_info.name.clone(),
                    c.server_info.version.clone(),
                    c.pid,
                    true,
                    c.is_indexing(),
                    c.server_info.diagnostics_mode().to_string(),
                ),
                _ => (None, None, None, false, false, "none".to_string()),
            };
        Identity {
            langs: self.config.lang_ids.clone(),
            server_name,
            server_version,
            workspace_root: self.config.workspace_root.to_string_lossy().into_owned(),
            pid,
            uptime_secs: self.uptime_secs(),
            healthy,
            indexing,
            diagnostics_mode,
        }
    }

    /// Gracefully stop the current child, if any — called from the
    /// adapter's own SIGTERM handler.
    pub async fn shutdown(&self) {
        let state = self.state.lock().await;
        if let Some(c) = &state.client {
            c.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RestartBackoff;

    fn cfg_pointing_nowhere() -> Config {
        Config {
            lang_ids: vec!["ruby".to_string()],
            command: vec!["/definitely/does/not/exist/kb-lip-fixture-missing".to_string()],
            workspace_root: std::env::temp_dir(),
            port: 0,
            initialization_options: None,
            restart_backoff: RestartBackoff {
                initial_ms: 10,
                max_ms: 100,
                multiplier: 2.0,
            },
            diagnostics_wait_ms: 50,
        }
    }

    #[tokio::test]
    async fn client_reports_down_when_spawn_target_is_missing() {
        let sup = Supervisor::new(cfg_pointing_nowhere());
        sup.start_initial().await;
        assert!(sup.client().await.is_none());
        let id = sup.identity().await;
        assert!(!id.healthy);
        assert_eq!(id.pid, None);
        assert_eq!(id.diagnostics_mode, "none");
    }

    #[tokio::test]
    async fn identity_before_any_spawn_attempt_is_down() {
        let sup = Supervisor::new(cfg_pointing_nowhere());
        // No start_initial() call — identity must still be well-formed.
        let id = sup.identity().await;
        assert!(!id.healthy);
        assert_eq!(id.server_name, None);
        assert_eq!(id.diagnostics_mode, "none");
    }
}
