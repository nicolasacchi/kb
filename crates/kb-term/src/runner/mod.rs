//! The kb-termd runner: one private tmux holder per session behind a
//! 0600 `runner.sock`, launched in a fixed start order (tap, anchor,
//! release) and shut down by exit keys plus a cgroup/process-group kill.
//!
//! The runner keeps NO database. Discovery globs `<run-dir>/s/*.sock` and
//! reads the `@kb_*` user options each holder carries, so a runner restart
//! re-lists and re-adopts (re-anchoring) sessions it did not launch.

pub mod anchor;
pub mod config;
pub mod discovery;
pub mod ids;
pub mod kill;
pub mod launch;
pub mod rpc;
pub mod server;
pub mod supervisor;
pub mod tmux;

pub use config::RunnerConfig;

use std::path::Path;

use tokio::sync::Mutex;

use crate::runner::anchor::Anchor;
use crate::runner::config::SupervisorKind;
use crate::runner::supervisor::{DirectSupervisor, Supervisor, SystemdSupervisor};
use crate::runner::tmux::RealTmux;

/// Machine-readable failure categories for the runner RPC (wire `code`).
#[derive(Debug, Clone)]
pub enum RunnerError {
    /// An id failed [`ids::valid_id`].
    InvalidId,
    /// A harness name is outside the closed set.
    InvalidHarness(String),
    /// A request failed validation or used an unsupported op/field.
    BadRequest(String),
    /// The id names no live session.
    NotFound(String),
    /// No control-mode tap attached within `tap_ready_secs`.
    TapTimeout(String),
    /// A tmux or holder operation failed.
    TmuxFailed(String),
    /// The pinned tmux binary is not the expected version.
    TmuxVersion(String),
    /// Anything else the runner could not complete.
    Internal(String),
}

impl RunnerError {
    /// The wire error code for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            RunnerError::InvalidId => "invalid-id",
            RunnerError::InvalidHarness(_) => "invalid-harness",
            RunnerError::BadRequest(_) => "bad-request",
            RunnerError::NotFound(_) => "not-found",
            RunnerError::TapTimeout(_) => "tap-timeout",
            RunnerError::TmuxFailed(_) => "tmux-failed",
            RunnerError::TmuxVersion(_) => "tmux-version",
            RunnerError::Internal(_) => "internal",
        }
    }

    /// Human-readable detail (never argv/env/secret-bearing text).
    pub fn message(&self) -> String {
        match self {
            RunnerError::InvalidId => "invalid session id".to_string(),
            RunnerError::InvalidHarness(h) => format!("unknown harness {h}"),
            RunnerError::BadRequest(m)
            | RunnerError::NotFound(m)
            | RunnerError::TapTimeout(m)
            | RunnerError::TmuxFailed(m)
            | RunnerError::TmuxVersion(m)
            | RunnerError::Internal(m) => m.clone(),
        }
    }
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

impl std::error::Error for RunnerError {}

/// The runner. Generic over the supervisor so systemd and direct modes
/// compile without trait-object indirection on the async path.
pub struct Runner<S: Supervisor> {
    pub cfg: RunnerConfig,
    pub tmux: RealTmux,
    pub supervisor: S,
    anchors: Mutex<std::collections::HashMap<String, Anchor>>,
    locks: Mutex<std::collections::HashMap<String, std::sync::Arc<Mutex<()>>>>,
    /// Peer-uid accepted by the runner socket (this process's uid).
    pub server_uid: u32,
}

impl Runner<DirectSupervisor> {
    /// Build a direct-mode runner (tests and CI: no `systemd --user`).
    pub fn direct(cfg: RunnerConfig) -> Self {
        let tmux = RealTmux::new(cfg.tmux_bin.clone());
        let supervisor = DirectSupervisor::new(
            cfg.tmux_bin.clone(),
            cfg.conf.clone(),
            cfg.run_dir.clone(),
            cfg.tap_bin.clone(),
        );
        Runner {
            cfg,
            tmux,
            supervisor,
            anchors: Mutex::new(std::collections::HashMap::new()),
            locks: Mutex::new(std::collections::HashMap::new()),
            server_uid: unsafe { libc::getuid() },
        }
    }
}

