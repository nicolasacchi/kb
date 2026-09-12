//! Provenance + blame HTTP harness.
//!
//! Filter example: `cargo test -p kb-code-server --test provenance`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod blame_routes;
mod kb_daemon_default;
mod provenance_routes;
