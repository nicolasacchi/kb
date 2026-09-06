//! Blame / provenance / review / checkout CLI harness.
//!
//! Filter example: `cargo test -p kb-code-cli --test provenance_cli`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod blame;
mod checkout;
mod provenance;
mod review;
mod review_v4;
