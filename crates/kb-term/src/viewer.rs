//! Hidden input clients: one per viewer, output drained and discarded.
//!
//! The anchor and every viewer share [`spawn_tmux_client`]: a
//! responder-equipped `tmux attach` on a pty whose output is drained
//! (answering only queries) and never forwarded anywhere. Typing flows
//! the other way: keys / Enter go through the writer each pump owns.
//! The anchor (permanent, sizing) shares the env and responder, but the
//! anchor attaches WITHOUT `-f ignore-size` (needed so tmux adopts its
//! pixel fields and colour answers; under `window-size manual` that
//! flag is a no-op for grid stability) while viewers keep `-f
//! ignore-size` so a hidden client can never move the window size.
//!
//! The runner keeps the [`Viewer`] map (see `runner/mod.rs`); this module
//! owns the client handle and the id validation.

use std::os::unix::io::OwnedFd;

use tokio::process::Command;

use crate::pty::{async_fd_dup, drop_kill, open_pty, set_winsize, spawn_on_pty, winsize_of};
use crate::responder::{spawn_pump, PumpHandle, ResponderCfg};
use crate::runner::RunnerError;

/// One attached hidden client (the anchor or a viewer).
pub struct ClientHandle {
    pump: PumpHandle,
    pid: u32,
    /// Master fd for `TIOCSWINSZ` grid updates (kept alongside the pump).
    master: OwnedFd,
    cell_w: u16,
    cell_h: u16,
}

impl std::fmt::Debug for ClientHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientHandle")
            .field("pid", &self.pid)
            .finish()
    }
}

impl ClientHandle {
    /// The client process pid.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Pump handle (writer for keys/Enter, stats, quiet clock).
    pub fn pump(&self) -> &PumpHandle {
        &self.pump
    }

    /// Pump handle (owned clone).
    pub fn pump_handle(&self) -> PumpHandle {
        self.pump.clone()
    }

    /// Current pty size: (cols, rows, xpix, ypix).
    pub fn winsize(&self) -> (u16, u16, u16, u16) {
        winsize_of(&self.master)
    }

    /// Resize the pty (cells and pixel fields).
    pub fn set_winsize(&self, cols: u16, rows: u16) {
        set_winsize(
            &self.master,
            cols,
            rows,
            cols.saturating_mul(self.cell_w),
            rows.saturating_mul(self.cell_h),
        );
    }

    /// Queue one item to the writer task (whole, never split); `busy-pty`
    /// on timeout.
    pub async fn write(&self, item: Vec<u8>) -> Result<(), RunnerError> {
        self.pump.write(item).await
    }
}

impl Drop for ClientHandle {
    fn drop(&mut self) {
        // Dropping the pump closes the master (SIGHUP on the client);
        // SIGTERM as well keeps a dropped runner from leaking the client
        // process if it ignores HUP. Reaping is the pty wait task.
        // All process signalling lives in `pty` (no unsafe here).
        drop_kill(self.pid);
    }
}

/// Spawn ONE hidden client: the path both the anchor and every viewer use.
///
/// Command: `<tmux> -u -S <sock> attach-session [-f ignore-size] -t kb`.
/// Environment: `env_clear()` plus exactly `TERM=xterm-256color`,
/// `LANG=C.UTF-8`, `PATH=/usr/bin:/bin`. Winsize pixel fields are the
/// profile cell size, so tmux answers a pane's `CSI 14t/16t/18t`
/// consistently with the anchor's starting size.
///
/// `ignore_size` is true for viewers (they must never move the grid).
/// The anchor passes false: under `window-size manual` the flag is a
/// no-op for grid stability, but it also blocks tmux from adopting the
/// anchor's pixel fields and colour answers as the session's
/// `CSI 14t/16t/18t` and OSC 10/11 source (verified on CI: with the
/// flag on every client, pane 14t is always `cells x default 32x16` and
/// OSC 11 is always black, regardless of the client's winsize pixels).
pub fn spawn_tmux_client(
    tmux_bin: &str,
    sock: &std::path::Path,
    cols: u16,
    rows: u16,
    cell_w: u16,
    cell_h: u16,
    cfg: &ResponderCfg,
    ignore_size: bool,
) -> Result<ClientHandle, RunnerError> {
    let xpix = cols.saturating_mul(cell_w);
    let ypix = rows.saturating_mul(cell_h);
    let (master, slave) = open_pty(cols, rows, xpix, ypix)?;
    let mut cmd = Command::new(tmux_bin);
    cmd.arg("-u").arg("-S").arg(sock).arg("attach-session");
    if ignore_size {
        cmd.args(["-f", "ignore-size"]);
    }
    cmd.arg("-t").arg("kb");
    cmd.env_clear();
    cmd.env("TERM", "xterm-256color");
    cmd.env("LANG", "C.UTF-8");
    cmd.env("PATH", "/usr/bin:/bin");
    let child = spawn_on_pty(cmd, &slave)?;
    drop(slave);
    // Two AsyncFd wraps over the same pty (dups): a reader and a writer.
    let reader = async_fd_dup(&master)?;
    let writer = async_fd_dup(&master)?;
    let pump = spawn_pump(reader, writer, cfg.clone());
    Ok(ClientHandle {
        pump,
        pid: child.pid,
        master,
        cell_w,
        cell_h,
    })
}

/// A registered viewer: its hidden client behind a viewer id.
pub struct Viewer {
    /// Hidden client handle.
    pub client: ClientHandle,
}

/// Whether `viewer` is a valid viewer id: `[a-z0-9:_-]{1,64}`.
pub fn valid_viewer(viewer: &str) -> bool {
    (1..=64).contains(&viewer.len())
        && viewer.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == ':' || c == '_' || c == '-'
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_id_and_session_validation() {
        assert!(valid_viewer("web"));
        assert!(valid_viewer("phone:1"));
        assert!(valid_viewer("a_b-c:1"));
        assert!(valid_viewer(&"a".repeat(64)));
        for bad in ["", "Web", "a b", "a/b", "UPPER", "wörk", &"a".repeat(65)] {
            assert!(!valid_viewer(bad), "viewer {bad:?} must be rejected");
        }
    }
}