impl Runner<SystemdSupervisor> {
    /// Build a systemd-mode runner (production on a systemd host).
    pub fn systemd(cfg: RunnerConfig) -> Self {
        let tmux = RealTmux::new(cfg.tmux_bin.clone());
        Runner {
            cfg,
            tmux,
            supervisor: SystemdSupervisor,
            anchors: Mutex::new(std::collections::HashMap::new()),
            locks: Mutex::new(std::collections::HashMap::new()),
            server_uid: unsafe { libc::getuid() },
        }
    }
}

impl<S: Supervisor> Runner<S> {
    /// The `<run-dir>/s` directory that holds per-session sockets and dirs.
    pub fn sessions_dir(&self) -> std::path::PathBuf {
        self.cfg.run_dir.join("s")
    }

    /// The socket of the holder for `id` (the id must already be validated).
    pub fn holder_socket(&self, id: &str) -> std::path::PathBuf {
        self.sessions_dir().join(format!("{id}.sock"))
    }

    /// Acquire the per-id lock serialising launch vs kill for one session.
    pub async fn id_lock(&self, id: &str) -> std::sync::Arc<Mutex<()>> {
        let mut guard = self.locks.lock().await;
        guard
            .entry(id.to_string())
            .or_insert_with(|| std::sync::Arc::new(Mutex::new(())))
            .clone()
    }

    /// Insert (or replace) the anchor for a session.
    pub async fn store_anchor(&self, id: &str, anchor: Anchor) {
        self.anchors.lock().await.insert(id.to_string(), anchor);
    }

    /// Take the anchor for a session, closing its master (the anchor client
    /// receives SIGHUP; the session survives).
    pub async fn take_anchor(&self, id: &str) -> Option<Anchor> {
        self.anchors.lock().await.remove(id)
    }

    /// The anchor's current pty winsize: (cols, rows, pixel_w, pixel_h).
    pub async fn anchor_winsize(&self, id: &str) -> Option<(u16, u16, u16, u16)> {
        let anchors = self.anchors.lock().await;
        anchors.get(id).map(Anchor::winsize)
    }

    /// Apply a new winsize to the anchor pty (resize RPC).
    pub async fn anchor_set_winsize(&self, id: &str, cols: u16, rows: u16) {
        let anchors = self.anchors.lock().await;
        if let Some(anchor) = anchors.get(id) {
            anchor.set_winsize(cols, rows);
        }
    }

    /// Whether the runner owns a live anchor master for `id`.
    pub async fn has_anchor(&self, id: &str) -> bool {
        self.anchors.lock().await.contains_key(id)
    }

    /// Close every anchor master (runner shutdown: holders are left alone).
    pub async fn close_all_anchors(&self) {
        self.anchors.lock().await.clear();
    }
}

/// Ensure `<run-dir>`, `s/` and `runner/` exist with the tmpfiles.d modes
/// in direct mode; in systemd mode fail naming the tmpfiles lines.
pub async fn ensure_layout(cfg: &RunnerConfig) -> Result<(), RunnerError> {
    let need = [
        (cfg.run_dir.join("s"), 0o700),
        (cfg.run_dir.join("runner"), 0o700),
    ];
    if cfg.supervisor == SupervisorKind::Direct {
        for (dir, mode) in &need {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| RunnerError::Internal(format!("create {}: {e}", dir.display())))?;
            set_mode(dir, *mode)?;
        }
        set_mode(&cfg.run_dir, 0o755)?;
        Ok(())
    } else {
        for (dir, _) in &need {
            if !dir.exists() {
                return Err(RunnerError::Internal(format!(
                    "missing {}: create it with the tmpfiles.d lines \
                     `d /run/kb-term/s 0700 <user> <user> -` and \
                     `d /run/kb-term/runner 0700 <user> <user> -`",
                    dir.display()
                )));
            }
        }
        Ok(())
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<(), RunnerError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| RunnerError::Internal(format!("chmod {}: {e}", path.display())))
}

