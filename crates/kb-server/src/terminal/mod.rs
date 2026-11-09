//! The SPA-only terminal relay (`[terminal]`, T11).
//!
//! kb-server is only a relay: the `kb-termd` runner owns every terminal
//! on the host (tmux, the tap, the responder) and speaks one-JSON-line
//! RPCs over its unix socket. kb-server holds no terminal state except
//! the in-memory ticket table in [`ticket`]. Nothing here is persisted
//! (LF-7); a ticket never survives a restart.
//!
//! [`gate`] is the handler-level gate (exact Origin, intent header, no
//! loopback bypass, caller class); [`runner_client`] is the per-request
//! unix-socket client built from the live config; [`wire`] is the
//! serde mirror of the runner JSON types.

pub mod binding;
pub mod events;
pub mod gate;
pub mod runner_client;
pub mod ticket;
pub mod wire;
pub mod ws;

/// The in-memory terminal relay state on [`KbHandles`](crate::state::KbHandles).
/// One field here, not on `KbHandles`, so T12/T13 extend this struct
/// rather than growing the handles struct.
#[derive(Debug, Default)]
pub struct TerminalState {
    pub(crate) tickets: ticket::TicketStore,
    /// T13 — the terminal <-> session directory (id -> pane_pid) refreshed
    /// from the runner's `list` reply. In memory only (LF-7).
    pub(crate) term_dir: binding::TermDirectory,
    /// T13 — at most one detached directory-refresh task at a time.
    pub(crate) refresh_inflight: std::sync::atomic::AtomicBool,
    /// T13 — the minimum interval between directory-refresh spawns.
    pub(crate) refresh_min: std::sync::Mutex<Option<std::time::Instant>>,
    /// T12 — live WebSocket socket count (`ws::MAX_TERM_SOCKETS`) and
    /// the `term.ended` id dedup set (`events::EndedSet`). In memory
    /// only (LF-7).
    pub(crate) sockets: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) ended: events::EndedSet,
}
