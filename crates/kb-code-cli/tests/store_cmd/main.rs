//! `kb-code store` — the EXIT-STATUS contract for agent callers (RS-U3
//! / RS-U9, defects 9+10 of the `c247d52` ReviewStore review wave).
//!
//! The contract lives INSIDE `store_cmd::run`, which reaches it only by a
//! real HTTP round trip to a daemon, so these are end-to-end: a real
//! `kb-code` process against [`stub`]'s one-shot HTTP server, with the
//! daemon's answer chosen per test. Nothing here needs a forge, a git
//! fixture or a live daemon — the bugs were in how the CLI READ a daemon
//! answer, not in the daemon.
//!
//! Filter example: `cargo test -p kb-code-cli --test store_cmd`

mod gc_exit_contract;
mod stub;
mod sync_base_upstream;
mod sync_partial;
