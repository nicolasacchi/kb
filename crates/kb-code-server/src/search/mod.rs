//! W2.1 — the INSTANT search lanes: three independently-callable, pure
//! (no-axum) modules, each wired to exactly one `GET /api/search/*` route in
//! `routes.rs`. Latency is the contract, not a nice-to-have: files/symbols
//! target p50 < 50ms and text targets a first-hit p50 < 250ms on a
//! kb-repo-sized corpus (`tests/measure/latency.rs` pins measured numbers).
//!
//! - [`files`] — nucleo fuzzy match over the `files` table's paths, blended
//!   with an open-history frecency signal (`store::Store`'s new
//!   `file_opens` table, V0002).
//! - [`symbols`] — nucleo fuzzy match over symbol names (name + container
//!   context), joined to CURRENTLY-live files via blob hash (reuses
//!   `store::Store::symbols_for_repo`'s own join).
//! - [`text`] — `grep-searcher` streaming literal/regex search over a
//!   repo's WORKING-TREE files, walking the `files` table's path list
//!   rather than a fresh filesystem walk.
//!
//! **W2.4** adds the unified, sectioned `GET /api/search` box that fans
//! these three lanes out together, alongside the semantic/sessions/
//! transcripts lanes: [`grammar`] (pure query-prefix/filter parsing) and
//! [`unified`] (the per-lane concurrent runners + response assembly, behind
//! `routes::search_unified`). [`sessions`] is new here too — the box's
//! SESSIONS lane, which federates to the operator's own `kb` daemon over
//! HTTP (a different corpus from every other lane in this crate, which all
//! read kb-code's OWN store).

pub mod files;
pub mod grammar;
pub mod sessions;
pub mod symbols;
pub mod text;
pub mod unified;

pub use files::{FileHit, FileIndex};
pub use symbols::{SymbolHit, SymbolIndex};
pub use text::{search_text, TextFileResult, TextMatch, TextSearchError, TextSearchResponse};

/// Shared default/ceiling for the `files`/`symbols` lanes' `?limit=` query
/// param — the `text` lane has its own caps (`text::MAX_TOTAL_MATCHES`/
/// `text::MAX_MATCHES_PER_FILE`), since it counts MATCHES, not candidates.
pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 200;

/// Generation-gated per-repo cache entry — the shared shape behind both
/// [`files::FileIndex`] and [`symbols::SymbolIndex`]'s in-memory snapshots.
/// Rebuilt whenever `Store::generation()` has moved past what this entry was
/// built from (see `store.rs`'s `generation` field doc for why a lazy,
/// pull-based generation check was chosen over a subscribed `EventBus`
/// listener: every search-lane call already holds a `&Store`, so this is
/// strictly simpler with no task lifetime to manage).
pub(crate) struct GenCached<T> {
    pub(crate) generation: u64,
    pub(crate) value: std::sync::Arc<T>,
}

/// `path:`/`repo:` PRE-filter (V70-A3X) — `true` if `path` passes an
/// already-lowercased `filter_lower` (case-insensitive substring, matching
/// `search::grammar`'s `path:` semantics), or trivially `true` when
/// `filter_lower` is `None`. Every lane that accepts a candidate-narrowing
/// path filter (files/symbols/text) applies this INSIDE its own candidate
/// loop, before scoring/ranking/truncation — never as a post-filter over an
/// already-truncated page, which is the bug this exists to close (see
/// `unified`'s module doc, "Filters and repo scoping"). Callers lower the
/// filter ONCE outside their per-candidate loop and pass the lowered form
/// here, rather than re-lowering per candidate.
pub(crate) fn path_prefilter_matches(path: &str, filter_lower: Option<&str>) -> bool {
    match filter_lower {
        Some(want) => path.to_lowercase().contains(want),
        None => true,
    }
}
