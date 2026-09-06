//! W2.5 — the RAW TRANSCRIPTS lane: a PULL-ONLY full-text index over the
//! operator's own local Claude Code transcript JSONL
//! (`~/.claude/projects/**/*.jsonl` by default — `[transcripts]` in
//! `kb-code.toml`, `config::TranscriptsSection`).
//!
//! # Why this exists, and why it's kept separate
//!
//! kb's own R0 rationale (`crates/kb-core`'s sessions-as-episodic-memory
//! work — see the root `CLAUDE.md` invariant #11) excludes raw session
//! transcripts from kb's RANKED search/recall: mixing raw chat exhaust
//! into a BM25+vector-ranked corpus pollutes ranking quality for every
//! other query. That rationale is about ranking pollution, NOT a blanket
//! ban on ever searching raw transcripts — "what did I say about X last
//! Tuesday" is a real, common need this lane exists to serve, just kept
//! on its own pull-only path:
//!
//! - **Never fused into code/doc ranking** — no cross-lane score blending
//!   with `search::{files,symbols,text}`; a transcripts hit only ever
//!   appears in `GET /api/search/transcripts`'s own response.
//! - **Never embedded** — no vector index, no bge-small pass; FTS5
//!   `MATCH` only (`store::Store::search_transcripts`).
//! - **Never in any non-loopback response** — both HTTP entry points
//!   ([`search::search_transcripts`], [`search::transcripts_status`]) sit
//!   behind [`search::loopback_only`], a STRICTER gate than the rest of
//!   `/api` (`auth_bearer`): a valid bearer token does not open this lane
//!   to a non-loopback caller, and a non-loopback probe gets a 404
//!   indistinguishable from an unknown route (`router.rs` merges these
//!   two routes under `loopback_only` INSTEAD OF `auth_bearer`, not
//!   alongside it). This also means the lane is excluded from any future
//!   cross-daemon federation BY CONSTRUCTION — a fan-out caller is, by
//!   definition, never a loopback peer of every fanned-out daemon.
//!
//! # This closes W3.4's pre-capture window
//!
//! Whatever indexer eventually captures/curates transcript content into
//! kb's own corpus (W3.4 and later), there is inherently a window between
//! "a turn happened" and "it got captured" — a session in flight, or one
//! that ended without triggering a capture hook yet. This lane's tail
//! indexer (`indexer::TranscriptWatcher`) observes the SAME JSONL files
//! Claude Code itself writes, live, with no capture step in between — so
//! `GET /api/search/transcripts` can find a turn from five seconds ago,
//! not just whatever's already been curated.
//!
//! # Module map
//!
//! - [`parse`] — one JSONL line → zero or more indexable turns (pure, no
//!   I/O — the extraction rules live entirely in this module's doc).
//! - [`indexer`] — walk-on-startup + a live `notify` watcher that tails
//!   each file from its stored byte offset, feeding [`parse`]'s output
//!   into `store::Store`'s transcript tables (`V0003`).
//! - [`search`] — the two HTTP routes + the loopback-only guard + the
//!   snippet builder that re-reads the source JSONL (the FTS5 index
//!   itself is contentless — see `store.rs`'s transcripts section doc).
//!
//! Schema lives in `migrations/V0003__transcripts.sql`; store-layer
//! methods live in `store.rs`'s own `--- transcripts (W2.5) ---` section
//! (kept there, not duplicated here, so every sqlite write for this
//! daemon still goes through the ONE `Store`/`Mutex<Connection>` — see
//! `store.rs`'s module doc's "single-writer discipline").

pub mod indexer;
pub mod parse;
pub mod search;
