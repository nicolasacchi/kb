//! Host-side kb-term crate: the `kb-termd` runner and the operator-local
//! `kb-term` client.
//!
//! This crate is deliberately NOT a kb verb surface and carries no `kb-core`
//! edge: depending on `kb-core` (and its lance/arrow graph) would make every
//! kb-term build pay a cold-compile giant, so the runner, the local client,
//! and kb-server stay decoupled and talk over unix sockets and the wire
//! contract in the `wire` module. Later units may add bin-only stamping (via
//! a bin-only crate or `option_env!`) without adding a lib-level dependency.
//!
//! The `wire` module is the single wire contract for the kb-term/1 frame set
//! and the runner RPC; it is pinned by golden JSON files under `tests/golden`
//! and mirrored in TypeScript by later units, so later changes there are
//! additive only (with the goldens updated in the same change).

pub mod wire;
