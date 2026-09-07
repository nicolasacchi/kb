//! History / time / timeseries / stacks HTTP harness.
//!
//! Filter example: `cargo test -p kb-code-server --test history`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

/// V75-M3 (D15) — `branch-facts/1`, the conflict radar, favourites and
/// "compare with common base".
mod branch_facts_route;
mod stacks_route;
mod time_routes;
mod timeseries_route;
