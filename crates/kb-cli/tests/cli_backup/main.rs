//! Isolated `kb backup` integration tests (lance+sqlite store).
//!
//! Filter example: `cargo test -p kb-cli --test cli_backup backup::`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod backup;
