//! Lock-free latency histograms + pipeline timing (TM-track).
//!
//! Two layers live here:
//!
//! - [`LatencyHist`] — the shared, lock-free `[AtomicU64; 13]` histogram
//!   primitive. It is the single source of truth for bucket boundaries +
//!   percentile math, used by BOTH kb-server's per-route request metrics
//!   (`kb_server::state::RouteMetrics` embeds one) AND kb-core's
//!   [`PipelineMetrics`]. Keeping one implementation here (the lower crate)
//!   means the two layers can't drift on bucket boundaries — a p95 means the
//!   same thing on every section of the `/api/metrics` payload.
//!
//! - [`PipelineMetrics`] — daemon-wide ingest-pipeline timing (indexer
//!   throughput, index-side embed latency, storage-actor queue-wait +
//!   handler-time). Written from inside the storage actor + indexer; the
//!   detailed metrics layer is opt-in (`[server] metrics = true`), so every
//!   `observe_*` early-returns when the metrics struct is disabled.
//!
//! The histogram is approximate but stable: cumulative bucket counts let
//! [`percentile_ms`] walk the distribution and report the spanning bucket's
//! upper boundary. Memory is tiny — 13 atomics per histogram.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::storage::actor::StorageMsg;

/// Latency histogram bucket boundaries in ms. A sample observed at `obs_ms`
/// lands in bucket `i` where `obs_ms <= LATENCY_BUCKETS_MS[i]`. The overflow
/// bucket at index `LATENCY_BUCKETS_MS.len()` catches >10s.
pub const LATENCY_BUCKETS_MS: &[u32] = &[1, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000];

/// 12 boundaries + 1 overflow = 13 atomic counters per histogram.
pub const LATENCY_BUCKET_COUNT: usize = 13;

/// A lock-free latency histogram: 13 cumulative bucket counters plus a
/// running total. Cheap to `observe` (two relaxed atomic adds) and cheap to
/// snapshot (relaxed loads). Derives `Default` (all-zero), so it composes
/// into fixed-size arrays for per-variant breakdowns.
#[derive(Debug, Default)]
pub struct LatencyHist {
    /// Total observations (== sum of `buckets`). Tracked separately so a
    /// reader can report a count without summing the buckets.
    pub count: AtomicU64,
    /// Cumulative observed latencies (one atomic per boundary in
    /// `LATENCY_BUCKETS_MS`, plus one overflow at index `len`). 13 total.
    pub buckets: [AtomicU64; LATENCY_BUCKET_COUNT],
}

