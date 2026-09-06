//! Daemon state — multi-kb shape from day one (the Plan agent's call:
//! refactor cost too high to defer, even though v0.0.1 typically has one
//! kb).

use chrono::{DateTime, Utc};
use kb_core::atlas::LayoutKind;
use kb_core::config::{AtlasSection, KbConfig, RateLimitSection, ShareSection, UiSection};
use kb_core::docs_query::{DocRow, FacetBucket};
use kb_core::embed::Embedder;
use kb_core::events::EventBus;
use kb_core::history::{QueriesRing, RunsRing};
use kb_core::ids::SourceSlug;
use kb_core::paths::KbPaths;
use kb_core::storage::lance::DocSummary;
use kb_core::storage::StorageHandle;
use kb_core::types::KbName;
use kb_core::watcher::Watcher;
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;

/// Parse a kb.toml `[server] trusted_proxies` list into `IpAddr`s,
/// dropping (with a warn log) any entry that doesn't parse. Shared by
/// both `serve_*` entrypoints so the auth, rate-limit, and outbound-scrub
/// layers all see the same trusted set.
pub fn parse_trusted_proxies(raw: &[String]) -> Vec<IpAddr> {
    raw.iter()
        .filter_map(|s| match s.trim().parse::<IpAddr>() {
            Ok(ip) => Some(ip),
            Err(e) => {
                tracing::warn!(entry = %s, error = %e, "ignoring invalid trusted_proxies entry");
                None
            }
        })
        .collect()
}

/// Per-kb runtime context. Held as a value in `KbHandles::kbs`.
pub struct KbContext {
    pub kb_name: KbName,
    pub source_path: PathBuf,
    pub source_slug: SourceSlug,
    pub storage: StorageHandle,
    pub bus: Arc<EventBus>,
    /// G7 — back-pressured ingest sink for this kb. Route handlers that need
    /// to (re)index a file (operator reindex, quarantine restore, note/memory
    /// nudges) push `WatchWork` through this instead of emitting `watch.*` to
    /// the bus, so the work reaches the indexer's dedicated channel with
    /// back-pressure (and still mirrors to the bus for observability).
    pub ingest: kb_core::indexer::IngestSink,
    /// Owns the `notify` watcher; dropped when the daemon shuts down.
    pub _watcher: Arc<Watcher>,
    /// Per-kb embedder. `None` if the kb has no `embedding_model` set in
    /// kb.toml — in that case, mode=hybrid|semantic search returns 400.
    pub embedder: Option<Arc<Mutex<Embedder>>>,
    /// SQ4 — optional cross-encoder reranker (opt-in per kb via
    /// `reranker_model`). `None` when unconfigured or when the model failed
    /// to load; the search route then skips the rerank stage. Driven over
    /// IPC (a `kb-embedder --reranker` subprocess) so the daemon links no ONNX.
    pub reranker: Option<Arc<Mutex<kb_core::embed_ipc::RerankerClient>>>,
    /// SQ5 — whether this kb uses passage/chunk embeddings. When true the
    /// search vector arm queries the `artifact_chunks` table (max-pooled to
    /// per-doc) instead of the doc-level embedding, so semantic search sees
    /// the whole document rather than the first ~400 words.
    pub chunked: bool,
    /// GS-track — opt-in graph-degree ranking signal weight
    /// (`[kb.*] graph_boost` in kb.toml). `None` = off (the default).
    /// Read by the hybrid search path, which adds
    /// `fusion::apply_graph_boost` over `Storage::edge_counts` after the
    /// title boost. Query-time only; no reindex on flip.
    pub graph_boost: Option<f32>,
    /// GC-D1 — opt-in typo tolerance for this kb's keyword search arm
    /// (`[kb.*.search] typo_tolerance` in kb.toml). `false` = off (the
    /// default). Read by the `mode=keyword` route and the BM25 half of
    /// `mode=hybrid`; query-time only, no reindex on flip.
    pub typo_tolerance: bool,
    /// v0.3 G3 — outbound scrubbing config + lazily-compiled regexes.
    /// `None` means no [outbound] section in kb.toml (the v0.0.1
    /// behavior). The artifact serve handler reads this when the
    /// request looks non-loopback.
    pub outbound: Option<Arc<OutboundCache>>,
    /// v0.5 P2 — per-kb atlas overrides resolved at boot. The atlas
    /// route handler passes these to `kb_core::atlas::recompute_for_kb_with`.
    pub atlas: AtlasOverrides,
    /// W2.9 — per-kb resurfacing-queue scoring weights, resolved at boot
    /// from `[kb.*.resurface]` via
    /// `kb_core::resurface::ResurfaceWeights::from_section`. `Default` when
    /// unset. `routes/resurface.rs` scores with this AND echoes it on the
    /// wire (`ResurfaceResponse.weights`) so the CLI/SPA explain renderers
    /// never hardcode a mirror of the shipped constants.
    pub resurface: kb_core::resurface::ResurfaceWeights,
    /// v0.6 R1 — per-kb run history. Fed by a background task that
    /// subscribes to the firehose and routes `index.start`/`index.complete`
    /// envelopes. Exposed via `GET /api/kb/{kb}/runs`.
    pub runs: Arc<RunsRing>,
    /// v0.6 R1 — per-kb query history. Same lifecycle as `runs`;
    /// fed from `query` envelopes. Exposed via `GET /api/kb/{kb}/queries`.
    pub queries: Arc<QueriesRing>,
    /// Skip-patterns from `[kb.foo] skip_patterns` — threaded so the
    /// reconciler + the explicit reindex endpoint honour the same
    /// exclusion rule as the live watcher. Pre-v0.7.x the reconciler
    /// ignored these (deep-review K1).
    pub skip_patterns: Vec<String>,
    /// R1 — snapshot of the most recent reconciler pass.
    /// `None` until the first pass completes (or stays `None` forever
    /// when `reconcile_secs = 0`). Written by the reconcile loop in
    /// `serve_*`, read by `routes/stats.rs` to expose
    /// `last_reconcile_*` fields on `KbStats`.
    pub last_reconcile: Arc<Mutex<Option<kb_core::indexer::ReconcileSummary>>>,
    /// R1 — resolved `[indexer] reconcile_secs` for this kb. Surfaced
    /// alongside `last_reconcile` so the SPA / CLI can render
    /// staleness ("reconcile is older than 5× the interval → red").
    pub reconcile_secs: u64,
    /// v0.9 M2 — `[kb.foo] memory_scope` ("global" | "project"), threaded
    /// from the `KbSection` so the recall handler resolves in-scope
    /// memory corpora without re-reading config. `None` → not a memory
    /// corpus.
    pub memory_scope: Option<String>,
    /// R0-opt-in — `[kb.foo] default_search_category`, threaded from the
    /// `KbSection` so `routes/search.rs`'s single-kb (`scope=one`) path can
    /// default a `?category`-less request to it (e.g. `"memory-session"` on
    /// a sessions corpus). `None` → no default, today's behavior. Never
    /// consulted by the federated `scope=all` path (R0 stays
    /// default-exclude there — see architecture invariant #11).
    pub default_search_category: Option<String>,
    /// CT-F5 — resolved `[kb.foo.slo]` targets, threaded from the
    /// `KbSection` at bring-up so `routes/slo.rs` never re-reads config.
    /// Every field inside is independently optional; the all-`None` default
    /// (a kb with no `[slo]` section) still produces a full four-indicator
    /// report — every indicator measured, every status `unknown` for want of
    /// a target. SURFACED, NEVER ENFORCED: nothing anywhere branches on a
    /// missed target, so this field is read by exactly one route and one CLI
    /// verb and by nothing on any hot path.
    pub slo_targets: kb_core::slo::SloTargets,
    /// DCB — `[kb.foo] code_url`, threaded from the `KbSection` so
    /// `routes/kbs.rs`'s `KbSummary` can surface it and the SPA's Code
    /// section knows where to send the browser's own fetch. kb itself never
    /// opens an HTTP client to this URL (invariant #2/#4). `None` → this
    /// corpus isn't linked to a kb-code daemon.
    pub code_url: Option<String>,
    /// v0.13 — per-kb decay-policy override resolved at boot from
    /// kb.toml `[kb.foo] decay_policy = "strict|balanced|loose"`. Only
    /// applies when this corpus is a memory corpus; the recall route
    /// reads it on the per-corpus hits before falling back to the
    /// daemon-wide cell in `KbHandles.memory_policy`.
    pub memory_decay_policy: Option<kb_core::memory::DecayPolicy>,
    /// Track V — the git repo root that owns this corpus, found by walking
    /// up from `source_path` for a `.git`. `None` when the corpus isn't
    /// under git (deployed container, plain dir) → the version timeline
    /// falls back to index snapshots. Computed once at bring-up.
    pub git_root: Option<PathBuf>,
    /// Track V — resolved `[kb.*] versions` mode (auto|git|index|both|off).
    /// Drives the Versions/Diff route + gates index-snapshot capture.
    pub versions_mode: kb_core::vcs::VersionsMode,
    /// RP-track — resolved `[kb.*] reading_progress` (default true). When
    /// false, `POST …/history/reading` no-ops (204) so nothing per-section
    /// is captured; scroll-resume + the TOC mini-spy stay on.
    pub reading_progress: bool,
    /// P1 — the gallery row-set memo (see [`GalleryCache`]). `None` until
    /// the first `GET /docs`; invalidated implicitly by the storage
    /// actor's index-generation counter (a hit requires the stored
    /// generation to equal `storage.index_generation()`), so no explicit
    /// eviction path is needed.
    pub gallery_cache: Arc<Mutex<Option<GalleryCache>>>,
    /// Wave-2 — per-(kb, index-generation) precomputed wikilink candidate
    /// index (see [`LinksCache`]). Shares the gallery memo's one
    /// `list_docs(u32::MAX)` scan, then caches the per-doc lowercased
    /// title/basename keys so the `[[` typeahead (fired per keystroke) and
    /// note-link resolution filter a memo instead of re-scanning +
    /// re-lowercasing the corpus each request. Same generation-keyed
    /// discipline as `gallery_cache` (invariant #15).
    pub links_cache: Arc<Mutex<Option<LinksCache>>>,
    /// Wave-2 — per-(kb, index-generation) memo of the corpus link graph
    /// (`link_pairs()`, i.e. `SELECT ... FROM edges WHERE kind='link'`), so
    /// concurrent `GET /edges` opens (fired on every reader navigation)
    /// share one full edges-table scan instead of each re-running it on the
    /// single storage actor. The generation bumps on edge mutations
    /// (`record_edges → bump_generation`), so a hit is always fresh.
    pub edges_cache: Arc<Mutex<Option<EdgesCache>>>,
    /// SC3 — the `/facets` aggregate memo (see [`FacetsCache`]). `None`
    /// until the first `GET /facets`; invalidated implicitly by the
    /// storage actor's index-generation counter, same discipline as
    /// `gallery_cache` (invariant #15).
    pub facets_cache: Arc<Mutex<Option<FacetsCache>>>,
    /// M-a — the `/atlas/points` full-corpus memo (see [`AtlasPointsCache`]).
    /// `None` until the first `GET /atlas/points`; invalidated implicitly by
    /// the storage actor's index-generation counter, same discipline as
    /// `gallery_cache`/`facets_cache` (invariant #15).
    pub atlas_points_cache: Arc<Mutex<Option<AtlasPointsCache>>>,
    /// P3 — single-flight guard for `POST /atlas/recompute`, holding the
    /// in-flight recompute's run id (`None` when idle). The atlas layout
    /// is the most expensive per-kb job (O(n·d) PCA up to O(n²·d) UMAP
    /// below the cap), so a concurrent recompute is wasted work. The route
    /// checks this under the lock: if one is already running it returns
    /// 202 with THAT run id, so the second caller tails the same
    /// `atlas.recompute.complete` event instead of starting a second
    /// layout. The spawned task resets it to `None` via a drop-guard (so a
    /// panic can't strand it).
    pub atlas_recompute: Arc<Mutex<Option<String>>>,
    /// SC5 — the same resolved per-kb extension→pipeline map (X1) already
    /// installed on `ingest` + the indexer at bring-up. Every route deciding
    /// render-vs-raw (or Markdown-vs-HTML edit syntax) off a file's extension
    /// reads THIS instead of the hardcoded `kb_core::indexer::is_markdown`, so
    /// a `[indexer.indexable_extensions]` mapping (e.g. `txt = "markdown"`)
    /// stays consistent from index-time parse dispatch through to serve.
    pub ext_map: kb_core::extmap::ExtensionMap,
    /// FU1 — shared indexer content-hash dedup cache (same `Arc` the indexer
    /// task pre-populates and mutates). Relocate rekeys `old_id → new_id`
    /// after a successful move so the post-rename `Created` event hits the
    /// pre-gate without a redundant re-embed (invariant #27). Hold the
    /// mutex only in short synchronous scopes (invariant #15).
    pub dedup: kb_core::indexer::DedupCache,
}

