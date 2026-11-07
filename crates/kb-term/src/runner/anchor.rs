//! The anchor: one permanent client per session, carrying the responder.
//!
//! The runner keeps one [`Anchor`] per session in a `HashMap<id, Anchor>`.
//! It is a hidden client like every viewer (same [`spawn_tmux_client`]),
//! but with a responder pump: its output is drained and discarded, and it
//! answers tmux's startup queries so the harness is released only after
//! the anchor has identified itself. On a runner restart the old anchor
//! dies of SIGHUP (its master closed) and the new runner spawns a fresh
//! one; the session survives.
//!
//! `ignore-size` plus `window-size manual` never resizes the window.

use crate::responder::{PumpHandle, ResponderCfg};
use crate::runner::RunnerError;
use crate::viewer::{spawn_tmux_client, ClientHandle};

/// One permanent responder-equipped client attached to a holder.
pub struct Anchor {
    client: ClientHandle,
}

impl Anchor {
    /// Spawn the anchor via the ONE hidden-client function shared with
    /// viewers (`<tmux> -u -S <sock> attach-session -f ignore-size -t kb`
    /// on a pty sized to the grid with profile-cell pixel fields).
    pub fn spawn(
        tmux_bin: &str,
        sock: &std::path::Path,
        cols: u16,
        rows: u16,
        cell_w: u16,
        cell_h: u16,
        cfg: &ResponderCfg,
    ) -> Result<Anchor, RunnerError> {
        Ok(Anchor {
            client: spawn_tmux_client(tmux_bin, sock, cols, rows, cell_w, cell_h, cfg)?,
        })
    }

    /// The anchor client's pid.
    pub fn pid(&self) -> u32 {
        self.client.pid()
    }

    /// The anchor's pump (quiet clock + stats).
    pub fn pump(&self) -> &PumpHandle {
        self.client.pump()
    }

    /// Current pty size: (cols, rows, pixel_w, pixel_h).
    pub fn winsize(&self) -> (u16, u16, u16, u16) {
        self.client.winsize()
    }

    /// Resize the pty (cells and pixel fields) for a grid take.
    pub fn set_winsize(&self, cols: u16, rows: u16) {
        self.client.set_winsize(cols, rows);
    }
}

// Drop is inherited from the ClientHandle (closes the master, SIGHUPing
// the client, and SIGTERMs the pid so a dropped runner leaks nothing).
