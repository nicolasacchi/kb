//! Daemon boot + e2e watcher + SPA route-precedence harness.
//!
//! Filter example: `cargo test -p kb-code-server --test boot_e2e`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod boot;
mod e2e_daemon;
mod spa_route;
