//! Measure-lane tests (latency contract, occurrences bench, semantic e2e).
//!
//! Gated out of default CI via `#[ignore]`. Run the lane manually:
//! `cargo test -p kb-code-server --test measure -- --ignored`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod latency;
mod occurrences_bench;
mod semantic_e2e;
