//! Child-process `kb daemon` tracing/log-level and state-dir lock tests.
//!
//! Filter example: `cargo test -p kb-cli --test cli_spawn daemon_logging::`
//! (or `daemon_lock::`).

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod daemon_lock;
mod daemon_log_level;
mod daemon_logging;