impl LatencyHist {
    /// Observe one sample with elapsed `ms`. Lock-free: locates the matching
    /// bucket and bumps it. Samples above the top boundary land in the final
    /// overflow bucket (10s+).
    pub fn observe(&self, ms: u64) {
        let ms_u32 = ms.min(u64::from(u32::MAX)) as u32;
        let mut idx = LATENCY_BUCKETS_MS.len(); // overflow
        for (i, &bound) in LATENCY_BUCKETS_MS.iter().enumerate() {
            if ms_u32 <= bound {
                idx = i;
                break;
            }
        }
        self.buckets[idx].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// Read the current cumulative bucket counts as `[u64; 13]`.
    pub fn buckets_snapshot(&self) -> [u64; LATENCY_BUCKET_COUNT] {
        let mut out = [0u64; LATENCY_BUCKET_COUNT];
        for (i, atom) in self.buckets.iter().enumerate() {
            out[i] = atom.load(Ordering::Relaxed);
        }
        out
    }

    /// Total observations recorded so far.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Convenience: snapshot the buckets and compute the p50/p95/p99 trio in
    /// one pass. Returns `(p50, p95, p99)` in ms.
    pub fn percentiles(&self) -> (u32, u32, u32) {
        let b = self.buckets_snapshot();
        (
            percentile_ms(&b, 0.50),
            percentile_ms(&b, 0.95),
            percentile_ms(&b, 0.99),
        )
    }
}

/// Compute the `p`-th percentile (0.0..=1.0) latency in ms from a bucket
/// histogram. `buckets[i]` is the count of observations whose ms ≤
/// `LATENCY_BUCKETS_MS[i]`; the final slot is the overflow (10s+). Returns 0
/// when the histogram is empty. The overflow bucket reports as
/// `LATENCY_BUCKETS_MS.last() + 1` (a >10s sentinel).
pub fn percentile_ms(buckets: &[u64], p: f32) -> u32 {
    let total: u64 = buckets.iter().sum();
    if total == 0 {
        return 0;
    }
    let target = ((total as f32) * p).ceil() as u64;
    let mut cumulative = 0u64;
    for (i, &c) in buckets.iter().enumerate() {
        cumulative += c;
        if cumulative >= target {
            return LATENCY_BUCKETS_MS
                .get(i)
                .copied()
                .unwrap_or(LATENCY_BUCKETS_MS[LATENCY_BUCKETS_MS.len() - 1] + 1);
        }
    }
    LATENCY_BUCKETS_MS[LATENCY_BUCKETS_MS.len() - 1] + 1
}

/// Coarse classification of a [`StorageMsg`] for per-class timing. Kept
/// deliberately coarse (8 classes) so the snapshot table stays legible and
/// the classifier stays maintainable across the actor's ~80 message variants
/// — mirrors `kb_server::state::RouteKind`. Indexed by `kind as usize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum StorageKind {
    /// BM25 / vector / hybrid search — the latency-sensitive reads.
    Query = 0,
    /// Document + source upserts — the core index-write path.
    Upsert = 1,
    /// Primary content deletion (doc/path/history/drop).
    Delete = 2,
    /// Unfiltered list / get / count reads.
    Read = 3,
    /// History + reading side-channel writes (high frequency under the SPA).
    History = 4,
    /// Post-upsert enrichment writes (edges, sessions, snapshots, memory
    /// links, run/error bookkeeping).
    Enrich = 5,
    /// Index maintenance + operator/meta mutations (fts/vector index,
    /// compaction, atlas, shares, lists, corkboard, pins).
    Admin = 6,
    /// Shutdown + anything unclassified.
    Other = 7,
}

pub const STORAGE_KIND_COUNT: usize = 8;

