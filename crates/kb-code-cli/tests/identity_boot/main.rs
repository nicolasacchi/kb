//! Identity / daemon / backfill CLI harness.
//!
//! Filter example: `cargo test -p kb-code-cli --test identity_boot`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod backfill;
mod daemon;
mod identity;
mod scip_run;
