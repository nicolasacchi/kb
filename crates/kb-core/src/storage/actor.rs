//! Single-writer-per-kb storage actor. mpsc-driven (NOT mutex) — fairer
//! under v0.1+ SPA load (the Plan agent's call) and avoids refactoring
//! later. The actor owns one lance `Storage` + one sqlite `Db`; all
//! operations cross a channel and the actor still processes exactly one at
//! a time (single-writer-per-kb holds). SC4 splits the inbound flow into
//! TWO lanes — a read lane (searches/gets/lists/counts) and a write lane
//! (everything that mutates or is unsure) — and the loop drains the read
//! lane with priority (bounded so a read stream can't starve ingest), so a
//! foreground search never head-of-line-blocks behind a bulk-ingest write
//! backlog on the same actor. See `is_read_lane` + `next_from_lanes`.
//! The one exception is `CompactAll`: a multi-second
//! `Table::optimize(OptimizeAction::All)` runs on a spawned task while
//! the loop keeps servicing everything that doesn't mutate lance (see
//! `compact_off_loop`), so maintenance never head-of-line-blocks search.

use crate::cascade::CascadeMode;
use crate::ids::{ArtifactId, ErrorId, RunId, SourceSlug};
use crate::lists::{ImportMode, NewListEntry, Patch, PositionSpec, ResolutionUpdate};
use crate::metrics::{PipelineMetrics, StorageKind};
use crate::storage::lance::{CompactStats, DatasetStats, DocSummary, EmbeddingPair, Storage};
use crate::storage::schema::{ChunkDoc, Doc};
use crate::storage::sqlite::{
    AtlasFramePoint, AtlasFrameRow, AtlasLabelRow, CascadeDbOutcome, CodeRefDoc, CodeRefHeaderRow,
    CodeRefRow, CommitMapRow, CorkboardRow, DayKindCount, Db, EdgeRow, ErrorRow, ExclusionRow,
    FolderStats, FunnelCounts, HistoryRow, ListEntryRow, ListRow, MemoryCommitRow,
    MemoryRecallCount, MemoryRecallRow, MemoryRecallWeeklyRow, MemoryRecalledByRow, MoveRow,
    NewAtlasFrame, OpenResult, ProjectHarnessRow, ProjectStatsRow, ReadingResume,
    ResearchRollupRow, RunRow, SectionDwell, SessionCommitMatch, SessionCommitRow,
    SessionDecisionRow, SessionFileRow, SessionResearchRow, SessionRow, ShareRow, SloSnapshotRow,
    SnapshotMeta, SourceRow, SweepOutcome,
};
use crate::{Error, Result};
use futures::FutureExt;
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

/// Channel capacity. Topic 02's perf reality (slow embeds in v0.1+) means
/// the indexer can stall behind embed work; bound the queue so backpressure
/// surfaces as `try_send` errors rather than unbounded growth.
pub const CHANNEL_CAPACITY: usize = 1024;

/// Max lance mutations parked while an off-loop compaction is in flight.
/// Once hit, the drain loop stops pulling from the channel until the
/// optimize finishes, so a bulk-index storm during a compaction window
/// still backpressures through the bounded channel (the indexer's
/// `blocking_send` contract) instead of buffering unboundedly in memory.
const COMPACT_DEFER_CAP: usize = 512;

/// SC4 — write-starvation bound for the read-priority lanes. The actor drains
/// the read lane ahead of the write lane so a foreground search never queues
/// behind a bulk-ingest backlog; after this many consecutive reads are served
/// with nothing else changing, the loop forces one write so a relentless read
/// stream can never starve ingest forever. Tuned to favour reads heavily
/// (reads are fast + latency-sensitive; writes tolerate a small delay) while
/// keeping the guaranteed write cadence tight (≥1 write per N reads).
const READ_STARVATION_BOUND: u32 = 16;

/// One reconcile-projection row: `(id, path, mtime_unix)`. Narrow scan for
/// the reconcile safety net — `path`/`mtime` drive the delete + dedup passes,
/// `id` feeds the R2 orphan sweep — instead of the full `list_docs` slim scan.
pub type ReconcileRow = (String, String, Option<i64>);

/// One dedup-cache warm-up row: `(id, content_hash, stored mtime_unix)`.
/// The indexer's startup cache pre-population pulls this once; the stored
/// mtime (nullable — rows may predate the column) is what lets the dedup
/// pre-gate's mtime heal fire only when disk != stored (v0.24 SC1).
pub type ContentHashRow = (String, String, Option<i64>);

/// v0.33 X2 — one `doc_first_seen` bring-up seed row:
/// `(id, created_unix, mtime_unix, indexed_at_unix)`.
pub type FirstSeenSeedRow = (String, Option<i64>, Option<i64>, Option<i64>);