/// P1 — a per-(kb, index-generation) snapshot of the gallery's decorated
/// row-set so concurrent `GET /docs` requests share ONE
/// `list_docs(u32::MAX)` scan + `edge_counts()` GROUP BY instead of each
/// re-running both (the lone C-grade scale cliff). Single-slot: only the
/// latest generation is useful, so [`routes::docs::list`] just rebuilds on
/// a generation change rather than keeping an LRU. `rows` + `edge_counts`
/// are `Arc`-shared so a cache hit is two `Arc::clone`s and the per-request
/// filter/sort/paginate runs over borrowed refs into `rows` (cloning only
/// the visible page).
pub struct GalleryCache {
    pub generation: u64,
    pub rows: Arc<Vec<DocRow>>,
    pub edge_counts: Arc<HashMap<String, (u32, u32)>>,
}

/// Wave-2 — a per-(kb, index-generation) snapshot of the corpus wikilink
/// candidate set. Derived from the SAME `list_docs` scan the gallery memo
/// runs (`gallery_snapshot`), then augmented with the per-doc lowercased
/// title/basename keys the `[[` typeahead ranks on. The suggest handler and
/// note-link resolution borrow this instead of re-scanning + re-lowercasing
/// the whole corpus per request (the typeahead fires per keystroke).
/// Single-slot like [`GalleryCache`]; a generation change rebuilds it.
pub struct LinksCache {
    pub generation: u64,
    pub index: Arc<LinksIndex>,
}

/// The memoised corpus wikilink index: `entries` is the suggest/metadata
/// projection, `candidates` the parallel `DocLite` set `kb_core::links`
/// resolution runs over (same doc order; `entries[i].id ==
/// candidates[i].id`). Both are built once per index-generation so neither
/// the `[[` typeahead nor note-link resolution re-scans or re-clones the
/// corpus per request.
pub struct LinksIndex {
    pub entries: Vec<LinkCandidate>,
    pub candidates: Vec<kb_core::links::DocLite>,
}

/// One corpus artifact projected to everything the wikilink surface needs:
/// suggest payload (id/title/`source_relative`/is_note), resolved-link
/// metadata, and the precomputed lowercased ranking keys. Built once per
/// index-generation in [`LinksCache`].
pub struct LinkCandidate {
    pub id: String,
    pub title: String,
    pub source_relative: String,
    pub is_note: bool,
    pub title_lower: String,
    pub basename_lower: String,
}

/// Wave-2 — a per-(kb, index-generation) snapshot of the `kind='link'`
/// edge pairs, so concurrent `GET /edges` opens share one edges-table scan.
/// `pairs` is `Arc`-shared; a cache hit is one `Arc::clone`.
pub struct EdgesCache {
    pub generation: u64,
    pub pairs: Arc<Vec<(String, String)>>,
}

/// SC3 — a per-(kb, index-generation) snapshot of the `/facets` aggregate
/// (distinct category/status/severity values + counts), so concurrent
/// `GET /facets` requests share one set of folds instead of each re-running
/// `aggregate_categories`/`aggregate_statuses`/`aggregate_severities`. On a
/// miss it rebuilds via [`crate::routes::docs::gallery_snapshot`], so the
/// two memos share the SAME `list_docs(u32::MAX)` scan (invariant #15) —
/// mirrors [`LinksCache`]. Single-slot; a generation change rebuilds it.
pub struct FacetsCache {
    pub generation: u64,
    pub categories: Arc<Vec<FacetBucket>>,
    pub statuses: Arc<Vec<FacetBucket>>,
    pub severities: Arc<Vec<FacetBucket>>,
}

/// M-a — a per-(kb, index-generation) snapshot of the FULL-corpus atlas
/// point set (`list_docs_with_atlas(u32::MAX)`), so `GET
/// /api/kb/{kb}/atlas/points` — the whole-corpus map read the SPA's paged
/// `?projection=atlas` gallery call can't honestly serve (`MAX_ENVELOPE_LIMIT`
/// caps that path at 500) — doesn't re-run the uncached full scan on every
/// request. Same single-slot, generation-keyed discipline as
/// [`GalleryCache`]/[`FacetsCache`] (invariant #15): a hit requires the
/// stored generation to equal `storage.index_generation()`; `docs` is
/// `Arc`-shared so a hit is one clone.
pub struct AtlasPointsCache {
    pub generation: u64,
    pub docs: Arc<Vec<DocSummary>>,
}

