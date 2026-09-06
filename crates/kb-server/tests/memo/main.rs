//! Lance-heavy memo tests sharing one atlas_lance_lock.
//!
//! Filter example: `cargo test -p kb-server --test memo facets_memo::`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod atlas_backfill;
mod atlas_points_memo;
mod facets_memo;