/// Operations that mutate per-kb storage. Each variant carries a
/// `oneshot::Sender` so the caller awaits the result.
pub enum StorageMsg {
    UpsertSource {
        slug: SourceSlug,
        path: PathBuf,
        added_at_unix: i64,
        reply: oneshot::Sender<Result<()>>,
    },
    SetSourcePaused {
        slug: SourceSlug,
        paused: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    ListSources {
        reply: oneshot::Sender<Result<Vec<SourceRow>>>,
    },
    /// X2 — record a per-file exclusion (source-relative path, pre-normalised
    /// by the caller). Replies `false` when the path was already excluded.
    AddExclusion {
        path: String,
        excluded_at_unix: i64,
        note: Option<String>,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// X2 — drop a per-file exclusion. Replies `false` when it wasn't there.
    RemoveExclusion {
        path: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// X2 — every excluded path, newest first.
    ListExclusions {
        reply: oneshot::Sender<Result<Vec<ExclusionRow>>>,
    },
    BeginRun {
        source_slug: SourceSlug,
        started_at_unix: i64,
        reply: oneshot::Sender<Result<RunId>>,
    },
    FinishRun {
        run_id: RunId,
        ok_count: u32,
        err_count: u32,
        finished_at_unix: i64,
        reply: oneshot::Sender<Result<()>>,
    },
    LastRunForSource {
        slug: SourceSlug,
        reply: oneshot::Sender<Result<Option<RunRow>>>,
    },
    RecordError {
        kind: String,
        source_slug: SourceSlug,
        path: PathBuf,
        message: String,
        content_hash: Option<String>,
        created_at_unix: i64,
        reply: oneshot::Sender<Result<ErrorId>>,
    },
    ClearErrorsForPathHash {
        path: PathBuf,
        new_content_hash: String,
        reply: oneshot::Sender<Result<usize>>,
    },
    ClearErrorsForPath {
        path: PathBuf,
        reply: oneshot::Sender<Result<usize>>,
    },
    DismissError {
        id: ErrorId,
        reply: oneshot::Sender<Result<()>>,
    },
    ListOpenErrors {
        reply: oneshot::Sender<Result<Vec<ErrorRow>>>,
    },
    RetryCountForPathHash {
        path: PathBuf,
        content_hash: String,
        reply: oneshot::Sender<Result<u32>>,
    },
    UpsertDoc {
        doc: Box<Doc>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// GC-B7 — batched sibling of `UpsertDoc`: one Lance `merge_insert` /
    /// manifest commit for the WHOLE slice instead of one per doc. Wired by
    /// the indexer's drain-batching path (`indexer::flush_prepared_batch`)
    /// so a live-watcher burst or a cold bulk import doesn't shred the
    /// dataset into one fragment per file.
    UpsertDocs {
        docs: Vec<Doc>,
        reply: oneshot::Sender<Result<()>>,
    },
    DeleteDoc {
        id: ArtifactId,
        reply: oneshot::Sender<Result<()>>,
    },
    DeleteByPath {
        path: PathBuf,
        reply: oneshot::Sender<Result<()>>,
    },
    /// R2 — the storage side of the per-artifact delete cascade: lance
    /// doc+chunks (FIRST, mirroring `DropKbData`) then the sqlite dependent
    /// tables in ONE transaction, then one `bump_generation`. The filesystem
    /// side (`.review` sidecar + attachments) is NOT here — it can't reach the
    /// actor's stores and lives in `cascade::delete_artifact`.
    CascadeDeleteDoc {
        id: ArtifactId,
        path: PathBuf,
        mode: CascadeMode,
        reply: oneshot::Sender<Result<CascadeDbOutcome>>,
    },
    /// F3a — write a moves INTENT row (`completed_at` NULL) before the FS
    /// rename. Returns the row id for the subsequent [`StorageMsg::RelocateDoc`].
    MovesInsertIntent {
        old_id: String,
        new_id: String,
        old_rel: String,
        new_rel: String,
        moved_at: i64,
        reply: oneshot::Sender<Result<i64>>,
    },
    /// F3a — actor-atomic lance id rekey, sqlite rekey, moves.completed_at,
    /// and one generation bump. The FS rename and review/attachments rekey
    /// run outside the actor (under `review_lock`) before this message is sent.
    /// Reply carries the DISTINCT list_ids whose entries were rekeyed.
    RelocateDoc {
        old_id: String,
        new_id: String,
        /// Canonical absolute path stored on the lance row.
        new_path: String,
        old_rel: String,
        new_rel: String,
        moves_row_id: i64,
        completed_at: i64,
        reply: oneshot::Sender<Result<Vec<String>>>,
    },
    /// F3a — chain-aware redirect lookup (old_id or old_rel → newest new).
    MovesLookup {
        key: String,
        reply: oneshot::Sender<Result<Option<(String, String)>>>,
    },
    /// F3a — durable delete-suppression check for the watcher race guard.
    MovesSuppressesDelete {
        old_rel: String,
        now_unix: i64,
        grace_secs: i64,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// F3a — incomplete intent rows for startup replay.
    MovesListIncomplete {
        reply: oneshot::Sender<Result<Vec<MoveRow>>>,
    },
    /// F3a — abandon (or force-complete) an intent row without a full rekey.
    MovesMarkCompleted {
        row_id: i64,
        completed_at: i64,
        reply: oneshot::Sender<Result<()>>,
    },
    /// R2 — the reconcile orphan backstop: prune sqlite dependents whose
    /// artifact id isn't in `keep_ids` (every live lance id ∪ the exempt set).
    SweepOrphans {
        keep_ids: std::collections::HashSet<String>,
        reply: oneshot::Sender<Result<SweepOutcome>>,
    },
    EnsureFtsIndex {
        reply: oneshot::Sender<Result<()>>,
    },
    CountRows {
        reply: oneshot::Sender<Result<u64>>,
    },
    /// GC-B2 — cumulative typed-decode skips (malformed batches dropped by
    /// `batches_to_summaries`/`batches_to_embeddings`) since this kb's
    /// `Storage` was opened. Observability for otherwise-silent result-set
    /// shrinkage; surfaced in `/api/stats`.
    DecodeSkipCount {
        reply: oneshot::Sender<Result<u64>>,
    },
    Bm25Query {
        q: String,
        limit: u32,
        /// GC-D1 — see `Storage::bm25_query`'s `typo_tolerance` param.
        typo_tolerance: bool,
        reply: oneshot::Sender<Result<Vec<DocSummary>>>,
    },
    /// v0.1: vector-only semantic query. `query_vec` must match the kb's
    /// embedding model dimension (384 for bge-small).
    VectorQuery {
        query_vec: Vec<f32>,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<DocSummary>>>,
    },
    /// v0.1: hybrid BM25 + vector via lance RRF (k=60).
    HybridQuery {
        q: String,
        query_vec: Vec<f32>,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<DocSummary>>>,
    },
    /// v0.1: idempotent IVF-PQ index over the embedding column.
    EnsureVectorIndex {
        reply: oneshot::Sender<Result<()>>,
    },
    /// SQ5: replace a doc's passage chunks (search-only sidecar; does NOT
    /// bump the gallery generation).
    UpsertChunks {
        doc_id: String,
        chunks: Vec<ChunkDoc>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// F3a — read-class listing of passage chunks for one doc (relocate
    /// tests + future admin). Does not mutate.
    ListChunksForDoc {
        doc_id: String,
        reply: oneshot::Sender<Result<Vec<ChunkDoc>>>,
    },
    /// SQ5: vector search over passage chunks, max-pooled per doc.
    ChunkVectorQuery {
        query_vec: Vec<f32>,
        over_fetch: u32,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<DocSummary>>>,
    },
    /// SQ5: idempotent IVF-PQ index over the chunk embedding column.
    EnsureChunkVectorIndex {
        reply: oneshot::Sender<Result<()>>,
    },
    /// Run `Table::optimize(OptimizeAction::All)` — compact data
    /// fragments, rebuild indices, prune old versions. Single message
    /// because the underlying lance call already runs the three steps
    /// internally. The actor runs the (multi-second) optimize on a
    /// spawned task and keeps servicing non-lance-mutating messages
    /// meanwhile; lance mutations arriving during the window are parked
    /// and replayed FIFO after it completes, so single-writer-per-kb is
    /// preserved without freezing search behind maintenance.
    CompactAll {
        reply: oneshot::Sender<Result<CompactStats>>,
    },
    /// GC-B7 — same as `CompactAll` but lets the caller override the
    /// old-version retention window + `delete_unverified`, bypassing
    /// `compact_all`'s safe production default (`Storage::compact_all`'s
    /// `PRUNE_RETENTION_MINUTES`). Exists so tests (and an eventual ops
    /// "vacuum now" verb) can exercise the physical-reclaim mechanism
    /// deterministically instead of waiting out the real window. Routed
    /// through the same off-loop treatment as `CompactAll` — see
    /// `compact_off_loop`.
    CompactAllWithRetention {
        retention_minutes: i64,
        delete_unverified: bool,
        reply: oneshot::Sender<Result<CompactStats>>,
    },
    /// Cheap shape snapshot (rows / fragments / indices / versions) for
    /// the startup auto-compact heuristic. No data scan.
    DatasetStats {
        reply: oneshot::Sender<Result<DatasetStats>>,
    },
    /// v0.1: unfiltered scan returning up to `limit` doc summaries — the
    /// SPA gallery's "show me everything" view.
    ListDocs {
        limit: u32,
        reply: oneshot::Sender<Result<Vec<DocSummary>>>,
    },
    /// v0.9 M3: every non-null `kb_supersedes` value across the table.
    /// Recall builds its supersede tombstone set from this so an old
    /// memory drops even when its superseder wasn't in the hit window.
    ListSupersedeTargets {
        reply: oneshot::Sender<Result<Vec<String>>>,
    },
    /// v0.16: every non-null (id, content_hash, mtime_unix) triple. The
    /// indexer pulls this once at startup to pre-populate its dedup cache
    /// so the watcher's initial-walk `watch.create` events skip the embed
    /// pipeline when bytes are unchanged; the stored mtime gates the
    /// pre-gate's heal to genuinely-touched files (v0.24 SC1).
    ListContentHashes {
        reply: oneshot::Sender<Result<Vec<ContentHashRow>>>,
    },
    /// v0.33 X2 — narrow seed projection for `doc_first_seen` bring-up:
    /// `(id, created_unix, mtime_unix, indexed_at_unix)`.
    ListFirstSeenSeedRows {
        reply: oneshot::Sender<Result<Vec<FirstSeenSeedRow>>>,
    },
    /// Reconcile projection: every row's `(path, mtime_unix)`. The periodic
    /// reconcile safety net pulls just these two columns (delete pass + dedup
    /// map) instead of the full `list_docs` slim projection, so a full-corpus
    /// reconcile tick occupies the single-writer actor for less time against
    /// live search/read traffic.
    ListReconcileRows {
        reply: oneshot::Sender<Result<Vec<ReconcileRow>>>,
    },
    /// v0.1: exact-id lookup. The artifact subdomain handler uses this
    /// to resolve a path-based artifact id to its row; /api/kb/{kb}/docs/{id}
    /// uses it instead of the BM25 + post-filter trick (which fails for
    /// hex IDs that don't tokenise as text).
    GetById {
        id: String,
        reply: oneshot::Sender<Result<Option<DocSummary>>>,
    },
    /// MI-W2.4a — `kb memory log <id>`'s FORWARD hop: what does `id` say
    /// it supersedes?
    LineageById {
        id: String,
        reply: oneshot::Sender<Result<Option<DocSummary>>>,
    },
    /// MI-W2.4a — `kb memory log <id>`'s REVERSE hop: which row(s) claim
    /// `kb_supersedes == target_id`?
    FindSupersededBy {
        target_id: String,
        reply: oneshot::Sender<Result<Vec<DocSummary>>>,
    },
    /// Batch exact-id lookup — resolve a slice of ids in one lance scan. The
    /// fleet inbox uses this to resolve every commented artifact's
    /// title/source-path without one actor round-trip per review file.
    GetByIds {
        ids: Vec<String>,
        reply: oneshot::Sender<Result<Vec<DocSummary>>>,
    },
    /// Q-track (board B1) — batch FULL-body lookup for the search route's
    /// match-context snippet extraction, bounded to the exact page of
    /// hits it's about to return (see `storage::lance::Storage::
    /// get_bodies_by_ids`, which projects `body` — distinct from
    /// `GetByIds`'s capped `body_text_excerpt`).
    GetBodiesByIds {
        ids: Vec<String>,
        reply: oneshot::Sender<Result<Vec<(String, String)>>>,
    },
    /// W2.3a — fetch one doc's embedding vector (see
    /// `storage::lance::Storage::embedding_by_id`). The true-neighbors
    /// route's seed-vector lookup.
    EmbeddingById {
        id: String,
        reply: oneshot::Sender<Result<Option<Vec<f32>>>>,
    },
    /// W2.3a — batch variant of `EmbeddingById` (see `storage::lance::
    /// Storage::embeddings_by_ids`). The true-neighbors route uses this to
    /// fetch its small (`limit`-bounded) neighbor set's vectors for an
    /// in-route cosine computation.
    EmbeddingsByIds {
        ids: Vec<String>,
        reply: oneshot::Sender<Result<Vec<EmbeddingPair>>>,
    },
    /// W2.11 — fetch one doc's stored generation prompt + its capped byte
    /// size (see `storage::lance::Storage::prompt_by_id`). The prompt-browse
    /// route's read; scrub-gating happens in `kb-server`, not here.
    PromptById {
        id: String,
        reply: oneshot::Sender<Result<Option<(String, u32)>>>,
    },
    /// Exact-path lookup over the lance `path` column. The artifact
    /// subdomain handler uses this for cross-artifact relative links:
    /// when a sub-path canonicalises to a file in the kb's source root,
    /// this answers "is that file itself an indexed artifact?".
    GetBySourcePath {
        path: String,
        reply: oneshot::Sender<Result<Option<DocSummary>>>,
    },
    /// Batch exact-path lookup — resolve a slice of stored paths in one
    /// `path IN (…)` scan. The edge-record enrichment hook uses this to
    /// resolve a link-heavy page's relative hrefs without one actor
    /// round-trip per href.
    GetBySourcePaths {
        paths: Vec<String>,
        reply: oneshot::Sender<Result<Vec<DocSummary>>>,
    },
    /// v0.3: write a batch of (id, x, y, cluster) atlas tuples.
    UpdateAtlas {
        rows: Vec<(String, f32, f32, i16)>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// W1.B: replace the whole `atlas_labels` table with a fresh c-TF-IDF
    /// label set (see `kb_core::atlas_labels`). Like `UpdateAtlas`, this
    /// never bumps the index generation (invariant #15) — it's a sqlite
    /// side table, not a lance row-set change.
    SetAtlasLabels {
        labels: Vec<AtlasLabelRow>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// W1.B: read every stored atlas label, ordered `(cluster, rank)`.
    GetAtlasLabels {
        reply: oneshot::Sender<Result<Vec<AtlasLabelRow>>>,
    },
    /// W3 T-a: append one atlas time-lapse frame + its points (V0028), then
    /// prune to `DEFAULT_ATLAS_FRAMES_KEEP`. `Ok(None)` = the geometry was
    /// bit-identical to the newest frame, so nothing was written. WRITE-class
    /// and, like `SetAtlasLabels`, sqlite-only — it must NOT bump the index
    /// generation (invariant #15).
    AtlasSnapshotInsert {
        frame: Box<NewAtlasFrame>,
        points: Vec<AtlasFramePoint>,
        reply: oneshot::Sender<Result<Option<i64>>>,
    },
    /// W3 T-a: frame metadata, newest first, capped at `limit`.
    AtlasSnapshotsList {
        limit: u32,
        reply: oneshot::Sender<Result<Vec<AtlasFrameRow>>>,
    },
    /// W3 T-a: one frame's points, ordered by `artifact_id`.
    AtlasSnapshotPoints {
        snapshot_id: i64,
        reply: oneshot::Sender<Result<Vec<AtlasFramePoint>>>,
    },
    /// W3 T-a: drop all but the newest `keep` frames (explicit operator
    /// prune; the insert path already self-prunes). WRITE-class.
    AtlasSnapshotPrune {
        keep: u32,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// Heal a stale `mtime_unix` for one row so the reconcile producer-side
    /// dedup stops re-emitting a touched-but-unchanged file every pass.
    TouchMtime {
        id: String,
        mtime_unix: i64,
        reply: oneshot::Sender<Result<()>>,
    },
    /// v0.3: read all (id, embedding) pairs for atlas recompute.
    ListEmbeddings {
        reply: oneshot::Sender<Result<Vec<EmbeddingPair>>>,
    },
    /// v0.3: list all docs WITH atlas coords populated.
    ListDocsWithAtlas {
        limit: u32,
        reply: oneshot::Sender<Result<Vec<DocSummary>>>,
    },
    /// v0.3 F1: replace outbound edges from `from_id`.
    RecordEdges {
        from_id: String,
        to_kinds: Vec<(String, String)>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// DCB W1.A — replace an artifact's code-ref extraction. `Ok(true)` = the
    /// set changed. NEVER bumps the index generation (see the handler).
    RecordCodeRefs {
        /// Boxed: 8 fields, and `StorageMsg`'s size is the per-message cost of
        /// every send on the channel.
        header: Box<CodeRefHeaderRow>,
        refs: Vec<CodeRefRow>,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// DCB W1.A — one artifact's extraction (`None` = never scanned).
    CodeRefsOf {
        artifact_id: String,
        reply: oneshot::Sender<Result<Option<CodeRefDoc>>>,
    },
    /// DCB W1.A — keyset page over the corpus feed.
    CodeRefsFeed {
        after: Option<(i64, String)>,
        limit: u32,
        with_refs: bool,
        reply: oneshot::Sender<Result<Vec<CodeRefDoc>>>,
    },
    /// CT-B3 — every doc citing `path` (exact `path_hint` match). A
    /// complete resolution (no cursor), backing `?by_target=` on the feed
    /// route.
    CodeRefsByTarget {
        path: String,
        with_refs: bool,
        reply: oneshot::Sender<Result<Vec<CodeRefDoc>>>,
    },
    /// CT-F5 — `(total_hints, path_shaped_hints)` over `code_refs`.
    CodeRefShapeCounts {
        reply: oneshot::Sender<Result<(u64, u64)>>,
    },
    /// CT-F5 — distinct non-empty `kb_session` values with their doc counts
    /// (lance scan). Includes transcripts — see `kb_session_doc_counts`.
    KbSessionDocCounts {
        reply: oneshot::Sender<Result<std::collections::HashMap<String, u64>>>,
    },
    /// CT-F5 — which of these session ids have a `sessions` row here.
    SessionIdsPresent {
        session_ids: Vec<String>,
        reply: oneshot::Sender<Result<Vec<String>>>,
    },
    /// CT-F5 — newest `sessions.started_at`, or `None` on an empty table.
    SessionsNewestStartedAt {
        reply: oneshot::Sender<Result<Option<i64>>>,
    },
    /// CT-F5 — CT-A3 recall census sums over the newest capture per session.
    SessionsRecallCensusTotals {
        reply: oneshot::Sender<Result<(u64, u64, u64, u64)>>,
    },
    /// CT-F5 — write one capture's CT-A3 recall parse census (V0039).
    SessionsSetRecallCensus {
        artifact_id: String,
        marker_parsed: u32,
        fallback_parsed: u32,
        failed: u32,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// CT-F5 — append one `kb slo snapshot` run (one row per indicator).
    SloSnapshotAppend {
        taken_at_unix: i64,
        indicators: Vec<crate::slo::SloIndicator>,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// CT-F5 — newest-first page over the append-only snapshot log.
    SloSnapshotsList {
        limit: u32,
        reply: oneshot::Sender<Result<Vec<SloSnapshotRow>>>,
    },
    /// v0.3 F1: BFS outbound from `start_id` to `max_depth` (clamped to 1..=3).
    EdgesFrom {
        start_id: String,
        max_depth: u32,
        reply: oneshot::Sender<Result<Vec<EdgeRow>>>,
    },
    /// Inbound edges to `id` — every artifact linking here (depth 1).
    /// Backs the "Linked from" / "Referenced in notes" panels.
    BacklinksOf {
        id: String,
        reply: oneshot::Sender<Result<Vec<EdgeRow>>>,
    },
    /// v0.6 B2: per-artifact (outbound, inbound) edge counts. Returns
    /// a map keyed by artifact id; artifacts with zero edges are
    /// absent (callers treat missing as `(0, 0)`).
    EdgeCounts {
        reply: oneshot::Sender<Result<std::collections::HashMap<String, (u32, u32)>>>,
    },
    /// Bulk-fetch every `kind = 'link'` edge as `(src, dst)` pairs.
    /// Backs the SPA atlas view's edge layer.
    LinkPairs {
        reply: oneshot::Sender<Result<Vec<(String, String)>>>,
    },
    /// v0.3 G4: NULL the embedding column on every row (`kb model set
    /// --in-place` drives this for same-dim swaps so the indexer's
    /// next pass repopulates them without dropping the table).
    ClearEmbeddings {
        reply: oneshot::Sender<Result<()>>,
    },
    /// v0.6+ H1: begin or resume an artifact-view visit (30-min gap
    /// rule). Returns `OpenResult { id, scroll_y, is_new_visit }` so the
    /// SPA can auto-resume on reopen and the HTTP layer can emit
    /// `history.recorded` only on new visits (not bumps).
    HistoryRecordOpen {
        artifact_id: String,
        now_unix: i64,
        /// GC-B5 — `Some("web")`/`Some("cli")`; `None` from any caller
        /// that predates the distinction.
        source: Option<String>,
        /// v0.34 X1 — attribution username (lowercase).
        user: String,
        reply: oneshot::Sender<Result<OpenResult>>,
    },
    /// v0.6+ H1: UPDATE scroll position on an open visit. Returns the
    /// rows-affected count (0 = visit_id not found / not an open row).
    HistoryUpdateScroll {
        visit_id: i64,
        scroll_y: i64,
        scroll_max: i64,
        now_unix: i64,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// v0.6+ H1: record a search query (5-second dedup window).
    HistoryRecordSearch {
        query: String,
        now_unix: i64,
        user: String,
        reply: oneshot::Sender<Result<i64>>,
    },
    /// v0.6+ H1: record a new-comment event (always inserts; caller
    /// decides whether the upstream save was a new comment vs an edit).
    HistoryRecordComment {
        artifact_id: String,
        comment_id: String,
        now_unix: i64,
        user: String,
        reply: oneshot::Sender<Result<i64>>,
    },
    /// v0.6+ H1: newest-first list with optional kind filter and
    /// `started_at < before_unix` cursor.
    HistoryList {
        limit: u32,
        before_unix: Option<i64>,
        kind_filter: Option<String>,
        /// `None` = all users (team timeline).
        user: Option<String>,
        reply: oneshot::Sender<Result<Vec<HistoryRow>>>,
    },
    /// RP-track — UPSERT per-section reading dwell for a visit (cumulative,
    /// max-merged). Returns rows written. Like all reading arms it is a
    /// sqlite side-channel that does NOT bump the index generation.
    ReadingUpsertSections {
        visit_id: i64,
        artifact_id: String,
        sections: Vec<SectionDwell>,
        now_unix: i64,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// RP-track — set a visit's active_ms + stop-point. Rows-affected 0 =
    /// unknown visit (the reading endpoint's 404 gate).
    ReadingSetActive {
        visit_id: i64,
        active_ms: i64,
        last_section: Option<String>,
        now_unix: i64,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// RP-track — a visit's resume baseline (seed-on-open).
    ReadingStateForVisit {
        visit_id: i64,
        reply: oneshot::Sender<Result<ReadingResume>>,
    },
    /// RP-track — section rows + visit roll-ups for an artifact's summary.
    /// `user = Some` scopes to that user's visits (v0.34 Y1).
    ReadingInputsForArtifact {
        artifact_id: String,
        user: Option<String>,
        #[allow(clippy::type_complexity)]
        reply: oneshot::Sender<
            Result<(
                Vec<crate::reading::ReadingSectionRow>,
                Vec<crate::reading::VisitRollup>,
            )>,
        >,
    },
    /// RP-track — batched section rows + visit roll-ups for a SET of
    /// artifacts in two grouped queries. Backs the reading-lists
    /// enrichment (collapses the per-entry `ReadingInputsForArtifact`
    /// fan-out into one actor round-trip per id set).
    /// `user = Some` scopes to that user's visits (v0.34 Y1).
    ReadingInputsForArtifacts {
        artifact_ids: Vec<String>,
        user: Option<String>,
        #[allow(clippy::type_complexity)]
        reply: oneshot::Sender<
            Result<
                std::collections::HashMap<
                    String,
                    (
                        Vec<crate::reading::ReadingSectionRow>,
                        Vec<crate::reading::VisitRollup>,
                    ),
                >,
            >,
        >,
    },
    /// v0.34 Y1 — distinct non-empty history.user values (GET /api/users).
    HistoryDistinctUsers {
        reply: oneshot::Sender<Result<Vec<String>>>,
    },
    /// RP-track — cheap latest-visit (completion_pct, last_section,
    /// last_read_at) for recall enrichment.
    ReadingLatestForArtifact {
        artifact_id: String,
        user: String,
        #[allow(clippy::type_complexity)]
        reply: oneshot::Sender<Result<Option<(u8, Option<String>, i64)>>>,
    },
    /// RP-track — batched [`ReadingLatestForArtifact`] for a SET of ids in one
    /// window pass. Recall enrichment groups its hits by kb and issues one of
    /// these per kb instead of one round-trip per hit.
    ReadingLatestForIds {
        artifact_ids: Vec<String>,
        user: String,
        #[allow(clippy::type_complexity)]
        reply:
            oneshot::Sender<Result<std::collections::HashMap<String, (u8, Option<String>, i64)>>>,
    },
    /// Q-track — batched read-state rollup over the whole `history` table
    /// (newest open visit per artifact + list override overlay). Backs the
    /// search read-state facet and `opened` / `progress` sorts.
    ReadingRollup {
        user: String,
        reply:
            oneshot::Sender<Result<std::collections::HashMap<String, crate::reading::ReadRollup>>>,
    },
    /// Q-track — [`ReadingRollup`] scoped to a candidate id set (the search
    /// page consumes rollup entries only for the surviving hits), keeping the
    /// window scan proportional to the page, not the whole history table.
    ReadingRollupForIds {
        ids: Vec<String>,
        user: String,
        reply:
            oneshot::Sender<Result<std::collections::HashMap<String, crate::reading::ReadRollup>>>,
    },
    /// v0.33 X2 — INSERT OR IGNORE first-indexed timestamp for one artifact.
    FirstSeenInsertIgnore {
        artifact_id: String,
        ts: i64,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// v0.33 X2 — batched first-indexed lookup for a page of ids.
    FirstSeenForIds {
        ids: Vec<String>,
        reply: oneshot::Sender<Result<std::collections::HashMap<String, i64>>>,
    },
    /// v0.33 X2 — bring-up bulk seed (one tx of INSERT OR IGNORE).
    FirstSeenSeed {
        rows: Vec<(String, i64)>,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// v0.33 X2 — is `doc_first_seen` empty? (bring-up seed gate).
    FirstSeenIsEmpty {
        reply: oneshot::Sender<Result<bool>>,
    },
    /// v0.34 X1 — idempotent identity backfill (WRITE lane; no generation bump).
    IdentityBackfill {
        operator: String,
        now_unix: i64,
        reply: oneshot::Sender<Result<u64>>,
    },
    /// v0.34 X1 — set/clear per-user list read override (WRITE; no gen bump).
    ListEntrySetUserOverride {
        entry_id: String,
        user: String,
        override_: Option<String>,
        now_unix: i64,
        reply: oneshot::Sender<Result<()>>,
    },
    /// v0.34 X1 — per-user override map for a list's entries (READ lane).
    ListEntryUserOverridesForList {
        list_id: String,
        user: String,
        reply: oneshot::Sender<Result<std::collections::HashMap<String, String>>>,
    },
    /// RP-track — open-visits in a [from,to] window (session readings).
    HistoryOpensInWindow {
        from_unix: i64,
        to_unix: i64,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<HistoryRow>>>,
    },
    /// R8 — comment-creation events in a time window ("raised during").
    HistoryCommentsInWindow {
        from_unix: i64,
        to_unix: i64,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<HistoryRow>>>,
    },
    /// W2.10 — per-day, per-kind event counts in a [from,to] window (the
    /// activity-calendar density grid).
    HistoryCountsByDay {
        from_unix: i64,
        to_unix: i64,
        reply: oneshot::Sender<Result<Vec<DayKindCount>>>,
    },
    /// kb share registry (V0004): insert/upsert a share row keyed on name.
    /// Boxed — `ShareRow` is ~13 fields, kept off the hot enum.
    SharesInsert {
        row: Box<ShareRow>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// kb share registry: all shares, newest-first.
    SharesList {
        reply: oneshot::Sender<Result<Vec<ShareRow>>>,
    },
    /// kb share registry: one share by name.
    SharesGet {
        name: String,
        reply: oneshot::Sender<Result<Option<ShareRow>>>,
    },
    /// kb share registry: most-recent share for a target (drives --update).
    SharesGetByTarget {
        target: String,
        reply: oneshot::Sender<Result<Option<ShareRow>>>,
    },
    /// kb share registry: delete a share by name (revoke). Rows affected.
    SharesDelete {
        name: String,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// Corkboard (anchor bookmarks, V0005) — pin an artifact. Replies
    /// with `true` if a fresh row was inserted, `false` if the artifact
    /// was already pinned.
    CorkboardAdd {
        artifact_id: String,
        now_unix: i64,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Corkboard — unpin an artifact. Replies with `true` if a row was
    /// deleted, `false` if it wasn't pinned.
    CorkboardRemove {
        artifact_id: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Corkboard — read-only list, newest-first.
    CorkboardList {
        reply: oneshot::Sender<Result<Vec<CorkboardRow>>>,
    },
    /// Corkboard — cheap count for the Header anchor-pill badge.
    CorkboardCount {
        reply: oneshot::Sender<Result<u64>>,
    },
    /// Pinned memories (V0006) — pin a memory artifact in this kb.
    PinnedMemoryAdd {
        artifact_id: String,
        now_unix: i64,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Pinned memories — unpin.
    PinnedMemoryRemove {
        artifact_id: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Pinned memories — full set for the recall route's fan-out
    /// (HashSet for O(1) per-hit lookup).
    PinnedMemoriesSet {
        reply: oneshot::Sender<Result<std::collections::HashSet<String>>>,
    },
    /// Memory links (V0010) — full link set for one memory (including
    /// the `*` global sentinel when present).
    MemoryLinksFor {
        artifact_id: String,
        reply: oneshot::Sender<Result<Vec<String>>>,
    },
    /// Memory links — idempotent INSERT of one (artifact, kb) edge.
    MemoryLinkAdd {
        artifact_id: String,
        linked_kb: String,
        now_unix: i64,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Memory links — idempotent DELETE of one (artifact, kb) edge.
    MemoryLinkRemove {
        artifact_id: String,
        linked_kb: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Memory links — atomic replace of the entire link set for one
    /// memory. `global` toggles the `*` sentinel; an empty
    /// `linked_kbs` + `global = false` unlinks the memory entirely.
    MemoryLinksReplace {
        artifact_id: String,
        linked_kbs: Vec<String>,
        global: bool,
        now_unix: i64,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Memory links — drop every edge for a memory + clear its seeded
    /// tombstone. Called by the indexer's `process_delete`.
    MemoryLinksRemoveAll {
        artifact_id: String,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Memory links — bulk fetch for the recall fan-out. One scan per
    /// memory corpus per recall request.
    MemoryLinksAll {
        reply: oneshot::Sender<
            Result<std::collections::HashMap<String, std::collections::HashSet<String>>>,
        >,
    },
    /// Memory links seeded (V0011) — has this memory ever been seeded?
    MemoryLinksSeededHas {
        artifact_id: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Memory links seeded (V0011) — mark a memory as seeded so the
    /// indexer never re-imports its metas.
    MemoryLinksSeededMark {
        artifact_id: String,
        now_unix: i64,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Reading lists (V0015, RL-track) — create a list. `Conflict` on a
    /// case-insensitive title clash.
    ListCreate {
        id: String,
        title: String,
        description: Option<String>,
        pinned: bool,
        now_unix: i64,
        reply: oneshot::Sender<Result<ListRow>>,
    },
    /// Reading lists — single-list lookup.
    ListGet {
        id: String,
        reply: oneshot::Sender<Result<Option<ListRow>>>,
    },
    /// Reading lists — all lists, pinned-first then newest-touched.
    ListsAll {
        reply: oneshot::Sender<Result<Vec<ListRow>>>,
    },
    /// Reading lists — patch header fields. `Ok(None)` when missing.
    ListUpdate {
        id: String,
        title: Option<String>,
        description: Patch<String>,
        pinned: Option<bool>,
        archived: Option<bool>,
        now_unix: i64,
        reply: oneshot::Sender<Result<Option<ListRow>>>,
    },
    /// Reading lists — delete (entries cascade). `true` if removed.
    ListDelete {
        id: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Reading lists — insert one entry at a position.
    /// `user` stamps a read marker into `list_entry_user_state` when
    /// `entry.read_override` is set (legacy column frozen, v0.34 X1).
    ListEntryAdd {
        entry: NewListEntry,
        pos: PositionSpec,
        user: String,
        now_unix: i64,
        reply: oneshot::Sender<Result<ListEntryRow>>,
    },
    /// Reading lists — one list's entries in display order.
    ListEntriesForList {
        list_id: String,
        reply: oneshot::Sender<Result<Vec<ListEntryRow>>>,
    },
    /// Reading lists — every entry in the kb (cross-kb index roll-up).
    ListEntriesAll {
        reply: oneshot::Sender<Result<Vec<ListEntryRow>>>,
    },
    /// Reading lists — patch one entry's content (note/anchor/override).
    /// `read_override` routes to `list_entry_user_state` for `user`
    /// (legacy column frozen, v0.34 X1).
    ListEntryUpdate {
        entry_id: String,
        note: Patch<String>,
        anchor: Patch<(String, Option<i64>)>,
        read_override: Patch<String>,
        user: String,
        now_unix: i64,
        reply: oneshot::Sender<Result<Option<ListEntryRow>>>,
    },
    /// Reading lists — reorder one entry within its list.
    ListEntryMove {
        list_id: String,
        entry_id: String,
        pos: PositionSpec,
        now_unix: i64,
        reply: oneshot::Sender<Result<Option<ListEntryRow>>>,
    },
    /// Reading lists — remove one entry (returns the removed row).
    ListEntryRemove {
        list_id: String,
        entry_id: String,
        now_unix: i64,
        reply: oneshot::Sender<Result<Option<ListEntryRow>>>,
    },
    /// v0.33 X3 — bulk-remove entry ids from a list in ONE tx (prune).
    ListEntriesRemoveMany {
        list_id: String,
        entry_ids: Vec<String>,
        now_unix: i64,
        reply: oneshot::Sender<Result<Vec<ListEntryRow>>>,
    },
    /// Reading lists — entries targeting one artifact, across lists
    /// (the ListAnchorHook's per-reindex lookup).
    ListEntriesForArtifact {
        artifact_id: String,
        reply: oneshot::Sender<Result<Vec<ListEntryRow>>>,
    },
    /// Reading lists — the hook's machine write: refreshed anchor
    /// resolution + word estimates. Never touches `updated_at`.
    ListEntriesSyncResolution {
        updates: Vec<ResolutionUpdate>,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// Reading lists — bulk import (one transaction). Returns inserted.
    /// Read markers land in `list_entry_user_state` for `user` (legacy
    /// column frozen, v0.34 X1).
    ListImportEntries {
        list_id: String,
        mode: ImportMode,
        entries: Vec<NewListEntry>,
        user: String,
        now_unix: i64,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// Sessions enrichment (V0008) — upsert one row. The indexer
    /// calls this after the lance upsert for any doc with
    /// `kb_category = "memory-session"`.
    ///
    /// V0029 — `Box`ed: the row grew past `clippy::large_enum_variant`'s
    /// threshold once the project/harness/closure/substance columns landed
    /// (~450 bytes, mostly `Option<String>`s), which would otherwise size
    /// EVERY `StorageMsg` (even a cheap variant like a delete) to this
    /// variant's width.
    SessionsUpsert {
        row: Box<SessionRow>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Sessions enrichment — drop a row when its source file is
    /// unlinked (paired with the storage actor's `DeleteByPath`).
    SessionsDelete {
        artifact_id: String,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// Sessions enrichment — newest-first list. The HTTP layer fans
    /// out across each kb's actor and merges by `started_at DESC`.
    /// T5 — `before` (exclusive) restricts to rows older than the
    /// caller's cursor; `None` is the page-1 case.
    SessionsList {
        limit: u32,
        before: Option<i64>,
        before_id: Option<String>,
        /// A1 — optional working-directory (folder) filter; applied in SQL
        /// before LIMIT so keyset pagination within a folder stays coherent.
        folder: Option<String>,
        /// P6 — optional keyword filter (title/first_user_prompt/cwd).
        q: Option<String>,
        /// W3.A — optional `?project=` filter (SQL WHERE, same coherence
        /// rule as `folder`).
        project: crate::sessions::ProjectFilter,
        /// W3.A/S1 — optional `?substance=` csv set (SQL WHERE).
        substance: Vec<String>,
        /// W5/I — optional `?harness=` csv set (SQL WHERE, closed-set
        /// validated at the route).
        harness: Vec<String>,
        reply: oneshot::Sender<Result<Vec<SessionRow>>>,
    },
    /// Sessions enrichment — the folder facet (distinct cwd + count + latest).
    SessionsFolders {
        reply: oneshot::Sender<Result<Vec<FolderStats>>>,
    },
    /// W3.A/P4 — the `/api/sessions/projects` facet's per-project rollup,
    /// PRE-registry-merge.
    SessionsProjectsStats {
        reply: oneshot::Sender<Result<Vec<ProjectStatsRow>>>,
    },
    /// W3.A/P4 — the harness breakdown behind `harness_mix`, same grouping.
    SessionsProjectsHarnessMix {
        reply: oneshot::Sender<Result<Vec<ProjectHarnessRow>>>,
    },
    /// R9 — research queries aggregated by (cwd, kind, query).
    SessionsResearchRollup {
        substance: Vec<String>,
        reply: oneshot::Sender<Result<Vec<ResearchRollupRow>>>,
    },
    /// R9 — the activity-funnel stage counts (optionally folder-scoped).
    SessionsFunnelCounts {
        folder: Option<String>,
        /// W3.A — optional `?project=` filter (SQL WHERE).
        project: crate::sessions::ProjectFilter,
        substance: Vec<String>,
        reply: oneshot::Sender<Result<FunnelCounts>>,
    },
    /// R9 — in-corpus (session_id, kb, artifact_id) touches for a folder
    /// (feeds the funnel's `commented` stage).
    SessionFilesInFolder {
        folder: Option<String>,
        reply: oneshot::Sender<Result<Vec<(String, String, String)>>>,
    },
    /// Sessions enrichment — lookup by Claude Code session id. Most
    /// kbs hold at most one row per session id; collisions return
    /// the most-recently-started row.
    SessionsGet {
        session_id: String,
        reply: oneshot::Sender<Result<Option<SessionRow>>>,
    },
    /// Session files (V0017) — replace the full edge set for one session
    /// artifact in one transaction. Written by the indexer's session-capture
    /// hook after parsing the transcript.
    SessionFilesReplace {
        artifact_id_session: String,
        files: Vec<SessionFileRow>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Session files — the per-session file manifest (`/sessions/{sid}/files`).
    SessionFilesForSession {
        session_id: String,
        reply: oneshot::Sender<Result<Vec<SessionFileRow>>>,
    },
    /// Session files — the reverse "sessions that touched this artifact"
    /// edge set (A7); in-corpus rows only.
    SessionFilesForArtifact {
        target_artifact_id: String,
        reply: oneshot::Sender<Result<Vec<SessionFileRow>>>,
    },
    /// R2 (`kb why`) — session edges matching a file basename, the robust key
    /// for "which sessions touched this file" (most touched files aren't kb
    /// artifacts). The route refines each to Exact/Fuzzy by path alignment.
    SessionFilesForBasename {
        basename: String,
        reply: oneshot::Sender<Result<Vec<SessionFileRow>>>,
    },
    /// R2 (`kb why`) — batched session-metadata lookup for a set of ids (one
    /// `IN (...)` query, bounding the WHY assembler on a hot file).
    SessionsGetMany {
        session_ids: Vec<String>,
        reply: oneshot::Sender<Result<Vec<SessionRow>>>,
    },
    /// #11/recollect-R3 — resolve lance artifact ids to their sqlite session
    /// rows (the `sessions` table's PK is `artifact_id`, so this is the
    /// identity join recollect's candidate stage needs: the lance
    /// `kb_session` meta value is a dirty hint, never a join key).
    SessionsGetByArtifactIds {
        artifact_ids: Vec<String>,
        reply: oneshot::Sender<Result<Vec<SessionRow>>>,
    },
    /// Session decisions (V0018/S9) — replace one session's decisions log.
    SessionDecisionsReplace {
        artifact_id_session: String,
        decisions: Vec<SessionDecisionRow>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Session decisions — the log for one session, in order.
    SessionDecisionsForSession {
        session_id: String,
        reply: oneshot::Sender<Result<Vec<SessionDecisionRow>>>,
    },
    /// Session decisions — batched form keyed by session_id (newest capture
    /// only, #11). One `IN (...)` query for the ledger's per-day fan-in.
    SessionDecisionsForSessions {
        session_ids: Vec<String>,
        reply: oneshot::Sender<Result<std::collections::HashMap<String, Vec<SessionDecisionRow>>>>,
    },
    /// Session commits (V0019/P5) — replace one session's commits list.
    SessionCommitsReplace {
        artifact_id_session: String,
        commits: Vec<SessionCommitRow>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Session commits — the list for one session, in order.
    SessionCommitsForSession {
        session_id: String,
        reply: oneshot::Sender<Result<Vec<SessionCommitRow>>>,
    },
    /// Session commits — batched form keyed by session_id (newest capture
    /// only, #11). One `IN (...)` query for the ledger's per-day fan-in.
    SessionCommitsForSessions {
        session_ids: Vec<String>,
        reply: oneshot::Sender<Result<std::collections::HashMap<String, Vec<SessionCommitRow>>>>,
    },
    /// kb-code Wave 0 (W0.6) — `GET /api/sessions/by-commit?sha=`: commits
    /// whose sha/sha_full starts with `prefix`, newest capture only.
    SessionCommitsBySha {
        prefix: String,
        reply: oneshot::Sender<Result<Vec<SessionCommitMatch>>>,
    },
    /// kb-code Wave 0 (W0.6) — `GET /api/sessions/commit-map`: the flat,
    /// offset-paginated bulk feed of every commit, newest capture only.
    SessionCommitsPage {
        since: Option<i64>,
        limit: u32,
        offset: u32,
        reply: oneshot::Sender<Result<Vec<CommitMapRow>>>,
    },
    /// R4 — session research: replace one session's research signals.
    SessionResearchReplace {
        artifact_id_session: String,
        research: Vec<SessionResearchRow>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// R4 — session research: the signals for one session, in order.
    SessionResearchForSession {
        session_id: String,
        reply: oneshot::Sender<Result<Vec<SessionResearchRow>>>,
    },
    /// R4 — session research: batched form keyed by session_id (newest
    /// capture only, #11). One `IN (...)` query for the ledger's fan-in.
    SessionResearchForSessions {
        session_ids: Vec<String>,
        reply: oneshot::Sender<Result<std::collections::HashMap<String, Vec<SessionResearchRow>>>>,
    },
    /// W4/R8/ADD-2 — every `grok_job` research row (across every session)
    /// whose query equals this job ulid — the `by-job` join.
    SessionResearchByJob {
        ulid: String,
        reply: oneshot::Sender<Result<Vec<SessionResearchRow>>>,
    },
    /// MI-W1.1 (revised) — the memory-recall ledger (V0035): replace the
    /// full `memory_recalls` set for one CAPTURE (`artifact_id`) in one
    /// transaction (delete-then-insert — see `Db::memory_recalls_replace`'s
    /// doc comment; keyed the same way as `SessionFilesReplace`'s
    /// `artifact_id_session`, not `session_id`). Written by the
    /// `memory-recall-ledger` enrichment hook after parsing the
    /// just-captured transcript.
    MemoryRecallsReplace {
        artifact_id: String,
        rows: Vec<MemoryRecallRow>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// MI-W1.1 — every recalled hit for one session id (test/debug read).
    MemoryRecallsForSession {
        session_id: String,
        reply: oneshot::Sender<Result<Vec<MemoryRecallRow>>>,
    },
    /// MI-W1.2/W1.3 — aggregate recall stats (count + last-recalled) for a
    /// batch of memory ids, scoped to THIS kb's `memory_recalls` table.
    /// `memory_kb` narrows to hits recalled from one memory corpus (the
    /// census route's `?kb=` scope); `None` counts a recall regardless of
    /// its recorded `memory_kb`. Callers wanting the true corpus-wide total
    /// fan out across every kb (invariant #28) and sum the partials.
    MemoryRecallsCountsForIds {
        memory_kb: Option<String>,
        memory_ids: Vec<String>,
        reply: oneshot::Sender<Result<Vec<MemoryRecallCount>>>,
    },
    /// MI-W4.2a — per-week injection histogram for a batch of memory ids
    /// (the `/memory` row sparkline's data source); same scoping rules as
    /// `MemoryRecallsCountsForIds`, its per-week sibling.
    MemoryRecallsWeeklyForIds {
        memory_kb: Option<String>,
        memory_ids: Vec<String>,
        now_unix: i64,
        reply: oneshot::Sender<Result<Vec<MemoryRecallWeeklyRow>>>,
    },
    /// CT-B2 — the memory-side reverse read: every session that recalled
    /// ONE memory (`memory_kb`/`memory_id`) within THIS kb's
    /// `memory_recalls` table, newest-first. Powers `GET
    /// /api/kb/{kb}/memories/{id}/recalled-by`, which fans this out across
    /// every kb on the daemon (invariant #28 — the ledger lives with the
    /// RECALLING session's kb, not the memory's own).
    MemoryRecallsForMemory {
        memory_kb: String,
        memory_id: String,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<MemoryRecalledByRow>>>,
    },
    /// CT-F1 — the memory↔commit exact-id join (V0038): replace THIS
    /// CAPTURE's `memory_commits` claims, parsed back out of the capture's
    /// own `Kb-Memory:` commit trailers by the `memory-commit-ledger`
    /// enrichment hook. Delete-by-capture + `INSERT OR REPLACE` on the
    /// `(memory_id, sha_full)` FACT — see `Db::memory_commits_replace`.
    MemoryCommitsReplace {
        artifact_id: String,
        rows: Vec<MemoryCommitRow>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// CT-F1 — every commit that cited ONE memory within THIS kb's
    /// `memory_commits` table, newest-first. Powers `GET
    /// /api/kb/{kb}/memories/{id}/commits`, fanned out across every kb
    /// (invariant #28 — the rows live with the RECORDING session's kb, the
    /// same placement `memory_recalls` has).
    MemoryCommitsForMemory {
        memory_id: String,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<MemoryCommitRow>>>,
    },
    /// Artifact snapshots (V0013, Track V) — latest stored content hash;
    /// the snapshot-capture hook's dedup gate.
    SnapshotLatestHash {
        artifact_id: String,
        reply: oneshot::Sender<Result<Option<String>>>,
    },
    /// Artifact snapshots — append one revision.
    SnapshotInsert {
        artifact_id: String,
        content_hash: String,
        raw_source: String,
        captured_at: i64,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Artifact snapshots — newest-first metadata (no body) for the timeline.
    SnapshotList {
        artifact_id: String,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<SnapshotMeta>>>,
    },
    /// Artifact snapshots — one revision's verbatim source, by row id.
    SnapshotRaw {
        id: i64,
        reply: oneshot::Sender<Result<Option<String>>>,
    },
    /// Artifact snapshots — prune to the newest `keep` per artifact.
    SnapshotPrune {
        artifact_id: String,
        keep: u32,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// Artifact snapshots — drop all for an artifact (delete pass).
    SnapshotsDeleteForArtifact {
        artifact_id: String,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// v0.14 S3 — cheap count of lance rows whose `kb_session`
    /// matches. Backs the `memory_count` field on /api/sessions rows.
    CountDocsWithKbSession {
        session_id: String,
        reply: oneshot::Sender<Result<u64>>,
    },
    /// Perf sweep 2026-07 — batched sibling of `CountDocsWithKbSession`:
    /// group-count every requested session id in ONE projection scan.
    /// Backs the /api/sessions list page's `memory_count` pass. Ids with
    /// zero matching docs are absent from the map.
    CountDocsByKbSession {
        session_ids: Vec<String>,
        reply: oneshot::Sender<Result<std::collections::HashMap<String, u64>>>,
    },
    /// v0.14 S3 — slim-projection list of lance rows whose
    /// `kb_session` matches. Powers /api/sessions/{sid}/memories.
    ListDocsWithKbSession {
        session_id: String,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<DocSummary>>>,
    },
    /// CT-A1 (U3 parse-back) — slim-projection list of lance rows whose
    /// `kb_source_kb`/`kb_source_artifact` name a given origin artifact.
    /// Powers `GET /api/kb/{kb}/docs/{id}/memories-from`.
    ListDocsWithKbSourceArtifact {
        source_kb: String,
        source_artifact: String,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<DocSummary>>>,
    },
    /// N-track — slim-projection list of `kb_category = 'note'` rows (incl.
    /// task counts). Powers the per-kb + cross-kb notes list. A read — it
    /// does NOT bump the index generation.
    ListNotes {
        limit: u32,
        reply: oneshot::Sender<Result<Vec<DocSummary>>>,
    },
    /// S5 admin — wipe the history table. Returns rows deleted.
    HistoryPurge {
        reply: oneshot::Sender<Result<usize>>,
    },
    /// R3 (v0.24) — opt-in age-based retention prune. Deletes history +
    /// reading_sections rows older than the given windows (each `None` =
    /// keep forever). Returns total rows deleted. See `Db::retention_prune`.
    RetentionPrune {
        now_unix: i64,
        history_max_age_secs: Option<i64>,
        reading_max_age_secs: Option<i64>,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// S5 admin — drop every transient row for a kb (lance rows +
    /// sqlite history/errors/edges). Preserves shares + `.review/*`
    /// files (see the per-fn docs for why). Returns (lance_rows,
    /// sqlite_rows_deleted) for telemetry.
    DropKbData {
        reply: oneshot::Sender<Result<(u64, usize)>>,
    },
    Shutdown,
    /// R1e — test-only: deliberately panics inside `handle` to prove the
    /// per-message supervision loop survives a poisoned message. `#[cfg(test)]`
    /// gates it out of every production build, so there is zero prod surface.
    #[cfg(test)]
    PanicForTest {
        reply: oneshot::Sender<Result<()>>,
    },
    /// SC4 — test-only: a write-class message whose handler sleeps `ms`,
    /// standing in for a slow ingest write. Lets the read-priority latency test
    /// build a synthetic 500-write backlog that drains in a bounded, cheap,
    /// deterministic time (real lance upserts go superlinear at that depth).
    /// Write-class by `is_read_lane`'s default fallthrough. `#[cfg(test)]`.
    #[cfg(test)]
    SlowWriteForTest {
        ms: u64,
        reply: oneshot::Sender<Result<()>>,
    },
}

/// SC4 — is this message read-class (routed to the priority read lane)?
///
/// Read-class = searches, gets, lists, counts, and the search-support
/// `Ensure*Index` trio (idempotent, generation-neutral, self-healing via the
/// per-index dirty flag, and on the foreground search path — so they MUST ride
/// the read lane or a real search still parks behind ingest). EVERYTHING else
/// — every mutation, every enrichment/history/reading write, every admin/meta
/// op, compaction, shutdown, and anything unlisted — is write-class.
///
/// The default is deliberately WRITE (the fallthrough `_ => false`): a message
/// misclassified as read could be pulled ahead of earlier-queued writes and
/// break write-FIFO, so the conservative direction is write. A NEW read-style
/// variant that someone forgets to list here merely loses the fast-lane
/// optimisation (still correct); a new write variant is correct by default.
/// The `read_lane_classification_is_conservative` test pins representatives.
///
/// This is INTENTIONALLY separate from [`StorageKind::from_msg`] (metrics):
/// that classifier's `_ => Read` fallthrough would route unlisted variants to
/// the read lane, the wrong (unsafe) default here.
fn is_read_lane(msg: &StorageMsg) -> bool {
    matches!(
        msg,
        // Searches — the latency-sensitive foreground reads.
        StorageMsg::Bm25Query { .. }
            | StorageMsg::VectorQuery { .. }
            | StorageMsg::HybridQuery { .. }
            | StorageMsg::ChunkVectorQuery { .. }
            // Search-support: idempotent, no generation bump, dirty-flag
            // self-healing, and awaited on the search path — read-class so a
            // real `ensure_*_index → query` search jumps the ingest backlog.
            | StorageMsg::EnsureFtsIndex { .. }
            | StorageMsg::EnsureVectorIndex { .. }
            | StorageMsg::EnsureChunkVectorIndex { .. }
            // Row/edge/count lookups.
            | StorageMsg::CountRows { .. }
            | StorageMsg::DecodeSkipCount { .. }
            | StorageMsg::DatasetStats { .. }
            | StorageMsg::ListDocs { .. }
            | StorageMsg::ListNotes { .. }
            | StorageMsg::ListSupersedeTargets { .. }
            | StorageMsg::ListContentHashes { .. }
            | StorageMsg::ListFirstSeenSeedRows { .. }
            | StorageMsg::ListReconcileRows { .. }
            | StorageMsg::GetById { .. }
            | StorageMsg::LineageById { .. }
            | StorageMsg::FindSupersededBy { .. }
            | StorageMsg::GetByIds { .. }
            | StorageMsg::GetBodiesByIds { .. }
            | StorageMsg::EmbeddingById { .. }
            | StorageMsg::EmbeddingsByIds { .. }
            | StorageMsg::PromptById { .. }
            | StorageMsg::GetBySourcePath { .. }
            | StorageMsg::GetBySourcePaths { .. }
            | StorageMsg::ListEmbeddings { .. }
            | StorageMsg::ListDocsWithAtlas { .. }
            | StorageMsg::GetAtlasLabels { .. }
            | StorageMsg::MovesLookup { .. }
            | StorageMsg::MovesSuppressesDelete { .. }
            | StorageMsg::MovesListIncomplete { .. }
            | StorageMsg::ListChunksForDoc { .. }
            // W3 T-a atlas time-lapse: the two FRAME READS are read-class.
            // `AtlasSnapshotInsert`/`AtlasSnapshotPrune` are deliberately
            // absent — they mutate sqlite and must stay write-lane (FIFO
            // with the `UpdateAtlas` write they follow).
            | StorageMsg::AtlasSnapshotsList { .. }
            | StorageMsg::AtlasSnapshotPoints { .. }
            | StorageMsg::EdgesFrom { .. }
            // DCB W1.A code-ref READS. `RecordCodeRefs` is deliberately
            // absent — write lane by the conservative default.
            | StorageMsg::CodeRefsOf { .. }
            | StorageMsg::CodeRefsFeed { .. }
            | StorageMsg::CodeRefsByTarget { .. }
            // CT-F5 SLO indicator READS — pure aggregates over existing
            // tables. `SloSnapshotAppend` + `SessionsSetRecallCensus` are
            // deliberately absent (write lane, conservative default).
            | StorageMsg::CodeRefShapeCounts { .. }
            | StorageMsg::KbSessionDocCounts { .. }
            | StorageMsg::SessionIdsPresent { .. }
            | StorageMsg::SessionsNewestStartedAt { .. }
            | StorageMsg::SessionsRecallCensusTotals { .. }
            | StorageMsg::SloSnapshotsList { .. }
            | StorageMsg::BacklinksOf { .. }
            | StorageMsg::EdgeCounts { .. }
            | StorageMsg::LinkPairs { .. }
            | StorageMsg::ListSources { .. }
            | StorageMsg::ListExclusions { .. }
            | StorageMsg::LastRunForSource { .. }
            | StorageMsg::ListOpenErrors { .. }
            | StorageMsg::RetryCountForPathHash { .. }
            // History / reading READS (the writes in these families are
            // write-class — see the mixed-family note in the test).
            | StorageMsg::HistoryList { .. }
            | StorageMsg::HistoryOpensInWindow { .. }
            | StorageMsg::HistoryCommentsInWindow { .. }
            | StorageMsg::HistoryCountsByDay { .. }
            | StorageMsg::ReadingStateForVisit { .. }
            | StorageMsg::ReadingInputsForArtifact { .. }
            | StorageMsg::ReadingInputsForArtifacts { .. }
            | StorageMsg::HistoryDistinctUsers { .. }
            | StorageMsg::ReadingLatestForArtifact { .. }
            | StorageMsg::ReadingLatestForIds { .. }
            | StorageMsg::ReadingRollup { .. }
            | StorageMsg::ReadingRollupForIds { .. }
            | StorageMsg::FirstSeenForIds { .. }
            | StorageMsg::FirstSeenIsEmpty { .. }
            | StorageMsg::ListEntryUserOverridesForList { .. }
            // Shares / corkboard / pins / memory-link READS.
            | StorageMsg::SharesList { .. }
            | StorageMsg::SharesGet { .. }
            | StorageMsg::SharesGetByTarget { .. }
            | StorageMsg::CorkboardList { .. }
            | StorageMsg::CorkboardCount { .. }
            | StorageMsg::PinnedMemoriesSet { .. }
            | StorageMsg::MemoryLinksFor { .. }
            | StorageMsg::MemoryLinksAll { .. }
            | StorageMsg::MemoryLinksSeededHas { .. }
            // Reading-list READS.
            | StorageMsg::ListGet { .. }
            | StorageMsg::ListsAll { .. }
            | StorageMsg::ListEntriesForList { .. }
            | StorageMsg::ListEntriesAll { .. }
            | StorageMsg::ListEntriesForArtifact { .. }
            // Sessions READS.
            | StorageMsg::SessionsList { .. }
            | StorageMsg::SessionsFolders { .. }
            | StorageMsg::SessionsProjectsStats { .. }
            | StorageMsg::SessionsProjectsHarnessMix { .. }
            | StorageMsg::SessionsResearchRollup { .. }
            | StorageMsg::SessionsFunnelCounts { .. }
            | StorageMsg::SessionFilesInFolder { .. }
            | StorageMsg::SessionsGet { .. }
            | StorageMsg::SessionsGetMany { .. }
            | StorageMsg::SessionsGetByArtifactIds { .. }
            | StorageMsg::SessionFilesForSession { .. }
            | StorageMsg::SessionFilesForArtifact { .. }
            | StorageMsg::SessionFilesForBasename { .. }
            | StorageMsg::SessionDecisionsForSession { .. }
            | StorageMsg::SessionDecisionsForSessions { .. }
            | StorageMsg::SessionCommitsForSession { .. }
            | StorageMsg::SessionCommitsForSessions { .. }
            | StorageMsg::SessionCommitsBySha { .. }
            | StorageMsg::SessionCommitsPage { .. }
            | StorageMsg::SessionResearchForSession { .. }
            | StorageMsg::SessionResearchForSessions { .. }
            | StorageMsg::SessionResearchByJob { .. }
            // MI-W1.2/W1.3 — the memory-recall ledger's aggregate read.
            // `MemoryRecallsReplace` is deliberately absent — it's a write
            // (default-write is the conservative call per SC4's own doc
            // comment).
            | StorageMsg::MemoryRecallsCountsForIds { .. }
            | StorageMsg::MemoryRecallsWeeklyForIds { .. }
            | StorageMsg::MemoryRecallsForSession { .. }
            // CT-B2 — the memory-side reverse read.
            | StorageMsg::MemoryRecallsForMemory { .. }
            // CT-F1 — the memory↔commit exact-id read. Its write sibling
            // (`MemoryCommitsReplace`) stays on the default WRITE lane.
            | StorageMsg::MemoryCommitsForMemory { .. }
            // Snapshot / kb_session READS.
            | StorageMsg::SnapshotLatestHash { .. }
            | StorageMsg::SnapshotList { .. }
            | StorageMsg::SnapshotRaw { .. }
            | StorageMsg::CountDocsWithKbSession { .. }
            | StorageMsg::CountDocsByKbSession { .. }
            | StorageMsg::ListDocsWithKbSession { .. }
            // CT-A1 (U3 parse-back) — reverse-provenance READ.
            | StorageMsg::ListDocsWithKbSourceArtifact { .. }
    )
}

/// SC4 — the outcome of a biased two-lane pull in [`StorageActor::next_from_lanes`].
enum Picked {
    Read(Option<Stamped>),
    Write(Option<Stamped>),
}

/// Channel envelope: a message plus the instant it was enqueued, so the actor
/// can measure per-message queue-wait time for [`PipelineMetrics`]. The stamp
/// is taken unconditionally at send (≈20ns); the observe is gated on the
/// metrics being enabled, in `run()`.
struct Stamped {
    enqueued: Instant,
    msg: StorageMsg,
}

/// Cheap-clone handle. Senders are Arc-cloneable; the path is for read-side
/// consumers that want to open their own lance connection (v0.1+).
#[derive(Clone)]
pub struct StorageHandle {
    /// SC4 — read-class lane (searches/gets/lists/counts). Drained with
    /// priority by the actor loop so a foreground read never parks behind a
    /// bulk-ingest write backlog. Routing is by [`is_read_lane`].
    read_tx: mpsc::Sender<Stamped>,
    /// SC4 — write-class lane (mutations, enrichment/history/reading writes,
    /// admin/meta, compaction, shutdown, and anything unsure). This is the
    /// back-pressure surface the indexer's ingest relies on (invariant #17).
    write_tx: mpsc::Sender<Stamped>,
    /// P1 — index-generation counter, shared with the owning actor. The
    /// actor bumps it on every mutation that changes what `list_docs` /
    /// `edge_counts` return (upsert, delete, edge-record, drop); read-side
    /// callers use it as a cheap cache key for the gallery row-set memo.
    /// A plain atomic load — no actor round-trip — so the hot read path is
    /// free even while the actor is saturated.
    generation: Arc<AtomicU64>,
}

impl StorageHandle {
    /// N8: approximate queue depth — how many messages are pending in
    /// the WRITE lane. The actor processes one msg at a time so this is
    /// the back-pressure signal: a non-zero value means writers are
    /// queuing faster than the actor drains. Capacity is
    /// `CHANNEL_CAPACITY` so depth / capacity is the load fraction.
    /// SC4 — the read lane is deliberately excluded: reads are drained with
    /// priority and clear near-instantly, so the ingest back-pressure gauge
    /// this feeds must reflect the write lane (where a backlog actually
    /// builds), matching N8's "writers are queuing" semantics.
    pub fn queue_depth(&self) -> usize {
        CHANNEL_CAPACITY.saturating_sub(self.write_tx.capacity())
    }

    /// N8: maximum queue depth (channel capacity). Pair with
    /// `queue_depth()` for the gauge ratio.
    pub fn queue_capacity() -> usize {
        CHANNEL_CAPACITY
    }

    /// P1 — the current index generation. Bumps whenever the lance
    /// row-set or the edges table changes; a stable value across two
    /// reads means a cached gallery snapshot taken at the first read is
    /// still valid. Acquire-ordered against the actor's release bumps.
    pub fn index_generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// The actor itself. Owns one `Storage` (lance) + one `Db` (sqlite).
/// The `Storage` sits behind an `Arc` solely so `compact_off_loop` can
/// lend it to the spawned optimize task; the actor loop remains the only
/// place lance mutations execute.
pub struct StorageActor {
    storage: Arc<Storage>,
    db: Db,
    /// SC4 — priority read lane (see [`StorageHandle::read_tx`]).
    read_rx: mpsc::Receiver<Stamped>,
    /// SC4 — write lane (see [`StorageHandle::write_tx`]).
    write_rx: mpsc::Receiver<Stamped>,
    /// P1 — shared with every [`StorageHandle`] clone (see its doc).
    /// Bumped in `handle` on row-set / edge mutations via
    /// [`StorageActor::bump_generation`].
    generation: Arc<AtomicU64>,
    /// TM-track — daemon-wide pipeline metrics. The actor records
    /// per-`StorageKind` queue-wait + handler time in `run()` when enabled.
    metrics: Arc<PipelineMetrics>,
}

impl StorageActor {
    /// Open both backends and spawn the actor task. Returns the handle.
    ///
    /// `config_dim` is the dim the kb's configured `embedding_model`
    /// resolves to (via `kb_core::embed::model_info(name).dim as i32`).
    /// Forwarded to `Storage::open` for the dim-mismatch guard — see
    /// that fn's doc for the policy.
    pub async fn spawn(
        lance_path: PathBuf,
        sqlite_path: PathBuf,
        config_dim: Option<i32>,
    ) -> Result<StorageHandle> {
        Self::spawn_with_metrics(
            lance_path,
            sqlite_path,
            config_dim,
            Arc::new(PipelineMetrics::disabled()),
            // Back-compat shim (tests + tools): pre-knob lance behavior, same
            // as `Storage::open`. The daemon passes its resolved `[storage]`
            // options through `spawn_with_metrics` directly.
            crate::storage::lance::LanceOptions::unbounded(),
        )
        .await
    }

    /// Like [`spawn`](Self::spawn) but wires a shared [`PipelineMetrics`] so
    /// the actor records per-`StorageKind` queue-wait + handler time
    /// (TM-track), and takes the daemon's resolved lance tuning (`[storage]`
    /// in kb.toml → `Storage::open_with_options`). `spawn` is the back-compat
    /// shim (disabled metrics + unbounded lance options) used by tests +
    /// tools that don't surface metrics; `bring_up_kb` calls this with the
    /// daemon's real metrics Arc + resolved options.
    pub async fn spawn_with_metrics(
        lance_path: PathBuf,
        sqlite_path: PathBuf,
        config_dim: Option<i32>,
        metrics: Arc<PipelineMetrics>,
        lance_options: crate::storage::lance::LanceOptions,
    ) -> Result<StorageHandle> {
        let storage = Storage::open_with_options(&lance_path, config_dim, lance_options).await?;
        let db = Db::open(&sqlite_path)?;
        // SC4 — two lanes, each `CHANNEL_CAPACITY`. The write lane keeps the
        // ingest back-pressure contract (invariant #17); the read lane is
        // sized the same so a foreground read burst can enqueue without
        // blocking a caller mid-request.
        let (read_tx, read_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (write_tx, write_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let generation = Arc::new(AtomicU64::new(0));
        let actor = Self {
            storage: Arc::new(storage),
            db,
            read_rx,
            write_rx,
            generation: Arc::clone(&generation),
            metrics,
        };
        tokio::spawn(actor.run());
        Ok(StorageHandle {
            read_tx,
            write_tx,
            generation,
        })
    }

    async fn run(mut self) {
        // Lance mutations parked while an off-loop compaction was in flight;
        // replayed FIFO ahead of any fresh channel pull (`compact_off_loop`).
        let mut pending: VecDeque<Stamped> = VecDeque::new();
        // SC4 — read-lane messages served since the last write; drives the
        // write-starvation bound in `next_from_lanes`.
        let mut reads_since_write: u32 = 0;
        // SC4 — per-lane liveness. Every `StorageHandle` holds BOTH senders so
        // the lanes close together, but buffered messages can drain from one
        // after the other has already returned `None`; track each so a
        // closed-but-drained lane is never re-selected (recv on a closed mpsc
        // resolves instantly → a hot spin).
        let mut read_open = true;
        let mut write_open = true;
        loop {
            let stamped = match pending.pop_front() {
                // Compaction-deferred writes replay FIFO ahead of any fresh
                // pull — this preserves write ordering + read-your-writes for
                // the deferred batch; reset the read counter since a replayed
                // message is always a write.
                Some(s) => {
                    reads_since_write = 0;
                    s
                }
                None => {
                    match self
                        .next_from_lanes(&mut reads_since_write, &mut read_open, &mut write_open)
                        .await
                    {
                        Some(s) => s,
                        None => break, // both lanes closed + drained
                    }
                }
            };
            if matches!(stamped.msg, StorageMsg::Shutdown) {
                break;
            }
            if matches!(
                stamped.msg,
                StorageMsg::CompactAll { .. } | StorageMsg::CompactAllWithRetention { .. }
            ) {
                let kind = StorageKind::from_msg(&stamped.msg);
                let Stamped { enqueued, msg } = stamped;
                self.compact_off_loop(enqueued, kind, msg, &mut pending)
                    .await;
                continue;
            }
            self.process(stamped).await;
        }
    }

    /// SC4 — pull the next message, favouring the read lane so a foreground
    /// search never head-of-line-blocks behind a bulk-ingest write backlog on
    /// this actor. Returns `None` only when BOTH lanes are closed and drained.
    ///
    /// Fairness: while `reads_since_write < READ_STARVATION_BOUND` the biased
    /// select polls the read lane first (reads win every tie); once the bound
    /// is hit it flips to poll the write lane first, so a waiting write is
    /// served after at most `READ_STARVATION_BOUND` reads and the counter
    /// resets. If no write is waiting the read lane simply keeps draining (the
    /// counter runs past the bound harmlessly) and the very next write to
    /// arrive is served immediately — bounded starvation, never a stall.
    ///
    /// Both lanes feed the SAME serial `handle()`, so single-writer-per-kb
    /// (invariant #2) is untouched; only the *dispatch order* changes.
    async fn next_from_lanes(
        &mut self,
        reads_since_write: &mut u32,
        read_open: &mut bool,
        write_open: &mut bool,
    ) -> Option<Stamped> {
        loop {
            let picked = match (*read_open, *write_open) {
                (false, false) => return None,
                // One lane closed — recv the survivor directly (selecting a
                // closed lane would busy-spin on instant `None`s).
                (true, false) => Picked::Read(self.read_rx.recv().await),
                (false, true) => Picked::Write(self.write_rx.recv().await),
                (true, true) => {
                    if *reads_since_write >= READ_STARVATION_BOUND {
                        tokio::select! {
                            biased;
                            w = self.write_rx.recv() => Picked::Write(w),
                            r = self.read_rx.recv() => Picked::Read(r),
                        }
                    } else {
                        tokio::select! {
                            biased;
                            r = self.read_rx.recv() => Picked::Read(r),
                            w = self.write_rx.recv() => Picked::Write(w),
                        }
                    }
                }
            };
            match picked {
                Picked::Read(Some(s)) => {
                    *reads_since_write += 1;
                    return Some(s);
                }
                Picked::Write(Some(s)) => {
                    *reads_since_write = 0;
                    return Some(s);
                }
                // A lane just closed AND drained: mark it and loop; the next
                // pass serves the survivor or returns `None` if both are gone.
                Picked::Read(None) => *read_open = false,
                Picked::Write(None) => *write_open = false,
            }
        }
    }

    /// Metrics-wrapped, panic-supervised dispatch of one message. Timing
    /// wraps AROUND handle() only — every bump_generation call stays inside
    /// handle, untouched (root CLAUDE.md invariant #15). When metrics are
    /// disabled the only cost is one relaxed load + the (already-paid) enqueue
    /// stamp. A message that was parked behind a compaction keeps its original
    /// enqueue stamp, so queue-wait honestly includes the compaction window.
    ///
    /// Every `handle()` call routes through here — both the steady-state
    /// `run()` loop and the messages serviced inline during a compaction
    /// window (`compact_off_loop`) — so the panic supervision below is the
    /// single chokepoint that keeps the actor alive across a panicking arm.
    async fn process(&mut self, stamped: Stamped) {
        let Stamped { enqueued, msg } = stamped;
        // Capture the class + start instant BEFORE dispatch (from_msg reads
        // only the discriminant) so a message that PANICS is still timed and
        // lands in its per-StorageKind slot.
        let timing = self.metrics.is_enabled().then(|| {
            (
                StorageKind::from_msg(&msg),
                enqueued.elapsed().as_millis() as u64,
                Instant::now(),
            )
        });
        // R1a — per-message panic supervision. `AssertUnwindSafe` is sound
        // here: this actor is the SINGLE writer of its lance `Storage` +
        // sqlite `Db` (kb-core invariant #2), so no other task can ever
        // observe a half-updated `self` across an unwind. rusqlite rolls back
        // any open transaction when its statement/txn guard drops during
        // unwinding, and each lance call is internally transactional per call,
        // so a panic mid-arm leaves both stores at a consistent committed
        // boundary. The in-flight `oneshot::Sender` for this message drops
        // unsent as the arm's future unwinds → its caller gets the existing
        // `Error::Storage("storage actor reply lost")` (see `send_and_await`),
        // exactly as if the reply had been lost. So we catch, log, and keep the
        // loop alive instead of letting the panic kill the task — which would
        // 503 the whole kb until a daemon restart.
        let outcome = AssertUnwindSafe(self.handle(msg)).catch_unwind().await;
        if let Some((kind, queue_wait_ms, started)) = timing {
            let handler_ms = started.elapsed().as_millis() as u64;
            self.metrics
                .observe_storage(kind, queue_wait_ms, handler_ms);
        }
        if let Err(payload) = outcome {
            let detail = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("<non-string panic payload>");
            tracing::error!(
                panic = %detail,
                "storage actor handler panicked; the in-flight caller got a \
                 reply-lost error and the actor survived to serve the next message",
            );
        }
    }

    /// True for messages that MUTATE the lance table(s) — parked while an
    /// off-loop compaction is running so the optimize never races a writer
    /// (kb-core CLAUDE.md invariant #2: single-writer-per-kb). Any NEW
    /// lance-mutating variant MUST be added here. Everything else is
    /// serviced inline during a compaction window: sqlite-only work never
    /// touches lance, and lance reads see a consistent MVCC snapshot while
    /// an optimize proceeds. The Ensure*Index trio stays inline
    /// deliberately — search awaits it per-request, so parking it would
    /// re-freeze search behind the compaction; its common case is a
    /// clean-dirty-flag no-op, and the rare in-window `create_index`
    /// worst-cases as a commit-conflict `Err` that every caller already
    /// tolerates (`let _ =`), leaving the flag dirty so the next search
    /// rebuilds after the compaction.
    fn defers_during_compact(msg: &StorageMsg) -> bool {
        matches!(
            msg,
            StorageMsg::UpsertDoc { .. }
                | StorageMsg::UpsertDocs { .. }
                | StorageMsg::DeleteDoc { .. }
                | StorageMsg::DeleteByPath { .. }
                | StorageMsg::RelocateDoc { .. }
                | StorageMsg::UpsertChunks { .. }
                | StorageMsg::UpdateAtlas { .. }
                | StorageMsg::TouchMtime { .. }
                | StorageMsg::ClearEmbeddings { .. }
                | StorageMsg::DropKbData { .. }
                | StorageMsg::CompactAll { .. }
                | StorageMsg::CompactAllWithRetention { .. }
        )
    }

    /// Run `compact_all` on a spawned task instead of inline, so a
    /// multi-second `Table::optimize(OptimizeAction::All)` never
    /// head-of-line-blocks the actor queue (measured: seconds of frozen
    /// search/gallery/reader traffic per compaction window). While the
    /// optimize is in flight the loop keeps servicing every message that
    /// doesn't mutate lance; genuine lance mutations are parked in `pending`
    /// (bounded by [`COMPACT_DEFER_CAP`]) and replayed FIFO by `run()` once
    /// the optimize finishes — at no point do two lance mutations run
    /// concurrently, so single-writer-per-kb (kb-core invariant #2) holds.
    /// Write callers still see reply-gated causality: a deferred write's
    /// oneshot fires only after it actually lands, exactly as when it queued
    /// behind an inline compaction.
    async fn compact_off_loop(
        &mut self,
        enqueued: Instant,
        kind: StorageKind,
        msg: StorageMsg,
        pending: &mut VecDeque<Stamped>,
    ) {
        let storage = Arc::clone(&self.storage);
        let metrics = Arc::clone(&self.metrics);
        let (done_tx, mut done_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let queue_wait_ms = enqueued.elapsed().as_millis() as u64;
            let started = Instant::now();
            match msg {
                StorageMsg::CompactAll { reply } => {
                    let res = storage.compact_all().await;
                    let _ = reply.send(res);
                }
                StorageMsg::CompactAllWithRetention {
                    retention_minutes,
                    delete_unverified,
                    reply,
                } => {
                    let res = storage
                        .compact_all_with_retention(retention_minutes, delete_unverified)
                        .await;
                    let _ = reply.send(res);
                }
                _ => unreachable!("guarded by the matches! in run() before dispatch"),
            }
            if metrics.is_enabled() {
                let handler_ms = started.elapsed().as_millis() as u64;
                metrics.observe_storage(kind, queue_wait_ms, handler_ms);
            }
            let _ = done_tx.send(());
        });
        // SC4 — keep servicing the read lane inline (reads never mutate lance,
        // so they see a consistent MVCC snapshot while the optimize runs) and
        // gate the write lane the way the old single channel did: lance
        // mutations (and a Shutdown) are parked, sqlite-only writes run inline.
        // `biased` keeps done-first (fast exit) then reads (priority) then
        // writes, mirroring the steady-state read-priority ordering. Each
        // `pull_*` goes false on that lane's close; a parked Shutdown stops
        // both (so writes enqueued before it still land on replay, matching
        // the pre-existing FIFO shutdown semantics).
        let mut pull_read = true;
        let mut pull_write = true;
        loop {
            tokio::select! {
                biased;
                _ = &mut done_rx => break,
                r = self.read_rx.recv(), if pull_read => {
                    match r {
                        None => pull_read = false,
                        Some(stamped) => self.process(stamped).await,
                    }
                }
                w = self.write_rx.recv(), if pull_write && pending.len() < COMPACT_DEFER_CAP => {
                    match w {
                        None => pull_write = false,
                        Some(stamped) => {
                            let is_shutdown = matches!(stamped.msg, StorageMsg::Shutdown);
                            if is_shutdown || Self::defers_during_compact(&stamped.msg) {
                                pending.push_back(stamped);
                                if is_shutdown {
                                    pull_read = false;
                                    pull_write = false;
                                }
                            } else {
                                self.process(stamped).await;
                            }
                        }
                    }
                }
            }
        }
    }

    /// P1 — advance the index generation. Called from `handle` on every
    /// mutation that changes what `list_docs` / `edge_counts` return, so
    /// the gallery row-set memo invalidates exactly when — and only when —
    /// the corpus rows or the edges table actually change (atlas-coord,
    /// embedding-clear, and compaction writes deliberately do NOT bump).
    fn bump_generation(&self) {
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// F3a — lance copy (preserve embedding) + chunk rekey + delete old id
    /// (which cascades leftover old chunks) + sqlite rekey tx + one bump.
    /// Idempotent for startup replay: if `new_id` already exists with the
    /// **same** `content_hash` as `old_id`, the lance copy is skipped; a
    /// stale orphan under `new_id` with a different hash is overwritten so
    /// the moved doc's content/embedding is not lost. If `old_id` is already
    /// gone the delete is a no-op.
    ///
    /// Returns the DISTINCT list_ids rekeyed by the sqlite cascade.
    #[allow(clippy::too_many_arguments)]
    async fn handle_relocate_doc(
        &mut self,
        old_id: &str,
        new_id: &str,
        new_path: &str,
        old_rel: &str,
        new_rel: &str,
        moves_row_id: i64,
        completed_at: i64,
    ) -> Result<Vec<String>> {
        // 1. Lance: ensure the new-id row carries the old row's content +
        // embedding (skip only when new already matches by content_hash).
        let old_full = self.storage.get_full_doc(old_id).await?;
        let new_full = self.storage.get_full_doc(new_id).await?;
        let hashes_equal = match (&old_full, &new_full) {
            (Some(o), Some(n)) => o.content_hash.is_some() && o.content_hash == n.content_hash,
            _ => false,
        };
        let need_copy = old_full.is_some() && (new_full.is_none() || !hashes_equal);
        if need_copy {
            if let Some(mut doc) = old_full {
                // Re-key chunks BEFORE deleting the old doc (delete_by_id
                // cascades chunks for the deleted id only). Always replace
                // chunks under new_id (even when empty) so a stale orphan's
                // chunks cannot linger after an overwrite.
                let old_chunks = self.storage.list_chunks_for_doc(old_id).await?;
                doc.id = new_id.to_string();
                doc.path = new_path.to_string();
                self.storage.upsert_docs(&[doc]).await?;
                let rekeyed: Vec<ChunkDoc> = old_chunks
                    .into_iter()
                    .map(|c| ChunkDoc {
                        chunk_id: format!("{new_id}#{}", c.chunk_idx),
                        doc_id: new_id.to_string(),
                        chunk_idx: c.chunk_idx,
                        text: c.text,
                        embedding: c.embedding,
                    })
                    .collect();
                self.storage.upsert_chunks(new_id, &rekeyed).await?;
            }
        } else if new_full.is_some() {
            // new_id present with matching hash (replay). Still try to rekey
            // any leftover old chunks that weren't migrated.
            let old_chunks = self.storage.list_chunks_for_doc(old_id).await?;
            if !old_chunks.is_empty() {
                let new_chunks = self.storage.list_chunks_for_doc(new_id).await?;
                if new_chunks.is_empty() {
                    let rekeyed: Vec<ChunkDoc> = old_chunks
                        .into_iter()
                        .map(|c| ChunkDoc {
                            chunk_id: format!("{new_id}#{}", c.chunk_idx),
                            doc_id: new_id.to_string(),
                            chunk_idx: c.chunk_idx,
                            text: c.text,
                            embedding: c.embedding,
                        })
                        .collect();
                    self.storage.upsert_chunks(new_id, &rekeyed).await?;
                }
            }
        }
        // else: old already gone and new missing — sqlite rekey + complete
        // still run (partial crash after FS rename, before lance copy).

        // 2. Drop the old lance row (cascades any remaining old-id chunks).
        // delete_by_id is safe when the row is already gone.
        if self.storage.get_by_id(old_id).await?.is_some() {
            self.storage.delete_by_id(old_id).await?;
        }

        // 3. Sqlite rekey + moves.completed_at in ONE transaction.
        let list_ids = self.db.cascade_relocate_doc(
            old_id,
            new_id,
            old_rel,
            new_rel,
            moves_row_id,
            completed_at,
        )?;

        // 4. One generation bump for the row-set mutation (invariant #15).
        self.bump_generation();
        Ok(list_ids)
    }

    async fn handle(&mut self, msg: StorageMsg) {
        match msg {
            StorageMsg::UpsertSource {
                slug,
                path,
                added_at_unix,
                reply,
            } => {
                let _ = reply.send(self.db.upsert_source(&slug, &path, added_at_unix));
            }
            StorageMsg::SetSourcePaused {
                slug,
                paused,
                reply,
            } => {
                let _ = reply.send(self.db.set_source_paused(&slug, paused));
            }
            StorageMsg::ListSources { reply } => {
                let _ = reply.send(self.db.list_sources());
            }
            StorageMsg::AddExclusion {
                path,
                excluded_at_unix,
                note,
                reply,
            } => {
                let _ = reply.send(
                    self.db
                        .add_exclusion(&path, excluded_at_unix, note.as_deref()),
                );
            }
            StorageMsg::RemoveExclusion { path, reply } => {
                let _ = reply.send(self.db.remove_exclusion(&path));
            }
            StorageMsg::ListExclusions { reply } => {
                let _ = reply.send(self.db.list_exclusions());
            }
            StorageMsg::BeginRun {
                source_slug,
                started_at_unix,
                reply,
            } => {
                let _ = reply.send(self.db.start_run(&source_slug, started_at_unix));
            }
            StorageMsg::FinishRun {
                run_id,
                ok_count,
                err_count,
                finished_at_unix,
                reply,
            } => {
                let _ =
                    reply.send(
                        self.db
                            .finish_run(&run_id, ok_count, err_count, finished_at_unix),
                    );
            }
            StorageMsg::LastRunForSource { slug, reply } => {
                let _ = reply.send(self.db.last_run_for_source(&slug));
            }
            StorageMsg::RecordError {
                kind,
                source_slug,
                path,
                message,
                content_hash,
                created_at_unix,
                reply,
            } => {
                let _ = reply.send(self.db.record_error(
                    &kind,
                    &source_slug,
                    &path,
                    &message,
                    content_hash.as_deref(),
                    created_at_unix,
                ));
            }
            StorageMsg::ClearErrorsForPathHash {
                path,
                new_content_hash,
                reply,
            } => {
                let _ = reply.send(self.db.clear_errors_for_path_hash(&path, &new_content_hash));
            }
            StorageMsg::ClearErrorsForPath { path, reply } => {
                let _ = reply.send(self.db.clear_errors_for_path(&path));
            }
            StorageMsg::DismissError { id, reply } => {
                let _ = reply.send(self.db.dismiss_error(&id));
            }
            StorageMsg::ListOpenErrors { reply } => {
                let _ = reply.send(self.db.list_open_errors());
            }
            StorageMsg::RetryCountForPathHash {
                path,
                content_hash,
                reply,
            } => {
                let _ = reply.send(self.db.retry_count_for_path_hash(&path, &content_hash));
            }
            StorageMsg::UpsertDoc { doc, reply } => {
                let res = self.storage.upsert_docs(&[*doc]).await;
                if res.is_ok() {
                    self.bump_generation();
                }
                let _ = reply.send(res);
            }
            StorageMsg::UpsertDocs { docs, reply } => {
                let res = self.storage.upsert_docs(&docs).await;
                if res.is_ok() {
                    self.bump_generation();
                }
                let _ = reply.send(res);
            }
            StorageMsg::DeleteDoc { id, reply } => {
                let res = self.storage.delete_by_id(id.as_str()).await;
                if res.is_ok() {
                    self.bump_generation();
                }
                let _ = reply.send(res);
            }
            StorageMsg::DeleteByPath { path, reply } => {
                let res = self.storage.delete_by_path(&path.to_string_lossy()).await;
                if res.is_ok() {
                    self.bump_generation();
                }
                let _ = reply.send(res);
            }
            StorageMsg::CascadeDeleteDoc {
                id,
                path,
                mode,
                reply,
            } => {
                // Lance FIRST (ordering precedent: `DropKbData`). A lance
                // failure is HARD — the caller records it and skips the SSE,
                // exactly as the pre-R2 `process_delete` did. Once lance
                // succeeds the row is gone, so bump the generation BEFORE the
                // sqlite tx (like `DropKbData`) — a spurious bump on a sqlite
                // failure only discards a valid cache; a missed one would serve
                // the just-deleted row. The sqlite tx is SOFT: on error we log
                // and return an empty outcome (the tx rolled back, so the
                // dependents are intact and the reconcile sweep reclaims them)
                // rather than suppressing `artifact.removed`.
                let res = match self.storage.delete_by_path(&path.to_string_lossy()).await {
                    Ok(()) => {
                        self.bump_generation();
                        match self.db.cascade_delete_doc(id.as_str(), mode) {
                            Ok(out) => Ok(out),
                            Err(e) => {
                                tracing::warn!(
                                    artifact_id = %id.as_str(),
                                    error = %e,
                                    "cascade sqlite tx failed; dependents intact, \
                                     reconcile sweep will reclaim",
                                );
                                Ok(CascadeDbOutcome::default())
                            }
                        }
                    }
                    Err(e) => Err(e),
                };
                let _ = reply.send(res);
            }
            StorageMsg::MovesInsertIntent {
                old_id,
                new_id,
                old_rel,
                new_rel,
                moved_at,
                reply,
            } => {
                let res = self
                    .db
                    .moves_insert_intent(&old_id, &new_id, &old_rel, &new_rel, moved_at);
                let _ = reply.send(res);
            }
            StorageMsg::RelocateDoc {
                old_id,
                new_id,
                new_path,
                old_rel,
                new_rel,
                moves_row_id,
                completed_at,
                reply,
            } => {
                let res = self
                    .handle_relocate_doc(
                        &old_id,
                        &new_id,
                        &new_path,
                        &old_rel,
                        &new_rel,
                        moves_row_id,
                        completed_at,
                    )
                    .await;
                let _ = reply.send(res);
            }
            StorageMsg::MovesLookup { key, reply } => {
                let _ = reply.send(self.db.moves_lookup(&key));
            }
            StorageMsg::MovesSuppressesDelete {
                old_rel,
                now_unix,
                grace_secs,
                reply,
            } => {
                let _ = reply.send(
                    self.db
                        .moves_suppresses_delete(&old_rel, now_unix, grace_secs),
                );
            }
            StorageMsg::MovesListIncomplete { reply } => {
                let _ = reply.send(self.db.moves_list_incomplete());
            }
            StorageMsg::MovesMarkCompleted {
                row_id,
                completed_at,
                reply,
            } => {
                let _ = reply.send(self.db.moves_mark_completed(row_id, completed_at));
            }
            StorageMsg::SweepOrphans { keep_ids, reply } => {
                let res = self.db.sweep_orphans(&keep_ids);
                // Removing orphaned edges / corkboard rows changes what
                // `edge_counts` + the gallery "anchored" projection return, so
                // bump the generation (invariant #15). list_entries removal
                // deliberately does NOT bump (invariant #25).
                if let Ok(out) = &res {
                    if out.edges_removed > 0 || out.corkboard_removed > 0 {
                        self.bump_generation();
                    }
                }
                let _ = reply.send(res);
            }
            StorageMsg::EnsureFtsIndex { reply } => {
                let _ = reply.send(self.storage.ensure_fts_index().await);
            }
            StorageMsg::CountRows { reply } => {
                let _ = reply.send(self.storage.count_rows().await);
            }
            StorageMsg::DecodeSkipCount { reply } => {
                let _ = reply.send(Ok(self.storage.decode_skip_count()));
            }
            StorageMsg::Bm25Query {
                q,
                limit,
                typo_tolerance,
                reply,
            } => {
                let _ = reply.send(self.storage.bm25_query(&q, limit, typo_tolerance).await);
            }
            StorageMsg::VectorQuery {
                query_vec,
                limit,
                reply,
            } => {
                let _ = reply.send(self.storage.vector_query(&query_vec, limit).await);
            }
            StorageMsg::HybridQuery {
                q,
                query_vec,
                limit,
                reply,
            } => {
                let _ = reply.send(self.storage.hybrid_query(&q, &query_vec, limit).await);
            }
            StorageMsg::EnsureVectorIndex { reply } => {
                let _ = reply.send(self.storage.ensure_vector_index().await);
            }
            StorageMsg::UpsertChunks {
                doc_id,
                chunks,
                reply,
            } => {
                // No bump_generation — chunks are invisible to the doc
                // row-set (root invariant #15).
                let _ = reply.send(self.storage.upsert_chunks(&doc_id, &chunks).await);
            }
            StorageMsg::ListChunksForDoc { doc_id, reply } => {
                let _ = reply.send(self.storage.list_chunks_for_doc(&doc_id).await);
            }
            StorageMsg::ChunkVectorQuery {
                query_vec,
                over_fetch,
                limit,
                reply,
            } => {
                let _ = reply.send(
                    self.storage
                        .chunk_vector_query(&query_vec, over_fetch, limit)
                        .await,
                );
            }
            StorageMsg::EnsureChunkVectorIndex { reply } => {
                let _ = reply.send(self.storage.ensure_chunk_vector_index().await);
            }
            StorageMsg::CompactAll { .. } | StorageMsg::CompactAllWithRetention { .. } => {
                unreachable!("compaction runs off-loop; dispatched in run()")
            }
            StorageMsg::DatasetStats { reply } => {
                let _ = reply.send(self.storage.dataset_stats().await);
            }
            StorageMsg::ListDocs { limit, reply } => {
                let _ = reply.send(self.storage.list_docs(limit).await);
            }
            StorageMsg::ListSupersedeTargets { reply } => {
                let _ = reply.send(self.storage.list_supersede_targets().await);
            }
            StorageMsg::ListContentHashes { reply } => {
                let _ = reply.send(self.storage.list_content_hashes().await);
            }
            StorageMsg::ListFirstSeenSeedRows { reply } => {
                let _ = reply.send(self.storage.list_first_seen_seed_rows().await);
            }
            StorageMsg::ListReconcileRows { reply } => {
                let _ = reply.send(self.storage.list_reconcile_rows().await);
            }
            StorageMsg::GetById { id, reply } => {
                let _ = reply.send(self.storage.get_by_id(&id).await);
            }
            StorageMsg::LineageById { id, reply } => {
                let _ = reply.send(self.storage.lineage_by_id(&id).await);
            }
            StorageMsg::FindSupersededBy { target_id, reply } => {
                let _ = reply.send(self.storage.find_superseded_by(&target_id).await);
            }
            StorageMsg::GetByIds { ids, reply } => {
                let _ = reply.send(self.storage.get_by_ids(&ids).await);
            }
            StorageMsg::GetBodiesByIds { ids, reply } => {
                let _ = reply.send(self.storage.get_bodies_by_ids(&ids).await);
            }
            StorageMsg::EmbeddingById { id, reply } => {
                let _ = reply.send(self.storage.embedding_by_id(&id).await);
            }
            StorageMsg::EmbeddingsByIds { ids, reply } => {
                let _ = reply.send(self.storage.embeddings_by_ids(&ids).await);
            }
            StorageMsg::PromptById { id, reply } => {
                let _ = reply.send(self.storage.prompt_by_id(&id).await);
            }
            StorageMsg::GetBySourcePath { path, reply } => {
                let _ = reply.send(self.storage.get_by_source_path(&path).await);
            }
            StorageMsg::GetBySourcePaths { paths, reply } => {
                let _ = reply.send(self.storage.get_by_source_paths(&paths).await);
            }
            StorageMsg::UpdateAtlas { rows, reply } => {
                let _ = reply.send(self.storage.update_atlas(&rows).await);
            }
            StorageMsg::SetAtlasLabels { labels, reply } => {
                // Sqlite side table — no gallery-generation bump (invariant
                // #15), same as `UpdateAtlas` above.
                let _ = reply.send(self.db.set_atlas_labels(&labels));
            }
            StorageMsg::GetAtlasLabels { reply } => {
                let _ = reply.send(self.db.atlas_labels());
            }
            StorageMsg::AtlasSnapshotInsert {
                frame,
                points,
                reply,
            } => {
                // Sqlite side tables (V0028) — no gallery-generation bump
                // (invariant #15), same as `SetAtlasLabels`/`UpdateAtlas`
                // above. A frame write must never invalidate the gallery +
                // facets memos on every atlas recompute.
                let _ = reply.send(self.db.atlas_frame_insert(&frame, &points));
            }
            StorageMsg::AtlasSnapshotsList { limit, reply } => {
                let _ = reply.send(self.db.atlas_frames(limit));
            }
            StorageMsg::AtlasSnapshotPoints { snapshot_id, reply } => {
                let _ = reply.send(self.db.atlas_frame_points(snapshot_id));
            }
            StorageMsg::AtlasSnapshotPrune { keep, reply } => {
                // Deletes sqlite rows only — still no generation bump.
                let _ = reply.send(self.db.atlas_frames_prune(keep));
            }
            StorageMsg::TouchMtime {
                id,
                mtime_unix,
                reply,
            } => {
                let _ = reply.send(self.storage.touch_mtime(&id, mtime_unix).await);
            }
            StorageMsg::ListEmbeddings { reply } => {
                let _ = reply.send(self.storage.list_embeddings().await);
            }
            StorageMsg::ListDocsWithAtlas { limit, reply } => {
                let _ = reply.send(self.storage.list_docs_with_atlas(limit).await);
            }
            StorageMsg::RecordEdges {
                from_id,
                to_kinds,
                reply,
            } => {
                let res = self.db.record_edges(&from_id, &to_kinds);
                // G8 — bump the gallery generation only when the edge set
                // ACTUALLY changed. Edge rows back the gallery's backlink/
                // outlink counts, memoised alongside the row-set; a reindex
                // whose links are identical leaves the table untouched, so the
                // snapshot stays valid. (This runs downstream of upsert_doc,
                // which already bumped for a genuinely-changed artifact —
                // gating here avoids a redundant second bump, and protects a
                // future bare record_edges caller from a spurious one.)
                if matches!(res, Ok(true)) {
                    self.bump_generation();
                }
                let _ = reply.send(res.map(|_| ()));
            }
            StorageMsg::RecordCodeRefs {
                header,
                refs,
                reply,
            } => {
                // NO bump_generation — code_refs are invisible to the doc row
                // set and to `edge_counts`, so bumping would invalidate the
                // whole gallery memo (invariant #15) for a signal the gallery
                // never renders. The precedent is `UpsertChunks` above ("chunks
                // are invisible to the doc row-set"), NOT `RecordEdges`, which
                // bumps precisely because edges back the backlink/outlink
                // counts.
                let _ = reply.send(self.db.record_code_refs(&header, &refs));
            }
            StorageMsg::CodeRefsOf { artifact_id, reply } => {
                let _ = reply.send(self.db.code_refs_of(&artifact_id));
            }
            StorageMsg::CodeRefsFeed {
                after,
                limit,
                with_refs,
                reply,
            } => {
                let after = after.as_ref().map(|(t, id)| (*t, id.as_str()));
                let _ = reply.send(self.db.code_refs_feed(after, limit, with_refs));
            }
            StorageMsg::CodeRefsByTarget {
                path,
                with_refs,
                reply,
            } => {
                let _ = reply.send(self.db.code_refs_by_target(&path, with_refs));
            }
            // --- CT-F5 corpus-health SLOs ---------------------------------
            // None of these bump the index generation: the reads mutate
            // nothing, and the two writes touch a side column + an
            // append-only log that no doc row-set consumer can see
            // (the `RecordCodeRefs`/`UpsertChunks` precedent, invariant #15).
            StorageMsg::CodeRefShapeCounts { reply } => {
                let _ = reply.send(self.db.code_ref_shape_counts());
            }
            StorageMsg::KbSessionDocCounts { reply } => {
                let _ = reply.send(self.storage.kb_session_doc_counts().await);
            }
            StorageMsg::SessionIdsPresent { session_ids, reply } => {
                let _ = reply.send(self.db.session_ids_present(&session_ids));
            }
            StorageMsg::SessionsNewestStartedAt { reply } => {
                let _ = reply.send(self.db.sessions_newest_started_at());
            }
            StorageMsg::SessionsRecallCensusTotals { reply } => {
                let _ = reply.send(self.db.sessions_recall_census_totals());
            }
            StorageMsg::SessionsSetRecallCensus {
                artifact_id,
                marker_parsed,
                fallback_parsed,
                failed,
                reply,
            } => {
                let _ = reply.send(self.db.sessions_set_recall_census(
                    &artifact_id,
                    marker_parsed,
                    fallback_parsed,
                    failed,
                ));
            }
            StorageMsg::SloSnapshotAppend {
                taken_at_unix,
                indicators,
                reply,
            } => {
                let _ = reply.send(self.db.slo_snapshot_append(taken_at_unix, &indicators));
            }
            StorageMsg::SloSnapshotsList { limit, reply } => {
                let _ = reply.send(self.db.slo_snapshots_list(limit));
            }
            StorageMsg::EdgesFrom {
                start_id,
                max_depth,
                reply,
            } => {
                let _ = reply.send(self.db.edges_from(&start_id, max_depth));
            }
            StorageMsg::BacklinksOf { id, reply } => {
                let _ = reply.send(self.db.backlinks_of(&id));
            }
            StorageMsg::EdgeCounts { reply } => {
                let _ = reply.send(self.db.edge_counts());
            }
            StorageMsg::LinkPairs { reply } => {
                let _ = reply.send(self.db.link_pairs());
            }
            StorageMsg::ClearEmbeddings { reply } => {
                let _ = reply.send(self.storage.clear_embeddings().await);
            }
            StorageMsg::HistoryRecordOpen {
                artifact_id,
                now_unix,
                source,
                user,
                reply,
            } => {
                let _ = reply.send(self.db.history_record_open(
                    &artifact_id,
                    now_unix,
                    source.as_deref(),
                    &user,
                ));
            }
            StorageMsg::HistoryUpdateScroll {
                visit_id,
                scroll_y,
                scroll_max,
                now_unix,
                reply,
            } => {
                let _ = reply.send(
                    self.db
                        .history_update_scroll(visit_id, scroll_y, scroll_max, now_unix),
                );
            }
            StorageMsg::HistoryRecordSearch {
                query,
                now_unix,
                user,
                reply,
            } => {
                let _ = reply.send(self.db.history_record_search(&query, now_unix, &user));
            }
            StorageMsg::HistoryRecordComment {
                artifact_id,
                comment_id,
                now_unix,
                user,
                reply,
            } => {
                let _ = reply.send(self.db.history_record_comment(
                    &artifact_id,
                    &comment_id,
                    now_unix,
                    &user,
                ));
            }
            StorageMsg::HistoryList {
                limit,
                before_unix,
                kind_filter,
                user,
                reply,
            } => {
                let _ = reply.send(self.db.history_list(
                    limit,
                    before_unix,
                    kind_filter.as_deref(),
                    user.as_deref(),
                ));
            }
            // RP-track reading arms — sqlite side-channel only; NONE bump the
            // index generation (mirrors every History* arm, invariant #15).
            StorageMsg::ReadingUpsertSections {
                visit_id,
                artifact_id,
                sections,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.reading_upsert_sections(
                    visit_id,
                    &artifact_id,
                    &sections,
                    now_unix,
                ));
            }
            StorageMsg::ReadingSetActive {
                visit_id,
                active_ms,
                last_section,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.reading_set_active(
                    visit_id,
                    active_ms,
                    last_section.as_deref(),
                    now_unix,
                ));
            }
            StorageMsg::ReadingStateForVisit { visit_id, reply } => {
                let _ = reply.send(self.db.reading_state_for_visit(visit_id));
            }
            StorageMsg::ReadingInputsForArtifact {
                artifact_id,
                user,
                reply,
            } => {
                let _ = reply.send(
                    self.db
                        .reading_inputs_for_artifact(&artifact_id, user.as_deref()),
                );
            }
            StorageMsg::ReadingInputsForArtifacts {
                artifact_ids,
                user,
                reply,
            } => {
                let _ = reply.send(
                    self.db
                        .reading_inputs_for_artifacts(&artifact_ids, user.as_deref()),
                );
            }
            StorageMsg::HistoryDistinctUsers { reply } => {
                let _ = reply.send(self.db.history_distinct_users());
            }
            StorageMsg::ReadingLatestForArtifact {
                artifact_id,
                user,
                reply,
            } => {
                let _ = reply.send(self.db.reading_latest_for_artifact(&artifact_id, &user));
            }
            StorageMsg::ReadingLatestForIds {
                artifact_ids,
                user,
                reply,
            } => {
                let _ = reply.send(self.db.reading_latest_for_ids(&artifact_ids, &user));
            }
            StorageMsg::ReadingRollup { user, reply } => {
                let _ = reply.send(self.db.reading_rollup(&user));
            }
            StorageMsg::ReadingRollupForIds { ids, user, reply } => {
                let _ = reply.send(self.db.reading_rollup_for_ids(&ids, &user));
            }
            StorageMsg::FirstSeenInsertIgnore {
                artifact_id,
                ts,
                reply,
            } => {
                let _ = reply.send(self.db.first_seen_insert_ignore(&artifact_id, ts));
            }
            StorageMsg::FirstSeenForIds { ids, reply } => {
                let _ = reply.send(self.db.first_seen_for_ids(&ids));
            }
            StorageMsg::FirstSeenSeed { rows, reply } => {
                let _ = reply.send(self.db.first_seen_seed(&rows));
            }
            StorageMsg::FirstSeenIsEmpty { reply } => {
                let _ = reply.send(self.db.first_seen_is_empty());
            }
            StorageMsg::IdentityBackfill {
                operator,
                now_unix,
                reply,
            } => {
                // WRITE lane, no generation bump (sqlite side-channel only).
                let _ = reply.send(self.db.identity_backfill(&operator, now_unix));
            }
            StorageMsg::ListEntrySetUserOverride {
                entry_id,
                user,
                override_,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.list_entry_set_user_override(
                    &entry_id,
                    &user,
                    override_.as_deref(),
                    now_unix,
                ));
            }
            StorageMsg::ListEntryUserOverridesForList {
                list_id,
                user,
                reply,
            } => {
                let _ = reply.send(self.db.list_entry_user_overrides_for_list(&list_id, &user));
            }
            StorageMsg::HistoryOpensInWindow {
                from_unix,
                to_unix,
                limit,
                reply,
            } => {
                let _ = reply.send(self.db.history_opens_in_window(from_unix, to_unix, limit));
            }
            StorageMsg::HistoryCommentsInWindow {
                from_unix,
                to_unix,
                limit,
                reply,
            } => {
                let _ = reply.send(
                    self.db
                        .history_comments_in_window(from_unix, to_unix, limit),
                );
            }
            StorageMsg::HistoryCountsByDay {
                from_unix,
                to_unix,
                reply,
            } => {
                let _ = reply.send(self.db.history_counts_by_day(from_unix, to_unix));
            }
            StorageMsg::SharesInsert { row, reply } => {
                let _ = reply.send(self.db.shares_insert(&row));
            }
            StorageMsg::SharesList { reply } => {
                let _ = reply.send(self.db.shares_list());
            }
            StorageMsg::SharesGet { name, reply } => {
                let _ = reply.send(self.db.shares_get(&name));
            }
            StorageMsg::SharesGetByTarget { target, reply } => {
                let _ = reply.send(self.db.shares_get_by_target(&target));
            }
            StorageMsg::SharesDelete { name, reply } => {
                let _ = reply.send(self.db.shares_delete(&name));
            }
            StorageMsg::CorkboardAdd {
                artifact_id,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.corkboard_add(&artifact_id, now_unix));
            }
            StorageMsg::CorkboardRemove { artifact_id, reply } => {
                let _ = reply.send(self.db.corkboard_remove(&artifact_id));
            }
            StorageMsg::CorkboardList { reply } => {
                let _ = reply.send(self.db.corkboard_list());
            }
            StorageMsg::CorkboardCount { reply } => {
                let _ = reply.send(self.db.corkboard_count());
            }
            StorageMsg::PinnedMemoryAdd {
                artifact_id,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.pinned_memory_add(&artifact_id, now_unix));
            }
            StorageMsg::PinnedMemoryRemove { artifact_id, reply } => {
                let _ = reply.send(self.db.pinned_memory_remove(&artifact_id));
            }
            StorageMsg::PinnedMemoriesSet { reply } => {
                let _ = reply.send(self.db.pinned_memories_set());
            }
            StorageMsg::MemoryLinksFor { artifact_id, reply } => {
                let _ = reply.send(self.db.memory_links_for(&artifact_id));
            }
            StorageMsg::MemoryLinkAdd {
                artifact_id,
                linked_kb,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.memory_link_add(&artifact_id, &linked_kb, now_unix));
            }
            StorageMsg::MemoryLinkRemove {
                artifact_id,
                linked_kb,
                reply,
            } => {
                let _ = reply.send(self.db.memory_link_remove(&artifact_id, &linked_kb));
            }
            StorageMsg::MemoryLinksReplace {
                artifact_id,
                linked_kbs,
                global,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.memory_links_replace(
                    &artifact_id,
                    &linked_kbs,
                    global,
                    now_unix,
                ));
            }
            StorageMsg::MemoryLinksRemoveAll { artifact_id, reply } => {
                let _ = reply.send(self.db.memory_links_remove_all(&artifact_id));
            }
            StorageMsg::MemoryLinksAll { reply } => {
                let _ = reply.send(self.db.memory_links_all());
            }
            StorageMsg::MemoryLinksSeededHas { artifact_id, reply } => {
                let _ = reply.send(self.db.memory_links_seeded_has(&artifact_id));
            }
            StorageMsg::MemoryLinksSeededMark {
                artifact_id,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.memory_links_seeded_mark(&artifact_id, now_unix));
            }
            // Reading lists (V0015) — all sqlite side-channel writes: no
            // bump_generation anywhere (the lance row-set and edges are
            // untouched; the gallery memo must survive list churn).
            StorageMsg::ListCreate {
                id,
                title,
                description,
                pinned,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.list_create(
                    &id,
                    &title,
                    description.as_deref(),
                    pinned,
                    now_unix,
                ));
            }
            StorageMsg::ListGet { id, reply } => {
                let _ = reply.send(self.db.list_get(&id));
            }
            StorageMsg::ListsAll { reply } => {
                let _ = reply.send(self.db.lists_all());
            }
            StorageMsg::ListUpdate {
                id,
                title,
                description,
                pinned,
                archived,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.list_update(
                    &id,
                    title.as_deref(),
                    &description,
                    pinned,
                    archived,
                    now_unix,
                ));
            }
            StorageMsg::ListDelete { id, reply } => {
                let _ = reply.send(self.db.list_delete(&id));
            }
            StorageMsg::ListEntryAdd {
                entry,
                pos,
                user,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.list_entry_add(&entry, &pos, &user, now_unix));
            }
            StorageMsg::ListEntriesForList { list_id, reply } => {
                let _ = reply.send(self.db.list_entries_for_list(&list_id));
            }
            StorageMsg::ListEntriesAll { reply } => {
                let _ = reply.send(self.db.list_entries_all());
            }
            StorageMsg::ListEntryUpdate {
                entry_id,
                note,
                anchor,
                read_override,
                user,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.list_entry_update(
                    &entry_id,
                    &note,
                    &anchor,
                    &read_override,
                    &user,
                    now_unix,
                ));
            }
            StorageMsg::ListEntryMove {
                list_id,
                entry_id,
                pos,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.list_entry_move(&list_id, &entry_id, &pos, now_unix));
            }
            StorageMsg::ListEntryRemove {
                list_id,
                entry_id,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.list_entry_remove(&list_id, &entry_id, now_unix));
            }
            StorageMsg::ListEntriesRemoveMany {
                list_id,
                entry_ids,
                now_unix,
                reply,
            } => {
                let _ = reply.send(
                    self.db
                        .list_entries_remove_many(&list_id, &entry_ids, now_unix),
                );
            }
            StorageMsg::ListEntriesForArtifact { artifact_id, reply } => {
                let _ = reply.send(self.db.list_entries_for_artifact(&artifact_id));
            }
            StorageMsg::ListEntriesSyncResolution { updates, reply } => {
                let _ = reply.send(self.db.list_entries_sync_resolution(&updates));
            }
            StorageMsg::ListImportEntries {
                list_id,
                mode,
                entries,
                user,
                now_unix,
                reply,
            } => {
                let _ = reply.send(
                    self.db
                        .list_import_entries(&list_id, mode, &entries, &user, now_unix),
                );
            }
            StorageMsg::SessionsUpsert { row, reply } => {
                let _ = reply.send(self.db.sessions_upsert(&row));
            }
            StorageMsg::SessionsDelete { artifact_id, reply } => {
                let _ = reply.send(self.db.sessions_delete(&artifact_id));
            }
            StorageMsg::SessionsList {
                limit,
                before,
                before_id,
                folder,
                q,
                project,
                substance,
                harness,
                reply,
            } => {
                let _ = reply.send(self.db.sessions_list(
                    limit,
                    before,
                    before_id,
                    folder.as_deref(),
                    q.as_deref(),
                    &project,
                    &substance,
                    &harness,
                ));
            }
            StorageMsg::SessionsFolders { reply } => {
                let _ = reply.send(self.db.sessions_folders());
            }
            StorageMsg::SessionsProjectsStats { reply } => {
                let _ = reply.send(self.db.sessions_projects_stats());
            }
            StorageMsg::SessionsProjectsHarnessMix { reply } => {
                let _ = reply.send(self.db.sessions_projects_harness_mix());
            }
            StorageMsg::SessionsResearchRollup { substance, reply } => {
                let _ = reply.send(self.db.sessions_research_rollup(&substance));
            }
            StorageMsg::SessionsFunnelCounts {
                folder,
                project,
                substance,
                reply,
            } => {
                let _ = reply.send(self.db.sessions_funnel_counts(
                    folder.as_deref(),
                    &project,
                    &substance,
                ));
            }
            StorageMsg::SessionFilesInFolder { folder, reply } => {
                let _ = reply.send(self.db.session_files_in_folder(folder.as_deref()));
            }
            StorageMsg::SessionFilesReplace {
                artifact_id_session,
                files,
                reply,
            } => {
                let _ = reply.send(self.db.session_files_replace(&artifact_id_session, &files));
            }
            StorageMsg::SessionFilesForSession { session_id, reply } => {
                let _ = reply.send(self.db.session_files_for_session(&session_id));
            }
            StorageMsg::SessionFilesForArtifact {
                target_artifact_id,
                reply,
            } => {
                let _ = reply.send(self.db.session_files_for_artifact(&target_artifact_id));
            }
            StorageMsg::SessionFilesForBasename { basename, reply } => {
                let _ = reply.send(self.db.session_files_for_basename(&basename));
            }
            StorageMsg::SessionsGetMany { session_ids, reply } => {
                let _ = reply.send(self.db.sessions_get_many(&session_ids));
            }
            StorageMsg::SessionsGetByArtifactIds {
                artifact_ids,
                reply,
            } => {
                let _ = reply.send(self.db.sessions_get_by_artifact_ids(&artifact_ids));
            }
            StorageMsg::SessionDecisionsReplace {
                artifact_id_session,
                decisions,
                reply,
            } => {
                let _ = reply.send(
                    self.db
                        .session_decisions_replace(&artifact_id_session, &decisions),
                );
            }
            StorageMsg::SessionDecisionsForSession { session_id, reply } => {
                let _ = reply.send(self.db.session_decisions_for_session(&session_id));
            }
            StorageMsg::SessionDecisionsForSessions { session_ids, reply } => {
                let _ = reply.send(self.db.session_decisions_for_sessions(&session_ids));
            }
            StorageMsg::SessionCommitsReplace {
                artifact_id_session,
                commits,
                reply,
            } => {
                let _ = reply.send(
                    self.db
                        .session_commits_replace(&artifact_id_session, &commits),
                );
            }
            StorageMsg::SessionCommitsForSession { session_id, reply } => {
                let _ = reply.send(self.db.session_commits_for_session(&session_id));
            }
            StorageMsg::SessionCommitsForSessions { session_ids, reply } => {
                let _ = reply.send(self.db.session_commits_for_sessions(&session_ids));
            }
            StorageMsg::SessionCommitsBySha { prefix, reply } => {
                let _ = reply.send(self.db.session_commits_by_sha_prefix(&prefix));
            }
            StorageMsg::SessionCommitsPage {
                since,
                limit,
                offset,
                reply,
            } => {
                let _ = reply.send(self.db.session_commits_page(since, limit, offset));
            }
            StorageMsg::SessionResearchReplace {
                artifact_id_session,
                research,
                reply,
            } => {
                let _ = reply.send(
                    self.db
                        .session_research_replace(&artifact_id_session, &research),
                );
            }
            StorageMsg::SessionResearchForSession { session_id, reply } => {
                let _ = reply.send(self.db.session_research_for_session(&session_id));
            }
            StorageMsg::SessionResearchForSessions { session_ids, reply } => {
                let _ = reply.send(self.db.session_research_for_sessions(&session_ids));
            }
            StorageMsg::SessionResearchByJob { ulid, reply } => {
                let _ = reply.send(self.db.session_research_by_job(&ulid));
            }
            StorageMsg::CountDocsWithKbSession { session_id, reply } => {
                let _ = reply.send(self.storage.count_docs_with_kb_session(&session_id).await);
            }
            StorageMsg::CountDocsByKbSession { session_ids, reply } => {
                let _ = reply.send(self.storage.count_docs_by_kb_session(&session_ids).await);
            }
            StorageMsg::ListDocsWithKbSession {
                session_id,
                limit,
                reply,
            } => {
                let _ = reply.send(
                    self.storage
                        .list_docs_with_kb_session(&session_id, limit)
                        .await,
                );
            }
            StorageMsg::ListDocsWithKbSourceArtifact {
                source_kb,
                source_artifact,
                limit,
                reply,
            } => {
                let _ = reply.send(
                    self.storage
                        .list_docs_with_kb_source_artifact(&source_kb, &source_artifact, limit)
                        .await,
                );
            }
            StorageMsg::ListNotes { limit, reply } => {
                // Read-only — no bump_generation (notes share the lance docs
                // table but the route filters; the gallery memo is unaffected).
                let _ = reply.send(self.storage.list_notes(limit).await);
            }
            StorageMsg::SessionsGet { session_id, reply } => {
                let _ = reply.send(self.db.sessions_get(&session_id));
            }
            StorageMsg::MemoryRecallsReplace {
                artifact_id,
                rows,
                reply,
            } => {
                let _ = reply.send(self.db.memory_recalls_replace(&artifact_id, &rows));
            }
            StorageMsg::MemoryRecallsCountsForIds {
                memory_kb,
                memory_ids,
                reply,
            } => {
                let _ = reply.send(
                    self.db
                        .memory_recalls_counts_for_ids(memory_kb.as_deref(), &memory_ids),
                );
            }
            StorageMsg::MemoryRecallsForSession { session_id, reply } => {
                let _ = reply.send(self.db.memory_recalls_for_session(&session_id));
            }
            StorageMsg::MemoryRecallsWeeklyForIds {
                memory_kb,
                memory_ids,
                now_unix,
                reply,
            } => {
                let _ = reply.send(self.db.memory_recalls_weekly_for_ids(
                    memory_kb.as_deref(),
                    &memory_ids,
                    now_unix,
                ));
            }
            StorageMsg::MemoryRecallsForMemory {
                memory_kb,
                memory_id,
                limit,
                reply,
            } => {
                let _ = reply.send(
                    self.db
                        .memory_recalls_for_memory(&memory_kb, &memory_id, limit),
                );
            }
            StorageMsg::MemoryCommitsReplace {
                artifact_id,
                rows,
                reply,
            } => {
                let _ = reply.send(self.db.memory_commits_replace(&artifact_id, &rows));
            }
            StorageMsg::MemoryCommitsForMemory {
                memory_id,
                limit,
                reply,
            } => {
                let _ = reply.send(self.db.memory_commits_for_memory(&memory_id, limit));
            }
            StorageMsg::SnapshotLatestHash { artifact_id, reply } => {
                let _ = reply.send(self.db.snapshot_latest_hash(&artifact_id));
            }
            StorageMsg::SnapshotInsert {
                artifact_id,
                content_hash,
                raw_source,
                captured_at,
                reply,
            } => {
                let _ = reply.send(self.db.snapshot_insert(
                    &artifact_id,
                    &content_hash,
                    &raw_source,
                    captured_at,
                ));
            }
            StorageMsg::SnapshotList {
                artifact_id,
                limit,
                reply,
            } => {
                let _ = reply.send(self.db.snapshot_list(&artifact_id, limit));
            }
            StorageMsg::SnapshotRaw { id, reply } => {
                let _ = reply.send(self.db.snapshot_raw(id));
            }
            StorageMsg::SnapshotPrune {
                artifact_id,
                keep,
                reply,
            } => {
                let _ = reply.send(self.db.snapshot_prune(&artifact_id, keep));
            }
            StorageMsg::SnapshotsDeleteForArtifact { artifact_id, reply } => {
                let _ = reply.send(self.db.snapshots_delete_for_artifact(&artifact_id));
            }
            StorageMsg::HistoryPurge { reply } => {
                let _ = reply.send(self.db.history_purge());
            }
            StorageMsg::RetentionPrune {
                now_unix,
                history_max_age_secs,
                reading_max_age_secs,
                reply,
            } => {
                let _ = reply.send(self.db.retention_prune(
                    now_unix,
                    history_max_age_secs,
                    reading_max_age_secs,
                ));
            }
            StorageMsg::DropKbData { reply } => {
                // Lance first — its deletes are append-only ops on the
                // table version log; sqlite's are committed in one
                // transaction. If lance fails the sqlite side never
                // runs (we'd otherwise leave the kb in a torn state
                // where the search index disagrees with the metadata).
                let result = async {
                    let lance_rows = self.storage.count_rows().await?;
                    self.storage.delete_all_rows().await?;
                    // The lance row-set is now emptied — bump BEFORE the
                    // sqlite purge so a purge failure (which makes this
                    // block `Err`) can't strand the gallery cache serving
                    // the just-dropped rows. Over-bumping on the err path
                    // is harmless (a spurious bump only discards a valid
                    // cache); a missed bump after a committed lance delete
                    // would serve stale rows until the next mutation.
                    self.bump_generation();
                    let sqlite_rows = self.db.purge_kb_data()?;
                    Ok((lance_rows, sqlite_rows))
                }
                .await;
                let _ = reply.send(result);
            }
            StorageMsg::Shutdown => unreachable!("handled in run()"),
            #[cfg(test)]
            StorageMsg::PanicForTest { reply: _reply } => {
                // Panic BEFORE replying: the `_reply` sender drops unsent as the
                // arm unwinds, so the caller sees the reply-lost path — exactly
                // the production failure mode R1a supervises.
                panic!("R1e deliberate handler panic");
            }
            #[cfg(test)]
            StorageMsg::SlowWriteForTest { ms, reply } => {
                // Occupies the single actor loop for `ms`, exactly as a slow
                // ingest write would — the whole point of the read-priority lane.
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                let _ = reply.send(Ok(()));
            }
        }
    }
}

// --- Handle API --------------------------------------------------------------

impl StorageHandle {
    async fn send_and_await<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T>>) -> StorageMsg,
    ) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        let msg = build(tx);
        // SC4 — route to the read or write lane by message class. The caller
        // still awaits `rx` (which resolves only after the actor processes the
        // message), so this caller always reads its own write; only UNRELATED
        // concurrent reads may observe pre-backlog state (see the actor doc).
        let stamped = Stamped {
            enqueued: Instant::now(),
            msg,
        };
        let lane = if is_read_lane(&stamped.msg) {
            &self.read_tx
        } else {
            &self.write_tx
        };
        lane.send(stamped)
            .await
            .map_err(|_| Error::Storage("storage actor closed".into()))?;
        rx.await
            .map_err(|_| Error::Storage("storage actor reply lost".into()))?
    }

    pub async fn upsert_source(
        &self,
        slug: SourceSlug,
        path: PathBuf,
        added_at_unix: i64,
    ) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::UpsertSource {
            slug,
            path,
            added_at_unix,
            reply,
        })
        .await
    }

    pub async fn set_source_paused(&self, slug: SourceSlug, paused: bool) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::SetSourcePaused {
            slug,
            paused,
            reply,
        })
        .await
    }

    pub async fn list_sources(&self) -> Result<Vec<SourceRow>> {
        self.send_and_await(|reply| StorageMsg::ListSources { reply })
            .await
    }

    /// X2 — record a per-file exclusion (source-relative, pre-normalised via
    /// [`crate::exclusions::normalize_rel`]). `false` = already excluded.
    pub async fn add_exclusion(
        &self,
        path: String,
        excluded_at_unix: i64,
        note: Option<String>,
    ) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::AddExclusion {
            path,
            excluded_at_unix,
            note,
            reply,
        })
        .await
    }

    /// X2 — drop a per-file exclusion. `false` = wasn't excluded.
    pub async fn remove_exclusion(&self, path: String) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::RemoveExclusion { path, reply })
            .await
    }

    /// X2 — every excluded path, newest first.
    pub async fn list_exclusions(&self) -> Result<Vec<ExclusionRow>> {
        self.send_and_await(|reply| StorageMsg::ListExclusions { reply })
            .await
    }

    pub async fn begin_run(&self, source_slug: SourceSlug, started_at_unix: i64) -> Result<RunId> {
        self.send_and_await(|reply| StorageMsg::BeginRun {
            source_slug,
            started_at_unix,
            reply,
        })
        .await
    }

    pub async fn finish_run(
        &self,
        run_id: RunId,
        ok_count: u32,
        err_count: u32,
        finished_at_unix: i64,
    ) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::FinishRun {
            run_id,
            ok_count,
            err_count,
            finished_at_unix,
            reply,
        })
        .await
    }

    pub async fn last_run_for_source(&self, slug: SourceSlug) -> Result<Option<RunRow>> {
        self.send_and_await(|reply| StorageMsg::LastRunForSource { slug, reply })
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_error(
        &self,
        kind: String,
        source_slug: SourceSlug,
        path: PathBuf,
        message: String,
        content_hash: Option<String>,
        created_at_unix: i64,
    ) -> Result<ErrorId> {
        self.send_and_await(|reply| StorageMsg::RecordError {
            kind,
            source_slug,
            path,
            message,
            content_hash,
            created_at_unix,
            reply,
        })
        .await
    }

    pub async fn clear_errors_for_path_hash(
        &self,
        path: PathBuf,
        new_content_hash: String,
    ) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::ClearErrorsForPathHash {
            path,
            new_content_hash,
            reply,
        })
        .await
    }

    pub async fn clear_errors_for_path(&self, path: PathBuf) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::ClearErrorsForPath { path, reply })
            .await
    }

    pub async fn dismiss_error(&self, id: ErrorId) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::DismissError { id, reply })
            .await
    }

    pub async fn list_open_errors(&self) -> Result<Vec<ErrorRow>> {
        self.send_and_await(|reply| StorageMsg::ListOpenErrors { reply })
            .await
    }

    pub async fn retry_count_for_path_hash(
        &self,
        path: PathBuf,
        content_hash: String,
    ) -> Result<u32> {
        self.send_and_await(|reply| StorageMsg::RetryCountForPathHash {
            path,
            content_hash,
            reply,
        })
        .await
    }

    pub async fn upsert_doc(&self, doc: Doc) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::UpsertDoc {
            doc: Box::new(doc),
            reply,
        })
        .await
    }

    /// GC-B7 — batched sibling of `upsert_doc`: one Lance `merge_insert` /
    /// manifest commit for the WHOLE slice instead of one per doc. See
    /// `StorageMsg::UpsertDocs` for the callers + rationale. `docs.is_empty()`
    /// is a cheap `Ok(())` (mirrors `Storage::upsert_docs`'s own guard).
    pub async fn upsert_docs(&self, docs: Vec<Doc>) -> Result<()> {
        if docs.is_empty() {
            return Ok(());
        }
        self.send_and_await(|reply| StorageMsg::UpsertDocs { docs, reply })
            .await
    }

    pub async fn delete_doc(&self, id: ArtifactId) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::DeleteDoc { id, reply })
            .await
    }

    pub async fn delete_by_path(&self, path: PathBuf) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::DeleteByPath { path, reply })
            .await
    }

    /// R2 — cascade one artifact out of storage (lance + sqlite). The
    /// filesystem side is the caller's (`cascade::delete_artifact`).
    pub async fn cascade_delete_doc(
        &self,
        id: ArtifactId,
        path: PathBuf,
        mode: CascadeMode,
    ) -> Result<CascadeDbOutcome> {
        self.send_and_await(|reply| StorageMsg::CascadeDeleteDoc {
            id,
            path,
            mode,
            reply,
        })
        .await
    }

    /// F3a — append a moves intent row; returns its row id.
    pub async fn moves_insert_intent(
        &self,
        old_id: String,
        new_id: String,
        old_rel: String,
        new_rel: String,
        moved_at: i64,
    ) -> Result<i64> {
        self.send_and_await(|reply| StorageMsg::MovesInsertIntent {
            old_id,
            new_id,
            old_rel,
            new_rel,
            moved_at,
            reply,
        })
        .await
    }

    /// F3a — actor-atomic lance + sqlite rekey for one relocate.
    /// Returns DISTINCT list_ids whose entries were rekeyed.
    #[allow(clippy::too_many_arguments)]
    pub async fn relocate_doc_storage(
        &self,
        old_id: String,
        new_id: String,
        new_path: String,
        old_rel: String,
        new_rel: String,
        moves_row_id: i64,
        completed_at: i64,
    ) -> Result<Vec<String>> {
        self.send_and_await(|reply| StorageMsg::RelocateDoc {
            old_id,
            new_id,
            new_path,
            old_rel,
            new_rel,
            moves_row_id,
            completed_at,
            reply,
        })
        .await
    }

    /// F3a — chain-aware moves lookup (old_id or old_rel → (new_id, new_rel)).
    pub async fn moves_lookup(&self, key: String) -> Result<Option<(String, String)>> {
        self.send_and_await(|reply| StorageMsg::MovesLookup { key, reply })
            .await
    }

    /// F3a — durable watcher-delete suppression for `old_rel`.
    pub async fn moves_suppresses_delete(
        &self,
        old_rel: String,
        now_unix: i64,
        grace_secs: i64,
    ) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::MovesSuppressesDelete {
            old_rel,
            now_unix,
            grace_secs,
            reply,
        })
        .await
    }

    /// F3a — incomplete moves rows (startup replay).
    pub async fn moves_list_incomplete(&self) -> Result<Vec<MoveRow>> {
        self.send_and_await(|reply| StorageMsg::MovesListIncomplete { reply })
            .await
    }

    /// F3a — mark a moves row completed (abandon path).
    pub async fn moves_mark_completed(&self, row_id: i64, completed_at: i64) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::MovesMarkCompleted {
            row_id,
            completed_at,
            reply,
        })
        .await
    }

    /// R2 — prune sqlite dependents whose artifact id isn't in `keep_ids`.
    pub async fn sweep_orphans(
        &self,
        keep_ids: std::collections::HashSet<String>,
    ) -> Result<SweepOutcome> {
        self.send_and_await(|reply| StorageMsg::SweepOrphans { keep_ids, reply })
            .await
    }

    pub async fn ensure_fts_index(&self) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::EnsureFtsIndex { reply })
            .await
    }

    pub async fn count_rows(&self) -> Result<u64> {
        self.send_and_await(|reply| StorageMsg::CountRows { reply })
            .await
    }

    /// GC-B2 — cumulative typed-decode skips since this kb's `Storage` was
    /// opened. See `StorageMsg::DecodeSkipCount`.
    pub async fn decode_skip_count(&self) -> Result<u64> {
        self.send_and_await(|reply| StorageMsg::DecodeSkipCount { reply })
            .await
    }

    /// GC-D1 — `typo_tolerance` see `Storage::bm25_query`. Existing callers
    /// (memory recall, recollect, the CLI) pass `false` to stay exact; the
    /// search route reads `[kb.*.search] typo_tolerance` off `KbContext`.
    pub async fn bm25_query(
        &self,
        q: String,
        limit: u32,
        typo_tolerance: bool,
    ) -> Result<Vec<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::Bm25Query {
            q,
            limit,
            typo_tolerance,
            reply,
        })
        .await
    }

    pub async fn vector_query(&self, query_vec: Vec<f32>, limit: u32) -> Result<Vec<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::VectorQuery {
            query_vec,
            limit,
            reply,
        })
        .await
    }