/// Resolved atlas overrides for a single kb. Loaded once at boot from
/// the kb.toml `[kb.foo.atlas]` section + cached on KbContext.
#[derive(Debug, Clone, Copy, Default)]
pub struct AtlasOverrides {
    pub k: Option<usize>,
    pub layout: AtlasLayoutChoice,
}

/// Wraps `kb_core::atlas::LayoutKind` so the default lives here (the
/// kb-core enum doesn't impl Default — picking one would couple it to
/// the v0.5 default choice + complicate v0.6+ migrations).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AtlasLayoutChoice {
    #[default]
    Umap,
    Pca,
}

impl AtlasLayoutChoice {
    pub fn to_kind(self) -> LayoutKind {
        match self {
            AtlasLayoutChoice::Umap => LayoutKind::Umap,
            AtlasLayoutChoice::Pca => LayoutKind::Pca,
        }
    }
}

impl AtlasOverrides {
    /// Resolve from a kb.toml `[atlas]` section. Unknown layout strings
    /// log a warning + fall back to UMAP.
    pub fn from_section(section: Option<&AtlasSection>) -> Self {
        let Some(s) = section else {
            return Self::default();
        };
        let layout = match s.layout.as_deref().map(|l| l.to_ascii_lowercase()) {
            None => AtlasLayoutChoice::Umap,
            Some(ref v) if v == "umap" => AtlasLayoutChoice::Umap,
            Some(ref v) if v == "pca" => AtlasLayoutChoice::Pca,
            Some(other) => {
                tracing::warn!(
                    layout = %other,
                    "unknown [atlas] layout, falling back to umap"
                );
                AtlasLayoutChoice::Umap
            }
        };
        Self { k: s.k, layout }
    }
}

/// The outbound scrub cache + rule compilation moved to `kb_core::scrub`
/// (so the static-export engine reuses the identical strip+redact).
/// Re-exported here because `KbContext.outbound` and the boot-time
/// `from_section` call site still reference `crate::state::OutboundCache`.
pub use kb_core::scrub::{CompiledRule, OutboundCache};

/// Daemon-wide state passed to every axum handler via `State<Arc<KbHandles>>`.
/// N7: lightweight HTTP request metrics. Lives behind an `Arc` so the
/// middleware can clone-and-bump cheaply, and the ticker can read
/// snapshots without locking. Counts every /api/* request that
/// reaches the middleware — including OPTIONS preflight + 404s.
///
/// P3+P4: per-route-family counters + lock-free latency histogram
/// (13 logarithmic buckets per route). Total memory: 8 routes ×
/// (8 + 8 + 13 × 8) = ~900 bytes. The histogram is approximate but
/// stable: cumulative bucket counts let `percentile` walk the
/// distribution and linearly interpolate within the spanning bucket.
#[derive(Debug, Default)]
pub struct RequestMetrics {
    /// Total requests since daemon boot.
    pub total: std::sync::atomic::AtomicU64,
    /// N8: storage actor channel depth. Snapshotted by the metrics
    /// ticker by reading the StorageHandle's mpsc::Sender capacity.
    pub storage_channel_depth: std::sync::atomic::AtomicU32,
    /// SW1: live `/api/events` HTTP consumers — incremented per stream in
    /// routes::events::get, decremented by a Drop-guard owned by the
    /// response stream (covers client disconnect, shutdown take_until,
    /// and error paths). Counts EVERY events consumer: SPA tabs / the
    /// shared worker, kb-cli watchers, test probes. Deliberately
    /// NOT `bus.sender.receiver_count()`, which would also count the
    /// daemon's internal subscribers (watcher, indexer, webhook bridge,
    /// history ring) and report a per-kb baseline offset.
    pub sse_clients: std::sync::atomic::AtomicUsize,
    /// v0.24 T1 — daemon-wide embedder health, refreshed at 1 Hz by the
    /// metrics ticker's liveness probe (the same values `metrics.tick`
    /// streams). `true` iff any embedder subprocess is currently
    /// unrecoverable (search degraded to keyword-only). Read by
    /// `GET /api/metrics` so `kb fleet status`/`kb metrics` see it
    /// without an SSE subscription; up to 1 s stale by construction.
    pub embedder_degraded: AtomicBool,
    /// v0.24 T1 — cumulative embedder-subprocess respawns across every
    /// distinct backend since boot (ticker-refreshed, 1 Hz). A climbing
    /// value is the "embedder keeps crashing" signal.
    pub embedder_respawn_count: std::sync::atomic::AtomicU64,
    /// P3+P4: per-route stats (count + latency histogram). One slot
    /// per `RouteKind` variant, indexed by `kind as usize`.
    pub by_route: [RouteMetrics; ROUTE_KIND_COUNT],
    // --- TM-track detailed layer (opt-in via `[server] metrics`) ------------
    /// Whether the detailed layer below is recorded + exposed. The coarse
    /// fields above are ALWAYS on; these only when `[server] metrics = true`.
    pub detail_enabled: AtomicBool,
    /// Per-search-stage latency (embed / bm25 / vector / hybrid), indexed by
    /// `SearchStage as usize`. Recorded in `routes::search` when detail is on.
    pub search_stages: [RouteMetrics; SEARCH_STAGE_COUNT],
    /// Per-kb request latency, keyed by kb name. PRE-SEEDED at boot from the
    /// (fixed-for-the-daemon's-life) kb set, so the middleware hot path is a
    /// lock-free `get` + atomic bump — never an insert. A request for an
    /// unknown kb just misses + skips.
    pub per_kb: HashMap<String, RouteMetrics>,
}

/// TM-track — the search pipeline stages timed individually when the detailed
/// metrics layer is on. The route already measures `embed_ms` + the
/// bm25/vector/hybrid query split per response; these aggregate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum SearchStage {
    Embed = 0,
    Bm25 = 1,
    Vector = 2,
    Hybrid = 3,
}

pub const SEARCH_STAGE_COUNT: usize = 4;

impl SearchStage {
    pub const ALL: &'static [SearchStage] = &[
        SearchStage::Embed,
        SearchStage::Bm25,
        SearchStage::Vector,
        SearchStage::Hybrid,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            SearchStage::Embed => "embed",
            SearchStage::Bm25 => "bm25",
            SearchStage::Vector => "vector",
            SearchStage::Hybrid => "hybrid",
        }
    }
}

impl RequestMetrics {
    /// Build with the detailed layer enabled/disabled + the per-kb map
    /// pre-seeded from the daemon's kb set. The coarse fields start at zero
    /// either way. Called once at boot via `KbHandles::with_metrics`.
    pub fn with_detail(enabled: bool, kb_names: impl IntoIterator<Item = String>) -> Self {
        let m = Self::default();
        m.detail_enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        let per_kb = kb_names.into_iter().map(|k| (k, RouteMetrics::default()));
        Self {
            per_kb: per_kb.collect(),
            ..m
        }
    }

    /// Is the detailed layer recorded + exposed?
    pub fn detailed_enabled(&self) -> bool {
        self.detail_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Record one search stage's latency (no-op when detail is off).
    pub fn observe_search_stage(&self, stage: SearchStage, ms: u64) {
        if !self.detailed_enabled() {
            return;
        }
        self.search_stages[stage as usize].observe(ms);
    }

    /// Record one request's latency against its kb (no-op when detail is off
    /// or the kb wasn't pre-seeded).
    pub fn observe_kb(&self, kb: &str, ms: u64) {
        if !self.detailed_enabled() {
            return;
        }
        if let Some(rm) = self.per_kb.get(kb) {
            rm.observe(ms);
        }
    }
}

/// The latency histogram primitive + bucket boundaries + percentile math
/// live in `kb_core::metrics` so kb-server's per-route request metrics and
/// kb-core's pipeline metrics share ONE implementation (no bucket-boundary
/// drift — a p95 means the same thing on every section of `/api/metrics`).
/// Re-exported here so existing call sites + tests keep their
/// `crate::state::{percentile_ms, LATENCY_BUCKET_COUNT, …}` paths.
pub use kb_core::metrics::{percentile_ms, LatencyHist, LATENCY_BUCKETS_MS, LATENCY_BUCKET_COUNT};

pub const ROUTE_KIND_COUNT: usize = 8;

/// P3: HTTP route family. Coarse enough to surface at-a-glance
/// "which subsystem is busy" without dragging an unbounded per-path
/// counter map into the middleware. Classified from the request URI
/// path by `classify_route`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum RouteKind {
    Search = 0,
    Atlas = 1,
    History = 2,
    Review = 3,
    Events = 4,
    Read = 5,
    Ops = 6,
    Other = 7,
}

