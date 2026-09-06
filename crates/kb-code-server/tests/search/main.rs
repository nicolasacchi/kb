//! Search-lane HTTP harness (dedicated lanes + unified box).
//!
//! Filter example: `cargo test -p kb-code-server --test search`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod search_routes;
mod search_unified;
