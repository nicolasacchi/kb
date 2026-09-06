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
//! **V71-D1** (kbcq/1, design D3) adds [`matcher`] — the ONE name-shaped
//! matcher every lane above ranks through, carrying the hard exact tier and
//! the UTF-16 match indices the SPA highlights with — plus [`Factors`], the
//! per-signal flags (kb's MI-W5.R precedent: each factor behind its own
//! boolean and SURFACED iff its own flag is on, never silently folded into
//! one score).
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
pub mod matcher;
pub mod results;
pub mod sessions;
pub mod symbols;
pub mod text;
pub mod unified;

pub use files::{FileHit, FileIndex};
pub use matcher::{HaystackKind, MatchTier, NameMatcher};
pub use symbols::{SymbolHit, SymbolIndex};
pub use text::{search_text, TextFileResult, TextMatch, TextSearchError, TextSearchResponse};

/// V71-D1b — the text lane's symbol-name candidate signal (see `text`'s
/// module doc's "Scan order" section): every path carrying a symbol whose
/// name contains one of `atoms` (a query's [`matcher::identifier_atoms`]),
/// read from a symbol snapshot ONLY IF IT IS ALREADY WARM
/// ([`symbols::SymbolIndex::cached_snapshot_if_warm`] — this never triggers
/// a cold `store::symbols_for_repo` rebuild, which would turn a best-effort
/// ranking hint into the exact dozens-of-seconds stall this unit exists to
/// avoid). `None` when there is nothing useful to offer (no atoms, no warm
/// snapshot, or no symbol matched) — callers treat that identically to "no
/// hint was ever computed".
pub(crate) fn symbol_candidate_paths(
    symbol_index: &SymbolIndex,
    store: &crate::store::Store,
    repo_id: i64,
    atoms: &[String],
) -> Option<std::collections::HashSet<String>> {
    if atoms.is_empty() {
        return None;
    }
    let rows = symbol_index.cached_snapshot_if_warm(store, repo_id)?;
    let set: std::collections::HashSet<String> = rows
        .iter()
        .filter(|(_, sym)| {
            let name_lower = sym.name.to_lowercase();
            atoms.iter().any(|a| name_lower.contains(a.as_str()))
        })
        .map(|(path, _)| path.clone())
        .collect();
    if set.is_empty() {
        None
    } else {
        Some(set)
    }
}

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

/// V71-D1 — the per-signal ranking flags, from `[search]` in `kb-code.toml`
/// (`config::SearchSection`). kb's MI-W5.R amendment is the precedent and
/// the discipline: every factor gets its OWN boolean, a default chosen for
/// a stated reason rather than a vibe, and is SURFACED on the `explain`
/// decomposition iff its own flag is on — never absorbed into a single
/// opaque score. A factor whose flag is off is not multiplied by a neutral
/// 1.0; it is skipped entirely, so the arithmetic with it off is
/// byte-identical to the arithmetic before it existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Factors {
    /// The files lane's open-history recency boost (`files::recency_boost`).
    /// DEFAULT ON — it shipped on in W2.1 and this flag only makes it
    /// switchable and explainable; flipping the default off would be a
    /// silent ranking change, which is the thing this struct exists to
    /// prevent.
    pub frecency: bool,
    /// Demote vendored/generated paths (`matcher`-free, `is_generated_path`).
    /// DEFAULT OFF — plausible, unmeasured. No bench has scored it, and
    /// MI-W5.R's ruling is that an unmeasured factor ships off.
    pub demote_generated: bool,
    /// Rank the lexical (text) lane's files by matched-atom rarity instead
    /// of alphabetically (`text::rank_by_rarity`). DEFAULT ON: the order it
    /// replaces is `ORDER BY path`, which is not a relevance signal at all,
    /// and the replacement is derived purely from the query and the hits in
    /// hand (nothing learned, nothing personal).
    pub lexical_rarity: bool,
}

