//! Doc-lens HTTP harness (golden table + CORS pins + remap).
//!
//! Filter example: `cargo test -p kb-code-server --test doclens`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod doclens_ct_f2;
mod doclens_remap;
mod doclens_route;
