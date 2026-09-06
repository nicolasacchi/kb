//! V70-A2 — local-daemon hardening HTTP harness.
//!
//! Filter example: `cargo test -p kb-code-server --test security`
//!
//! Boot pattern mirrors `tests/review/main.rs` (each e2e file in this
//! crate duplicates its own small helper set — see `review_routes.rs`'s
//! own doc for why).

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod audit_route;
mod git_argv_lint;
mod guards;
mod path_and_secrets;
