//! History / time / timeseries / stacks HTTP harness.
//!
//! Filter example: `cargo test -p kb-code-server --test history`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod stacks_route;
mod time_routes;
mod timeseries_route;
