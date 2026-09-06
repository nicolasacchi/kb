//! Child-process `kb daemon` tracing/log-level tests.
//!
//! Filter example: `cargo test -p kb-cli --test cli_spawn daemon_logging::`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod daemon_log_level;
mod daemon_logging;