impl RouteKind {
    pub const ALL: &'static [RouteKind] = &[
        RouteKind::Search,
        RouteKind::Atlas,
        RouteKind::History,
        RouteKind::Review,
        RouteKind::Events,
        RouteKind::Read,
        RouteKind::Ops,
        RouteKind::Other,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            RouteKind::Search => "search",
            RouteKind::Atlas => "atlas",
            RouteKind::History => "history",
            RouteKind::Review => "review",
            RouteKind::Events => "events",
            RouteKind::Read => "read",
            RouteKind::Ops => "ops",
            RouteKind::Other => "other",
        }
    }
}

/// Map a `/api/*` URI path to a `RouteKind`. Path is matched without
/// query string — caller passes `uri.path()` not `uri.path_and_query()`.
///
/// NB: `count_requests` is layered INSIDE `.nest("/api", …)`, so the path it
/// hands in is nest-STRIPPED (`/kb/…`, no `/api` prefix); direct callers +
/// the unit tests pass the full `/api/…`. We normalise both — drop the
/// leading slash + an optional `api/` prefix — so a route is classified the
/// same either way. (Before this, the stripped form fell through to `Other`,
/// so every per-route counter except the catch-all read zero.)
pub fn classify_route(path: &str) -> RouteKind {
    let p = path.trim_start_matches('/');
    let p = p.strip_prefix("api/").unwrap_or(p);
    // Order matters: most-specific first, then prefix matches.
    if p == "search" {
        return RouteKind::Search;
    }
    if p == "events" || p == "events.schema.json" || p.starts_with("events/") {
        return RouteKind::Events;
    }
    // GC-B3 — cross-kb zero-hit report, the `/queries` sibling of `/stats`.
    if p == "queries" || p.starts_with("queries/") {
        return RouteKind::Read;
    }
    // /kb/{kb}/<segment> — peek at the trailing segment.
    if let Some(rest) = p.strip_prefix("kb/") {
        // skip the kb name, find the segment after the next '/'
        let after_kb = rest.split_once('/').map(|(_, a)| a).unwrap_or("");
        let head = after_kb.split('/').next().unwrap_or("");
        return match head {
            "atlas" => RouteKind::Atlas,
            "history" => RouteKind::History,
            "review" => RouteKind::Review,
            // Read-side: surfaces the corpus to humans / SPAs.
            "docs" | "artifact" | "graph" | "edges" | "tags" | "folders" | "stats" | "runs"
            | "queries" => RouteKind::Read,
            // Operational mutations.
            "sources" | "errors" | "reindex" | "exclusions" => RouteKind::Ops,
            _ => RouteKind::Other,
        };
    }
    // Top-level shared metadata.
    match p {
        "identity" | "kbs" | "stats" | "settings" | "metrics" => RouteKind::Read,
        _ => RouteKind::Other,
    }
}

/// TM-track — extract the kb name from a `kb/{kb}/…` path for per-kb request
/// attribution, or `None` for non-kb-scoped paths (`/api/search`,
/// `/api/identity`, …). Normalises the same nest-stripped vs full path forms
/// as `classify_route` (see its note); the caller already strips the query
/// string (`uri.path()`).
pub fn kb_from_path(path: &str) -> Option<&str> {
    let p = path.trim_start_matches('/');
    let p = p.strip_prefix("api/").unwrap_or(p);
    let rest = p.strip_prefix("kb/")?;
    let kb = rest.split('/').next().unwrap_or("");
    if kb.is_empty() {
        None
    } else {
        Some(kb)
    }
}

/// Per-route stats: a [`LatencyHist`] (count + 13 buckets) backed by the
/// shared kb-core primitive. Thin delegating wrapper so existing call sites
/// (`.observe`, `.buckets_snapshot`, `.count()`) read unchanged.
#[derive(Debug, Default)]
pub struct RouteMetrics {
    pub hist: LatencyHist,
}

impl RouteMetrics {
    /// Observe one request with elapsed `ms`.
    pub fn observe(&self, ms: u64) {
        self.hist.observe(ms);
    }

    /// Read the current cumulative bucket counts as `[u64; 13]`.
    pub fn buckets_snapshot(&self) -> [u64; LATENCY_BUCKET_COUNT] {
        self.hist.buckets_snapshot()
    }

    /// Total observations recorded.
    pub fn count(&self) -> u64 {
        self.hist.count()
    }
}

