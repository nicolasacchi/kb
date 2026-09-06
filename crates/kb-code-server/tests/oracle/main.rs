//! Oracle-bar + usages HTTP harness.
//!
//! Filter example: `cargo test -p kb-code-server --test oracle`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod oracle_hierarchy;
mod oracle_resolution;
// V71-E1 — D4's Ruby STRICT-rule oracle (the release gate) + the
// `usages/2` wire contract and its dead-surface walk.
mod usages2_wire;
mod usages_route;
