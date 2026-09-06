//! W3.2 — the JOIN LADDER: deterministic commit→session resolution, the
//! thing kb-code's session-blame surface is built on. Three submodules:
//!
//! - [`kb_client`] — a typed reqwest client for kb's session↔commit join
//!   surface (`GET /api/sessions/by-commit`, `GET /api/sessions/
//!   commit-map` — `crates/kb-server/src/routes/sessions.rs`, W0.6), with a
//!   cached, TTL'd commit-map snapshot so the fuzzy/squash arms don't
//!   round-trip per commit.
//! - [`local`] — local (no-daemon-needed) commit resolution: reuses
//!   `kb_core::vcs::resolve_commit` for the official trailer-block parse
//!   (byte-identical to what kb's own `kb sessions capture` already
//!   trusts) plus this crate's gix handle for the author TIMESTAMP that
//!   `kb_core::vcs` doesn't carry.
//! - [`ladder`] — [`ladder::resolve_commit`], the ladder itself: six arms,
//!   first-hit-wins, cache-backed (`store::commit_sessions`, migration
//!   V0005). See that module's doc for the full arm-by-arm contract.
//! - [`backfill`] — W3.6's [`backfill::backfill_repo`], the join ladder's
//!   PRECOMPUTE: proactively walks a repo's commit history through
//!   [`ladder::resolve_commit`] (unchanged), warming the `commit_sessions`
//!   cache ahead of a live query. See that module's doc for the full
//!   contract.
//!
//! Consumed by `routes::join_commit` (`GET /api/join/commit?repo=&sha=`),
//! `routes::backfill_route` (`POST /api/backfill?repo=`), and `kb-code
//! join`/`kb-code backfill` (`kb-code-cli`).

pub mod backfill;
pub mod kb_client;
pub mod ladder;
pub mod local;