pub struct KbHandles {
    pub daemon_name: String,
    pub paths: Arc<KbPaths>,
    pub started_at: DateTime<Utc>,
    /// `BTreeMap` (alphabetical iteration order) so the multi-kb
    /// artifact serve path resolves kbs deterministically. Don't switch
    /// back to `HashMap` — random iteration breaks subdomain dispatch
    /// (route picks the wrong kb half the time when two kbs are loaded).
    pub kbs: BTreeMap<KbName, KbContext>,
    /// Live UI prefs (theme/accent/density). PATCH /api/settings mutates;
    /// GET /api/settings reads. v0.1 keeps it in-memory only — durable
    /// kb.toml persistence defers to v0.2 (the SPA's localStorage is the
    /// durable cache for now).
    pub ui: Arc<Mutex<UiSection>>,
    /// Path to the SPA dist directory (`web/dist/`). Resolved at startup;
    /// `None` when the daemon was started without a built SPA — in that
    /// case the parent-origin fallback returns 404 problem+json.
    pub spa_dist: Option<PathBuf>,
    /// Per-kb serializer for review mutations. Closes the TOCTOU window
    /// between load and `save_atomic`'s rename, and serialises the
    /// stale-anchor sidecar prune (which is per-kb). Sharded per-kb so
    /// comment edits in different kbs proceed concurrently while every
    /// mutation — and the shared sidecar — stays serialised WITHIN a kb.
    /// Acquire via `review_lock_for(&kb_name)`. See root CLAUDE.md
    /// invariant #6.
    pub review_locks: std::sync::Mutex<std::collections::BTreeMap<KbName, Arc<AsyncMutex<()>>>>,
    /// W2.15b — per-kb serializer for the `.proposals/` sidecar dir. Same
    /// sharded-map shape as `review_locks` above (`proposal_lock_for`
    /// mirrors `review_lock_for`), but a DISTINCT lock: proposal writes
    /// never touch `.review/` and vice versa, so the two never contend.
    pub proposal_locks: std::sync::Mutex<std::collections::BTreeMap<KbName, Arc<AsyncMutex<()>>>>,
    /// v0.4 A2 — bearer-token auth state. Loaded once at startup from
    /// `paths.token_file()`. `None` token disables auth entirely (the
    /// v0.3 personal-mode default); `Some(token)` enforces it on
    /// non-loopback `/api/*` requests via the `auth_bearer` layer.
    pub auth: Arc<AuthConfig>,
    /// v0.5 Q1 — per-route rate-limit overrides resolved at boot from
    /// kb.toml `[server.rate_limit]`. Each field is the requests-per-
    /// minute cap for the corresponding endpoint family. Defaults to
    /// 60/min/token (the v0.4 baked-in policy).
    pub rate_limits: RateLimits,
    /// PF-R1 — resolved `[server] fanout_cap` (default 8, byte-identical to
    /// the pre-existing hardcoded `routes::FANOUT_CAP`). Read directly by
    /// federated (`scope=all`) route handlers as the `cap` argument to
    /// `routes::buffered_join` (invariant #28) instead of the constant, so
    /// an operator can dial per-corpus fan-out concurrency without a
    /// rebuild. Resolved once at boot/restart by `with_config` (CE,
    /// invariant #13 — a config edit restarts in-process and re-resolves
    /// this the same way `operator`/`rate_limits` do).
    pub fanout_cap: usize,
    /// v0.6 — origin config (artifact host suffix + parent origin)
    /// resolved at boot from `[server] artifact_host_suffix` /
    /// `parent_origin`. The dispatcher reads `artifact_host_suffix` to
    /// pick artifact vs. SPA; `origin_allowlist` reads both for CSRF.
    pub origin: Arc<OriginConfig>,
    /// `kb share` publishing config (`[share]`), resolved at boot. The
    /// share route reads it to build the Cloudflare/GitHub backend; the
    /// host API tokens come from the daemon's own environment, not here.
    pub share: Arc<ShareSection>,
    /// N7: HTTP request counter. Bumped by `middleware::count_requests`
    /// on every incoming /api/* request. A background ticker in
    /// `main::spawn_metrics_ticker` snapshots this every second and
    /// emits a `metrics.tick` SSE event so consumers (the TUI's
    /// TRAFFIC tab) can show a real req/sec metric.
    pub metrics: Arc<RequestMetrics>,
    /// TM-track — daemon-wide ingest-pipeline metrics (indexer throughput,
    /// index-side embed latency, storage-actor queue-wait + handler-time).
    /// Shared into every kb's storage actor + indexer at `bring_up_kb`; read
    /// by `GET /api/metrics`. Enabled iff `[server] metrics = true`; disabled
    /// it's a near-free no-op on the write path.
    pub pipeline: Arc<kb_core::metrics::PipelineMetrics>,
    /// v0.7.1 H5 — the single daemon-wide event bus. Every kb's watcher,
    /// indexer, and history task share it; `GET /api/events` subscribes
    /// to it directly. One bus = one monotonic id space, so a merged
    /// SSE stream has globally-unique ids and `Last-Event-ID` resume
    /// works. (Pre-H5 each kb had its own bus with ids from 1, so two
    /// kbs both emitted `id: 42` and resume dropped/duplicated events.)
    /// Each `KbContext.bus` is a clone of this same `Arc`.
    pub bus: Arc<EventBus>,
    /// Broadcasts daemon shutdown to long-lived handlers. `serve()` flips
    /// it to `true` when SIGTERM/SIGINT arrives; the `/api/events` SSE
    /// handler watches it (`take_until`) and ends its stream so axum's
    /// graceful drain completes — an SSE subscription never finishes on
    /// its own, so without this the drain blocks forever and `kb daemon
    /// stop` times out. A bounded backstop in `serve()` force-exits if the
    /// drain still overruns. Receivers via `state.shutdown.subscribe()`.
    pub shutdown: tokio::sync::watch::Sender<bool>,
    /// CE — the full config this daemon is serving. Held behind an async
    /// `RwLock` so `GET /api/config` can serialise a snapshot and
    /// `PUT /api/config` can swap in the validated edit before tripping a
    /// restart. The authoritative copy on the next boot is re-read from
    /// `config_path`; this cell just reflects what's running now.
    pub config: Arc<tokio::sync::RwLock<KbConfig>>,
    /// Cached `[identity].operator` (default `"operator"`). Kept in sync
    /// with `with_config` / boot; config edits restart in-process (#13).
    /// `operator_user()` serves the paths with no per-request Identity:
    /// startup backfill, legacy no-user row ownership, all-users defaults.
    operator: String,
    /// CE — the resolved path the config was loaded from (explicit
    /// `--config <file>` or the home-local `~/.config/kb/kb.toml`
    /// default). `PUT /api/config` writes edits back HERE — to whichever
    /// file this daemon actually loaded — via `save_preserving`.
    pub config_path: PathBuf,
    /// CE — set by `PUT /api/config` before it trips `shutdown`, so the
    /// `serve_loop` boot loop returns `ServeOutcome::Restart` (re-read
    /// config + rebuild) instead of exiting. Reset each loop iteration.
    pub restart_requested: Arc<AtomicBool>,
    /// CE — set by the OS-signal handler (`shutdown_signal`). Takes
    /// priority over `restart_requested` in the outcome decision so a
    /// SIGTERM that lands mid-restart-drain still stops the daemon
    /// (`kb daemon stop` wins over a racing config save).
    pub terminate: Arc<AtomicBool>,
    /// v0.10 M2 — memory recall's auto-decay aggressiveness, daemon-wide.
    /// `rerank_with_policy` reads this on every /api/memory/recall call.
    /// Per-daemon (resets to default on restart); kb.toml-persisted
    /// per-kb override is v0.11.
    pub memory_policy: Arc<tokio::sync::RwLock<kb_core::memory::DecayPolicy>>,
    /// MI-W2.4c — EPOCH HONESTY marker: the unix timestamp this daemon
    /// first became capable of soft-forgetting (MI-W2.3) instead of hard-
    /// deleting. Set ONCE per `KB_HOME` (persisted, `ensure_tombstone_era`)
    /// and never moved forward. `kb memory log` / `kb diff --between`
    /// compare a requested window's start against this and print a caveat
    /// when it predates it — see `paths::KbPaths::tombstone_era_file`.
    pub tombstone_era_started_unix: i64,
    /// Daemon-wide LRU of query embeddings, keyed on (model, query).
    /// `/api/search` and `/api/memory/recall` consult it before paying
    /// the ~50–100 ms IPC embed round-trip. See
    /// `crate::embed_cache::QueryEmbedCache`.
    pub embed_cache: Arc<crate::embed_cache::QueryEmbedCache>,
    /// v0.14 S4 — small LRU memoising the per-session
    /// `extract_touched_ids` scan. Keyed on `(kb, artifact_id,
    /// mtime_unix)`; an mtime bump invalidates the entry naturally.
    /// Backs the SPA atlas-overlay's "show sessions" toggle which
    /// re-fetches the same top-N transcripts on each render.
    pub touches_cache: Arc<crate::touches_cache::TouchesCache>,
    /// W3.R-b — small LRU memoising the per-session replay timeline
    /// (`GET /api/sessions/{sid}/replay`). Keyed on `(kb, artifact_id,
    /// mtime_unix, scrub-posture)`; a re-indexed capture invalidates on
    /// mtime and a redaction-posture change never reads another posture's
    /// entry (invariant #4). Process-local — NOT a storage message, never
    /// bumps the index generation (#15). See `crate::replay_cache`.
    pub replay_cache:
        Arc<crate::replay_cache::ReplayCache<Arc<crate::routes::sessions::ResolvedReplay>>>,
    /// W2/R1 — small LRU memoising the per-session `session-view/1` IR
    /// (`GET /api/sessions/{sid}/view`). Same key shape + posture discipline
    /// as `replay_cache` (reused verbatim, `ReplayCache` is generic over its
    /// value); `?fields=`/`?turns=` projection is applied AFTER the cache
    /// hit, so every projection of one capture shares ONE cached IR build.
    pub view_cache:
        Arc<crate::replay_cache::ReplayCache<Arc<kb_core::sessions::view::SessionView>>>,
    /// W7 (R15/LF-4) — the `(sid, inode)`-keyed live-tail carry LRU behind
    /// `GET /api/sessions/{sid}/live`. See `crate::live_tail_cache` — never
    /// load-bearing, a miss always falls back to a stateless bootstrap.
    pub live_tail_cache: Arc<crate::live_tail_cache::LiveTailCache>,
    /// LSC-2 (design §6 "Storage: nothing") — the daemon-wide, in-memory
    /// live-beat registry behind `POST /api/sessions/beat` / `GET
    /// /api/sessions/live-status`. NOT per-kb (a beat arrives before any
    /// capture exists, so there is no kb to attribute it to yet) — see
    /// `crate::live_registry`.
    pub live_registry: Arc<crate::live_registry::LiveRegistry>,
    /// SL2 (kb-slate/1, design §7 "Storage" → "Lock") — the per-slate
    /// mutation lock + cached head seq + per-session rate window. Daemon-
    /// wide, NOT per-kb: a slate keys on a PROJECT, and a project maps to
    /// several kbs (or none). See `crate::slate_registry`.
    pub slates: Arc<crate::slate_registry::SlateRegistry>,
}

/// v0.6 — resolved origin config (artifact subdomain suffix + parent
/// origin). Lifted out of the per-request middleware so it's
/// configurable via kb.toml `[server]`.
#[derive(Debug, Clone)]
pub struct OriginConfig {
    /// Suffix such as `.artifacts.localhost` or `.artifacts.example.com`.
    pub artifact_host_suffix: String,
    /// Trusted parent origin, e.g. `http://localhost:4000` or
    /// `https://kb.example.com`. The same-origin Host==Origin check
    /// covers the production case regardless; this field handles the
    /// dev case where the SPA dev server runs on a different port.
    pub parent_origin: String,
    /// v0.7.1 — reverse-proxy IPs trusted to set `X-Forwarded-For`.
    /// Resolved once at boot from `[server] trusted_proxies`; shared
    /// (via the `Arc`) with `AuthConfig` + the rate limiters so every
    /// security layer evaluates the same trusted set. The artifact
    /// serve handler reads it for the outbound-scrub loopback check.
    pub trusted_proxies: Arc<Vec<IpAddr>>,
}

impl Default for OriginConfig {
    fn default() -> Self {
        Self {
            artifact_host_suffix: kb_core::iframe::DEFAULT_HOST_SUFFIX.to_string(),
            parent_origin: "http://localhost:4000".to_string(),
            trusted_proxies: Arc::new(Vec::new()),
        }
    }
}

/// Resolved per-route rate-limit caps. `build_router` constructs one
/// `RateLimiter` per field + stacks each on the matching route subset.
#[derive(Debug, Clone, Copy)]
pub struct RateLimits {
    pub search_per_min: u32,
    pub atlas_per_min: u32,
    pub review_per_min: u32,
    pub history_per_min: u32,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self {
            search_per_min: RateLimitSection::DEFAULT_PER_MIN,
            atlas_per_min: RateLimitSection::DEFAULT_PER_MIN,
            review_per_min: RateLimitSection::DEFAULT_REVIEW_PER_MIN,
            history_per_min: RateLimitSection::DEFAULT_HISTORY_PER_MIN,
        }
    }
}