impl StorageKind {
    pub const ALL: &'static [StorageKind] = &[
        StorageKind::Query,
        StorageKind::Upsert,
        StorageKind::Delete,
        StorageKind::Read,
        StorageKind::History,
        StorageKind::Enrich,
        StorageKind::Admin,
        StorageKind::Other,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            StorageKind::Query => "query",
            StorageKind::Upsert => "upsert",
            StorageKind::Delete => "delete",
            StorageKind::Read => "read",
            StorageKind::History => "history",
            StorageKind::Enrich => "enrich",
            StorageKind::Admin => "admin",
            StorageKind::Other => "other",
        }
    }

    /// Classify a storage message by its discriminant. Reads only the
    /// variant, never field values, so the caller can classify *before* the
    /// message moves into the actor's `handle`. New MUTATION variants must be
    /// added to the matching arm; everything unlisted defaults to `Read`
    /// (the common case for a new read-style message).
    pub fn from_msg(msg: &StorageMsg) -> StorageKind {
        match msg {
            StorageMsg::Bm25Query { .. }
            | StorageMsg::VectorQuery { .. }
            | StorageMsg::HybridQuery { .. } => StorageKind::Query,

            StorageMsg::UpsertSource { .. }
            | StorageMsg::UpsertDoc { .. }
            | StorageMsg::UpsertDocs { .. } => StorageKind::Upsert,

            StorageMsg::DeleteDoc { .. }
            | StorageMsg::DeleteByPath { .. }
            | StorageMsg::CascadeDeleteDoc { .. }
            | StorageMsg::SweepOrphans { .. }
            | StorageMsg::HistoryPurge { .. }
            | StorageMsg::DropKbData { .. } => StorageKind::Delete,

            StorageMsg::HistoryRecordOpen { .. }
            | StorageMsg::HistoryUpdateScroll { .. }
            | StorageMsg::HistoryRecordSearch { .. }
            | StorageMsg::HistoryRecordComment { .. }
            | StorageMsg::HistoryList { .. }
            | StorageMsg::HistoryOpensInWindow { .. }
            | StorageMsg::ReadingUpsertSections { .. }
            | StorageMsg::ReadingSetActive { .. }
            | StorageMsg::ReadingStateForVisit { .. }
            | StorageMsg::ReadingInputsForArtifact { .. }
            | StorageMsg::ReadingLatestForArtifact { .. } => StorageKind::History,

            StorageMsg::BeginRun { .. }
            | StorageMsg::FinishRun { .. }
            | StorageMsg::RecordError { .. }
            | StorageMsg::ClearErrorsForPathHash { .. }
            | StorageMsg::ClearErrorsForPath { .. }
            | StorageMsg::RecordEdges { .. }
            // DCB W1.A — the code-ref write is post-upsert enrichment, same
            // lane as `RecordEdges`. Its two READS fall through to
            // `StorageKind::Read` below.
            | StorageMsg::RecordCodeRefs { .. }
            // CT-F1 — the memory<->commit ledger write is post-upsert
            // enrichment too (the `memory-commit-ledger` hook). Its READ
            // (`MemoryCommitsForMemory`) falls through to `Read` below.
            | StorageMsg::MemoryCommitsReplace { .. }
            | StorageMsg::MemoryLinkAdd { .. }
            | StorageMsg::MemoryLinkRemove { .. }
            | StorageMsg::MemoryLinksReplace { .. }
            | StorageMsg::MemoryLinksRemoveAll { .. }
            | StorageMsg::MemoryLinksSeededMark { .. }
            | StorageMsg::SessionsUpsert { .. }
            | StorageMsg::SessionsDelete { .. }
            | StorageMsg::SnapshotInsert { .. }
            | StorageMsg::SnapshotPrune { .. }
            | StorageMsg::SnapshotsDeleteForArtifact { .. }
            | StorageMsg::ListEntriesSyncResolution { .. } => StorageKind::Enrich,

            StorageMsg::SetSourcePaused { .. }
            | StorageMsg::DismissError { .. }
            | StorageMsg::EnsureFtsIndex { .. }
            | StorageMsg::EnsureVectorIndex { .. }
            | StorageMsg::CompactAll { .. }
            | StorageMsg::CompactAllWithRetention { .. }
            | StorageMsg::UpdateAtlas { .. }
            | StorageMsg::ClearEmbeddings { .. }
            | StorageMsg::SharesInsert { .. }
            | StorageMsg::SharesDelete { .. }
            | StorageMsg::CorkboardAdd { .. }
            | StorageMsg::CorkboardRemove { .. }
            | StorageMsg::PinnedMemoryAdd { .. }
            | StorageMsg::PinnedMemoryRemove { .. }
            | StorageMsg::ListCreate { .. }
            | StorageMsg::ListUpdate { .. }
            | StorageMsg::ListDelete { .. }
            | StorageMsg::ListEntryAdd { .. }
            | StorageMsg::ListEntryUpdate { .. }
            | StorageMsg::ListEntryMove { .. }
            | StorageMsg::ListEntryRemove { .. }
            | StorageMsg::ListImportEntries { .. }
            | StorageMsg::IdentityBackfill { .. }
            | StorageMsg::ListEntrySetUserOverride { .. } => StorageKind::Admin,

            StorageMsg::Shutdown => StorageKind::Other,

            // All list / get / count reads (and any future read-style variant).
            _ => StorageKind::Read,
        }
    }
}

