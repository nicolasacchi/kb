//! Spawned kb-server binary tracing/log-level tests.
//!
//! Filter example: `cargo test -p kb-server --test process log_level::`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod file_logging;
mod log_level;