    pub async fn hybrid_query(
        &self,
        q: String,
        query_vec: Vec<f32>,
        limit: u32,
    ) -> Result<Vec<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::HybridQuery {
            q,
            query_vec,
            limit,
            reply,
        })
        .await
    }

    pub async fn ensure_vector_index(&self) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::EnsureVectorIndex { reply })
            .await
    }

    pub async fn upsert_chunks(&self, doc_id: String, chunks: Vec<ChunkDoc>) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::UpsertChunks {
            doc_id,
            chunks,
            reply,
        })
        .await
    }

    /// F3a — list passage chunks for `doc_id` (including embeddings).
    pub async fn list_chunks_for_doc(&self, doc_id: String) -> Result<Vec<ChunkDoc>> {
        self.send_and_await(|reply| StorageMsg::ListChunksForDoc { doc_id, reply })
            .await
    }

    pub async fn chunk_vector_query(
        &self,
        query_vec: Vec<f32>,
        over_fetch: u32,
        limit: u32,
    ) -> Result<Vec<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::ChunkVectorQuery {
            query_vec,
            over_fetch,
            limit,
            reply,
        })
        .await
    }

    pub async fn ensure_chunk_vector_index(&self) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::EnsureChunkVectorIndex { reply })
            .await
    }

    /// Block-on-completion wrapper around the actor's `CompactAll`. Held
    /// open until lance finishes compaction + index optimize + version
    /// prune. Callers running this on startup typically spawn it on a
    /// background task so kb boot doesn't pay the wall-time.
    pub async fn compact_all(&self) -> Result<CompactStats> {
        self.send_and_await(|reply| StorageMsg::CompactAll { reply })
            .await
    }

    /// GC-B7 — same as `compact_all` but with an explicit retention window +
    /// `delete_unverified`, bypassing the safe production default. See
    /// `StorageMsg::CompactAllWithRetention` / `Storage::compact_all_with_retention`
    /// for the full rationale; exists for tests (and an eventual ops
    /// "vacuum now" verb) to exercise the physical-reclaim path
    /// deterministically.
    pub async fn compact_all_with_retention(
        &self,
        retention_minutes: i64,
        delete_unverified: bool,
    ) -> Result<CompactStats> {
        self.send_and_await(|reply| StorageMsg::CompactAllWithRetention {
            retention_minutes,
            delete_unverified,
            reply,
        })
        .await
    }

    pub async fn dataset_stats(&self) -> Result<DatasetStats> {
        self.send_and_await(|reply| StorageMsg::DatasetStats { reply })
            .await
    }

    pub async fn list_docs(&self, limit: u32) -> Result<Vec<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::ListDocs { limit, reply })
            .await
    }

    /// N-track — slim list of `kb_category = 'note'` rows (incl. task
    /// counts). Read-only; does not bump the index generation.
    pub async fn list_notes(&self, limit: u32) -> Result<Vec<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::ListNotes { limit, reply })
            .await
    }

    pub async fn list_supersede_targets(&self) -> Result<Vec<String>> {
        self.send_and_await(|reply| StorageMsg::ListSupersedeTargets { reply })
            .await
    }

    pub async fn list_content_hashes(&self) -> Result<Vec<ContentHashRow>> {
        self.send_and_await(|reply| StorageMsg::ListContentHashes { reply })
            .await
    }

    /// v0.33 X2 — seed projection for `doc_first_seen` bring-up.
    pub async fn list_first_seen_seed_rows(&self) -> Result<Vec<FirstSeenSeedRow>> {
        self.send_and_await(|reply| StorageMsg::ListFirstSeenSeedRows { reply })
            .await
    }

    /// Reconcile projection: every row's `(path, mtime_unix)`. Narrower than
    /// `list_docs` (path + mtime only, no sort) so the periodic reconcile
    /// pass's full-corpus scan occupies the single-writer actor for less
    /// time. Read-only; does not bump the index generation.
    pub async fn list_reconcile_rows(&self) -> Result<Vec<ReconcileRow>> {
        self.send_and_await(|reply| StorageMsg::ListReconcileRows { reply })
            .await
    }

    pub async fn get_by_id(&self, id: String) -> Result<Option<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::GetById { id, reply })
            .await
    }

    /// MI-W2.4a — `kb memory log <id>`'s FORWARD hop (see
    /// `storage::lance::Storage::lineage_by_id`).
    pub async fn lineage_by_id(&self, id: String) -> Result<Option<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::LineageById { id, reply })
            .await
    }

    /// MI-W2.4a — `kb memory log <id>`'s REVERSE hop (see
    /// `storage::lance::Storage::find_superseded_by`).
    pub async fn find_superseded_by(&self, target_id: String) -> Result<Vec<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::FindSupersededBy { target_id, reply })
            .await
    }

    /// Batch exact-id lookup — one round-trip resolving many ids (see
    /// `storage::lance::Storage::get_by_ids`).
    pub async fn get_by_ids(&self, ids: Vec<String>) -> Result<Vec<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::GetByIds { ids, reply })
            .await
    }

    /// Q-track (board B1) — batch FULL-body lookup for the search route's
    /// match-context snippet extraction (see `storage::lance::Storage::
    /// get_bodies_by_ids`).
    pub async fn get_bodies_by_ids(&self, ids: Vec<String>) -> Result<Vec<(String, String)>> {
        self.send_and_await(|reply| StorageMsg::GetBodiesByIds { ids, reply })
            .await
    }

    /// W2.3a — one doc's embedding vector (see `storage::lance::Storage::
    /// embedding_by_id`). The true-neighbors route's seed-vector lookup.
    pub async fn embedding_by_id(&self, id: String) -> Result<Option<Vec<f32>>> {
        self.send_and_await(|reply| StorageMsg::EmbeddingById { id, reply })
            .await
    }

    /// W2.3a — batch variant of `embedding_by_id` (see `storage::lance::
    /// Storage::embeddings_by_ids`).
    pub async fn embeddings_by_ids(&self, ids: Vec<String>) -> Result<Vec<EmbeddingPair>> {
        self.send_and_await(|reply| StorageMsg::EmbeddingsByIds { ids, reply })
            .await
    }

    /// W2.11 — one doc's stored generation prompt + capped byte size (see
    /// `storage::lance::Storage::prompt_by_id`). The prompt-browse route's
    /// read.
    pub async fn prompt_by_id(&self, id: String) -> Result<Option<(String, u32)>> {
        self.send_and_await(|reply| StorageMsg::PromptById { id, reply })
            .await
    }

    pub async fn get_by_source_path(&self, path: String) -> Result<Option<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::GetBySourcePath { path, reply })
            .await
    }

    /// Batch exact-path lookup — one round-trip resolving many stored
    /// paths (see `storage::lance::Storage::get_by_source_paths`).
    pub async fn get_by_source_paths(&self, paths: Vec<String>) -> Result<Vec<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::GetBySourcePaths { paths, reply })
            .await
    }

    pub async fn update_atlas(&self, rows: Vec<(String, f32, f32, i16)>) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::UpdateAtlas { rows, reply })
            .await
    }

    /// W1.B — replace the whole `atlas_labels` table with a fresh c-TF-IDF
    /// label set. See `crate::atlas_labels` for the compute + `Db::set_atlas_labels`
    /// for the storage shape.
    pub async fn set_atlas_labels(&self, labels: Vec<AtlasLabelRow>) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::SetAtlasLabels { labels, reply })
            .await
    }

    /// W1.B — every stored atlas label, ordered `(cluster, rank)`. Backs
    /// `GET /api/kb/{kb}/atlas/labels`.
    pub async fn atlas_labels(&self) -> Result<Vec<AtlasLabelRow>> {
        self.send_and_await(|reply| StorageMsg::GetAtlasLabels { reply })
            .await
    }

    /// W3 T-a — append one atlas time-lapse frame (V0028) and self-prune to
    /// `DEFAULT_ATLAS_FRAMES_KEEP`. `Ok(None)` means the geometry was
    /// bit-identical to the newest stored frame and nothing was written; see
    /// [`crate::storage::sqlite::Db::atlas_frame_insert`].
    pub async fn atlas_frame_insert(
        &self,
        frame: NewAtlasFrame,
        points: Vec<AtlasFramePoint>,
    ) -> Result<Option<i64>> {
        self.send_and_await(|reply| StorageMsg::AtlasSnapshotInsert {
            frame: Box::new(frame),
            points,
            reply,
        })
        .await
    }

    /// W3 T-a — frame metadata, newest first, capped at `limit`.
    pub async fn atlas_frames(&self, limit: u32) -> Result<Vec<AtlasFrameRow>> {
        self.send_and_await(|reply| StorageMsg::AtlasSnapshotsList { limit, reply })
            .await
    }

    /// W3 T-a — one frame's points, ordered by `artifact_id`.
    pub async fn atlas_frame_points(&self, snapshot_id: i64) -> Result<Vec<AtlasFramePoint>> {
        self.send_and_await(|reply| StorageMsg::AtlasSnapshotPoints { snapshot_id, reply })
            .await
    }

    /// W3 T-a — drop all but the newest `keep` frames. Returns frames
    /// deleted; their points cascade.
    pub async fn atlas_frames_prune(&self, keep: u32) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::AtlasSnapshotPrune { keep, reply })
            .await
    }

    /// Heal a stale `mtime_unix` for one row so the reconcile producer-side
    /// dedup stops re-emitting a touched-but-unchanged file every pass. See
    /// [`crate::storage::lance::LanceStore::touch_mtime`].
    pub async fn touch_mtime(&self, id: String, mtime_unix: i64) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::TouchMtime {
            id,
            mtime_unix,
            reply,
        })
        .await
    }

    pub async fn list_embeddings(&self) -> Result<Vec<EmbeddingPair>> {
        self.send_and_await(|reply| StorageMsg::ListEmbeddings { reply })
            .await
    }

    pub async fn list_docs_with_atlas(&self, limit: u32) -> Result<Vec<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::ListDocsWithAtlas { limit, reply })
            .await
    }

    pub async fn record_edges(
        &self,
        from_id: String,
        to_kinds: Vec<(String, String)>,
    ) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::RecordEdges {
            from_id,
            to_kinds,
            reply,
        })
        .await
    }

    /// DCB W1.A — replace an artifact's code-ref extraction; `Ok(true)` = the
    /// set actually changed (the caller uses it only for observability — the
    /// actor never bumps the index generation on this message).
    pub async fn record_code_refs(
        &self,
        header: CodeRefHeaderRow,
        refs: Vec<CodeRefRow>,
    ) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::RecordCodeRefs {
            header: Box::new(header),
            refs,
            reply,
        })
        .await
    }

    // --- CT-F5 corpus-health SLOs -----------------------------------------

    /// CT-F5 — `(total_hints, path_shaped_hints)` over `code_refs`. See
    /// `Db::code_ref_shape_counts` for the ONE definition of "path shaped".
    pub async fn code_ref_shape_counts(&self) -> Result<(u64, u64)> {
        self.send_and_await(|reply| StorageMsg::CodeRefShapeCounts { reply })
            .await
    }

    /// CT-F5 — distinct non-empty `kb_session` values in this corpus with
    /// their doc counts. Includes `memory-session` transcripts (unlike the
    /// `memory_count` reads) — see `Storage::kb_session_doc_counts`.
    pub async fn kb_session_doc_counts(&self) -> Result<std::collections::HashMap<String, u64>> {
        self.send_and_await(|reply| StorageMsg::KbSessionDocCounts { reply })
            .await
    }

    /// CT-F5 — which of `session_ids` have a `sessions` row in THIS kb.
    /// Callers fan this out across every kb on the daemon: `kb_session` is a
    /// cross-corpus hint (invariant #11), so a per-kb answer alone would call
    /// every memory an orphan.
    pub async fn session_ids_present(&self, session_ids: Vec<String>) -> Result<Vec<String>> {
        self.send_and_await(|reply| StorageMsg::SessionIdsPresent { session_ids, reply })
            .await
    }

    /// CT-F5 — newest `sessions.started_at`, `None` on an empty table.
    pub async fn sessions_newest_started_at(&self) -> Result<Option<i64>> {
        self.send_and_await(|reply| StorageMsg::SessionsNewestStartedAt { reply })
            .await
    }

    /// CT-F5 — `(marker_parsed, fallback_parsed, failed, censused_captures)`
    /// over the newest capture per session, NULL censuses excluded.
    pub async fn sessions_recall_census_totals(&self) -> Result<(u64, u64, u64, u64)> {
        self.send_and_await(|reply| StorageMsg::SessionsRecallCensusTotals { reply })
            .await
    }

    /// CT-F5 — persist one capture's CT-A3 recall parse census. Returns the
    /// rows updated; `0` (no `sessions` row for this artifact) is a designed
    /// no-op that leaves the census NULL.
    pub async fn sessions_set_recall_census(
        &self,
        artifact_id: String,
        marker_parsed: u32,
        fallback_parsed: u32,
        failed: u32,
    ) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::SessionsSetRecallCensus {
            artifact_id,
            marker_parsed,
            fallback_parsed,
            failed,
            reply,
        })
        .await
    }

    /// CT-F5 — append one snapshot run: one row per indicator, all sharing
    /// `taken_at_unix`. Append-only; there is no update or delete path.
    pub async fn slo_snapshot_append(
        &self,
        taken_at_unix: i64,
        indicators: Vec<crate::slo::SloIndicator>,
    ) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::SloSnapshotAppend {
            taken_at_unix,
            indicators,
            reply,
        })
        .await
    }

    /// CT-F5 — newest-first page over the append-only snapshot log.
    pub async fn slo_snapshots_list(&self, limit: u32) -> Result<Vec<SloSnapshotRow>> {
        self.send_and_await(|reply| StorageMsg::SloSnapshotsList { limit, reply })
            .await
    }

    /// DCB W1.A — one artifact's extraction. `None` = never scanned (distinct
    /// from a scan that found nothing).
    pub async fn code_refs_of(&self, artifact_id: String) -> Result<Option<CodeRefDoc>> {
        self.send_and_await(|reply| StorageMsg::CodeRefsOf { artifact_id, reply })
            .await
    }

    /// DCB W1.A — keyset page over the corpus feed, ascending
    /// `(extracted_at, artifact_id)`.
    pub async fn code_refs_feed(
        &self,
        after: Option<(i64, String)>,
        limit: u32,
        with_refs: bool,
    ) -> Result<Vec<CodeRefDoc>> {
        self.send_and_await(|reply| StorageMsg::CodeRefsFeed {
            after,
            limit,
            with_refs,
            reply,
        })
        .await
    }

    /// CT-B3 — every doc citing `path` (exact `path_hint` match), complete
    /// (no cursor). Backs `?by_target=` on the feed route.
    pub async fn code_refs_by_target(
        &self,
        path: String,
        with_refs: bool,
    ) -> Result<Vec<CodeRefDoc>> {
        self.send_and_await(|reply| StorageMsg::CodeRefsByTarget {
            path,
            with_refs,
            reply,
        })
        .await
    }

    pub async fn edges_from(&self, start_id: String, max_depth: u32) -> Result<Vec<EdgeRow>> {
        self.send_and_await(|reply| StorageMsg::EdgesFrom {
            start_id,
            max_depth,
            reply,
        })
        .await
    }

    /// Inbound edges to `id` (depth 1) — the linkers. Reverse of the first
    /// hop of [`Self::edges_from`]; backs the backlink panels + CLI verbs.
    pub async fn backlinks_of(&self, id: String) -> Result<Vec<EdgeRow>> {
        self.send_and_await(|reply| StorageMsg::BacklinksOf { id, reply })
            .await
    }

    /// v0.6 B2 — bulk fetch (outbound, inbound) edge counts for every
    /// artifact that has at least one edge in this kb. The gallery's
    /// `list_docs` route joins the result into each `DocResponse`.
    pub async fn edge_counts(&self) -> Result<std::collections::HashMap<String, (u32, u32)>> {
        self.send_and_await(|reply| StorageMsg::EdgeCounts { reply })
            .await
    }

    /// All `kind = 'link'` edges in this kb as `(src, dst)` pairs.
    /// Backs the SPA atlas view's edge layer.
    pub async fn link_pairs(&self) -> Result<Vec<(String, String)>> {
        self.send_and_await(|reply| StorageMsg::LinkPairs { reply })
            .await
    }

    pub async fn clear_embeddings(&self) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::ClearEmbeddings { reply })
            .await
    }

    /// v0.6+ H1 — begin or resume an artifact-view visit. Returns
    /// `OpenResult { id, scroll_y, is_new_visit }`. The SPA detail route
    /// calls this on mount and posts the returned `scroll_y` to the
    /// iframe. The HTTP layer emits `history.recorded` SSE iff
    /// `is_new_visit`. `source` (GC-B5) is `Some("web")`/`Some("cli")`, or
    /// `None` to leave the column unset.
    pub async fn history_record_open(
        &self,
        artifact_id: String,
        now_unix: i64,
        source: Option<String>,
        user: String,
    ) -> Result<OpenResult> {
        self.send_and_await(|reply| StorageMsg::HistoryRecordOpen {
            artifact_id,
            now_unix,
            source,
            user,
            reply,
        })
        .await
    }

    /// v0.6+ H1 — UPDATE scroll position on an open visit. Returns
    /// rows-affected count; 0 means the visit_id doesn't exist or
    /// isn't an open-kind row (caller should 404).
    pub async fn history_update_scroll(
        &self,
        visit_id: i64,
        scroll_y: i64,
        scroll_max: i64,
        now_unix: i64,
    ) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::HistoryUpdateScroll {
            visit_id,
            scroll_y,
            scroll_max,
            now_unix,
            reply,
        })
        .await
    }

    /// v0.6+ H1 — record a search query with 5-second dedup.
    pub async fn history_record_search(
        &self,
        query: String,
        now_unix: i64,
        user: String,
    ) -> Result<i64> {
        self.send_and_await(|reply| StorageMsg::HistoryRecordSearch {
            query,
            now_unix,
            user,
            reply,
        })
        .await
    }

    /// v0.6+ H1 — record a new-comment event.
    pub async fn history_record_comment(
        &self,
        artifact_id: String,
        comment_id: String,
        now_unix: i64,
        user: String,
    ) -> Result<i64> {
        self.send_and_await(|reply| StorageMsg::HistoryRecordComment {
            artifact_id,
            comment_id,
            now_unix,
            user,
            reply,
        })
        .await
    }

    /// v0.6+ H1 — newest-first history list. `user = None` = all users.
    pub async fn history_list(
        &self,
        limit: u32,
        before_unix: Option<i64>,
        kind_filter: Option<String>,
        user: Option<String>,
    ) -> Result<Vec<HistoryRow>> {
        self.send_and_await(|reply| StorageMsg::HistoryList {
            limit,
            before_unix,
            kind_filter,
            user,
            reply,
        })
        .await
    }

    /// RP-track — UPSERT per-section reading dwell for a visit (cumulative,
    /// max-merged). Returns rows written.
    pub async fn reading_upsert_sections(
        &self,
        visit_id: i64,
        artifact_id: String,
        sections: Vec<SectionDwell>,
        now_unix: i64,
    ) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::ReadingUpsertSections {
            visit_id,
            artifact_id,
            sections,
            now_unix,
            reply,
        })
        .await
    }

    /// RP-track — set a visit's active_ms + stop-point. Rows-affected 0 =
    /// unknown visit (the reading endpoint's 404 gate).
    pub async fn reading_set_active(
        &self,
        visit_id: i64,
        active_ms: i64,
        last_section: Option<String>,
        now_unix: i64,
    ) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::ReadingSetActive {
            visit_id,
            active_ms,
            last_section,
            now_unix,
            reply,
        })
        .await
    }

    /// RP-track — a visit's resume baseline (seed-on-open).
    pub async fn reading_state_for_visit(&self, visit_id: i64) -> Result<ReadingResume> {
        self.send_and_await(|reply| StorageMsg::ReadingStateForVisit { visit_id, reply })
            .await
    }

    /// RP-track — section rows + visit roll-ups for an artifact's summary.
    /// `user = Some` scopes to that user's visits (v0.34 Y1).
    #[allow(clippy::type_complexity)]
    pub async fn reading_inputs_for_artifact(
        &self,
        artifact_id: String,
        user: Option<String>,
    ) -> Result<(
        Vec<crate::reading::ReadingSectionRow>,
        Vec<crate::reading::VisitRollup>,
    )> {
        self.send_and_await(|reply| StorageMsg::ReadingInputsForArtifact {
            artifact_id,
            user,
            reply,
        })
        .await
    }

    /// RP-track — batched section rows + visit roll-ups for a SET of
    /// artifacts, keyed by artifact id (ids with no reading rows are
    /// absent). One actor round-trip for the whole set; backs the
    /// reading-lists enrichment. `user = Some` scopes per-requester.
    #[allow(clippy::type_complexity)]
    pub async fn reading_inputs_for_artifacts(
        &self,
        artifact_ids: Vec<String>,
        user: Option<String>,
    ) -> Result<
        std::collections::HashMap<
            String,
            (
                Vec<crate::reading::ReadingSectionRow>,
                Vec<crate::reading::VisitRollup>,
            ),
        >,
    > {
        self.send_and_await(|reply| StorageMsg::ReadingInputsForArtifacts {
            artifact_ids,
            user,
            reply,
        })
        .await
    }

    /// v0.34 Y1 — distinct non-empty `history.user` values in this kb.
    pub async fn history_distinct_users(&self) -> Result<Vec<String>> {
        self.send_and_await(|reply| StorageMsg::HistoryDistinctUsers { reply })
            .await
    }

    /// RP-track — cheap latest-visit reading state for recall enrichment.
    pub async fn reading_latest_for_artifact(
        &self,
        artifact_id: String,
        user: String,
    ) -> Result<Option<(u8, Option<String>, i64)>> {
        self.send_and_await(|reply| StorageMsg::ReadingLatestForArtifact {
            artifact_id,
            user,
            reply,
        })
        .await
    }

    /// RP-track — batched latest-visit reading state for a set of ids (one
    /// window pass). Recall enrichment groups hits by kb and calls this once
    /// per kb instead of one round-trip per hit. Scoped to `user`.
    #[allow(clippy::type_complexity)]
    pub async fn reading_latest_for_ids(
        &self,
        artifact_ids: Vec<String>,
        user: String,
    ) -> Result<std::collections::HashMap<String, (u8, Option<String>, i64)>> {
        self.send_and_await(|reply| StorageMsg::ReadingLatestForIds {
            artifact_ids,
            user,
            reply,
        })
        .await
    }

    /// Q-track — batched read-state rollup over the whole `history` table
    /// for `user`. One entry per artifact with an open visit or a
    /// per-user list `read_override`.
    pub async fn reading_rollup(
        &self,
        user: String,
    ) -> Result<std::collections::HashMap<String, crate::reading::ReadRollup>> {
        self.send_and_await(|reply| StorageMsg::ReadingRollup { user, reply })
            .await
    }

    /// Q-track — [`Self::reading_rollup`] scoped to a candidate id set for
    /// `user`, so the search page's window scan is proportional to its page.
    pub async fn reading_rollup_for_ids(
        &self,
        ids: Vec<String>,
        user: String,
    ) -> Result<std::collections::HashMap<String, crate::reading::ReadRollup>> {
        self.send_and_await(|reply| StorageMsg::ReadingRollupForIds { ids, user, reply })
            .await
    }

    /// v0.34 X1 — idempotent identity backfill (WRITE; no generation bump).
    pub async fn identity_backfill(&self, operator: String, now_unix: i64) -> Result<u64> {
        self.send_and_await(|reply| StorageMsg::IdentityBackfill {
            operator,
            now_unix,
            reply,
        })
        .await
    }

    /// v0.34 X1 — set/clear per-user list read override.
    pub async fn list_entry_set_user_override(
        &self,
        entry_id: String,
        user: String,
        override_: Option<String>,
        now_unix: i64,
    ) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::ListEntrySetUserOverride {
            entry_id,
            user,
            override_,
            now_unix,
            reply,
        })
        .await
    }

    /// v0.34 X1 — per-user override map for a list (READ lane).
    pub async fn list_entry_user_overrides_for_list(
        &self,
        list_id: String,
        user: String,
    ) -> Result<std::collections::HashMap<String, String>> {
        self.send_and_await(|reply| StorageMsg::ListEntryUserOverridesForList {
            list_id,
            user,
            reply,
        })
        .await
    }

    /// v0.33 X2 — record first-indexed time (INSERT OR IGNORE).
    pub async fn first_seen_insert_ignore(&self, artifact_id: String, ts: i64) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::FirstSeenInsertIgnore {
            artifact_id,
            ts,
            reply,
        })
        .await
    }

    /// v0.33 X2 — batched first-indexed lookup.
    pub async fn first_seen_for_ids(
        &self,
        ids: Vec<String>,
    ) -> Result<std::collections::HashMap<String, i64>> {
        self.send_and_await(|reply| StorageMsg::FirstSeenForIds { ids, reply })
            .await
    }

    /// v0.33 X2 — bring-up bulk seed.
    pub async fn first_seen_seed(&self, rows: Vec<(String, i64)>) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::FirstSeenSeed { rows, reply })
            .await
    }

    /// v0.33 X2 — empty-table gate for the bring-up seed.
    pub async fn first_seen_is_empty(&self) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::FirstSeenIsEmpty { reply })
            .await
    }

    /// RP-track — open-visits in a [from,to] window (session readings).
    pub async fn history_opens_in_window(
        &self,
        from_unix: i64,
        to_unix: i64,
        limit: u32,
    ) -> Result<Vec<HistoryRow>> {
        self.send_and_await(|reply| StorageMsg::HistoryOpensInWindow {
            from_unix,
            to_unix,
            limit,
            reply,
        })
        .await
    }

    /// R8 — comment-creation events in a time window ("raised during").
    pub async fn history_comments_in_window(
        &self,
        from_unix: i64,
        to_unix: i64,
        limit: u32,
    ) -> Result<Vec<HistoryRow>> {
        self.send_and_await(|reply| StorageMsg::HistoryCommentsInWindow {
            from_unix,
            to_unix,
            limit,
            reply,
        })
        .await
    }

    /// W2.10 — per-day, per-kind event counts in a [from,to] window (the
    /// activity-calendar density grid). See `Db::history_counts_by_day` for
    /// the UTC-day grammar.
    pub async fn history_counts_by_day(
        &self,
        from_unix: i64,
        to_unix: i64,
    ) -> Result<Vec<DayKindCount>> {
        self.send_and_await(|reply| StorageMsg::HistoryCountsByDay {
            from_unix,
            to_unix,
            reply,
        })
        .await
    }

    /// kb share registry — insert or upsert a share row (keyed on name).
    pub async fn shares_insert(&self, row: ShareRow) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::SharesInsert {
            row: Box::new(row),
            reply,
        })
        .await
    }

    /// kb share registry — all shares, newest-first.
    pub async fn shares_list(&self) -> Result<Vec<ShareRow>> {
        self.send_and_await(|reply| StorageMsg::SharesList { reply })
            .await
    }

    /// kb share registry — one share by name.
    pub async fn shares_get(&self, name: String) -> Result<Option<ShareRow>> {
        self.send_and_await(|reply| StorageMsg::SharesGet { name, reply })
            .await
    }

    /// kb share registry — most-recent share for a target (drives --update).
    pub async fn shares_get_by_target(&self, target: String) -> Result<Option<ShareRow>> {
        self.send_and_await(|reply| StorageMsg::SharesGetByTarget { target, reply })
            .await
    }

    /// kb share registry — delete a share by name (revoke). Rows affected.
    pub async fn shares_delete(&self, name: String) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::SharesDelete { name, reply })
            .await
    }

    /// Corkboard (V0005) — pin an artifact. Returns `true` if a new row
    /// was inserted, `false` if the artifact was already pinned.
    pub async fn corkboard_add(&self, artifact_id: String, now_unix: i64) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::CorkboardAdd {
            artifact_id,
            now_unix,
            reply,
        })
        .await
    }

    /// Corkboard — unpin an artifact. Returns `true` if a row was
    /// deleted, `false` if it wasn't pinned.
    pub async fn corkboard_remove(&self, artifact_id: String) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::CorkboardRemove { artifact_id, reply })
            .await
    }

    /// Corkboard — read-only list, newest-first.
    pub async fn corkboard_list(&self) -> Result<Vec<CorkboardRow>> {
        self.send_and_await(|reply| StorageMsg::CorkboardList { reply })
            .await
    }

    /// Corkboard — cheap count for the Header anchor-pill badge.
    pub async fn corkboard_count(&self) -> Result<u64> {
        self.send_and_await(|reply| StorageMsg::CorkboardCount { reply })
            .await
    }

    /// Pinned memories (V0006) — pin a memory artifact in this kb.
    pub async fn pinned_memory_add(&self, artifact_id: String, now_unix: i64) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::PinnedMemoryAdd {
            artifact_id,
            now_unix,
            reply,
        })
        .await
    }

    /// Pinned memories — unpin.
    pub async fn pinned_memory_remove(&self, artifact_id: String) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::PinnedMemoryRemove { artifact_id, reply })
            .await
    }

    /// Pinned memories — full pinned-id set for the recall fan-out.
    pub async fn pinned_memories_set(&self) -> Result<std::collections::HashSet<String>> {
        self.send_and_await(|reply| StorageMsg::PinnedMemoriesSet { reply })
            .await
    }

    /// Memory links (V0010) — full link set for one memory.
    pub async fn memory_links_for(&self, artifact_id: String) -> Result<Vec<String>> {
        self.send_and_await(|reply| StorageMsg::MemoryLinksFor { artifact_id, reply })
            .await
    }

    /// Memory links — idempotent INSERT of one edge.
    pub async fn memory_link_add(
        &self,
        artifact_id: String,
        linked_kb: String,
        now_unix: i64,
    ) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::MemoryLinkAdd {
            artifact_id,
            linked_kb,
            now_unix,
            reply,
        })
        .await
    }

    /// Memory links — idempotent DELETE of one edge.
    pub async fn memory_link_remove(&self, artifact_id: String, linked_kb: String) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::MemoryLinkRemove {
            artifact_id,
            linked_kb,
            reply,
        })
        .await
    }

    /// Memory links — atomic replace of the link set.
    pub async fn memory_links_replace(
        &self,
        artifact_id: String,
        linked_kbs: Vec<String>,
        global: bool,
        now_unix: i64,
    ) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::MemoryLinksReplace {
            artifact_id,
            linked_kbs,
            global,
            now_unix,
            reply,
        })
        .await
    }

    /// Memory links — drop every edge + clear seeded tombstone for a
    /// memory. Paired with `process_delete` in the indexer.
    pub async fn memory_links_remove_all(&self, artifact_id: String) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::MemoryLinksRemoveAll { artifact_id, reply })
            .await
    }

    /// Memory links — bulk fetch (one scan per memory corpus per
    /// recall request).
    pub async fn memory_links_all(
        &self,
    ) -> Result<std::collections::HashMap<String, std::collections::HashSet<String>>> {
        self.send_and_await(|reply| StorageMsg::MemoryLinksAll { reply })
            .await
    }

    /// V0011 — has this memory ever been seeded?
    pub async fn memory_links_seeded_has(&self, artifact_id: String) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::MemoryLinksSeededHas { artifact_id, reply })
            .await
    }

    /// V0011 — mark a memory as seeded.
    pub async fn memory_links_seeded_mark(&self, artifact_id: String, now_unix: i64) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::MemoryLinksSeededMark {
            artifact_id,
            now_unix,
            reply,
        })
        .await
    }

    /// Reading lists (V0015) — create. `Conflict` on a title clash.
    pub async fn list_create(
        &self,
        id: String,
        title: String,
        description: Option<String>,
        pinned: bool,
        now_unix: i64,
    ) -> Result<ListRow> {
        self.send_and_await(|reply| StorageMsg::ListCreate {
            id,
            title,
            description,
            pinned,
            now_unix,
            reply,
        })
        .await
    }

    /// Reading lists — single-list lookup.
    pub async fn list_get(&self, id: String) -> Result<Option<ListRow>> {
        self.send_and_await(|reply| StorageMsg::ListGet { id, reply })
            .await
    }

    /// Reading lists — all lists, pinned-first then newest-touched.
    pub async fn lists_all(&self) -> Result<Vec<ListRow>> {
        self.send_and_await(|reply| StorageMsg::ListsAll { reply })
            .await
    }

    /// Reading lists — patch header fields. `Ok(None)` when missing.
    pub async fn list_update(
        &self,
        id: String,
        title: Option<String>,
        description: Patch<String>,
        pinned: Option<bool>,
        archived: Option<bool>,
        now_unix: i64,
    ) -> Result<Option<ListRow>> {
        self.send_and_await(|reply| StorageMsg::ListUpdate {
            id,
            title,
            description,
            pinned,
            archived,
            now_unix,
            reply,
        })
        .await
    }

    /// Reading lists — delete (entries cascade). `true` if removed.
    pub async fn list_delete(&self, id: String) -> Result<bool> {
        self.send_and_await(|reply| StorageMsg::ListDelete { id, reply })
            .await
    }

    /// Reading lists — insert one entry at a position.
    /// `user` stamps a read marker into `list_entry_user_state` when set.
    pub async fn list_entry_add(
        &self,
        entry: NewListEntry,
        pos: PositionSpec,
        user: String,
        now_unix: i64,
    ) -> Result<ListEntryRow> {
        self.send_and_await(|reply| StorageMsg::ListEntryAdd {
            entry,
            pos,
            user,
            now_unix,
            reply,
        })
        .await
    }

    /// Reading lists — one list's entries in display order.
    pub async fn list_entries_for_list(&self, list_id: String) -> Result<Vec<ListEntryRow>> {
        self.send_and_await(|reply| StorageMsg::ListEntriesForList { list_id, reply })
            .await
    }

    /// Reading lists — every entry in the kb.
    pub async fn list_entries_all(&self) -> Result<Vec<ListEntryRow>> {
        self.send_and_await(|reply| StorageMsg::ListEntriesAll { reply })
            .await
    }

    /// Reading lists — patch one entry's content fields.
    /// `read_override` routes to `list_entry_user_state` for `user`.
    pub async fn list_entry_update(
        &self,
        entry_id: String,
        note: Patch<String>,
        anchor: Patch<(String, Option<i64>)>,
        read_override: Patch<String>,
        user: String,
        now_unix: i64,
    ) -> Result<Option<ListEntryRow>> {
        self.send_and_await(|reply| StorageMsg::ListEntryUpdate {
            entry_id,
            note,
            anchor,
            read_override,
            user,
            now_unix,
            reply,
        })
        .await
    }

    /// Reading lists — reorder one entry within its list.
    pub async fn list_entry_move(
        &self,
        list_id: String,
        entry_id: String,
        pos: PositionSpec,
        now_unix: i64,
    ) -> Result<Option<ListEntryRow>> {
        self.send_and_await(|reply| StorageMsg::ListEntryMove {
            list_id,
            entry_id,
            pos,
            now_unix,
            reply,
        })
        .await
    }

    /// Reading lists — remove one entry (returns the removed row).
    pub async fn list_entry_remove(
        &self,
        list_id: String,
        entry_id: String,
        now_unix: i64,
    ) -> Result<Option<ListEntryRow>> {
        self.send_and_await(|reply| StorageMsg::ListEntryRemove {
            list_id,
            entry_id,
            now_unix,
            reply,
        })
        .await
    }

    /// v0.33 X3 — bulk-remove entry ids from a list in ONE transaction.
    pub async fn list_entries_remove_many(
        &self,
        list_id: String,
        entry_ids: Vec<String>,
        now_unix: i64,
    ) -> Result<Vec<ListEntryRow>> {
        self.send_and_await(|reply| StorageMsg::ListEntriesRemoveMany {
            list_id,
            entry_ids,
            now_unix,
            reply,
        })
        .await
    }

    /// Reading lists — entries targeting one artifact, across lists.
    pub async fn list_entries_for_artifact(
        &self,
        artifact_id: String,
    ) -> Result<Vec<ListEntryRow>> {
        self.send_and_await(|reply| StorageMsg::ListEntriesForArtifact { artifact_id, reply })
            .await
    }

    /// Reading lists — the ListAnchorHook's machine write (resolution
    /// state + word estimates). Never touches `updated_at`.
    pub async fn list_entries_sync_resolution(
        &self,
        updates: Vec<ResolutionUpdate>,
    ) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::ListEntriesSyncResolution { updates, reply })
            .await
    }

    /// Reading lists — bulk import in one transaction. Returns inserted.
    /// Read markers land in `list_entry_user_state` for `user`.
    pub async fn list_import_entries(
        &self,
        list_id: String,
        mode: ImportMode,
        entries: Vec<NewListEntry>,
        user: String,
        now_unix: i64,
    ) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::ListImportEntries {
            list_id,
            mode,
            entries,
            user,
            now_unix,
            reply,
        })
        .await
    }

    /// Sessions enrichment (V0008) — upsert one row. Best-effort
    /// caller: the indexer logs+drops on failure rather than failing
    /// the whole index pass (enrichment is a side-channel).
    pub async fn sessions_upsert(&self, row: SessionRow) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::SessionsUpsert {
            row: Box::new(row),
            reply,
        })
        .await
    }

    /// Sessions enrichment — drop a row. Paired with `delete_by_path`
    /// when a memory-session file is unlinked.
    pub async fn sessions_delete(&self, artifact_id: String) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::SessionsDelete { artifact_id, reply })
            .await
    }

    /// Sessions enrichment — newest-first list. `before`/`before_id` are
    /// the keyset cursor (see `Db::sessions_list`); `folder` (A1) narrows to
    /// one working directory; `q` (P6) is a keyword filter.
    // Same W3.A justification as `Db::sessions_list` (sqlite.rs) — this
    // wrapper mirrors its param list one-for-one.
    #[allow(clippy::too_many_arguments)]
    pub async fn sessions_list(
        &self,
        limit: u32,
        before: Option<i64>,
        before_id: Option<String>,
        folder: Option<String>,
        q: Option<String>,
        project: crate::sessions::ProjectFilter,
        substance: Vec<String>,
        harness: Vec<String>,
    ) -> Result<Vec<SessionRow>> {
        self.send_and_await(|reply| StorageMsg::SessionsList {
            limit,
            before,
            before_id,
            folder,
            q,
            project,
            substance,
            harness,
            reply,
        })
        .await
    }

    /// Sessions enrichment — the folder facet: `(cwd, count, latest_started_at)`
    /// per distinct working directory, newest-active first.
    pub async fn sessions_folders(&self) -> Result<Vec<FolderStats>> {
        self.send_and_await(|reply| StorageMsg::SessionsFolders { reply })
            .await
    }

    /// W3.A/P4 — the `/api/sessions/projects` facet's per-project rollup.
    pub async fn sessions_projects_stats(&self) -> Result<Vec<ProjectStatsRow>> {
        self.send_and_await(|reply| StorageMsg::SessionsProjectsStats { reply })
            .await
    }

    /// W3.A/P4 — the harness breakdown behind `harness_mix`.
    pub async fn sessions_projects_harness_mix(&self) -> Result<Vec<ProjectHarnessRow>> {
        self.send_and_await(|reply| StorageMsg::SessionsProjectsHarnessMix { reply })
            .await
    }

    /// R9 — research queries aggregated by (cwd, kind, query).
    pub async fn sessions_research_rollup(
        &self,
        substance: Vec<String>,
    ) -> Result<Vec<ResearchRollupRow>> {
        self.send_and_await(|reply| StorageMsg::SessionsResearchRollup { substance, reply })
            .await
    }

    /// R9 — the activity-funnel stage counts (optionally folder-scoped).
    pub async fn sessions_funnel_counts(
        &self,
        folder: Option<String>,
        project: crate::sessions::ProjectFilter,
        substance: Vec<String>,
    ) -> Result<FunnelCounts> {
        self.send_and_await(|reply| StorageMsg::SessionsFunnelCounts {
            folder,
            project,
            substance,
            reply,
        })
        .await
    }

    /// R9 — in-corpus touches for a folder (the funnel's `commented` stage).
    #[allow(clippy::type_complexity)]
    pub async fn session_files_in_folder(
        &self,
        folder: Option<String>,
    ) -> Result<Vec<(String, String, String)>> {
        self.send_and_await(|reply| StorageMsg::SessionFilesInFolder { folder, reply })
            .await
    }

    /// Sessions enrichment — lookup by Claude Code session id.
    pub async fn sessions_get(&self, session_id: String) -> Result<Option<SessionRow>> {
        self.send_and_await(|reply| StorageMsg::SessionsGet { session_id, reply })
            .await
    }

    /// Session files (V0017) — replace one session's edge set wholesale.
    /// Best-effort caller (the indexer's session-capture hook).
    pub async fn session_files_replace(
        &self,
        artifact_id_session: String,
        files: Vec<SessionFileRow>,
    ) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::SessionFilesReplace {
            artifact_id_session,
            files,
            reply,
        })
        .await
    }

    /// Session files — the per-session file manifest.
    pub async fn session_files_for_session(
        &self,
        session_id: String,
    ) -> Result<Vec<SessionFileRow>> {
        self.send_and_await(|reply| StorageMsg::SessionFilesForSession { session_id, reply })
            .await
    }

    /// Session files — the reverse "sessions that touched this artifact"
    /// lookup (A7).
    pub async fn session_files_for_artifact(
        &self,
        target_artifact_id: String,
    ) -> Result<Vec<SessionFileRow>> {
        self.send_and_await(|reply| StorageMsg::SessionFilesForArtifact {
            target_artifact_id,
            reply,
        })
        .await
    }

    /// R2 (`kb why`) — session edges matching a file basename.
    pub async fn session_files_for_basename(
        &self,
        basename: String,
    ) -> Result<Vec<SessionFileRow>> {
        self.send_and_await(|reply| StorageMsg::SessionFilesForBasename { basename, reply })
            .await
    }

    /// R2 (`kb why`) — batched session-metadata lookup for a set of ids.
    pub async fn sessions_get_many(&self, session_ids: Vec<String>) -> Result<Vec<SessionRow>> {
        self.send_and_await(|reply| StorageMsg::SessionsGetMany { session_ids, reply })
            .await
    }

    /// #11/recollect-R3 — resolve lance artifact ids to their sqlite session
    /// rows (the identity join; see [`StorageMsg::SessionsGetByArtifactIds`]).
    pub async fn sessions_get_by_artifact_ids(
        &self,
        artifact_ids: Vec<String>,
    ) -> Result<Vec<SessionRow>> {
        self.send_and_await(|reply| StorageMsg::SessionsGetByArtifactIds {
            artifact_ids,
            reply,
        })
        .await
    }

    /// Session decisions (V0018/S9) — replace one session's decisions log.
    pub async fn session_decisions_replace(
        &self,
        artifact_id_session: String,
        decisions: Vec<SessionDecisionRow>,
    ) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::SessionDecisionsReplace {
            artifact_id_session,
            decisions,
            reply,
        })
        .await
    }

    /// Session decisions — the log for one session, in order.
    pub async fn session_decisions_for_session(
        &self,
        session_id: String,
    ) -> Result<Vec<SessionDecisionRow>> {
        self.send_and_await(|reply| StorageMsg::SessionDecisionsForSession { session_id, reply })
            .await
    }

    /// Session decisions — batched form (newest capture only, #11).
    pub async fn session_decisions_for_sessions(
        &self,
        session_ids: Vec<String>,
    ) -> Result<std::collections::HashMap<String, Vec<SessionDecisionRow>>> {
        self.send_and_await(|reply| StorageMsg::SessionDecisionsForSessions { session_ids, reply })
            .await
    }

    /// Session commits (V0019/P5) — replace one session's commits list.
    pub async fn session_commits_replace(
        &self,
        artifact_id_session: String,
        commits: Vec<SessionCommitRow>,
    ) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::SessionCommitsReplace {
            artifact_id_session,
            commits,
            reply,
        })
        .await
    }

    /// Session commits — the list for one session, in order.
    pub async fn session_commits_for_session(
        &self,
        session_id: String,
    ) -> Result<Vec<SessionCommitRow>> {
        self.send_and_await(|reply| StorageMsg::SessionCommitsForSession { session_id, reply })
            .await
    }

    /// Session commits — batched form (newest capture only, #11).
    pub async fn session_commits_for_sessions(
        &self,
        session_ids: Vec<String>,
    ) -> Result<std::collections::HashMap<String, Vec<SessionCommitRow>>> {
        self.send_and_await(|reply| StorageMsg::SessionCommitsForSessions { session_ids, reply })
            .await
    }

    /// kb-code Wave 0 (W0.6) — commits whose sha/sha_full starts with
    /// `prefix`, newest capture only (#11).
    pub async fn session_commits_by_sha_prefix(
        &self,
        prefix: String,
    ) -> Result<Vec<SessionCommitMatch>> {
        self.send_and_await(|reply| StorageMsg::SessionCommitsBySha { prefix, reply })
            .await
    }

    /// kb-code Wave 0 (W0.6) — the flat, offset-paginated `commit-map` bulk
    /// feed, newest capture only (#11).
    pub async fn session_commits_page(
        &self,
        since: Option<i64>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<CommitMapRow>> {
        self.send_and_await(|reply| StorageMsg::SessionCommitsPage {
            since,
            limit,
            offset,
            reply,
        })
        .await
    }

    /// R4 — session research: replace one session's research signals.
    pub async fn session_research_replace(
        &self,
        artifact_id_session: String,
        research: Vec<SessionResearchRow>,
    ) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::SessionResearchReplace {
            artifact_id_session,
            research,
            reply,
        })
        .await
    }

    /// R4 — session research: the signals for one session, in order.
    pub async fn session_research_for_session(
        &self,
        session_id: String,
    ) -> Result<Vec<SessionResearchRow>> {
        self.send_and_await(|reply| StorageMsg::SessionResearchForSession { session_id, reply })
            .await
    }

    /// R4 — session research: batched form (newest capture only, #11).
    pub async fn session_research_for_sessions(
        &self,
        session_ids: Vec<String>,
    ) -> Result<std::collections::HashMap<String, Vec<SessionResearchRow>>> {
        self.send_and_await(|reply| StorageMsg::SessionResearchForSessions { session_ids, reply })
            .await
    }

    /// W4/R8/ADD-2 — every `grok_job` research row (across every session in
    /// this kb) whose query equals `ulid` — the `by-job` join.
    pub async fn session_research_by_job(&self, ulid: String) -> Result<Vec<SessionResearchRow>> {
        self.send_and_await(|reply| StorageMsg::SessionResearchByJob { ulid, reply })
            .await
    }

    /// MI-W1.1 (revised) — replace one CAPTURE's full `memory_recalls` set
    /// (see `Db::memory_recalls_replace`'s doc comment). Best-effort caller
    /// (the `memory-recall-ledger` enrichment hook); `artifact_id` is that
    /// capture's own `sessions.artifact_id`, not the session id.
    pub async fn memory_recalls_replace(
        &self,
        artifact_id: String,
        rows: Vec<MemoryRecallRow>,
    ) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::MemoryRecallsReplace {
            artifact_id,
            rows,
            reply,
        })
        .await
    }

    /// MI-W1.1 — every recalled hit for one session id (test/debug read).
    pub async fn memory_recalls_for_session(
        &self,
        session_id: String,
    ) -> Result<Vec<MemoryRecallRow>> {
        self.send_and_await(|reply| StorageMsg::MemoryRecallsForSession { session_id, reply })
            .await
    }

    /// MI-W1.2/W1.3 — aggregate recall stats for a batch of memory ids
    /// within THIS kb's `memory_recalls` table.
    pub async fn memory_recalls_counts_for_ids(
        &self,
        memory_kb: Option<String>,
        memory_ids: Vec<String>,
    ) -> Result<Vec<MemoryRecallCount>> {
        self.send_and_await(|reply| StorageMsg::MemoryRecallsCountsForIds {
            memory_kb,
            memory_ids,
            reply,
        })
        .await
    }

    /// MI-W4.2a — per-week injection histogram for a batch of memory ids
    /// within THIS kb's `memory_recalls` table (the `/memory` row
    /// sparkline's data source).
    pub async fn memory_recalls_weekly_for_ids(
        &self,
        memory_kb: Option<String>,
        memory_ids: Vec<String>,
        now_unix: i64,
    ) -> Result<Vec<MemoryRecallWeeklyRow>> {
        self.send_and_await(|reply| StorageMsg::MemoryRecallsWeeklyForIds {
            memory_kb,
            memory_ids,
            now_unix,
            reply,
        })
        .await
    }

    /// CT-B2 — every session that recalled ONE memory within THIS kb's
    /// `memory_recalls` table, newest-first. The `recalled-by` route fans
    /// this out across every kb on the daemon (invariant #28).
    pub async fn memory_recalls_for_memory(
        &self,
        memory_kb: String,
        memory_id: String,
        limit: u32,
    ) -> Result<Vec<MemoryRecalledByRow>> {
        self.send_and_await(|reply| StorageMsg::MemoryRecallsForMemory {
            memory_kb,
            memory_id,
            limit,
            reply,
        })
        .await
    }

    /// CT-F1 — replace THIS CAPTURE's `memory_commits` claims (V0038). The
    /// `memory-commit-ledger` enrichment hook's ONLY write, and like every
    /// hook it goes through the actor, never sqlite directly (kb-core
    /// invariant #2).
    pub async fn memory_commits_replace(
        &self,
        artifact_id: String,
        rows: Vec<MemoryCommitRow>,
    ) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::MemoryCommitsReplace {
            artifact_id,
            rows,
            reply,
        })
        .await
    }

    /// CT-F1 — every commit that cited ONE memory within THIS kb's
    /// `memory_commits` table, newest-first. The `commits` route fans this
    /// out across every kb on the daemon (invariant #28).
    pub async fn memory_commits_for_memory(
        &self,
        memory_id: String,
        limit: u32,
    ) -> Result<Vec<MemoryCommitRow>> {
        self.send_and_await(|reply| StorageMsg::MemoryCommitsForMemory {
            memory_id,
            limit,
            reply,
        })
        .await
    }

    /// Artifact snapshots (V0013, Track V) — latest stored content hash for
    /// an artifact (the capture hook's dedup gate). Best-effort caller.
    pub async fn snapshot_latest_hash(&self, artifact_id: String) -> Result<Option<String>> {
        self.send_and_await(|reply| StorageMsg::SnapshotLatestHash { artifact_id, reply })
            .await
    }

    /// Artifact snapshots — append one revision.
    pub async fn snapshot_insert(
        &self,
        artifact_id: String,
        content_hash: String,
        raw_source: String,
        captured_at: i64,
    ) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::SnapshotInsert {
            artifact_id,
            content_hash,
            raw_source,
            captured_at,
            reply,
        })
        .await
    }

    /// Artifact snapshots — newest-first metadata for the timeline list.
    pub async fn snapshot_list(
        &self,
        artifact_id: String,
        limit: u32,
    ) -> Result<Vec<SnapshotMeta>> {
        self.send_and_await(|reply| StorageMsg::SnapshotList {
            artifact_id,
            limit,
            reply,
        })
        .await
    }

    /// Artifact snapshots — one revision's verbatim source, by row id.
    pub async fn snapshot_raw(&self, id: i64) -> Result<Option<String>> {
        self.send_and_await(|reply| StorageMsg::SnapshotRaw { id, reply })
            .await
    }

    /// Artifact snapshots — prune to the newest `keep` per artifact.
    pub async fn snapshot_prune(&self, artifact_id: String, keep: u32) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::SnapshotPrune {
            artifact_id,
            keep,
            reply,
        })
        .await
    }

    /// Artifact snapshots — drop all for an artifact (delete pass).
    pub async fn snapshots_delete_for_artifact(&self, artifact_id: String) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::SnapshotsDeleteForArtifact { artifact_id, reply })
            .await
    }

    /// v0.14 S3 — count lance rows whose `kb_session` matches.
    pub async fn count_docs_with_kb_session(&self, session_id: String) -> Result<u64> {
        self.send_and_await(|reply| StorageMsg::CountDocsWithKbSession { session_id, reply })
            .await
    }

    /// Perf sweep 2026-07 — batched grouped count of lance rows per
    /// `kb_session` id (ONE projection scan for the whole page). Ids
    /// with zero matching docs are absent from the map.
    pub async fn count_docs_by_kb_session(
        &self,
        session_ids: Vec<String>,
    ) -> Result<std::collections::HashMap<String, u64>> {
        self.send_and_await(|reply| StorageMsg::CountDocsByKbSession { session_ids, reply })
            .await
    }

    /// v0.14 S3 — list lance rows whose `kb_session` matches.
    pub async fn list_docs_with_kb_session(
        &self,
        session_id: String,
        limit: u32,
    ) -> Result<Vec<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::ListDocsWithKbSession {
            session_id,
            limit,
            reply,
        })
        .await
    }

    /// CT-A1 (U3 parse-back) — list lance rows whose `kb_source_kb`/
    /// `kb_source_artifact` name a given origin artifact (the reverse of
    /// `MemoryProvenance`).
    pub async fn list_docs_with_kb_source_artifact(
        &self,
        source_kb: String,
        source_artifact: String,
        limit: u32,
    ) -> Result<Vec<DocSummary>> {
        self.send_and_await(|reply| StorageMsg::ListDocsWithKbSourceArtifact {
            source_kb,
            source_artifact,
            limit,
            reply,
        })
        .await
    }

    /// S5 admin — wipe the history table. Returns rows deleted.
    pub async fn history_purge(&self) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::HistoryPurge { reply })
            .await
    }

    /// R3 (v0.24) — opt-in age-based retention prune. Deletes history +
    /// reading_sections rows older than the given windows (each `None` =
    /// keep forever). Returns total rows deleted. See `Db::retention_prune`.
    pub async fn retention_prune(
        &self,
        now_unix: i64,
        history_max_age_secs: Option<i64>,
        reading_max_age_secs: Option<i64>,
    ) -> Result<usize> {
        self.send_and_await(|reply| StorageMsg::RetentionPrune {
            now_unix,
            history_max_age_secs,
            reading_max_age_secs,
            reply,
        })
        .await
    }

    /// S5 admin — drop every transient row for a kb (lance + sqlite
    /// history/errors/edges). Preserves shares + `.review/*` per the
    /// per-fn docs (external state + user state respectively).
    /// Returns `(lance_rows_deleted, sqlite_rows_deleted)`.
    pub async fn drop_kb_data(&self) -> Result<(u64, usize)> {
        self.send_and_await(|reply| StorageMsg::DropKbData { reply })
            .await
    }

    /// Best-effort shutdown signal; the actor breaks out of `run()` and the
    /// task returns. Subsequent sends fail with "storage actor closed".
    pub async fn shutdown(&self) {
        // SC4 — Shutdown rides the WRITE lane so it stays FIFO-ordered behind
        // writes enqueued before it (they land first); the read lane must not
        // let a later read jump ahead of a pending shutdown's write ordering.
        let _ = self
            .write_tx
            .send(Stamped {
                enqueued: Instant::now(),
                msg: StorageMsg::Shutdown,
            })
            .await;
    }

    /// R1e — test-only: dispatch the deliberately-panicking message and await
    /// its (lost) reply. Returns the reply-lost error the panic produces.
    #[cfg(test)]
    async fn panic_for_test(&self) -> Result<()> {
        self.send_and_await(|reply| StorageMsg::PanicForTest { reply })
            .await
    }

    /// SC4 test helper — enqueue a message onto its classified lane WITHOUT
    /// awaiting the reply, returning the receiver. Uses `try_send` (never
    /// awaits) so a `#[tokio::test]` current-thread runtime can stage a full,
    /// deterministic backlog before the actor task ever runs — the two-lane
    /// scheduling tests depend on nothing draining mid-enqueue.
    #[cfg(test)]
    fn send_no_wait<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T>>) -> StorageMsg,
    ) -> oneshot::Receiver<Result<T>> {
        let (tx, rx) = oneshot::channel();
        let stamped = Stamped {
            enqueued: Instant::now(),
            msg: build(tx),
        };
        let lane = if is_read_lane(&stamped.msg) {
            &self.read_tx
        } else {
            &self.write_tx
        };
        lane.try_send(stamped)
            .unwrap_or_else(|_| panic!("send_no_wait: lane full (raise CHANNEL_CAPACITY)"));
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn handle() -> (StorageHandle, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let lance = tmp.path().join("lance");
        let sqlite = tmp.path().join("index.db");
        let h = StorageActor::spawn(lance, sqlite, None).await.unwrap();
        (h, tmp)
    }

    #[tokio::test]
    async fn round_trip_source_and_run() {
        let (h, _tmp) = handle().await;
        let slug = SourceSlug::from_path(std::path::Path::new("/tmp/canon"));

        h.upsert_source(slug.clone(), "/tmp/canon".into(), 1700000000)
            .await
            .unwrap();

        let sources = h.list_sources().await.unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].raw_slug, slug.as_str());

        let run = h.begin_run(slug.clone(), 1700000100).await.unwrap();
        h.finish_run(run.clone(), 5, 0, 1700000200).await.unwrap();

        let last = h.last_run_for_source(slug).await.unwrap().unwrap();
        assert_eq!(last.id, run.as_str());
        assert_eq!(last.ok_count, 5);
    }

    /// R1a/R1e — per-message panic supervision. A message whose handler
    /// panics must (a) surface the reply-lost error to its in-flight caller
    /// and (b) NOT kill the actor task: the very next operation on the same
    /// handle must still succeed, proving the `run()` loop caught the unwind
    /// and kept draining the channel (pre-R1 the task died and every later
    /// call got "storage actor closed" until daemon restart).
    #[tokio::test]
    async fn handler_panic_is_supervised_actor_survives() {
        let (h, _tmp) = handle().await;

        // A poisoned message: its caller sees the reply-lost path…
        let err = h.panic_for_test().await.unwrap_err();
        match err {
            Error::Storage(msg) => {
                assert!(
                    msg.contains("reply lost"),
                    "expected reply-lost error, got: {msg}"
                );
            }
            other => panic!("expected Error::Storage(reply lost), got {other:?}"),
        }

        // …and the actor is still alive: a normal roundtrip succeeds.
        h.upsert_doc(Doc::placeholder("survivor", "/tmp/s.html"))
            .await
            .expect("actor survived the panic and handled the next message");
        let docs = h.list_docs(10).await.expect("read after panic works");
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].id, "survivor");
    }

    #[tokio::test]
    async fn upsert_doc_then_search() {
        let (h, _tmp) = handle().await;
        let mut doc = Doc::placeholder("docid", "/tmp/x.html");
        doc.title = "Borrow Checker".into();
        doc.body = "lifetime conflicts in real time".into();

        h.upsert_doc(doc).await.unwrap();
        h.ensure_fts_index().await.unwrap();

        let hits = h.bm25_query("borrow".into(), 5, false).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "docid");
    }

    /// Reconcile dedup heal — a file touched-but-unchanged (mtime bumped,
    /// bytes identical) leaves `mtime_unix` stale, which made the reconcile
    /// producer-side dedup re-emit a `watch.modify` for it every pass forever
    /// (SSE storm → SPA 100% CPU). `touch_mtime` updates JUST the mtime so the
    /// next pass sees disk == stored and stops; the rest of the row (id/title)
    /// is preserved (by-id column update, not a re-upsert).
    #[tokio::test]
    async fn touch_mtime_heals_stale_mtime_only() {
        let (h, _tmp) = handle().await;
        let mut doc = Doc::placeholder("docid", "/tmp/x.html");
        doc.title = "Sprint 2026-06-01".into();
        doc.mtime_unix = 1_700_000_000;
        h.upsert_doc(doc).await.unwrap();

        let before = h.list_docs(10).await.unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].mtime_unix, Some(1_700_000_000));

        // Heal to the newer on-disk mtime (the touched-but-unchanged case).
        h.touch_mtime("docid".into(), 1_700_000_044).await.unwrap();

        let after = h.list_docs(10).await.unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(
            after[0].mtime_unix,
            Some(1_700_000_044),
            "mtime healed to disk value"
        );
        assert_eq!(after[0].id, "docid", "row preserved");
        assert_eq!(after[0].title, "Sprint 2026-06-01", "title preserved");

        // A non-matching id is a no-op, not an error.
        h.touch_mtime("nope".into(), 1_700_000_999).await.unwrap();
        let still = h.list_docs(10).await.unwrap();
        assert_eq!(still.len(), 1);
        assert_eq!(still[0].mtime_unix, Some(1_700_000_044));
    }

    /// TM-track — with metrics enabled the actor records per-`StorageKind`
    /// handler time. One op of each class lands in its slot.
    #[tokio::test]
    async fn enabled_metrics_record_per_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let lance = tmp.path().join("lance");
        let sqlite = tmp.path().join("index.db");
        let metrics = Arc::new(PipelineMetrics::new(true));
        let h = StorageActor::spawn_with_metrics(
            lance,
            sqlite,
            None,
            Arc::clone(&metrics),
            crate::storage::lance::LanceOptions::unbounded(),
        )
        .await
        .unwrap();

        h.upsert_doc(Doc::placeholder("docid", "/tmp/x.html"))
            .await
            .unwrap(); // Upsert
        h.ensure_fts_index().await.unwrap(); // Admin
        let _ = h.bm25_query("anything".into(), 5, false).await.unwrap(); // Query
        let _ = h.list_docs(10).await.unwrap(); // Read

        let snap = metrics.snapshot();
        let count_of = |k: &str| snap.storage.iter().find(|v| v.kind == k).unwrap().count;
        assert!(count_of("upsert") >= 1, "upsert recorded");
        assert!(count_of("admin") >= 1, "ensure_fts_index → admin recorded");
        assert!(count_of("query") >= 1, "bm25 → query recorded");
        assert!(count_of("read") >= 1, "list_docs → read recorded");
    }

    /// P1 — the index generation must bump on exactly the mutations that
    /// change what `list_docs` / `edge_counts` return (upsert, delete,
    /// record_edges, drop) and NOT on reads or on atlas-coord / embedding
    /// / compaction writes. The gallery row-set memo keys on this counter,
    /// so a missed bump serves stale rows and a spurious bump throws away
    /// a still-valid cache. Pins the bump set against accidental drift.
    // invariant:15 generation-bump
    #[tokio::test]
    async fn index_generation_bumps_only_on_row_set_and_edge_mutations() {
        let (h, _tmp) = handle().await;
        assert_eq!(h.index_generation(), 0, "fresh actor starts at 0");

        h.upsert_doc(Doc::placeholder("a", "/tmp/a.html"))
            .await
            .unwrap();
        let g1 = h.index_generation();
        assert_eq!(g1, 1, "upsert must bump");

        // Pure reads never bump.
        h.list_docs(u32::MAX).await.unwrap();
        h.list_notes(u32::MAX).await.unwrap();
        h.count_rows().await.unwrap();
        h.edge_counts().await.unwrap();
        assert_eq!(h.index_generation(), g1, "reads must not bump");

        // Edges back the gallery's backlink/outlink counts → bump.
        h.record_edges("a".into(), vec![("b".into(), "link".into())])
            .await
            .unwrap();
        let g2 = h.index_generation();
        assert_eq!(g2, g1 + 1, "record_edges must bump");

        // G8 — re-recording the IDENTICAL edge set is a no-op: the table is
        // unchanged, so the generation must NOT bump (else a links-unchanged
        // reindex would needlessly throw away a still-valid gallery cache).
        h.record_edges("a".into(), vec![("b".into(), "link".into())])
            .await
            .unwrap();
        assert_eq!(
            h.index_generation(),
            g2,
            "re-recording an identical edge set must NOT bump"
        );

        // Atlas coords, embedding clears, and compaction don't change the
        // gallery `list_docs` projection → must NOT bump (else every atlas
        // recompute would needlessly throw away the gallery cache).
        h.update_atlas(vec![("a".into(), 0.1, 0.2, 0)])
            .await
            .unwrap();
        h.clear_embeddings().await.unwrap();
        h.compact_all().await.unwrap();
        assert_eq!(
            h.index_generation(),
            g2,
            "atlas / clear_embeddings / compact must not bump"
        );

        // W1.B — atlas labels live in a sqlite side table, exactly like
        // `update_atlas`'s lance columns; writing them must not bump either
        // (invariant #15 — a label refresh must never discard a valid
        // gallery cache).
        h.set_atlas_labels(vec![AtlasLabelRow {
            cluster: 0,
            rank: 1,
            term: "test".into(),
            tf: 1.0,
            ft: 1.0,
            score: 0.5,
            computed_at: 1_700_000_000,
        }])
        .await
        .unwrap();
        assert_eq!(
            h.index_generation(),
            g2,
            "set_atlas_labels must not bump (invariant #15)"
        );

        // W3 T-a — atlas time-lapse frames (V0028) are sqlite side tables
        // too. Neither writing a frame nor pruning frames touches the lance
        // row-set, so neither may bump: a corpus time-lapse that discarded
        // every gallery + facets memo on each recompute would be a pure
        // regression (invariant #15).
        let frame_id = h
            .atlas_frame_insert(
                NewAtlasFrame {
                    created_at_unix: 1_700_000_000,
                    layout: "umap".into(),
                    provenance: crate::storage::sqlite::FrameProvenance::Recorded,
                },
                vec![AtlasFramePoint {
                    artifact_id: "a".into(),
                    x: 0.1,
                    y: 0.2,
                    cluster: 0,
                }],
            )
            .await
            .unwrap();
        assert!(frame_id.is_some(), "the first frame must land");
        assert_eq!(
            h.index_generation(),
            g2,
            "atlas_frame_insert must not bump (invariant #15)"
        );
        // Reads are reads.
        assert_eq!(h.atlas_frames(10).await.unwrap().len(), 1);
        assert_eq!(
            h.atlas_frame_points(frame_id.unwrap()).await.unwrap().len(),
            1
        );
        assert_eq!(h.index_generation(), g2, "frame reads must not bump");
        // And the prune (a DELETE) is still generation-neutral.
        assert_eq!(h.atlas_frames_prune(0).await.unwrap(), 1);
        assert_eq!(
            h.index_generation(),
            g2,
            "atlas_frames_prune must not bump (invariant #15)"
        );

        // Reading-list mutations live entirely in the sqlite side-channel —
        // the gallery `list_docs` projection is untouched, so NONE of them
        // may bump (a spurious bump would discard a valid gallery cache on
        // every list edit).
        let list = h
            .list_create(
                "l_pin".into(),
                "Pin test".into(),
                None,
                false,
                1_700_000_000,
            )
            .await
            .unwrap();
        let entry = h
            .list_entry_add(
                crate::lists::NewListEntry {
                    id: "le_pin".into(),
                    list_id: list.id.clone(),
                    kb: "canon".into(),
                    artifact_id: "abcdef012345".into(),
                    anchor_json: None,
                    note: None,
                    words: None,
                    read_override: None,
                },
                crate::lists::PositionSpec::Last,
                "operator".to_string(),
                1_700_000_001,
            )
            .await
            .unwrap();
        h.list_entry_update(
            entry.id.clone(),
            crate::lists::Patch::Set("note".into()),
            crate::lists::Patch::Keep,
            crate::lists::Patch::Keep,
            "operator".to_string(),
            1_700_000_002,
        )
        .await
        .unwrap();
        h.list_entry_move(
            list.id.clone(),
            entry.id.clone(),
            crate::lists::PositionSpec::First,
            1_700_000_003,
        )
        .await
        .unwrap();
        h.list_entries_sync_resolution(vec![crate::lists::ResolutionUpdate {
            entry_id: entry.id.clone(),
            anchor_stale: false,
            words: Some(10),
        }])
        .await
        .unwrap();
        h.list_import_entries(
            list.id.clone(),
            crate::lists::ImportMode::Append,
            vec![],
            "operator".to_string(),
            1_700_000_004,
        )
        .await
        .unwrap();
        h.list_entry_remove(list.id.clone(), entry.id.clone(), 1_700_000_005)
            .await
            .unwrap();
        h.lists_all().await.unwrap();
        h.list_entries_all().await.unwrap();
        h.list_delete(list.id.clone()).await.unwrap();
        assert_eq!(
            h.index_generation(),
            g2,
            "reading-list mutations must never bump (invariant #15)"
        );

        // Deletes bump.
        h.delete_by_path("/tmp/a.html".into()).await.unwrap();
        let g3 = h.index_generation();
        assert_eq!(g3, g2 + 1, "delete must bump");

        // Dropping all kb data bumps — and the bump fires the instant
        // lance is emptied (before the sqlite purge), so a torn drop can't
        // strand the gallery cache serving the dropped rows.
        h.upsert_doc(Doc::placeholder("c", "/tmp/c.html"))
            .await
            .unwrap();
        let g4 = h.index_generation();
        h.drop_kb_data().await.unwrap();
        assert_eq!(h.index_generation(), g4 + 1, "drop_kb_data must bump");
    }

    /// DCB W1.A — `code_refs` is a sibling side table the gallery never
    /// renders, so a code-ref write must be generation-NEUTRAL even when it
    /// genuinely changed the set. The contrast with `RecordEdges` (which DOES
    /// bump, because edges back the backlink/outlink counts) is the whole
    /// point: a corpus-wide reindex would otherwise discard the gallery memo
    /// once per doc for a signal it never shows.
    // invariant:2 no-generation-bump
    #[tokio::test]
    async fn record_code_refs_never_bumps_generation() {
        let (h, _tmp) = handle().await;
        h.upsert_doc(Doc::placeholder("a", "/tmp/a.html"))
            .await
            .unwrap();
        let g = h.index_generation();

        let header = CodeRefHeaderRow {
            artifact_id: "a".into(),
            doc_hash: "hash1".into(),
            extracted_at: 1_700_000_000,
            code_rev: None,
            ref_count: 1,
            group_count: 0,
            ungrouped_count: 1,
            truncated: false,
        };
        let row = CodeRefRow {
            ordinal: 0,
            kind: "path".into(),
            raw_text: "app/models/order.rb".into(),
            path_hint: Some("app/models/order.rb".into()),
            line_start: None,
            line_end: None,
            line_spans: None,
            symbol_container: None,
            symbol_member: None,
            context: String::new(),
            context_tokens: String::new(),
            group_key: None,
            group_label: None,
            group_anchor: None,
            declared: false,
        };
        assert!(
            h.record_code_refs(header.clone(), vec![row.clone()])
                .await
                .unwrap(),
            "first write changes the set"
        );
        assert_eq!(
            h.index_generation(),
            g,
            "a CHANGING code-ref write must not bump (invariant #2 / #15)"
        );
        // Reads are reads.
        let doc = h.code_refs_of("a".into()).await.unwrap().expect("scanned");
        assert_eq!(doc.refs, vec![row.clone()]);
        assert_eq!(h.code_refs_feed(None, 10, false).await.unwrap().len(), 1);
        assert_eq!(h.index_generation(), g, "code-ref reads must not bump");
        // And the unchanged path is a plain `false`, still no bump.
        assert!(!h.record_code_refs(header, vec![row]).await.unwrap());
        assert_eq!(h.index_generation(), g);
    }

    #[tokio::test]
    async fn delete_doc_removes_row() {
        let (h, _tmp) = handle().await;
        let doc = Doc::placeholder("a", "/tmp/a.html");
        h.upsert_doc(doc).await.unwrap();
        assert_eq!(h.count_rows().await.unwrap(), 1);
        h.delete_doc(ArtifactId::from_html_bytes(
            b"placeholder for id-equality test",
        ))
        .await
        .unwrap();
        // Different artifact id; deletion against unrelated id is a no-op.
        assert_eq!(h.count_rows().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn record_error_and_list() {
        let (h, _tmp) = handle().await;
        let slug = SourceSlug::from_path(std::path::Path::new("/tmp/canon"));
        h.upsert_source(slug.clone(), "/tmp/canon".into(), 1700000000)
            .await
            .unwrap();
        h.record_error(
            "parse".into(),
            slug.clone(),
            "/tmp/canon/bad.html".into(),
            "syntax".into(),
            Some("h1".into()),
            1700000100,
        )
        .await
        .unwrap();
        let errs = h.list_open_errors().await.unwrap();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].kind, "parse");
    }

    #[tokio::test]
    async fn cloned_handle_targets_same_actor() {
        let (h, _tmp) = handle().await;
        let h2 = h.clone();
        let slug = SourceSlug::from_path(std::path::Path::new("/tmp/canon"));
        h.upsert_source(slug.clone(), "/tmp/canon".into(), 1700000000)
            .await
            .unwrap();
        // The clone should see the source.
        assert_eq!(h2.list_sources().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn vector_query_round_trip() {
        let (h, _tmp) = handle().await;
        let mut doc = Doc::placeholder("vec1", "/tmp/x.html");
        doc.title = "alpha".into();
        doc.body = "first doc".into();
        // Toy embedding (matches Storage tests).
        doc.embedding = Some(toy_embed("alpha first doc"));
        h.upsert_doc(doc).await.unwrap();

        let qvec = toy_embed("alpha first doc");
        let hits = h.vector_query(qvec, 3).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "vec1");
    }

    #[tokio::test]
    async fn hybrid_query_round_trip() {
        let (h, _tmp) = handle().await;
        let mut doc = Doc::placeholder("h1", "/tmp/x.html");
        doc.title = "Borrow".into();
        doc.body = "lifetime conflicts".into();
        doc.embedding = Some(toy_embed("Borrow lifetime conflicts"));
        h.upsert_doc(doc).await.unwrap();
        h.ensure_fts_index().await.unwrap();

        let qvec = toy_embed("Borrow lifetime conflicts");
        let hits = h.hybrid_query("borrow".into(), qvec, 3).await.unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].id, "h1");
    }

    #[tokio::test]
    async fn ensure_vector_index_round_trip() {
        let (h, _tmp) = handle().await;
        // Empty table: must not error.
        h.ensure_vector_index().await.unwrap();
        // Idempotent.
        h.ensure_vector_index().await.unwrap();
    }

    #[tokio::test]
    async fn compact_all_round_trip() {
        // Drive enough upserts to give the optimize pass something to do.
        // Lance's compaction won't always coalesce a tiny dataset (rows
        // < threshold), so we assert on the no-error path plus on the
        // before/after fragment count shape from `dataset_stats`. The
        // important thing is the wire — actor msg → lance call → stats
        // back — not a specific minimum bytes_pruned.
        let (h, _tmp) = handle().await;
        for i in 0..12 {
            let mut doc = Doc::placeholder(format!("c{i}"), format!("/tmp/c{i}.html"));
            doc.title = format!("doc {i}");
            doc.body = format!("body content {i}");
            doc.embedding = Some(toy_embed(&format!("doc body {i}")));
            h.upsert_doc(doc).await.unwrap();
        }
        let before = h.dataset_stats().await.unwrap();
        assert_eq!(before.rows, 12);
        // Each upsert creates its own fragment, so we expect ≥ 12.
        assert!(
            before.fragments >= 12,
            "expected ≥12 fragments after 12 individual upserts, got {}",
            before.fragments
        );
        let stats = h.compact_all().await.unwrap();
        let after = h.dataset_stats().await.unwrap();
        // After OptimizeAction::All the dataset should hold the same
        // logical rows but with strictly fewer fragments than the
        // pre-compact count.
        assert_eq!(after.rows, 12);
        assert!(
            after.fragments < before.fragments,
            "expected compaction to reduce fragments ({} → {}); stats: {:?}",
            before.fragments,
            after.fragments,
            stats
        );
    }

    /// Off-loop compaction: messages fired while a compact is in flight must
    /// all complete — reads are serviced inline, lance writes are parked and
    /// replayed FIFO after the optimize — and the deferred write's effects
    /// (row visible, exactly one generation bump) match the serial order.
    #[tokio::test]
    async fn compact_in_flight_defers_writes_and_serves_reads() {
        let (h, _tmp) = handle().await;
        for i in 0..12 {
            let mut doc = Doc::placeholder(format!("d{i}"), format!("/tmp/d{i}.html"));
            doc.title = format!("doc {i}");
            doc.body = format!("body content {i}");
            doc.embedding = Some(toy_embed(&format!("doc body {i}")));
            h.upsert_doc(doc).await.unwrap();
        }
        let gen_before = h.index_generation();

        // Fire the compact WITHOUT awaiting it, then immediately a write and
        // a read. Whatever the interleave (the optimize may finish before the
        // sends land), every reply must arrive — no deadlock, no lost message
        // — and the final state must equal the serial order's.
        let hc = h.clone();
        let compact = tokio::spawn(async move { hc.compact_all().await });
        let hw = h.clone();
        let write = tokio::spawn(async move {
            let mut doc = Doc::placeholder("d12".to_string(), "/tmp/d12.html".to_string());
            doc.title = "doc 12".into();
            doc.body = "body content 12".into();
            doc.embedding = Some(toy_embed("doc body 12"));
            hw.upsert_doc(doc).await
        });
        // Reads must not park behind the in-flight compact.
        let seen = h.list_docs(u32::MAX).await.unwrap();
        assert!(seen.len() >= 12, "read during compact lost rows");

        write.await.unwrap().unwrap();
        compact.await.unwrap().unwrap();
        assert_eq!(h.count_rows().await.unwrap(), 13);
        assert_eq!(
            h.index_generation(),
            gen_before + 1,
            "the deferred upsert must bump exactly once; compact never bumps"
        );
        // The doc written during the compaction window is queryable after.
        assert!(h.get_by_id("d12".to_string()).await.unwrap().is_some());
    }

    fn toy_embed(text: &str) -> Vec<f32> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        text.hash(&mut h);
        let seed = h.finish();
        let mut state = seed.wrapping_add(0x9E3779B97F4A7C15);
        let mut out = Vec::with_capacity(384);
        for _ in 0..384 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let f = (state as i64 as f64) / (i64::MAX as f64);
            out.push(f as f32);
        }
        let norm: f32 = out.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        for v in &mut out {
            *v /= norm;
        }
        out
    }

    #[tokio::test]
    async fn history_open_then_scroll_round_trip_via_actor() {
        let (h, _tmp) = handle().await;
        let first = h
            .history_record_open("artA".into(), 1_700_000_000, None, "operator".into())
            .await
            .unwrap();
        assert_eq!(first.scroll_y, 0);
        assert!(first.is_new_visit);
        let n = h
            .history_update_scroll(first.id, 768, 2400, 1_700_000_010)
            .await
            .unwrap();
        assert_eq!(n, 1);
        // Within the visit window — resume returns the saved scroll.
        let resumed = h
            .history_record_open("artA".into(), 1_700_000_300, None, "operator".into())
            .await
            .unwrap();
        assert_eq!(resumed.id, first.id);
        assert_eq!(resumed.scroll_y, 768);
        assert!(!resumed.is_new_visit);

        let rows = h.history_list(10, None, None, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "open");
        assert_eq!(rows[0].scroll_y, 768);
    }

    #[tokio::test]
    async fn history_search_and_comment_via_actor() {
        let (h, _tmp) = handle().await;
        h.history_record_search("rust".into(), 1_700_000_000, "operator".into())
            .await
            .unwrap();
        h.history_record_comment(
            "artB".into(),
            "c-1".into(),
            1_700_000_050,
            "operator".into(),
        )
        .await
        .unwrap();
        let rows = h.history_list(10, None, None, None).await.unwrap();
        assert_eq!(rows.len(), 2);
        // Newest first.
        assert_eq!(rows[0].kind, "comment");
        assert_eq!(rows[1].kind, "search");
    }

    #[tokio::test]
    async fn shares_round_trip_via_actor() {
        let (h, _tmp) = handle().await;
        let row = ShareRow {
            name: "kb-share-z-1".into(),
            target: "research/z.html".into(),
            host: "cloudflare-pages".into(),
            deployed_url: "https://kb-share-z-1.pages.dev".into(),
            gate: Some("google".into()),
            cf_account_id: Some("acct".into()),
            pages_project: Some("kb-share-z-1".into()),
            cf_deployment_id: Some("dep".into()),
            access_app_id: Some("app".into()),
            access_policy_id: Some("pol".into()),
            github_repo: None,
            created_at_unix: 1_700_000_000,
            updated_at_unix: 1_700_000_000,
        };
        h.shares_insert(row.clone()).await.unwrap();
        let got = h.shares_get("kb-share-z-1".into()).await.unwrap().unwrap();
        assert_eq!(got, row);
        let by_target = h
            .shares_get_by_target("research/z.html".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(by_target.name, "kb-share-z-1");
        assert_eq!(h.shares_list().await.unwrap().len(), 1);
        assert_eq!(h.shares_delete("kb-share-z-1".into()).await.unwrap(), 1);
        assert!(h.shares_list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn shutdown_closes_actor() {
        let (h, _tmp) = handle().await;
        h.shutdown().await;
        // Allow the task to process Shutdown and exit.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Subsequent op fails because the receiver dropped.
        let result = h.list_sources().await;
        assert!(result.is_err());
    }

    /// N8: queue_depth() reflects pending channel slots. With the actor
    /// freshly spawned and idle, depth is 0. Capacity matches the exposed
    /// `CHANNEL_CAPACITY` const (surfaced as the metrics queue gauge).
    #[tokio::test]
    async fn queue_depth_reports_zero_on_idle_handle() {
        let (h, _tmp) = handle().await;
        // Let the actor settle.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(h.queue_depth(), 0, "idle actor has no pending messages");
        assert_eq!(StorageHandle::queue_capacity(), CHANNEL_CAPACITY);
    }

    #[tokio::test]
    async fn corkboard_round_trip_via_actor() {
        let (h, _tmp) = handle().await;
        assert_eq!(h.corkboard_count().await.unwrap(), 0);
        assert!(h.corkboard_add("abc".into(), 1_700_000_000).await.unwrap());
        // Idempotent: second add returns false; count stays at 1.
        assert!(!h.corkboard_add("abc".into(), 1_700_999_999).await.unwrap());
        assert!(h.corkboard_add("zzz".into(), 1_700_000_500).await.unwrap());
        assert_eq!(h.corkboard_count().await.unwrap(), 2);

        let list = h.corkboard_list().await.unwrap();
        // Newest first.
        assert_eq!(list[0].artifact_id, "zzz");
        assert_eq!(list[1].artifact_id, "abc");
        // Idempotency preserved the original created_at on `abc`.
        assert_eq!(list[1].created_at_unix, 1_700_000_000);

        assert!(h.corkboard_remove("abc".into()).await.unwrap());
        assert!(!h.corkboard_remove("abc".into()).await.unwrap());
        assert_eq!(h.corkboard_count().await.unwrap(), 1);
    }

    /// SC4 — the read-lane classifier is conservative: searches/gets/lists/
    /// counts + the search-support `Ensure*Index` trio are read-class;
    /// EVERYTHING else — every mutation, the WRITE arms of the mixed
    /// history/reading families, admin/meta, and control — is write-class,
    /// including the default fallthrough. A write misclassified as read could
    /// jump earlier writes and break write-FIFO, so this pins the boundary.
    #[test]
    fn read_lane_classification_is_conservative() {
        fn lane<T>(build: impl FnOnce(oneshot::Sender<Result<T>>) -> StorageMsg) -> bool {
            let (tx, _rx) = oneshot::channel::<Result<T>>();
            is_read_lane(&build(tx))
        }

        // Reads (read lane).
        assert!(lane(|reply| StorageMsg::Bm25Query {
            q: "x".into(),
            limit: 5,
            typo_tolerance: false,
            reply
        }));
        assert!(lane(|reply| StorageMsg::EnsureFtsIndex { reply }));
        assert!(lane(|reply| StorageMsg::EnsureVectorIndex { reply }));
        assert!(lane(|reply| StorageMsg::EnsureChunkVectorIndex { reply }));
        assert!(lane(|reply| StorageMsg::ListDocs { limit: 1, reply }));
        assert!(lane(|reply| StorageMsg::GetById {
            id: "a".into(),
            reply
        }));
        // MI-W2.4a — the lineage-walk reads.
        assert!(lane(|reply| StorageMsg::LineageById {
            id: "a".into(),
            reply
        }));
        assert!(lane(|reply| StorageMsg::FindSupersededBy {
            target_id: "a".into(),
            reply
        }));
        // W2.3a — the true-neighbors route's embedding lookups.
        assert!(lane(|reply| StorageMsg::EmbeddingById {
            id: "a".into(),
            reply
        }));
        assert!(lane(|reply| StorageMsg::EmbeddingsByIds {
            ids: vec!["a".into()],
            reply
        }));
        // W2.11 — the prompt-browse route's read.
        assert!(lane(|reply| StorageMsg::PromptById {
            id: "a".into(),
            reply
        }));
        assert!(lane(|reply| StorageMsg::CountRows { reply }));
        assert!(lane(|reply| StorageMsg::BacklinksOf {
            id: "a".into(),
            reply
        }));
        assert!(lane(|reply| StorageMsg::HistoryList {
            limit: 1,
            before_unix: None,
            kind_filter: None,
            user: None,
            reply
        }));
        // W2.10 — the calendar's day/kind aggregate is a read too.
        assert!(lane(|reply| StorageMsg::HistoryCountsByDay {
            from_unix: 0,
            to_unix: 1,
            reply
        }));
        // A reading READ that from_msg's History arm does NOT list (it falls to
        // StorageKind::Read) — must still be read-class here.
        assert!(lane(|reply| StorageMsg::ReadingRollup {
            user: "operator".into(),
            reply
        }));
        assert!(lane(|reply| StorageMsg::SessionsGet {
            session_id: "s".into(),
            reply
        }));
        assert!(lane(|reply| StorageMsg::SessionsGetByArtifactIds {
            artifact_ids: vec!["a".into()],
            reply
        }));
        assert!(lane(|reply| StorageMsg::ListsAll { reply }));
        assert!(lane(|reply| StorageMsg::GetAtlasLabels { reply }));
        // W3 T-a — the atlas time-lapse READS (V0028 frame list + points).
        assert!(lane(|reply| StorageMsg::AtlasSnapshotsList {
            limit: 1,
            reply
        }));
        assert!(lane(|reply| StorageMsg::AtlasSnapshotPoints {
            snapshot_id: 1,
            reply
        }));
        // MI-W1.2/W1.3 — the memory-recall ledger's aggregate read.
        assert!(lane(|reply| StorageMsg::MemoryRecallsCountsForIds {
            memory_kb: None,
            memory_ids: vec!["a".into()],
            reply
        }));
        assert!(lane(|reply| StorageMsg::MemoryRecallsForSession {
            session_id: "s".into(),
            reply
        }));
        // MI-W4.2a — the per-week histogram sibling read.
        assert!(lane(|reply| StorageMsg::MemoryRecallsWeeklyForIds {
            memory_kb: None,
            memory_ids: vec!["a".into()],
            now_unix: 0,
            reply
        }));
        // CT-B2 — the memory-side reverse read.
        assert!(lane(|reply| StorageMsg::MemoryRecallsForMemory {
            memory_kb: "notes".into(),
            memory_id: "a".into(),
            limit: 50,
            reply
        }));
        // CT-F1 — the memory<->commit exact-id read.
        assert!(lane(|reply| StorageMsg::MemoryCommitsForMemory {
            memory_id: "a".into(),
            limit: 50,
            reply
        }));

        // Writes (write lane).
        assert!(!lane(|reply| StorageMsg::UpsertDoc {
            doc: Box::new(Doc::placeholder("a", "/a.html")),
            reply
        }));
        assert!(!lane(|reply| StorageMsg::DeleteByPath {
            path: "/a.html".into(),
            reply
        }));
        // Mixed-family WRITES: history/reading writes are NOT read-class.
        assert!(!lane(|reply| StorageMsg::HistoryRecordOpen {
            artifact_id: "a".into(),
            now_unix: 0,
            source: None,
            user: "operator".into(),
            reply
        }));
        assert!(!lane(|reply| StorageMsg::ReadingSetActive {
            visit_id: 1,
            active_ms: 0,
            last_section: None,
            now_unix: 0,
            reply
        }));
        assert!(!lane(|reply| StorageMsg::RecordEdges {
            from_id: "a".into(),
            to_kinds: vec![],
            reply
        }));
        // DCB W1.A — the code-ref WRITE stays write-lane (conservative
        // default); both READS are read-lane.
        assert!(!lane(|reply| StorageMsg::RecordCodeRefs {
            header: Box::new(CodeRefHeaderRow {
                artifact_id: "a".into(),
                doc_hash: "h".into(),
                extracted_at: 0,
                code_rev: None,
                ref_count: 0,
                group_count: 0,
                ungrouped_count: 0,
                truncated: false,
            }),
            refs: vec![],
            reply
        }));
        assert!(lane(|reply| StorageMsg::CodeRefsOf {
            artifact_id: "a".into(),
            reply
        }));
        assert!(lane(|reply| StorageMsg::CodeRefsFeed {
            after: None,
            limit: 10,
            with_refs: false,
            reply
        }));
        assert!(lane(|reply| StorageMsg::CodeRefsByTarget {
            path: "a.rb".into(),
            with_refs: false,
            reply
        }));
        assert!(!lane(|reply| StorageMsg::TouchMtime {
            id: "a".into(),
            mtime_unix: 0,
            reply
        }));
        assert!(!lane(|reply| StorageMsg::UpdateAtlas {
            rows: vec![],
            reply
        }));
        assert!(!lane(|reply| StorageMsg::SetAtlasLabels {
            labels: vec![],
            reply
        }));
        // W3 T-a — the atlas time-lapse WRITES. Sqlite-only, but still
        // write-class: pulling a frame insert ahead of the `UpdateAtlas`
        // write it follows would break write-FIFO.
        assert!(!lane(|reply| StorageMsg::AtlasSnapshotInsert {
            frame: Box::new(NewAtlasFrame {
                created_at_unix: 0,
                layout: "umap".into(),
                provenance: crate::storage::sqlite::FrameProvenance::Recorded,
            }),
            points: vec![],
            reply
        }));
        assert!(!lane(|reply| StorageMsg::AtlasSnapshotPrune {
            keep: 1,
            reply
        }));
        assert!(!lane(|reply| StorageMsg::CompactAll { reply }));
        // MI-W1.1 — the memory-recall ledger's write is write-class too.
        assert!(!lane(|reply| StorageMsg::MemoryRecallsReplace {
            artifact_id: "cap-1".into(),
            rows: vec![],
            reply
        }));
        // CT-F1 — so is the memory<->commit ledger's.
        assert!(!lane(|reply| StorageMsg::MemoryCommitsReplace {
            artifact_id: "cap-1".into(),
            rows: vec![],
            reply
        }));
        // Control + the test-only panic default to write.
        assert!(!is_read_lane(&StorageMsg::Shutdown));
        assert!(!lane(|reply| StorageMsg::PanicForTest { reply }));
    }

    /// SC4 (1) — a foreground read must not park behind a bulk-ingest write
    /// backlog on the same actor. We stage a synthetic 500-deep write backlog
    /// (each write occupies the actor 1ms, standing in for slow ingest), then
    /// fire two probes against it: the priority read (read lane) and a baseline
    /// write enqueued AFTER the 500 (position 501 — the latency a read would
    /// suffer under the old single FIFO queue). The read must beat that
    /// baseline by at least an order of magnitude. Both are measured in-test.
    #[tokio::test]
    async fn read_jumps_ingest_backlog_by_an_order_of_magnitude() {
        let (h, _tmp) = handle().await;
        h.upsert_doc(Doc::placeholder("seed", "/tmp/seed.html"))
            .await
            .unwrap();

        // Stage the backlog with NOTHING draining yet: the current-thread test
        // runtime can't run the actor task while this loop never awaits.
        const BACKLOG: usize = 500;
        let mut staged = Vec::with_capacity(BACKLOG);
        for _ in 0..BACKLOG {
            staged.push(h.send_no_wait(|reply| StorageMsg::SlowWriteForTest { ms: 1, reply }));
        }
        assert_eq!(
            h.queue_depth(),
            BACKLOG,
            "the 500 writes are staged in the write lane, undrained"
        );

        let hp = h.clone();
        let prio = async move {
            let t = Instant::now();
            hp.list_docs(10).await.unwrap();
            t.elapsed()
        };
        let hb = h.clone();
        let base = async move {
            let t = Instant::now();
            // A write enqueued AFTER the 500 → drains at position 501, the
            // latency a read would suffer under the old single FIFO queue.
            hb.send_and_await(|reply| StorageMsg::SlowWriteForTest { ms: 1, reply })
                .await
                .unwrap();
            t.elapsed()
        };
        let (prio_lat, base_lat) = tokio::join!(prio, base);
        assert!(
            prio_lat.as_micros() * 10 < base_lat.as_micros(),
            "read jumped the backlog: read {prio_lat:?} must be <10% of the \
             backlog-drain baseline {base_lat:?}"
        );
        // Keep the staged receivers alive until here so the actor's replies
        // aren't dropped mid-drain (handlers still run regardless).
        drop(staged);
    }

    /// SC4 (2) — writes stay FIFO among themselves even while reads jump the
    /// queue. We interleave a priority read before each append to the same
    /// list; the read-lane priority must never reorder the writes, so the
    /// entries land in exact append order.
    #[tokio::test]
    async fn write_lane_preserves_fifo_under_interleaved_reads() {
        let (h, _tmp) = handle().await;
        let list = h
            .list_create("L".into(), "L".into(), None, false, 1_700_000_000)
            .await
            .unwrap();

        const N: usize = 20;
        let mut entry_rxs = Vec::with_capacity(N);
        for i in 0..N {
            // A read jumps ahead (dropped — we don't need its reply)…
            let _read = h.send_no_wait(|reply| StorageMsg::CountRows { reply });
            drop(_read);
            // …but this append must keep its FIFO slot among the writes.
            let entry = crate::lists::NewListEntry {
                id: format!("e{i:02}"),
                list_id: list.id.clone(),
                kb: "canon".into(),
                artifact_id: format!("aid{i}"),
                anchor_json: None,
                note: None,
                words: None,
                read_override: None,
            };
            entry_rxs.push(h.send_no_wait(|reply| StorageMsg::ListEntryAdd {
                entry,
                pos: crate::lists::PositionSpec::Last,
                now_unix: 1_700_000_100 + i as i64,
                user: "operator".into(),
                reply,
            }));
        }
        for rx in entry_rxs {
            rx.await.unwrap().unwrap();
        }

        let entries = h.list_entries_for_list(list.id.clone()).await.unwrap();
        let got: Vec<String> = entries.into_iter().map(|e| e.id).collect();
        let expected: Vec<String> = (0..N).map(|i| format!("e{i:02}")).collect();
        assert_eq!(
            got, expected,
            "appends must land in FIFO order despite interleaved priority reads"
        );
    }

    /// SC4 (3) — the write-starvation bound holds: a relentless read stream
    /// cannot defer a waiting write forever. One write (an upsert taking the
    /// row count 1→2) is staged behind 2×BOUND reads. `count_rows` reads that
    /// run before the write observe 1, those after observe 2. The fairness
    /// counter is reset to 0 by the seed write (the last op before staging, so
    /// no stray read bumps it), so exactly `READ_STARVATION_BOUND` reads are
    /// served before the write is forced — and at least one runs first, so the
    /// earlier-staged write really was overtaken (read priority, bounded).
    #[tokio::test]
    async fn write_starvation_is_bounded() {
        let (h, _tmp) = handle().await;
        // Seed one row. This is a WRITE and the LAST op before staging, so the
        // actor's persistent read counter is 0 when the batch below drains
        // (a read here — e.g. a count_rows sanity check — would bump it).
        h.upsert_doc(Doc::placeholder("seed", "/tmp/seed.html"))
            .await
            .unwrap();

        let k = READ_STARVATION_BOUND as usize;
        // Stage the single write FIRST, then 2K reads — all before any drain
        // (current-thread runtime + no awaits ⇒ the actor stays parked).
        let write_rx = h.send_no_wait(|reply| StorageMsg::UpsertDoc {
            doc: Box::new(Doc::placeholder("second", "/tmp/second.html")),
            reply,
        });
        let mut read_rxs = Vec::with_capacity(2 * k);
        for _ in 0..(2 * k) {
            read_rxs.push(h.send_no_wait(|reply| StorageMsg::CountRows { reply }));
        }

        write_rx.await.unwrap().unwrap();
        let mut counts = Vec::with_capacity(2 * k);
        for rx in read_rxs {
            counts.push(rx.await.unwrap().unwrap());
        }
        let before = counts.iter().filter(|&&c| c == 1).count();
        let after = counts.iter().filter(|&&c| c == 2).count();
        assert!(
            (1..=k).contains(&before),
            "read priority + bounded starvation: 1..={k} reads precede the \
             earlier-staged write, got {before}"
        );
        assert_eq!(
            before, k,
            "exactly READ_STARVATION_BOUND reads run before the starved write is forced"
        );
        assert_eq!(after, k, "the remaining reads run after the write lands");
    }
}
