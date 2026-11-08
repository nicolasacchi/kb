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
//! The anchor is the session's sizing client (no `ignore-size`: tmux
//! adopts its pixel fields and colour answers under `window-size
//! manual`); viewers keep `ignore-size` so typing never moves the grid.

use crate::responder::{PumpHandle, ResponderCfg};
use crate::runner::RunnerError;
use crate::viewer::{spawn_tmux_client, ClientHandle};

/// One permanent responder-equipped client attached to a holder.
pub struct Anchor {
    client: ClientHandle,
}

impl Anchor {
    /// Spawn the anchor via the ONE hidden-client function shared with
    /// viewers (same argv/env/responder/winsize as [`spawn_tmux_client`]).
    /// The anchor attaches WITHOUT `-f ignore-size`: under `window-size
    /// manual` the flag is a no-op for grid stability (manual windows
    /// never resize from clients), but it also blocks tmux from adopting
    /// this client's pixel fields and colour answers as the session's
    /// `CSI 14t/16t/18t` and OSC 10/11 source. Viewers keep
    /// `ignore-size` (spec 5).
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
            client: spawn_tmux_client(tmux_bin, sock, cols, rows, cell_w, cell_h, cfg, false)?,
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
