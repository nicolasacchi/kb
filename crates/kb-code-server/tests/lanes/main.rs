//! V72-H4a — `aug-lane/1`'s HTTP harness.
//!
//! Filter example: `cargo test -p kb-code-server --test lanes`
//!
//! Boot pattern mirrors `tests/security/main.rs` (each e2e file in this
//! crate duplicates its own small helper set — see `review_routes.rs`'s
//! own doc for why).

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod ingest_route;