impl RateLimits {
    pub fn from_section(section: Option<&RateLimitSection>) -> Self {
        let Some(s) = section else {
            return Self::default();
        };
        Self {
            search_per_min: s.search_per_min(),
            atlas_per_min: s.atlas_per_min(),
            review_per_min: s.review_per_min(),
            history_per_min: s.history_per_min(),
        }
    }
}

/// Bearer-token auth state. Read once at startup; rotation requires
/// a daemon restart (kb token rotate + systemctl restart kb-daemon).
///
/// v0.34 Y1 — also carries the multi-user token registry + identity
/// ladder config (`operator`, `identity_header`). Authorization stays
/// one trust tier; the registry is ATTRIBUTION (+ optional admission
/// credential via `X-Kb-Token` / Authorization).
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Legacy shared daemon token (`<config>/token`).
    pub token: Option<String>,
    /// v0.7.1 — reverse-proxy IPs trusted to set `X-Forwarded-For`,
    /// shared (via the `Arc`) with `OriginConfig` + the rate limiters.
    /// `AuthConfig::load` leaves this empty; `serve_*` populates it from
    /// `[server] trusted_proxies` after the token file is read.
    pub trusted_proxies: Arc<Vec<IpAddr>>,
    /// Per-user token registry (`<config>/tokens`), loaded via
    /// [`kb_core::identity::parse_tokens_file`].
    pub tokens: Vec<kb_core::identity::TokenEntry>,
    /// `[identity].operator` — attribution for loopback + legacy-token.
    pub operator: String,
    /// `[identity].header` stored lowercase for case-insensitive match.
    pub identity_header: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            token: None,
            trusted_proxies: Arc::new(Vec::new()),
            tokens: Vec::new(),
            operator: kb_core::identity::DEFAULT_OPERATOR.to_string(),
            identity_header: kb_core::identity::DEFAULT_HEADER.to_ascii_lowercase(),
        }
    }
}

impl AuthConfig {
    /// Read the legacy token file at `path`, trim whitespace. Returns
    /// `AuthConfig { token: None }` when the file doesn't exist;
    /// other I/O errors propagate as `Err`. Registry / operator /
    /// header are left at defaults — `serve_*` fills them after load.
    pub fn load(path: &std::path::Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let trimmed = s.trim().to_string();
                Ok(Self {
                    token: if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed)
                    },
                    // Populated by `serve_*` from `[server] trusted_proxies`
                    // + `[identity]` + tokens file.
                    ..Self::default()
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Load the multi-user token registry from `<config>/tokens`.
    /// Missing file → empty. Malformed lines → WARN + skip (never fatal).
    /// Reuses [`kb_core::identity::parse_tokens_file`].
    pub fn load_tokens_registry(path: &std::path::Path) -> Vec<kb_core::identity::TokenEntry> {
        let body = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to read tokens registry; continuing with empty registry"
                );
                return Vec::new();
            }
        };
        // Walk lines for WARN on skips, then re-parse via the pure helper
        // so the accepted set stays single-sourced with kb-core tests.
        for (lineno, line) in body.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let one = kb_core::identity::parse_tokens_file(line);
            if one.is_empty() {
                tracing::warn!(
                    path = %path.display(),
                    line = lineno + 1,
                    "skipping malformed tokens-registry line"
                );
            }
        }
        let entries = kb_core::identity::parse_tokens_file(&body);
        if !entries.is_empty() {
            tracing::info!(
                path = %path.display(),
                count = entries.len(),
                "loaded multi-user token registry"
            );
        }
        entries
    }

    /// True when any admission credential is configured (legacy token
    /// and/or non-empty registry). Used by the public-bind fail-closed
    /// guard (invariant #4).
    pub fn has_auth(&self) -> bool {
        self.token.is_some() || !self.tokens.is_empty()
    }
}