impl Default for Factors {
    fn default() -> Self {
        Self {
            frecency: true,
            demote_generated: false,
            lexical_rarity: true,
        }
    }
}

/// Everything a lane runner needs beyond "the query and how many hits" —
/// one struct rather than a growing tail of `Option<&str>`/`bool`
/// parameters threaded through three lanes and their tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct LaneOpts<'a> {
    /// `path:`/`repo:`'s candidate-narrowing PRE-filter — see
    /// [`path_prefilter_matches`].
    pub path_filter: Option<&'a str>,
    pub factors: Factors,
    /// `explain:1` — populate each hit's ranking decomposition.
    pub explain: bool,
    /// V71-D1b — an optional, caller-supplied set of paths already known to
    /// be relevant to the query (today: the symbol lane's own name-matched
    /// paths, from a warm `search::symbols::SymbolIndex` snapshot — see
    /// `search::text`'s module doc's "Scan order" section). `None` when the
    /// caller has no such hint (every pre-V71-D1b call site, and any call
    /// while the symbol cache is cold): behaviour is then driven purely by
    /// the query's own path/basename token overlap, byte-identical to
    /// having no candidate signal at all.
    pub candidate_paths: Option<&'a std::collections::HashSet<String>>,
}

/// Path shapes that are checked in but not authored: vendored trees, build
/// output, minified bundles, and the two Rails files every monolith
/// regenerates. Consulted ONLY when [`Factors::demote_generated`] is on.
pub fn is_generated_path(path: &str) -> bool {
    const SEGMENTS: &[&str] = &[
        "node_modules/",
        "vendor/",
        "dist/",
        "build/",
        "target/",
        "coverage/",
        "tmp/",
        ".yarn/",
    ];
    const SUFFIXES: &[&str] = &[
        ".min.js",
        ".min.css",
        ".map",
        "db/schema.rb",
        "db/structure.sql",
        "package-lock.json",
        "yarn.lock",
        "Cargo.lock",
    ];
    let lower = path.to_lowercase();
    SEGMENTS
        .iter()
        .any(|s| lower.starts_with(s) || lower.contains(&format!("/{s}")))
        || SUFFIXES.iter().any(|s| lower.ends_with(&s.to_lowercase()))
}

/// The multiplier [`Factors::demote_generated`] applies to a generated
/// path's score — a demotion, never an exclusion: the file is still
/// findable, it just stops crowding out authored code.
pub const DEMOTE_GENERATED: f64 = 0.5;

/// V71-D1 — one hit's HONEST ranking decomposition (`explain:1`).
///
/// The box has no cross-lane fusion: sections are fixed and never
/// interleaved, so there is no single additive score across lanes to
/// explain, and this struct deliberately does not invent one. It reports
/// what actually happened: which LANE produced the hit, its RANK within
/// that lane's section, the hard [`MatchTier`] it sits in, the lane's own
/// base score, and each ACTIVE factor as a signed delta or a multiplier.
/// This is the Elasticsearch `_explain` lesson (explain the production
/// query, not a reconstruction) crossed with the RRF caveat the research
/// records: with rank-based fusion an explain surface must explain RANKS
/// AND MULTIPLIERS, never pretend to one additive number.
///
/// A factor whose flag is off does not appear here AT ALL — absence means
/// "not applied", never "applied and neutral" (kb's MI-W5.R decomposition
/// discipline).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Explain {
    pub lane: &'static str,
    /// 1-based position within this lane's section, filled in after the
    /// sort — `0` while the hit is still being scored.
    pub rank: usize,
    pub tier: MatchTier,
    /// The lane's own matcher score before any factor.
    pub base: f64,
    pub factors: Vec<ExplainFactor>,
    /// `base` with every listed factor applied, in listed order.
    pub final_score: f64,
}

/// One applied factor on an [`Explain`] — `kind` says how it combined, so a
/// reader can reproduce the arithmetic rather than trust the total.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ExplainFactor {
    pub name: &'static str,
    /// `"delta"` (added to the running score) or `"multiplier"`.
    pub kind: &'static str,
    pub value: f64,
    pub why: &'static str,
}