/// Daemon-wide ingest-pipeline timing. One `Arc<PipelineMetrics>` is shared
/// by every kb's storage actor + indexer. Lock-free, cheap to write. The
/// detailed metrics layer is opt-in (`[server] metrics = true`) — when
/// `enabled` is false every `observe_*` early-returns after one relaxed load,
/// so the off-path cost is a single branch (this matters most on the
/// per-message storage path). Derives `Default` (disabled, all-zero).
#[derive(Debug, Default)]
pub struct PipelineMetrics {
    enabled: AtomicBool,
    /// Per-file wall time through `indexer::index_file` (read→parse→embed→
    /// upsert→enrich). `index_file.count()` == files indexed.
    pub index_file: LatencyHist,
    /// Index-side embed call latency (distinct from the query-side
    /// `search.embed_ms` in kb-server). `embed_index.count()` == calls.
    pub embed_index: LatencyHist,
    /// Total docs embedded across all index-side embed calls (avg
    /// docs/call = `embed_index_docs / embed_index.count()`).
    pub embed_index_docs: AtomicU64,
    /// Per-`StorageKind` actor handler execution time.
    pub storage_handler: [LatencyHist; STORAGE_KIND_COUNT],
    /// Per-`StorageKind` time a message waited between enqueue and dispatch.
    pub storage_queue_wait: [LatencyHist; STORAGE_KIND_COUNT],
}

impl PipelineMetrics {
    /// Construct with the resolved `[server] metrics` flag.
    pub fn new(enabled: bool) -> Self {
        let m = Self::default();
        m.enabled.store(enabled, Ordering::Relaxed);
        m
    }