impl KbHandles {
    /// Get-or-create the per-kb review-mutation lock. Briefly locks the
    /// map to clone out (or insert) the per-kb `Arc<AsyncMutex>`; the
    /// caller then `.lock().await`s the returned handle for the load →
    /// mutate → save critical section. Sharding per kb (vs one
    /// daemon-wide lock) lets comment edits in different kbs run
    /// concurrently. See root CLAUDE.md invariant #6.
    pub fn review_lock_for(&self, kb: &KbName) -> Arc<AsyncMutex<()>> {
        self.review_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(kb.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    /// W2.15b — get-or-create the per-kb proposal-mutation lock. Same
    /// shape as [`review_lock_for`] but over `proposal_locks` — every
    /// `.proposals/<id>.json` write (submit/approve/reject) runs under the
    /// caller's `.lock().await`ed guard for its load → mutate → save (or
    /// delete) critical section.
    pub fn proposal_lock_for(&self, kb: &KbName) -> Arc<AsyncMutex<()>> {
        self.proposal_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(kb.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    /// SL2 — get-or-create the per-SLATE mutation lock (invariant #6's
    /// SLATE amendment names this method). Delegates to
    /// [`crate::slate_registry::SlateRegistry::lock_for`], which owns the
    /// map, the cached head seq and the per-session rate window in one
    /// place; the shape is `review_lock_for`'s, but the shard key is a
    /// project SLUG rather than a `KbName` because the slate store is
    /// daemon-wide (design §7, D2).
    pub fn slate_lock_for(&self, slug: &str) -> Arc<AsyncMutex<()>> {
        self.slates.lock_for(slug)
    }

    /// Distinct embedder backends across all kbs, deduped by `Arc`
    /// identity — kbs sharing a model share ONE `Arc<Mutex<Embedder>>`
    /// (see `shared_embedder`), so the metrics ticker's liveness probe
    /// checks each subprocess once rather than once per kb. (v0.16
    /// Q-track.)
    pub fn unique_embedders(&self) -> Vec<Arc<Mutex<Embedder>>> {
        let mut out: Vec<Arc<Mutex<Embedder>>> = Vec::new();
        for ctx in self.kbs.values() {
            if let Some(emb) = &ctx.embedder {
                if !out.iter().any(|e| Arc::ptr_eq(e, emb)) {
                    out.push(Arc::clone(emb));
                }
            }
        }
        out
    }

    pub fn new(daemon_name: String, paths: Arc<KbPaths>, started_at: DateTime<Utc>) -> Self {
        // Starts `false`; `serve()` flips it `true` on the shutdown signal.
        // We drop the initial receiver — handlers subscribe on demand.
        let (shutdown, _) = tokio::sync::watch::channel(false);
        // v0.12 — load the persisted memory decay policy from
        // `<state>/memory-policy.json` so PUTs survive a restart.
        // Missing / corrupt file → default (Balanced); the next PUT
        // will rewrite it cleanly.
        let policy = load_memory_policy(&paths);
        // MI-W2.4c — EPOCH HONESTY marker: idempotent, set once ever per
        // `KB_HOME`. Computed here (before `paths` moves into the struct
        // literal below) same as the embed-cache pre-warm just after.
        let tombstone_era_started_unix = ensure_tombstone_era(&paths);
        // CE — default config_path to the home-local default; `with_config`
        // overrides it with the path the daemon actually resolved (which may
        // be an explicit `--config <file>`).
        let config_path = paths.config_file();
        // Pre-warm the query-embed cache from its persisted file so hot queries
        // survive a restart (incl. the CE in-process config restart).
        // Best-effort — missing/corrupt → empty. Computed before the struct
        // literal because the `paths,` field below moves `paths`.
        let embed_cache = crate::embed_cache::QueryEmbedCache::load_or_new(
            crate::embed_cache::DEFAULT_CAPACITY,
            &paths.embed_cache_file(),
        );
        Self {
            daemon_name,
            paths,
            started_at,
            config: Arc::new(tokio::sync::RwLock::new(KbConfig::default())),
            operator: kb_core::identity::DEFAULT_OPERATOR.to_string(),
            config_path,
            restart_requested: Arc::new(AtomicBool::new(false)),
            terminate: Arc::new(AtomicBool::new(false)),
            kbs: BTreeMap::new(),
            ui: Arc::new(Mutex::new(UiSection::default())),
            spa_dist: None,
            review_locks: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            proposal_locks: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            auth: Arc::new(AuthConfig::default()),
            rate_limits: RateLimits::default(),
            // PF-R1 — same literal `routes::FANOUT_CAP` the federated fan-out
            // call sites default to; `with_config` overwrites this once the
            // real `[server] fanout_cap` (if any) is loaded.
            fanout_cap: crate::routes::FANOUT_CAP,
            origin: Arc::new(OriginConfig::default()),
            share: Arc::new(ShareSection::default()),
            // G6 — sized from KB_EVENT_BUS_CAPACITY (escape hatch for large
            // corpora whose changed-file bursts exceed the 1024 default),
            // else the default.
            bus: Arc::new(EventBus::from_env()),
            metrics: Arc::new(RequestMetrics::default()),
            pipeline: Arc::new(kb_core::metrics::PipelineMetrics::disabled()),
            shutdown,
            memory_policy: Arc::new(tokio::sync::RwLock::new(policy)),
            tombstone_era_started_unix,
            embed_cache: Arc::new(embed_cache),
            touches_cache: Arc::new(crate::touches_cache::TouchesCache::default()),
            replay_cache: Arc::new(crate::replay_cache::ReplayCache::default()),
            view_cache: Arc::new(crate::replay_cache::ReplayCache::default()),
            live_tail_cache: Arc::new(crate::live_tail_cache::LiveTailCache::default()),
            live_registry: Arc::new(crate::live_registry::LiveRegistry::new()),
            slates: Arc::new(crate::slate_registry::SlateRegistry::new()),
        }
    }

    /// v0.4 A2 — install the daemon's bearer-token auth state at boot.
    /// Builder chained alongside `with_ui` / `with_spa_dist`.
    pub fn with_auth(mut self, auth: AuthConfig) -> Self {
        self.auth = Arc::new(auth);
        self
    }

    /// v0.5 Q1 — install per-route rate-limit overrides at boot.
    pub fn with_rate_limits(mut self, limits: RateLimits) -> Self {
        self.rate_limits = limits;
        self
    }

    /// v0.6 — install the origin config (artifact host suffix +
    /// parent origin) resolved from kb.toml `[server]`.
    pub fn with_origin(mut self, origin: OriginConfig) -> Self {
        self.origin = Arc::new(origin);
        self
    }

    /// Install the `[share]` config resolved from kb.toml at boot.
    pub fn with_share(mut self, share: ShareSection) -> Self {
        self.share = Arc::new(share);
        self
    }

    /// TM-track — wire the detailed-metrics flag (`[server] metrics`) at boot.
    /// Rebuilds `metrics` with the detailed layer enabled + the per-kb map
    /// pre-seeded from `kb_names`, and `pipeline` with the same flag, so the
    /// storage actors + indexer (which receive `pipeline` at `bring_up_kb`)
    /// and the request middleware agree. Must run BEFORE the kb-bring-up loop
    /// so `handles.pipeline` is the Arc threaded into each kb. The flag is
    /// read once at boot; invariant #13 restarts in-process on any config
    /// edit, so a boot read is always current.
    pub fn with_metrics(
        mut self,
        enabled: bool,
        kb_names: impl IntoIterator<Item = String>,
    ) -> Self {
        self.metrics = Arc::new(RequestMetrics::with_detail(enabled, kb_names));
        self.pipeline = Arc::new(kb_core::metrics::PipelineMetrics::new(enabled));
        self
    }

    /// CE — install the full loaded config + the path it came from, so
    /// `GET/PUT /api/config` can read it and write edits back to the right
    /// file. `config_path` is the resolved location (explicit `--config`
    /// or the home-local default), not necessarily `paths.config_file()`.
    pub fn with_config(mut self, config: KbConfig, config_path: PathBuf) -> Self {
        self.operator = config.identity.operator.clone();
        // PF-R1 — `[server] fanout_cap`, resolved with its own default
        // (`ServerSection::default_fanout_cap`, = 8) by serde, so this is
        // never a partially-configured read.
        self.fanout_cap = config.server.fanout_cap;
        self.config = Arc::new(tokio::sync::RwLock::new(config));
        self.config_path = config_path;
        self
    }

    /// X-phase placeholder — phase Y replaces call sites with the resolved per-request Identity.
    pub fn operator_user(&self) -> &str {
        &self.operator
    }

    pub fn with_ui(mut self, ui: UiSection) -> Self {
        self.ui = Arc::new(Mutex::new(ui));
        self
    }

    pub fn with_spa_dist(mut self, spa_dist: Option<PathBuf>) -> Self {
        self.spa_dist = spa_dist;
        self
    }

    pub fn insert(&mut self, kb_name: KbName, context: KbContext) {
        self.kbs.insert(kb_name, context);
    }

    pub fn get(&self, kb: &KbName) -> Option<&KbContext> {
        self.kbs.get(kb)
    }
}

// v0.12 — memory decay policy persistence. Lives in a tiny JSON file
// alongside the daemon's state dir so the PUT /api/memory/policy
// flip survives a restart. Malformed/missing → default; the next PUT
// overwrites cleanly.

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedPolicy {
    policy: String,
}

pub(crate) fn load_memory_policy(paths: &KbPaths) -> kb_core::memory::DecayPolicy {
    let path = paths.memory_policy_file();
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return kb_core::memory::DecayPolicy::default(),
    };
    match serde_json::from_slice::<PersistedPolicy>(&bytes) {
        Ok(p) => kb_core::memory::DecayPolicy::parse(&p.policy).unwrap_or_default(),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "corrupt memory-policy.json — starting at default"
            );
            kb_core::memory::DecayPolicy::default()
        }
    }
}

pub(crate) fn save_memory_policy(
    paths: &KbPaths,
    policy: kb_core::memory::DecayPolicy,
) -> std::io::Result<()> {
    let path = paths.memory_policy_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(&PersistedPolicy {
        policy: policy.as_str().to_string(),
    })
    .map_err(std::io::Error::other)?;
    std::fs::write(&path, body)
}

// MI-W2.4c — EPOCH HONESTY marker. Same tiny-JSON-file precedent as
// `memory-policy.json` above.

#[derive(serde::Serialize, serde::Deserialize)]
struct TombstoneEraMarker {
    started_unix: i64,
}

/// Idempotent read-or-create: if `<state>/tombstone-era.json` already
/// exists, return its `started_unix` verbatim (the marker NEVER moves
/// forward once set — that would silently shrink the window a caveat
/// fires on). Otherwise this is the first time ANY daemon process at this
/// `KB_HOME` has run code capable of soft-forgetting (MI-W2.3), so `now`
/// becomes the honest boundary: write it and return it.
///
/// A persist failure (read-only state dir, etc.) doesn't fail startup —
/// it falls back to an in-memory-only `now` for this boot (logged loudly)
/// rather than crash the daemon over an observability nicety; the next
/// successful boot tries the write again.
pub(crate) fn ensure_tombstone_era(paths: &KbPaths) -> i64 {
    let path = paths.tombstone_era_file();
    if let Ok(bytes) = std::fs::read(&path) {
        match serde_json::from_slice::<TombstoneEraMarker>(&bytes) {
            Ok(m) => return m.started_unix,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "corrupt tombstone-era.json — re-stamping at `now` (loses the original boundary)"
                );
            }
        }
    }
    let now = chrono::Utc::now().timestamp();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body =
        serde_json::to_vec_pretty(&TombstoneEraMarker { started_unix: now }).unwrap_or_default();
    if let Err(e) = std::fs::write(&path, body) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "failed to persist tombstone-era.json — using an in-memory-only marker for this boot"
        );
    }
    now
}

// v0.13 Q4 — saved queries (daemon-wide JSON store at
// `<state>/saved-queries.json`). The SPA's localStorage cache is
// still the offline-first source of truth; the hook reconciles with
// the daemon when reachable.

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SavedQuery {
    pub name: String,
    pub path: String,
    pub search: String,
    pub saved_at: i64,
}

pub(crate) fn load_saved_queries(paths: &KbPaths) -> Vec<SavedQuery> {
    let path = paths.saved_queries_file();
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    match serde_json::from_slice::<Vec<SavedQuery>>(&bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "corrupt saved-queries.json — starting empty"
            );
            Vec::new()
        }
    }
}