/// Boot the version check: `<tmux-bin> -V` must print exactly
/// `tmux <expected>` or the runner refuses to start.
pub async fn check_tmux_version<T: tmux::Tmux>(
    tmux: &T,
    expected: &str,
) -> Result<(), RunnerError> {
    let out = tmux.version().await.map_err(RunnerError::TmuxFailed)?;
    if out.trim() == format!("tmux {expected}") {
        Ok(())
    } else {
        Err(RunnerError::TmuxVersion(format!(
            "expected `tmux {expected}`, pinned tmux reports `{}`",
            out.trim()
        )))
    }
}

/// Adopt every live session: an anchor whose pid is dead, or unknown to
/// `list-clients`, is recreated and `@kb_anchor_pid` rewritten.
pub async fn adopt_anchors<S: Supervisor>(runner: &Runner<S>) -> Result<(), RunnerError> {
    let entries = crate::runner::discovery::list_sessions(runner).await;
    for entry in entries {
        if entry.stale {
            continue;
        }
        let needs_new = match entry.anchor_pid {
            Some(pid) => {
                !pid_alive(pid)
                    || !crate::runner::discovery::client_pids(runner, &entry.id)
                        .await
                        .contains(&pid)
            }
            None => true,
        };
        if needs_new {
            let cols = entry.cols.unwrap_or(runner.cfg.default_cols);
            let rows = entry.rows.unwrap_or(runner.cfg.default_rows);
            crate::runner::launch::spawn_anchor_and_wait(runner, &entry.id, cols, rows).await?;
        }
    }
    Ok(())
}

/// Is `pid` a live (non-zombie) process?
pub fn pid_alive(pid: u32) -> bool {
    // A zombie answers kill(pid, 0) but is not alive for our purposes.
    if let Ok(state) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        if state
            .rsplit(')')
            .next()
            .and_then(|s| s.split_whitespace().next())
            == Some("Z")
        {
            return false;
        }
    }
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Signal a whole process group (the pane child is its own session leader).
///
/// Pids 0 and 1 are never valid targets: `kill(0)` would signal the
/// runner's own process group and `kill(-1)` every process this user can
/// signal. A parsed-zero/one pane or server pid therefore means "absent".
pub fn kill_group(pid: u32, sig: i32) {
    if pid <= 1 {
        return;
    }
    unsafe {
        libc::kill(-(pid as i32), sig);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::tmux::fake::FakeTmux;

    #[tokio::test]
    async fn tmux_version_mismatch_refuses() {
        let fake = FakeTmux {
            version_out: std::sync::Mutex::new("tmux 9.9\n".to_string()),
            ..FakeTmux::default()
        };
        let err = check_tmux_version(&fake, "3.7c").await.unwrap_err();
        assert_eq!(
            err.code(),
            "tmux-version",
            "a non-matching -V must refuse at boot: {err}"
        );
        // The default fake reports the pinned version and passes.
        let matching = FakeTmux::new();
        check_tmux_version(&matching, "3.7c")
            .await
            .expect("tmux 3.7c must satisfy expected 3.7c");
    }

    #[test]
    fn kill_group_ignores_pid_zero_and_one() {
        // Without the guard, kill(0, SIGTERM) would terminate this test
        // process's own group and kill(-1, SIGTERM) would signal every
        // process this user can signal. Survival proves the no-op.
        let me = std::process::id();
        assert!(pid_alive(me));
        kill_group(0, libc::SIGTERM);
        kill_group(1, libc::SIGTERM);
        assert!(pid_alive(me), "kill_group(0/1) must be a no-op");
    }
}