    /// A disabled instance — for tests + the back-compat plumbing that never
    /// opts into detailed metrics.
    pub fn disabled() -> Self {
        Self::new(false)
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// Record one indexed file's wall time.
    pub fn observe_index_file(&self, ms: u64) {
        if !self.is_enabled() {
            return;
        }
        self.index_file.observe(ms);
    }

    /// Record one index-side embed call: its latency + the doc count embedded.
    pub fn observe_embed_index(&self, ms: u64, docs: u64) {
        if !self.is_enabled() {
            return;
        }
        self.embed_index.observe(ms);
        self.embed_index_docs.fetch_add(docs, Ordering::Relaxed);
    }

    /// Record one storage-actor message: its time waiting in the queue and
    /// its handler execution time, attributed to `kind`.
    pub fn observe_storage(&self, kind: StorageKind, queue_wait_ms: u64, handler_ms: u64) {
        if !self.is_enabled() {
            return;
        }
        self.storage_handler[kind as usize].observe(handler_ms);
        self.storage_queue_wait[kind as usize].observe(queue_wait_ms);
    }

    /// Read a serializable point-in-time snapshot (percentiles materialised).
    pub fn snapshot(&self) -> PipelineSnapshot {
        let (i50, i95, i99) = self.index_file.percentiles();
        let (e50, e95, e99) = self.embed_index.percentiles();
        let storage = StorageKind::ALL
            .iter()
            .map(|k| {
                let h = &self.storage_handler[*k as usize];
                let qw = &self.storage_queue_wait[*k as usize];
                let (h50, h95, h99) = h.percentiles();
                let (q50, q95, q99) = qw.percentiles();
                StorageVariantStats {
                    kind: k.label(),
                    count: h.count(),
                    handler_p50_ms: h50,
                    handler_p95_ms: h95,
                    handler_p99_ms: h99,
                    queue_wait_p50_ms: q50,
                    queue_wait_p95_ms: q95,
                    queue_wait_p99_ms: q99,
                }
            })
            .collect();
        PipelineSnapshot {
            enabled: self.is_enabled(),
            indexer: IndexerStats {
                files_indexed: self.index_file.count(),
                p50_ms: i50,
                p95_ms: i95,
                p99_ms: i99,
            },
            embed_index: EmbedStats {
                calls: self.embed_index.count(),
                docs: self.embed_index_docs.load(Ordering::Relaxed),
                p50_ms: e50,
                p95_ms: e95,
                p99_ms: e99,
            },
            storage,
        }
    }
}

/// Serializable pipeline snapshot crossing the kb-core → kb-server boundary,
/// nested under `/api/metrics`'s detailed block. Stable shape: `storage`
/// always has one entry per `StorageKind`, in `StorageKind::ALL` order.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PipelineSnapshot {
    pub enabled: bool,
    pub indexer: IndexerStats,
    pub embed_index: EmbedStats,
    pub storage: Vec<StorageVariantStats>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexerStats {
    pub files_indexed: u64,
    pub p50_ms: u32,
    pub p95_ms: u32,
    pub p99_ms: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EmbedStats {
    pub calls: u64,
    pub docs: u64,
    pub p50_ms: u32,
    pub p95_ms: u32,
    pub p99_ms: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StorageVariantStats {
    pub kind: &'static str,
    pub count: u64,
    pub handler_p50_ms: u32,
    pub handler_p95_ms: u32,
    pub handler_p99_ms: u32,
    pub queue_wait_p50_ms: u32,
    pub queue_wait_p95_ms: u32,
    pub queue_wait_p99_ms: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_lands_in_correct_buckets() {
        let h = LatencyHist::default();
        // 0 and 1 → bucket 0 (<=1); 3 → bucket 1 (<=5); 8 → bucket 2 (<=10);
        // 75 → bucket 4 (<=100... wait: 50 boundary at idx 4) — verify below.
        h.observe(0);
        h.observe(3);
        h.observe(8);
        h.observe(75);
        h.observe(9000);
        h.observe(20000);
        let b = h.buckets_snapshot();
        // Boundaries: [1,5,10,25,50,100,250,500,1000,2500,5000,10000]
        assert_eq!(b[0], 1, "0ms ≤ 1");
        assert_eq!(b[1], 1, "3ms ≤ 5");
        assert_eq!(b[2], 1, "8ms ≤ 10");
        assert_eq!(b[5], 1, "75ms ≤ 100");
        assert_eq!(b[11], 1, "9000ms ≤ 10000");
        assert_eq!(b[12], 1, "20000ms → overflow");
        assert_eq!(h.count(), 6);
    }

    #[test]
    fn percentile_finds_spanning_bucket() {
        let h = LatencyHist::default();
        // 90 samples at ~3ms (bucket 1), 10 samples at ~9000ms (bucket 11).
        for _ in 0..90 {
            h.observe(3);
        }
        for _ in 0..10 {
            h.observe(9000);
        }
        let b = h.buckets_snapshot();
        assert_eq!(percentile_ms(&b, 0.50), 5, "p50 in the 5ms bucket");
        assert_eq!(percentile_ms(&b, 0.95), 10000, "p95 spills into 10000ms");
    }

    #[test]
    fn percentile_empty_histogram_is_zero() {
        let b = [0u64; LATENCY_BUCKET_COUNT];
        assert_eq!(percentile_ms(&b, 0.50), 0);
        assert_eq!(percentile_ms(&b, 0.99), 0);
    }

    #[test]
    fn overflow_percentile_reports_sentinel() {
        let h = LatencyHist::default();
        h.observe(50_000);
        let b = h.buckets_snapshot();
        assert_eq!(percentile_ms(&b, 0.50), 10001, ">10s sentinel");
    }

    #[test]
    fn percentiles_trio_matches_individual() {
        let h = LatencyHist::default();
        for _ in 0..100 {
            h.observe(42);
        }
        let (p50, p95, p99) = h.percentiles();
        assert_eq!(p50, 50);
        assert_eq!(p95, 50);
        assert_eq!(p99, 50);
    }

    #[test]
    fn disabled_pipeline_records_nothing() {
        let m = PipelineMetrics::disabled();
        m.observe_index_file(123);
        m.observe_embed_index(456, 4);
        m.observe_storage(StorageKind::Upsert, 7, 80);
        let s = m.snapshot();
        assert!(!s.enabled);
        assert_eq!(s.indexer.files_indexed, 0);
        assert_eq!(s.indexer.p50_ms, 0);
        assert_eq!(s.embed_index.calls, 0);
        assert_eq!(s.embed_index.docs, 0);
        assert!(s.storage.iter().all(|v| v.count == 0));
    }

    #[test]
    fn enabled_pipeline_records() {
        let m = PipelineMetrics::new(true);
        for _ in 0..10 {
            m.observe_index_file(42);
        }
        m.observe_embed_index(180, 1);
        m.observe_embed_index(220, 3);
        m.observe_storage(StorageKind::Query, 2, 8);
        m.observe_storage(StorageKind::Query, 3, 12);
        let s = m.snapshot();
        assert!(s.enabled);
        assert_eq!(s.indexer.files_indexed, 10);
        assert_eq!(s.indexer.p50_ms, 50, "42ms lands in the 50ms bucket");
        assert_eq!(s.embed_index.calls, 2);
        assert_eq!(s.embed_index.docs, 4);
        let q = s
            .storage
            .iter()
            .find(|v| v.kind == "query")
            .expect("query slot present");
        assert_eq!(q.count, 2);
        assert!(q.handler_p50_ms > 0);
    }

    #[test]
    fn snapshot_storage_shape_is_stable() {
        // Even with nothing observed, every StorageKind has a slot, in order.
        let m = PipelineMetrics::new(true);
        let s = m.snapshot();
        assert_eq!(s.storage.len(), STORAGE_KIND_COUNT);
        let labels: Vec<&str> = s.storage.iter().map(|v| v.kind).collect();
        assert_eq!(
            labels,
            vec!["query", "upsert", "delete", "read", "history", "enrich", "admin", "other"]
        );
    }

    #[test]
    fn from_msg_classifies_representative_variants() {
        use crate::ids::SourceSlug;
        use crate::storage::actor::StorageMsg;
        use tokio::sync::oneshot;

        // Helper makes a throwaway reply channel (rx dropped immediately).
        macro_rules! kind_of {
            ($build:expr) => {{
                let (tx, _rx) = oneshot::channel();
                StorageKind::from_msg(&$build(tx))
            }};
        }

        assert_eq!(
            kind_of!(|reply| StorageMsg::Bm25Query {
                q: "x".into(),
                limit: 5,
                typo_tolerance: false,
                reply
            }),
            StorageKind::Query
        );
        assert_eq!(
            kind_of!(|reply| StorageMsg::UpsertSource {
                slug: SourceSlug::from_path(std::path::Path::new("/tmp/c")),
                path: "/tmp/c".into(),
                added_at_unix: 0,
                reply
            }),
            StorageKind::Upsert
        );
        assert_eq!(
            kind_of!(|reply| StorageMsg::DeleteByPath {
                path: "/tmp/c".into(),
                reply
            }),
            StorageKind::Delete
        );
        assert_eq!(
            kind_of!(|reply| StorageMsg::HistoryRecordOpen {
                artifact_id: "a".into(),
                now_unix: 0,
                source: None,
                user: "operator".into(),
                reply
            }),
            StorageKind::History
        );
        assert_eq!(
            kind_of!(|reply| StorageMsg::RecordEdges {
                from_id: "a".into(),
                to_kinds: vec![],
                reply
            }),
            StorageKind::Enrich
        );
        assert_eq!(
            kind_of!(|reply| StorageMsg::EnsureFtsIndex { reply }),
            StorageKind::Admin
        );
        assert_eq!(
            kind_of!(|reply| StorageMsg::ListDocs { limit: 1, reply }),
            StorageKind::Read
        );
        assert_eq!(
            StorageKind::from_msg(&StorageMsg::Shutdown),
            StorageKind::Other
        );
    }
}