pub(crate) fn save_saved_queries(paths: &KbPaths, queries: &[SavedQuery]) -> std::io::Result<()> {
    let path = paths.saved_queries_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(queries).map_err(std::io::Error::other)?;
    std::fs::write(&path, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- P3+P4: route classification + histogram percentiles ----------

    #[test]
    fn classify_route_top_level_paths() {
        assert_eq!(classify_route("/api/identity"), RouteKind::Read);
        assert_eq!(classify_route("/api/kbs"), RouteKind::Read);
        assert_eq!(classify_route("/api/stats"), RouteKind::Read);
        assert_eq!(classify_route("/api/settings"), RouteKind::Read);
        assert_eq!(classify_route("/api/search"), RouteKind::Search);
        assert_eq!(classify_route("/api/events"), RouteKind::Events);
        assert_eq!(classify_route("/api/events.schema.json"), RouteKind::Events);
        assert_eq!(
            classify_route("/api/events/schema/index.file/1"),
            RouteKind::Events
        );
        assert_eq!(classify_route("/api/unknown"), RouteKind::Other);
        assert_eq!(classify_route("/api/queries/zero-hit"), RouteKind::Read);
    }

    #[test]
    fn classify_route_per_kb_segments() {
        assert_eq!(classify_route("/api/kb/canon/search"), RouteKind::Other);
        assert_eq!(
            classify_route("/api/kb/canon/atlas/recompute"),
            RouteKind::Atlas
        );
        assert_eq!(
            classify_route("/api/kb/canon/history/open"),
            RouteKind::History
        );
        assert_eq!(
            classify_route("/api/kb/canon/review/abc123"),
            RouteKind::Review
        );
        assert_eq!(
            classify_route("/api/kb/canon/review/abc123/export"),
            RouteKind::Review
        );
        assert_eq!(classify_route("/api/kb/canon/docs"), RouteKind::Read);
        assert_eq!(
            classify_route("/api/kb/canon/artifact/abc123"),
            RouteKind::Read
        );
        assert_eq!(classify_route("/api/kb/canon/runs"), RouteKind::Read);
        assert_eq!(
            classify_route("/api/kb/canon/sources/tmp-canon/reindex"),
            RouteKind::Ops
        );
        assert_eq!(
            classify_route("/api/kb/canon/errors/e1/dismiss"),
            RouteKind::Ops
        );
        assert_eq!(classify_route("/api/kb/canon/reindex"), RouteKind::Ops);
        // X3 — exclusion mutations are ops, not reads.
        assert_eq!(classify_route("/api/kb/canon/exclusions"), RouteKind::Ops);
        assert_eq!(
            classify_route("/api/kb/canon/exclusions/sub%2Fa.html"),
            RouteKind::Ops
        );
    }

    #[test]
    fn route_metrics_observe_buckets_correctly() {
        let m = RouteMetrics::default();
        m.observe(0); // ≤ 1ms
        m.observe(3); // ≤ 5ms
        m.observe(8); // ≤ 10ms
        m.observe(75); // ≤ 100ms
        m.observe(9_000); // ≤ 10000ms
        m.observe(20_000); // overflow
        assert_eq!(m.count(), 6);
        let buckets = m.buckets_snapshot();
        // boundaries: 1, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000, overflow
        assert_eq!(buckets[0], 1, "0ms → first bucket");
        assert_eq!(buckets[1], 1, "3ms → 5ms bucket");
        assert_eq!(buckets[2], 1, "8ms → 10ms bucket");
        assert_eq!(buckets[5], 1, "75ms → 100ms bucket");
        assert_eq!(buckets[11], 1, "9000ms → 10000ms bucket");
        assert_eq!(buckets[12], 1, "20000ms → overflow bucket");
    }

    #[test]
    fn percentile_ms_finds_spanning_bucket() {
        // 10 observations: 8 fast (≤ 5ms), 2 slow (≤ 250ms)
        let mut buckets = [0u64; 13];
        buckets[1] = 8; // ≤ 5ms
        buckets[6] = 2; // ≤ 250ms
        assert_eq!(
            percentile_ms(&buckets, 0.5),
            5,
            "p50 should land in the 5ms bucket"
        );
        assert_eq!(
            percentile_ms(&buckets, 0.95),
            250,
            "p95 spans into the slow tail"
        );
        assert_eq!(percentile_ms(&buckets, 1.0), 250);
    }

    #[test]
    fn percentile_ms_empty_histogram_is_zero() {
        let buckets = [0u64; 13];
        assert_eq!(percentile_ms(&buckets, 0.5), 0);
        assert_eq!(percentile_ms(&buckets, 0.95), 0);
    }

    #[test]
    fn kb_from_path_extracts_kb_segment() {
        // Full form (direct callers / tests).
        assert_eq!(kb_from_path("/api/kb/canon/docs"), Some("canon"));
        assert_eq!(kb_from_path("/api/kb/my-kb/history/open"), Some("my-kb"));
        assert_eq!(kb_from_path("/api/kb/canon"), Some("canon"));
        // Nest-stripped form (what `count_requests` actually sees).
        assert_eq!(kb_from_path("/kb/canon/docs"), Some("canon"));
        assert_eq!(kb_from_path("/kb/my-kb"), Some("my-kb"));
        // Non-kb-scoped paths have no kb, both forms.
        assert_eq!(kb_from_path("/api/search"), None);
        assert_eq!(kb_from_path("/search"), None);
        assert_eq!(kb_from_path("/identity"), None);
        assert_eq!(kb_from_path("/metrics"), None);
        // Degenerate: trailing-slash / empty segment.
        assert_eq!(kb_from_path("/api/kb/"), None);
        assert_eq!(kb_from_path("/kb/"), None);
    }

    #[test]
    fn classify_route_handles_nest_stripped_paths() {
        // The nest-stripped form (no `/api`) must classify identically to the
        // full form — the regression that put everything in `Other`.
        assert_eq!(classify_route("/search"), RouteKind::Search);
        assert_eq!(classify_route("/kb/canon/docs"), RouteKind::Read);
        assert_eq!(
            classify_route("/kb/canon/atlas/recompute"),
            RouteKind::Atlas
        );
        assert_eq!(classify_route("/kb/canon/history/open"), RouteKind::History);
        assert_eq!(classify_route("/kb/canon/reindex"), RouteKind::Ops);
        assert_eq!(classify_route("/identity"), RouteKind::Read);
        assert_eq!(classify_route("/metrics"), RouteKind::Read);
        assert_eq!(classify_route("/events"), RouteKind::Events);
    }

    #[test]
    fn detailed_metrics_gate_records_only_when_enabled() {
        let off = RequestMetrics::with_detail(false, ["canon".to_string()]);
        off.observe_search_stage(SearchStage::Bm25, 12);
        off.observe_kb("canon", 12);
        assert_eq!(off.search_stages[SearchStage::Bm25 as usize].count(), 0);
        assert_eq!(off.per_kb["canon"].count(), 0);

        let on = RequestMetrics::with_detail(true, ["canon".to_string()]);
        on.observe_search_stage(SearchStage::Bm25, 12);
        on.observe_kb("canon", 12);
        on.observe_kb("unknown", 99); // unseeded kb → silently skipped
        assert_eq!(on.search_stages[SearchStage::Bm25 as usize].count(), 1);
        assert_eq!(on.per_kb["canon"].count(), 1);
        assert!(!on.per_kb.contains_key("unknown"));
    }

    #[test]
    fn parse_trusted_proxies_keeps_valid_drops_invalid() {
        let parsed = parse_trusted_proxies(&[
            "127.0.0.1".to_string(),
            "  10.0.0.1  ".to_string(), // surrounding whitespace tolerated
            "::1".to_string(),
            "not-an-ip".to_string(),       // dropped with a warn
            "999.999.999.999".to_string(), // dropped with a warn
        ]);
        assert_eq!(
            parsed,
            vec![
                "127.0.0.1".parse::<IpAddr>().unwrap(),
                "10.0.0.1".parse().unwrap(),
                "::1".parse().unwrap(),
            ]
        );
    }

    #[test]
    fn parse_trusted_proxies_empty_is_empty() {
        assert!(parse_trusted_proxies(&[]).is_empty());
    }

    // ---- CE2 — config fields + with_config builder ----

    #[test]
    fn with_config_stores_config_and_path() {
        let paths = Arc::new(KbPaths::new("ce-builder-test").unwrap());
        let mut cfg = KbConfig::default();
        cfg.server.addr = "127.0.0.1:4999".into();
        let path = PathBuf::from("/tmp/explicit-kb.toml");
        let handles = KbHandles::new("ce-builder-test".into(), paths, Utc::now())
            .with_config(cfg, path.clone());

        assert_eq!(
            handles.config_path, path,
            "config_path is the explicit path"
        );
        assert_eq!(
            handles.config.try_read().unwrap().server.addr,
            "127.0.0.1:4999",
            "the stored config reflects the installed edit"
        );
        use std::sync::atomic::Ordering;
        assert!(!handles.restart_requested.load(Ordering::SeqCst));
        assert!(!handles.terminate.load(Ordering::SeqCst));
    }

    #[test]
    fn new_defaults_config_path_to_home_local() {
        let paths = Arc::new(KbPaths::new("ce-default-test").unwrap());
        let handles = KbHandles::new("ce-default-test".into(), paths.clone(), Utc::now());
        assert_eq!(
            handles.config_path,
            paths.config_file(),
            "without with_config, config_path falls back to the home-local default"
        );
    }
}
