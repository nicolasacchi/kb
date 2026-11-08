//! The kb-termd runner: one private tmux holder per session behind a
//! 0600 `runner.sock`, launched in a fixed start order (tap, anchor,
//! release) and shut down by exit keys plus a cgroup/process-group kill.
//!
//! The runner keeps NO database. Discovery globs `<run-dir>/s/*.sock` and
//! reads the `@kb_*` user options each holder carries, so a runner restart
//! re-lists and re-adopts (re-anchoring) sessions it did not launch.
//!
//! Per-session runtime state (in memory only, recreated on restart):
//! the anchor client (with responder pump), the viewers map
//! (`(id, viewer) -> Viewer`), the `paste_only` flag that gates the
//! composer Enter, and a paste-buffer sequence number. The grid owner
//! lives in the tmux `@kb_owner_grid` option, so it survives restarts.

pub mod anchor;
pub mod config;
pub mod discovery;
pub mod enter;
pub mod grid;
pub mod ids;
pub mod keys;
pub mod kill;
pub mod launch;
pub mod paste;
pub mod rpc;
pub mod server;
pub mod stream;
pub mod supervisor;
pub mod tmux;

pub use config::RunnerConfig;

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::runner::anchor::Anchor;
use crate::runner::config::SupervisorKind;
use crate::runner::supervisor::{DirectSupervisor, Supervisor, SystemdSupervisor};
use crate::runner::tmux::RealTmux;
use crate::viewer::{spawn_tmux_client, Viewer};

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
    /// Unknown session (T9 ops).
    NoSession(String),
    /// Unknown viewer for this session.
    NoViewer(String),
    /// Viewer id outside `[a-z0-9:_-]{1,64}`.
    BadViewer(String),
    /// Malformed input (e.g. base64, a named-key payload).
    BadInput(String),
    /// Named key outside the closed table.
    BadKey(String),
    /// Paste text contains forbidden bytes.
    BadText(String),
    /// Grid size or owner out of bounds.
    BadSize(String),
    /// Paste too large (or empty).
    TooLarge(String),
    /// Too many viewers for one session.
    TooManyViewers(String),
    /// Multi-line paste while bracketed paste is off.
    MultiLineRefused(String),
    /// Enter refused: the session is blocked.
    Blocked(String),
    /// Enter refused: the last paste was `paste_only`.
    PasteOnly(String),
    /// Enter refused: output never went quiet.
    Busy(String),
    /// The hidden client's writer is busy / the pty is closed.
    BusyPty(String),
    /// The grid is owned by someone else.
    GridOwned(String),
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
            RunnerError::NoSession(_) => "no-session",
            RunnerError::NoViewer(_) => "no-viewer",
            RunnerError::BadViewer(_) => "bad-viewer",
            RunnerError::BadInput(_) => "bad-input",
            RunnerError::BadKey(_) => "bad-key",
            RunnerError::BadText(_) => "bad-text",
            RunnerError::BadSize(_) => "bad-size",
            RunnerError::TooLarge(_) => "too-large",
            RunnerError::TooManyViewers(_) => "too-many-viewers",
            RunnerError::MultiLineRefused(_) => "multi-line-refused",
            RunnerError::Blocked(_) => "blocked",
            RunnerError::PasteOnly(_) => "paste-only",
            RunnerError::Busy(_) => "busy",
            RunnerError::BusyPty(_) => "busy-pty",
            RunnerError::GridOwned(_) => "grid-owned",
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
            | RunnerError::NoSession(m)
            | RunnerError::NoViewer(m)
            | RunnerError::BadViewer(m)
            | RunnerError::BadInput(m)
            | RunnerError::BadKey(m)
            | RunnerError::BadText(m)
            | RunnerError::BadSize(m)
            | RunnerError::TooLarge(m)
            | RunnerError::TooManyViewers(m)
            | RunnerError::MultiLineRefused(m)
            | RunnerError::Blocked(m)
            | RunnerError::PasteOnly(m)
            | RunnerError::Busy(m)
            | RunnerError::BusyPty(m)
            | RunnerError::GridOwned(m)
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
    anchors: std::sync::Arc<Mutex<HashMap<String, Anchor>>>,
    locks: Mutex<HashMap<String, std::sync::Arc<Mutex<()>>>>,
    viewers: std::sync::Arc<Mutex<HashMap<(String, String), Viewer>>>,
    paste_only: Mutex<HashMap<String, bool>>,
    buffer_seqs: Mutex<HashMap<String, u64>>,
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
            anchors: std::sync::Arc::new(Mutex::new(HashMap::new())),
            locks: Mutex::new(HashMap::new()),
            viewers: std::sync::Arc::new(Mutex::new(HashMap::new())),
            paste_only: Mutex::new(HashMap::new()),
            buffer_seqs: Mutex::new(HashMap::new()),
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
            anchors: std::sync::Arc::new(Mutex::new(HashMap::new())),
            locks: Mutex::new(HashMap::new()),
            viewers: std::sync::Arc::new(Mutex::new(HashMap::new())),
            paste_only: Mutex::new(HashMap::new()),
            buffer_seqs: Mutex::new(HashMap::new()),
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

    /// Insert (or replace) the anchor for a session, and arm the
    /// anchor-closed watcher: a background task that polls the anchor
    /// pump's `is_closed()` and, on the transition, closes the session's
    /// viewers promptly (spec 5) without waiting for the next RPC. The
    /// watcher reaps only if the closed pump is still the session's
    /// current anchor, so a replaced anchor (re-adoption) is not reaped
    /// by the stale task.
    pub async fn store_anchor(&self, id: &str, anchor: Anchor) {
        let pump = anchor.pump().clone();
        // Weak, not Arc: the watcher must not keep the maps (and hence the
        // anchor masters, i.e. the SIGHUP path) alive after the runner is
        // dropped. With Arc this was a reference cycle: map holds the
        // anchor -> master open -> pump open -> watcher alive.
        let anchors = std::sync::Arc::downgrade(&self.anchors);
        let viewers = std::sync::Arc::downgrade(&self.viewers);
        let id = id.to_string();
        self.anchors.lock().await.insert(id.clone(), anchor);
        tokio::spawn(async move {
            while !pump.is_closed() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let (Some(anchors), Some(viewers)) = (anchors.upgrade(), viewers.upgrade()) else {
                return;
            };
            let current_is_this = {
                let guard = anchors.lock().await;
                match guard.get(&id) {
                    None => true,
                    Some(current) => current.pump().is_same(&pump),
                }
            };
            if current_is_this {
                let mut viewers = viewers.lock().await;
                viewers.retain(|(sid, _), _| sid != &id);
            }
        });
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

    /// The anchor's pump quiet duration (the Enter quiet clock for `id`).
    pub async fn anchor_quiet_for(&self, id: &str) -> Option<Duration> {
        let anchors = self.anchors.lock().await;
        anchors.get(id).and_then(|anchor| anchor.pump().quiet_for())
    }

    /// Number of viewers registered for `id`.
    pub async fn viewer_count(&self, id: &str) -> usize {
        let viewers = self.viewers.lock().await;
        viewers.keys().filter(|(sid, _)| sid == id).count()
    }

    /// Close the session's viewers when its anchor reports closed (spec
    /// 5: all viewers for a session are closed when the session's anchor
    /// reports closed). The anchor-closed watcher armed by
    /// [`Runner::store_anchor`] fires promptly on the `is_closed()`
    /// transition; this dispatch-time reap is the fallback for any
    /// window before the watcher runs. (The kill path closes viewers
    /// directly.)
    pub async fn reap_anchor_closed_viewers(&self, id: &str) {
        let anchor_closed = {
            let anchors = self.anchors.lock().await;
            anchors
                .get(id)
                .map(|a| a.pump().is_closed())
                .unwrap_or(false)
        };
        if anchor_closed {
            self.close_viewers_for(id).await;
        }
    }

    /// Open (or re-open, idempotently) a hidden client for a viewer.
    ///
    /// The std Mutex guard is never held across an `.await`. The per-id
    /// lock (see [`Runner::id_lock`]) serializes cap-check / spawn /
    /// insert per session so two concurrent opens cannot both pass the
    /// cap check and insert 2 (cap 8 -> 9); `spawn_tmux_client` is
    /// synchronous, so no await spans the guard here.
    pub async fn viewer_open(
        &self,
        id: &str,
        viewer: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), RunnerError> {
        // When the anchor reports closed, its session is gone: the
        // viewers of that session close with it.
        self.reap_anchor_closed_viewers(id).await;
        if !crate::viewer::valid_viewer(viewer) {
            return Err(RunnerError::BadViewer(format!("bad viewer id `{viewer}`")));
        }
        let _lock = self.id_lock(id).await;
        let lock = _lock.lock().await;
        {
            let viewers = self.viewers.lock().await;
            if viewers.contains_key(&(id.to_string(), viewer.to_string())) {
                drop(lock);
                return Ok(());
            }
            if viewers.keys().filter(|(sid, _)| sid == id).count()
                >= self.cfg.max_viewers_per_session
            {
                return Err(RunnerError::TooManyViewers(format!(
                    "session {id} already has {} viewers",
                    self.cfg.max_viewers_per_session
                )));
            }
        }
        let (cell_w, cell_h) = self.cfg.cell_px_dims();
        // `spawn_tmux_client` is synchronous (no .await inside), so
        // holding the per-id lock across it cannot stall the runtime.
        let client = spawn_tmux_client(
            &self.cfg.tmux_bin,
            &self.holder_socket(id),
            cols,
            rows,
            cell_w,
            cell_h,
            &self.cfg.responder_cfg(),
            true,
        )?;
        let mut viewers = self.viewers.lock().await;
        // The per-id lock makes a concurrent insert impossible; the
        // entry API still discards a same-key race as defence in depth.
        viewers
            .entry((id.to_string(), viewer.to_string()))
            .or_insert(Viewer { client });
        drop(viewers);
        drop(lock);
        Ok(())
    }

    /// Close one viewer's hidden client (SIGHUP + reap, drop the pump).
    pub async fn viewer_close(&self, id: &str, viewer: &str) -> Result<(), RunnerError> {
        let mut viewers = self.viewers.lock().await;
        match viewers.remove(&(id.to_string(), viewer.to_string())) {
            Some(_) => Ok(()),
            None => Err(RunnerError::NoViewer(format!(
                "no viewer `{viewer}` for {id}"
            ))),
        }
    }

    /// Close every viewer of a session (kill; anchor-closed cleanup).
    pub async fn close_viewers_for(&self, id: &str) {
        let mut viewers = self.viewers.lock().await;
        viewers.retain(|(sid, _), _| sid != id);
    }

    /// Whether a viewer is registered.
    pub async fn has_viewer(&self, id: &str, viewer: &str) -> bool {
        let viewers = self.viewers.lock().await;
        viewers.contains_key(&(id.to_string(), viewer.to_string()))
    }

    /// The pump handle of a live viewer (keys/Enter writer + stats).
    pub async fn viewer_pump(
        &self,
        id: &str,
        viewer: &str,
    ) -> Option<crate::responder::PumpHandle> {
        let viewers = self.viewers.lock().await;
        viewers
            .get(&(id.to_string(), viewer.to_string()))
            .map(|v| v.client.pump_handle())
    }

    /// Pump stats for a viewer: `(bytes_drained, queries_answered)`.
    pub async fn viewer_stats(&self, id: &str, viewer: &str) -> Option<(u64, u64)> {
        self.viewer_pump(id, viewer)
            .await
            .map(|p| (p.bytes_drained(), p.queries_answered()))
    }

    /// Close every viewer of every session (runner shutdown).
    pub async fn close_viewers_for_all(&self) {
        let mut viewers = self.viewers.lock().await;
        viewers.clear();
    }

    /// Pump stats for the anchor: `(bytes_drained, queries_answered)`.
    pub async fn anchor_stats(&self, id: &str) -> Option<(u64, u64)> {
        let anchors = self.anchors.lock().await;
        anchors
            .get(id)
            .map(|a| (a.pump().bytes_drained(), a.pump().queries_answered()))
    }

    /// Apply a new winsize to the anchor and every hidden client.
    pub async fn apply_client_winsizes(&self, id: &str, cols: u16, rows: u16) {
        {
            let anchors = self.anchors.lock().await;
            if let Some(anchor) = anchors.get(id) {
                anchor.set_winsize(cols, rows);
            }
        }
        let viewers = self.viewers.lock().await;
        for ((sid, _), viewer) in viewers.iter() {
            if sid == id {
                viewer.client.set_winsize(cols, rows);
            }
        }
    }

    /// The current `paste_only` flag for a session.
    pub async fn paste_only(&self, id: &str) -> bool {
        *self.paste_only.lock().await.get(id).unwrap_or(&false)
    }

    /// Why the anchor pump closed, for tests/diagnostics:
    /// `Some((reason_code, errno))` — see `PumpShared` reason codes.
    /// `None` if the anchor is unknown or the pump is still open.
    pub async fn anchor_close_reason(&self, id: &str) -> Option<(u32, i32)> {
        let anchors = self.anchors.lock().await;
        anchors.get(id).and_then(|anchor| {
            let pump = anchor.pump();
            if pump.is_closed() {
                Some((pump.closed_reason(), pump.closed_errno()))
            } else {
                None
            }
        })
    }

    /// Record the `paste_only` flag for a session (in memory only).
    pub async fn set_paste_only(&self, id: &str, value: bool) {
        self.paste_only.lock().await.insert(id.to_string(), value);
    }

    /// Next paste-buffer sequence number for the session (monotonic).
    pub async fn next_buffer_seq(&self, id: &str) -> u64 {
        let mut seqs = self.buffer_seqs.lock().await;
        let entry = seqs.entry(id.to_string()).or_insert(0);
        *entry += 1;
        *entry
    }

    /// Current grid size for starting a viewer client (from discovery).
    pub async fn current_grid(&self, id: &str) -> Option<(u16, u16)> {
        let entries = crate::runner::discovery::list_sessions(self).await;
        entries
            .into_iter()
            .find(|e| e.id == id)
            .and_then(|e| Some((e.cols?, e.rows?)))
    }

    /// Whether session `id` names a live (non-stale) holder.
    pub async fn session_live(&self, id: &str) -> bool {
        if !crate::runner::ids::valid_id(id) {
            return false;
        }
        let entries = crate::runner::discovery::list_sessions(self).await;
        entries.iter().any(|e| e.id == id && !e.stale)
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