impl Explain {
    pub fn new(lane: &'static str, tier: MatchTier, base: f64) -> Self {
        Self {
            lane,
            rank: 0,
            tier,
            base,
            factors: Vec::new(),
            final_score: base,
        }
    }

    pub fn delta(&mut self, name: &'static str, value: f64, why: &'static str) {
        self.factors.push(ExplainFactor {
            name,
            kind: "delta",
            value,
            why,
        });
    }

    pub fn multiplier(&mut self, name: &'static str, value: f64, why: &'static str) {
        self.factors.push(ExplainFactor {
            name,
            kind: "multiplier",
            value,
            why,
        });
    }

    pub fn finish(&mut self, final_score: f64) {
        self.final_score = final_score;
    }
}

/// V71-D1 — a hit's STABLE id: `h-` plus 12 hex of a blake3 over the parts
/// that ADDRESS it. An agent can name a hit in a later turn (`kb-code
/// search explain <hit_id>`, a set entry, a finding's cite) and get the same
/// id back for the same location.
///
/// v1 keys on LOCATION (lane, repo, path, and a per-lane anchor such as a
/// symbol name or a line number), NOT on blob content — so it is stable
/// across re-runs but is deliberately NOT self-invalidating when the file
/// changes underneath it. The research's `h(blob_sha, byte_start, byte_end)`
/// form (which would let a stale id answer `gone: blob changed`) needs the
/// blob hash carried on every lane's hit shape, which the files/symbols
/// lanes do not have in hand today; that is a named follow-up, not a
/// silently weaker promise.
pub fn hit_id(lane: &str, repo: &str, path: &str, anchor: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in [lane, repo, path, anchor] {
        hasher.update(part.as_bytes());
        hasher.update(b"\x1f");
    }
    format!("h-{}", &hasher.finalize().to_hex()[..12])
}

#[cfg(test)]
mod mod_tests {
    use super::*;

    #[test]
    fn hit_id_is_stable_and_distinguishes_every_part() {
        let a = hit_id("files", "kb", "src/lib.rs", "");
        assert_eq!(a, hit_id("files", "kb", "src/lib.rs", ""));
        assert_eq!(a.len(), 14, "h- + 12 hex: {a}");
        assert!(a.starts_with("h-"));
        // Each component participates — no two different locations collide
        // by construction of the delimiter.
        assert_ne!(a, hit_id("symbols", "kb", "src/lib.rs", ""));
        assert_ne!(a, hit_id("files", "other", "src/lib.rs", ""));
        assert_ne!(a, hit_id("files", "kb", "src/other.rs", ""));
        assert_ne!(a, hit_id("files", "kb", "src/lib.rs", "42"));
        // The delimiter is load-bearing: ("ab","c") must not equal ("a","bc").
        assert_ne!(
            hit_id("files", "ab", "c", ""),
            hit_id("files", "a", "bc", "")
        );
    }

    #[test]
    fn factors_default_to_the_documented_posture() {
        let f = Factors::default();
        assert!(f.frecency, "shipped on since W2.1");
        assert!(!f.demote_generated, "unmeasured — MI-W5.R says ship it off");
        assert!(
            f.lexical_rarity,
            "replaces ORDER BY path, not a learned signal"
        );
    }

    #[test]
    fn generated_paths_are_recognised_at_any_depth() {
        assert!(is_generated_path("node_modules/react/index.js"));
        assert!(is_generated_path("app/assets/vendor/jquery.js"));
        assert!(is_generated_path("db/schema.rb"));
        assert!(is_generated_path("public/app.min.js"));
        assert!(!is_generated_path("app/models/order.rb"));
        // A file that merely MENTIONS a generated segment is not one.
        assert!(!is_generated_path("app/models/vendor.rb"));
    }
}
