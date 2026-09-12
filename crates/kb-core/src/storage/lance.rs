//! Per-kb lance dataset wrapper. Provides BM25 query (no vector path in
//! v0.0.1 since the embedding column stays nullable + empty). Schema lives
//! in the sibling `schema` module.
//!
//! lancedb features deliberately disabled (`default-features = false`) —
//! kb is local-only, no aws/gcs/azure/dynamodb/oss.

use crate::storage::schema::{
    chunk_schema, chunks_to_batches, docs_to_batches, schema, ChunkDoc, Doc,
};
use crate::Result;
use arrow::array::{Array, AsArray, BooleanArray};
use futures::TryStreamExt;
use lance_index::scalar::{
    inverted::query::{FtsQuery, MatchQuery, MultiMatchQuery},
    FullTextSearchQuery,
};
use lancedb::{
    connect,
    index::{
        scalar::{BTreeIndexBuilder, FtsIndexBuilder},
        vector::IvfPqIndexBuilder,
        Index, IndexType,
    },
    query::{ExecutableQuery, QueryBase, Select},
    table::{Duration, NewColumnTransform, OptimizeAction, Table as LanceTable},
    Connection,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const TABLE_NAME: &str = "artifacts";

/// SQ5 — sibling table holding per-passage embeddings (see `crate::chunk`).
const CHUNK_TABLE_NAME: &str = "artifact_chunks";

/// FTS-indexed text columns. Topic 01 §B BM25 over title/body/headings/code/prompt.
const FTS_COLUMNS: &[&str] = &["title", "body", "headings", "code", "prompt"];

/// Columns that get a scalar BTree index so exact-match filters seek
/// instead of scanning the whole table: `path` (used by
/// `get_by_source_path` for cross-artifact link resolution) and
/// `kb_session` (used by the `/api/sessions/*` filters
/// `count_docs_with_kb_session` / `list_docs_with_kb_session`). Both are
/// tolerant of empty/all-null tables (lance refuses to index those; we
/// skip and a later post-data pass retries once rows land). Built once
/// per process (see `Storage::scalar_indexes_ready`); rows added after
/// the build are found via lance's scan fallback until `compact_all`
/// (`OptimizeAction::All`, run by the startup heuristic) reconciles it.
const SCALAR_INDEX_COLUMNS: &[&str] = &["path", "kb_session"];

/// (id, embedding) pair returned by `Storage::list_embeddings`. The embedding
/// width matches the kb's configured model dim — 384 for bge-small, 768 for
/// bge-base, 1024 for bge-large. Callers read width from the data, not from a
/// const.
pub type EmbeddingPair = (String, Vec<f32>);

/// Per-kb lance tuning, resolved by the daemon from `[storage]` in kb.toml
/// (`StorageSection`) and handed to [`Storage::open_with_options`]. All three
/// knobs share one opt-out convention: `0` restores the pre-knob lance
/// behavior (uncapped caches / rebuild-on-every-dirty).
///
/// Why the cache caps exist: lance 11.0.0's `Session` defaults are
/// byte-weighted caches of 6 GiB (index) + 1 GiB (metadata) PER
/// connection (`lance::dataset::DEFAULT_INDEX_CACHE_SIZE` /
/// `DEFAULT_METADATA_CACHE_SIZE`, dataset.rs:179/183), held for the table's lifetime — on a
/// multi-kb daemon with a churny corpus this alone grew the resident heap to
/// ~10 GB. The shipped defaults (256 / 64 MiB) cap that per kb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanceOptions {
    /// Cap for lance's per-connection index cache, in MiB. `0` → lance's own
    /// default (6 GiB).
    pub index_cache_mb: u64,
    /// Cap for lance's per-connection metadata cache, in MiB. `0` → lance's
    /// own default (1 GiB).
    pub metadata_cache_mb: u64,
    /// Minimum seconds between search-triggered FTS rebuilds, and
    /// independently between vector rebuilds. The dirty flags stay the
    /// trigger; this only rate-limits the `replace=true` full retrains. The
    /// first build after open is always immediate. `0` → rebuild whenever
    /// dirty (pre-throttle behavior).
    pub index_rebuild_min_secs: u64,
}

impl Default for LanceOptions {
    /// Shipped production defaults — capped caches + a 5-minute rebuild
    /// throttle. Mirrors `StorageSection::DEFAULT_*`; the daemon resolves
    /// those and constructs this explicitly, so this `Default` is only a
    /// convenience for callers that want the production shape without
    /// parsing kb.toml.
    fn default() -> Self {
        Self {
            index_cache_mb: 256,
            metadata_cache_mb: 64,
            index_rebuild_min_secs: 300,
        }
    }
}

impl LanceOptions {
    /// Pre-knob behavior: lance's own cache sizes and a rebuild on every
    /// dirty flag. `Storage::open` keeps this so the existing call sites
    /// (short-lived CLI reads, the storage test suite) behave byte-for-byte
    /// as before; the daemon path resolves `[storage]` and calls
    /// [`Storage::open_with_options`].
    pub fn unbounded() -> Self {
        Self {
            index_cache_mb: 0,
            metadata_cache_mb: 0,
            index_rebuild_min_secs: 0,
        }
    }
}

/// Escape a value for safe interpolation into a lance filter *string
/// literal*. Lance's `only_if` / `delete` / `count_rows` predicate grammar
/// is SQL-flavoured, so a stray single quote in content- or
/// operator-derived data (an artifact path or wikilink id containing an
/// apostrophe, a caller-supplied `kb_session` id) would terminate the
/// literal early and — untreated — corrupt the predicate or inject filter
/// syntax. The only metacharacter inside a single-quoted SQL literal is the
/// quote itself; doubling it (`'` → `''`) is the standard escape.
///
/// This is the ONE place quote-escaping happens: every predicate that
/// interpolates a literal MUST route the value through here — directly for
/// compound (`… AND …`) or `IN (…)` predicates, or via [`filter_eq`] for the
/// common `col = 'literal'` shape — so a future call site can't forget it.
/// (The `get_by_id`/`get_by_ids` fast paths instead reject any non-`[A-Za-z0-9-]`
/// id up front, so no escaping is needed there — a deliberate allowlist.)
fn escape_literal(s: &str) -> String {
    s.replace('\'', "''")
}

/// Build an exact-match lance predicate `col = '<escaped>'`, escaping `val`
/// through [`escape_literal`]. `col` is always a hardcoded column name (never
/// caller input), so it is interpolated verbatim. Compound predicates and
/// `IN (…)` lists call [`escape_literal`] directly instead.
fn filter_eq(col: &str, val: &str) -> String {
    format!("{col} = '{}'", escape_literal(val))
}

/// Fix 3 — strict `_indices/<uuid>` dir-name check for the orphan GC: lance
/// names each index dir after its uuid (`8-4-4-4-12` hex). Anything that
/// doesn't match this shape EXACTLY is never deleted, no matter what.
fn is_uuid_dir_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, &b) in bytes.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if b != b'-' {
                    return false;
                }
            }
            _ => {
                if !b.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

/// Fix 3 — total size of a directory tree (best-effort: unreadable entries
/// count as 0). Used only for the orphan GC's reclaimed-bytes log line.
fn dir_size_bytes(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Per-kb lance handle. Wraps the connection + opened artifacts table. Each
/// kb captures its own embedding dim at `open` time — either from an existing
/// dataset's `embedding` `FixedSizeList<_, N>` field or from the kb's
/// configured `embedding_model` (resolved via `kb_core::embed::model_info`).
pub struct Storage {
    conn: Connection,
    table: LanceTable,
    dim: i32,
    /// Tuning knobs this Storage was opened with (cache caps + rebuild
    /// throttle) — see [`LanceOptions`].
    opts: LanceOptions,
    /// The lance DB directory passed to `open` (the dir holding
    /// `<table>.lance/`), kept for the orphan `_indices` GC (Fix 3) which
    /// walks the table dirs on disk.
    base_path: PathBuf,
    /// Set once the scalar BTree indexes (`SCALAR_INDEX_COLUMNS`) have been
    /// built for this process, so `ensure_scalar_indexes` (reached from the
    /// per-request `ensure_fts_index`) doesn't re-issue a `replace=true`
    /// rebuild on every search/recall. Rows added after the build are still
    /// found via lance's scan fallback on the unindexed delta; the startup
    /// `compact_all` (`OptimizeAction::All`) reconciles the index.
    scalar_indexes_ready: std::sync::atomic::AtomicBool,
    /// perf — `false` once `ensure_fts_index` has (re)built the FTS inverted
    /// indexes for the CURRENT row-set; set back to `true` by any mutation
    /// that changes FTS-indexed content (upsert / delete). lancedb's
    /// `create_index` defaults to `replace=true` — a full re-train across all
    /// `FTS_COLUMNS` (~hundreds of ms on a real corpus) — so WITHOUT this gate
    /// every search + recall paid a full FTS rebuild, which measured as the
    /// dominant search latency (≈500 ms). Freshness no longer depends on the
    /// rebuild having happened: lance unions the index with a flat BM25 scan
    /// over fragments newer than it (see the Fix 2 comment in
    /// `ensure_fts_index`), which is also what makes the
    /// `index_rebuild_min_secs` throttle safe.
    fts_needs_build: std::sync::atomic::AtomicBool,
    /// Same gate for the IVF-PQ vector index. Dirtied by upsert / delete /
    /// `clear_embeddings`. Vector queries brute-force-scan when no index is
    /// built, so a too-small kb stays correct between rebuilds.
    vector_needs_build: std::sync::atomic::AtomicBool,
    /// Count of actual FTS / vector index rebuilds since open. Proves the gate
    /// skips the rebuild on an unchanged row-set (tests) and is a cheap
    /// "index churn" observability hook.
    fts_build_count: std::sync::atomic::AtomicU64,
    vector_build_count: std::sync::atomic::AtomicU64,
    /// Fix 2 — last successful FTS / vector rebuild, gating the
    /// `index_rebuild_min_secs` throttle. `None` until the first build after
    /// open (which is always immediate). `Mutex<Option<Instant>>` because the
    /// file's atomic style can't hold an `Instant`; the lock is only ever
    /// held to read/replace the value, never across `.await` (#15).
    last_fts_build: std::sync::Mutex<Option<std::time::Instant>>,
    last_vector_build: std::sync::Mutex<Option<std::time::Instant>>,
    /// SQ5 — sibling table for passage embeddings, with its OWN IVF-PQ
    /// dirty gate so chunk writes never rebuild the doc-vector index (and
    /// vice-versa). Always opened; empty until a kb opts into chunking.
    chunk_table: LanceTable,
    chunk_vector_needs_build: std::sync::atomic::AtomicBool,
    chunk_vector_build_count: std::sync::atomic::AtomicU64,
    /// GC-B2 — typed-decode skip observability. `batches_to_summaries` /
    /// `batches_to_embeddings` warn+continue on a malformed batch (R1, commit
    /// 2ff019da) rather than panicking or erroring the whole read — a
    /// deliberate silent-shrinkage trade-off that had NO metric until now.
    decode_skips: DecodeSkipCounter,
}

/// GC-B2 — per-kb counter of typed-decode skips, bundled with a "have we
/// warned yet" latch. The raw count is always bumped (surfaced via
/// `Storage::decode_skip_count` as `decode_skips` in `/api/stats`); the WARN
/// fires only once per `Storage` instance (i.e. once per kb per process
/// lifetime, since a kb's `Storage` is opened once at boot/restart) so a
/// burst of malformed batches doesn't spam the log per-batch.
#[derive(Debug, Default)]
struct DecodeSkipCounter {
    count: std::sync::atomic::AtomicU64,
    warned: std::sync::atomic::AtomicBool,
}

impl DecodeSkipCounter {
    /// Bump the counter and, the first time it goes nonzero for this
    /// instance, log one WARN carrying `detail` (the specific decode
    /// failure). Every subsequent skip is counted silently.
    fn note(&self, detail: &str) {
        use std::sync::atomic::Ordering;
        self.count.fetch_add(1, Ordering::Relaxed);
        if !self.warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "typed arrow decode: first row/batch decode skip observed for this kb \
                 this process lifetime ({detail}); further skips are counted but not \
                 logged individually — see decode_skips in this kb's /api/stats",
            );
        }
    }
}

impl Storage {
    /// Embedding dimension this kb was opened at. Used by `upsert_docs` to
    /// validate incoming `Doc::embedding` widths against the on-disk schema.
    pub fn dim(&self) -> i32 {
        self.dim
    }

    /// Mark BOTH search indexes stale after a row-set change, so the next
    /// `ensure_fts_index` / `ensure_vector_index` rebuilds. Called by every
    /// content mutation (upsert / delete). Atlas-coord writes do NOT call this
    /// — they don't touch FTS columns or the embedding, so search is unaffected.
    fn mark_search_indexes_dirty(&self) {
        use std::sync::atomic::Ordering;
        self.fts_needs_build.store(true, Ordering::Relaxed);
        self.vector_needs_build.store(true, Ordering::Relaxed);
    }

    /// Fix 2 — true when a dirty-triggered rebuild is suppressed by the
    /// `index_rebuild_min_secs` throttle: a build has already happened this
    /// process AND less than the configured interval has elapsed since. The
    /// first build after open (`None`) and `index_rebuild_min_secs == 0`
    /// (opt-out) are never throttled. A throttled rebuild leaves the dirty
    /// flag SET, so the first ensure call after the window rebuilds.
    fn rebuild_throttled(&self, last_build: &std::sync::Mutex<Option<std::time::Instant>>) -> bool {
        if self.opts.index_rebuild_min_secs == 0 {
            return false;
        }
        let last = last_build.lock().unwrap_or_else(|e| e.into_inner());
        match *last {
            None => false,
            Some(t) => {
                t.elapsed() < std::time::Duration::from_secs(self.opts.index_rebuild_min_secs)
            }
        }
    }

    /// Number of real FTS index rebuilds since open (0 when every search hit
    /// the clean-skip fast path). Observability + test hook.
    pub fn fts_rebuild_count(&self) -> u64 {
        self.fts_build_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Number of real vector (IVF-PQ) index rebuilds since open.
    pub fn vector_rebuild_count(&self) -> u64 {
        self.vector_build_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// GC-B2 — number of typed-decode skips (malformed/short-column batches
    /// dropped by `batches_to_summaries`/`batches_to_embeddings`) since open.
    /// Observability for otherwise-silent result-set shrinkage.
    pub fn decode_skip_count(&self) -> u64 {
        self.decode_skips
            .count
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DocSummary {
    pub id: String,
    pub title: String,
    pub path: String,
    pub kb_category: Option<String>,
    /// v0.3 — atlas coordinates. `None` when the row hasn't been
    /// included in an atlas recompute yet OR when the caller asked
    /// for the slim shape (`include_atlas = false`).
    pub atlas_x: Option<f32>,
    pub atlas_y: Option<f32>,
    pub atlas_cluster: Option<i16>,
    /// v0.6 B1 — time fields lifted directly from the indexed row.
    /// Both unix seconds; both populated on every doc the indexer
    /// has touched. Used by the SPA card's age + isNew dot.
    pub mtime_unix: Option<i64>,
    pub indexed_at_unix: Option<i64>,
    /// v0.6 B1 — short body excerpt the parser emits. Used as the
    /// SPA card summary line.
    pub summary: Option<String>,
    /// v0.6 B1 — capability indicators. All already in the parser;
    /// just plumbing through to the docs API.
    pub svg_count: Option<u32>,
    pub has_canvas: Option<bool>,
    pub has_form: Option<bool>,
    pub has_animation: Option<bool>,
    pub has_details: Option<bool>,
    pub has_math: Option<bool>,
    pub has_drag: Option<bool>,
    /// LoC bucket strings ("static" / "lo" / "med" / "hi"). The SPA
    /// rolls js_loc + has_canvas + has_form into the "interactive"
    /// glyph; css_loc currently isn't surfaced in the card but is
    /// included so the same DocSummary shape covers the detail
    /// view's needs in a follow-up phase.
    pub js_loc: Option<String>,
    pub css_loc: Option<String>,
    /// v0.6 I1 — counters that drive the gallery card glyph strip.
    /// All four are None on rows indexed before the migration ran.
    pub table_count: Option<u32>,
    pub code_block_count: Option<u32>,
    pub word_count: Option<u32>,
    pub longread: Option<bool>,
    /// v0.6 T1 — tag slugs. Empty Vec when the row was indexed
    /// before the migration ran (the SPA falls back to path-derived
    /// tags via `web/src/lib/derive.ts`).
    pub tags: Vec<String>,
    /// v0.7 S1 — status from `<meta name="kb-status">`. None when
    /// absent on the artifact OR when the row was indexed before
    /// the v0.7 columns were added.
    pub kb_status: Option<String>,
    /// v0.7 S1 — severity from `<meta name="kb-severity">`. Same
    /// nullability semantics as `kb_status`.
    pub kb_severity: Option<String>,
    /// v0.9 M1 — memory salience in 0..1. None unless the caller's
    /// projection selected `kb_salience` AND the row carries one.
    /// Read by `memory::rerank`.
    pub kb_salience: Option<f32>,
    /// v0.9 M1 — memory recency fade rate ("slow" | "fast"). Same
    /// projection-dependent nullability as `kb_salience`.
    pub kb_decay: Option<String>,
    /// v0.9 M1 — artifact id this memory supersedes. Same
    /// projection-dependent nullability.
    pub kb_supersedes: Option<String>,
    /// v0.14 S1 — Claude Code session id that produced this memory.
    /// Same projection-dependent nullability as the other memory metas;
    /// None unless the caller selected `kb_session` AND the row carries
    /// one.
    pub kb_session: Option<String>,
    /// RA4 — one-line memory summary distinct from the title, from
    /// `<meta name="kb-summary">`. Same opt-in projection nullability;
    /// None unless the caller selected `kb_summary` AND the row carries one.
    pub kb_summary: Option<String>,
    /// v0.15 — filesystem birth time (btime) of the source file, captured
    /// at index time. Same opt-in projection nullability as the other
    /// columns: present only when the caller selected `created_unix`.
    /// Always `None` for rows on btime-less filesystems or indexed
    /// before the v15 migration.
    pub created_unix: Option<i64>,
    /// N-track — GFM task-list progress. Present only when the caller's
    /// projection selected `task_done`/`task_total` (the notes list path);
    /// `None` for other projections or rows indexed before the v17 migration.
    pub task_done: Option<u32>,
    pub task_total: Option<u32>,
    /// SQ1 — relevance score for a ranked search hit (higher = more
    /// relevant). Opt-in like the columns above: populated only when the
    /// lance query projected a score column — `_relevance_score` (hybrid),
    /// `_score` (BM25), or `_distance` (vector, mapped to a similarity).
    /// `None` for list / gallery / get projections, which don't rank.
    pub score: Option<f32>,
    /// MI-W3.3a — optional CoALA-minimal type classification, projected
    /// opt-in like the other memory metas (present only when the caller's
    /// projection selected `kb_memory_type` AND the row carries one).
    pub kb_memory_type: Option<String>,
    /// MI-W3.4 — write-time trust tag, same opt-in projection nullability.
    pub kb_source: Option<String>,
    /// CT-A1 (U3 parse-back) — the `you`/`claude` role the memory was
    /// highlighted under, from `kb_author`. Same opt-in projection
    /// nullability as the other memory metas.
    pub kb_author: Option<String>,
    /// CT-A1 — kb name of the artifact this memory was highlighted FROM.
    /// Same opt-in projection nullability.
    pub kb_source_kb: Option<String>,
    /// CT-A1 — artifact id of that origin artifact. Same opt-in projection
    /// nullability.
    pub kb_source_artifact: Option<String>,
    /// CT-A1 — the origin selection's `review::Anchor` JSON text (via
    /// `lists::anchor_to_json`). Same opt-in projection nullability.
    pub kb_source_anchor: Option<String>,
}

/// Result of `Storage::compact_all`. Mirrors the bits of lancedb's
/// `OptimizeStats` we care about, projected into a Serialize-friendly
/// shape so the HTTP route + CLI can render it without pulling
/// lancedb types into the boundary.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CompactStats {
    /// Number of small data fragments merged into larger ones. Zero
    /// when the dataset was already compact.
    pub fragments_removed: u64,
    /// Number of new (larger) data fragments produced by the merge.
    pub fragments_added: u64,
    /// Files unlinked as part of compaction (covers the per-fragment
    /// data files lance rewrote).
    pub files_removed: u64,
    /// Bytes freed by the old-version prune step. Zero when no
    /// versions were eligible (e.g. the default 7-day window).
    pub bytes_pruned: u64,
    /// Number of old manifest versions removed by the prune step.
    pub old_versions_removed: u64,
}

impl From<lancedb::table::OptimizeStats> for CompactStats {
    fn from(s: lancedb::table::OptimizeStats) -> Self {
        let mut out = CompactStats::default();
        if let Some(c) = s.compaction {
            out.fragments_removed = c.fragments_removed as u64;
            out.fragments_added = c.fragments_added as u64;
            out.files_removed = c.files_removed as u64;
        }
        if let Some(p) = s.prune {
            out.bytes_pruned = p.bytes_removed;
            out.old_versions_removed = p.old_versions;
        }
        out
    }
}

/// Fix 3 — what an orphan `_indices` GC pass reclaimed. Observability only
/// (INFO log + test assertions); the GC is best-effort and never fails its
/// caller, so this is informational, not an error channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrphanGcStats {
    /// Number of orphaned `_indices/<uuid>` dirs removed.
    pub dirs_removed: u64,
    /// Bytes those dirs held (walked before deletion).
    pub bytes_removed: u64,
}

/// Snapshot of dataset shape used by callers that need to decide
/// whether maintenance is overdue. Cheap — `Table::stats()` reads
/// manifest metadata, never row data.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DatasetStats {
    pub rows: u64,
    pub fragments: u64,
    pub small_fragments: u64,
    pub indices: u64,
    pub versions: u64,
}

impl Storage {
    /// Open (or create) the lance dataset at `path`. Idempotent — reopens if
    /// the dataset exists; constructs an empty `artifacts` table if not.
    ///
    /// `config_dim` is the dim the kb's configured `embedding_model` resolves
    /// to (via `kb_core::embed::model_info(name).dim`). Mismatch policy:
    ///
    /// - **Existing dataset, `config_dim = Some(c)`**: read the on-disk
    ///   `embedding` field's `FixedSizeList<_, N>` width. If `N == c`, open
    ///   normally. If `N != c`, return `Error::Config` so the operator can
    ///   choose to reindex (changing the dim of an existing dataset on disk
    ///   would silently corrupt every still-valid row).
    /// - **Existing dataset, `config_dim = None`**: trust disk dim. This is
    ///   the embed-less kb path — the daemon opens it but never writes
    ///   vectors.
    /// - **Fresh dataset, `config_dim = Some(c)`**: build the schema at dim
    ///   `c` so future upserts match the configured model.
    /// - **Fresh dataset, `config_dim = None`**: build at
    ///   `kb_core::embed::default_model().dim` (384 today) so the schema is
    ///   well-formed even if the operator never wires up an embedder.
    pub async fn open(path: &Path, config_dim: Option<i32>) -> Result<Self> {
        // Legacy entry point — pre-knob behavior (uncapped lance caches, no
        // rebuild throttle) so the storage test suite + short-lived CLI reads
        // are byte-for-byte unchanged. The daemon resolves `[storage]` from
        // kb.toml and calls `open_with_options` with the capped defaults.
        Self::open_with_options(path, config_dim, LanceOptions::unbounded()).await
    }

    /// Like [`open`](Self::open) but with explicit [`LanceOptions`] — cache
    /// caps + rebuild throttle resolved from `[storage]` in kb.toml. A `0`
    /// cache knob restores lance's own default for that cache (6 GiB index /
    /// 1 GiB metadata).
    pub async fn open_with_options(
        path: &Path,
        config_dim: Option<i32>,
        opts: LanceOptions,
    ) -> Result<Self> {
        std::fs::create_dir_all(path)?;
        let builder = connect(&path.to_string_lossy());
        // Fix 1 — cap lance's byte-weighted heap caches. lancedb's default
        // `Session` (created per connection when none is supplied) sizes the
        // index cache at 6 GiB and the metadata cache at 1 GiB
        // (`lance::dataset::DEFAULT_*`); a long-lived daemon with several kbs
        // holds one of each PER kb, which is where the ~10 GB anonymous-heap
        // growth came from. Both knobs are in MiB; `0` maps back to lance's
        // default for that cache. The custom session is otherwise identical
        // to the default one (same default `ObjectStoreRegistry`) and is
        // inherited by every table opened from this connection, reads and
        // writes alike (lancedb `ListingDatabase::connect_with_options`
        // threads `request.session` through open/create).
        let conn = if opts.index_cache_mb == 0 && opts.metadata_cache_mb == 0 {
            builder
                .execute()
                .await
                .map_err(|e| crate::Error::Storage(format!("lance connect: {e}")))?
        } else {
            const MIB: u64 = 1024 * 1024;
            // lance 11.0.0 `dataset.rs:179/183` — mirrored here because the
            // consts aren't re-exported through lancedb.
            const LANCE_DEFAULT_INDEX_CACHE_BYTES: u64 = 6 * 1024 * MIB;
            const LANCE_DEFAULT_METADATA_CACHE_BYTES: u64 = 1024 * MIB;
            let index_bytes = if opts.index_cache_mb == 0 {
                LANCE_DEFAULT_INDEX_CACHE_BYTES
            } else {
                opts.index_cache_mb.saturating_mul(MIB)
            };
            let metadata_bytes = if opts.metadata_cache_mb == 0 {
                LANCE_DEFAULT_METADATA_CACHE_BYTES
            } else {
                opts.metadata_cache_mb.saturating_mul(MIB)
            };
            let session = Arc::new(lancedb::Session::new(
                index_bytes as usize,
                metadata_bytes as usize,
                Arc::new(lancedb::ObjectStoreRegistry::default()),
            ));
            builder
                .session(session)
                .execute()
                .await
                .map_err(|e| crate::Error::Storage(format!("lance connect: {e}")))?
        };

        let names = conn
            .table_names()
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance table_names: {e}")))?;

        let (table, dim) = if names.iter().any(|n| n == TABLE_NAME) {
            let t = conn
                .open_table(TABLE_NAME)
                .execute()
                .await
                .map_err(|e| crate::Error::Storage(format!("lance open_table: {e}")))?;
            let disk_dim = read_embedding_dim(&t).await?;
            if let Some(cfg) = config_dim {
                if cfg != disk_dim {
                    return Err(crate::Error::Config(format!(
                        "kb at {}: lance embedding dim {} ≠ configured-model dim {} \
                         (the embedding_model in kb.toml does not match the dataset on disk). \
                         Either revert the kb.toml change, or trigger a full reindex \
                         (drop the lance dataset + re-embed every artifact).",
                        path.display(),
                        disk_dim,
                        cfg
                    )));
                }
            }
            (t, disk_dim)
        } else {
            let fresh_dim = config_dim.unwrap_or_else(|| crate::embed::default_model().dim as i32);
            let t = conn
                .create_empty_table(TABLE_NAME, Arc::new(schema(fresh_dim)))
                .execute()
                .await
                .map_err(|e| crate::Error::Storage(format!("lance create_empty_table: {e}")))?;
            (t, fresh_dim)
        };

        // v0.3: ensure atlas_* columns exist. Lazy migration via
        // add_columns — cheap (metadata-only) when the table is empty,
        // small constant cost on populated datasets per spike-lance.
        ensure_atlas_columns(&table).await?;
        // v0.6 I1: gallery card counters. Same pattern as the atlas
        // migration; new rows populate via the indexer, old rows stay
        // NULL until a `kb reindex` pass.
        ensure_v6_columns(&table).await?;
        // v0.7 S1: kb-status + kb-severity meta facets. Same lazy
        // pattern. Existing rows stay NULL until reindex.
        ensure_v7_kb_meta_columns(&table).await?;
        // v0.9 M1: memory metas (salience/decay/supersedes). Same lazy
        // pattern; existing rows stay NULL until reindex.
        ensure_v9_memory_columns(&table).await?;
        // v0.14 S1: origin Claude Code session id (`kb_session`). Same
        // lazy add_columns shape; old rows stay NULL until a reindex
        // re-runs `parser::extract` and repopulates them.
        ensure_v14_session_column(&table).await?;
        // v0.15: filesystem btime captured at index time (`created_unix`).
        // Same lazy add_columns shape; old rows stay NULL until a
        // reindex repopulates them from `metadata.created()`.
        ensure_v15_created_column(&table).await?;
        // v0.16: persisted content hash for the startup dedup cache.
        // Same lazy add_columns shape; old rows stay NULL until a
        // reindex repopulates them. Until then those rows still pay
        // the embed cost on the next startup.
        ensure_v16_content_hash_column(&table).await?;
        // N-track: GFM task-list progress (`task_done`/`task_total`). Same
        // lazy add_columns shape; old rows stay NULL until a reindex
        // re-runs `parser::extract` and counts their checkboxes.
        ensure_v17_task_columns(&table).await?;
        // RA4: one-line memory summary (`kb_summary`). Same lazy add_columns
        // shape; old rows stay NULL until a reindex re-runs `parser::extract`
        // and reads their `<meta name="kb-summary">`.
        ensure_v18_summary_column(&table).await?;
        // MI-W3.3a / MI-W3.4: optional memory-type + trust-source metas
        // (`kb_memory_type`, `kb_source`). Same lazy add_columns shape; old
        // rows stay NULL until a reindex re-runs `parser::extract`.
        ensure_mi_w3_memory_type_and_source_columns(&table).await?;
        // CT-A1 (U3 parse-back): highlight-provenance metas (`kb_author`,
        // `kb_source_kb`, `kb_source_artifact`, `kb_source_anchor`). Same
        // lazy add_columns shape; old rows stay NULL until a reindex
        // re-runs `parser::extract` and re-reads the provenance metas
        // `render_artifact` already wrote.
        ensure_u3_provenance_columns(&table).await?;

        // SQ5 — sibling chunk table for passage embeddings. Created empty
        // (metadata-only) at the kb's dim; populated only when a kb opts
        // into `chunked_embeddings`. Always opened so `chunk_vector_query`
        // is safe to call (it returns nothing when the table is empty).
        let chunk_table = if names.iter().any(|n| n == CHUNK_TABLE_NAME) {
            conn.open_table(CHUNK_TABLE_NAME)
                .execute()
                .await
                .map_err(|e| crate::Error::Storage(format!("lance open chunk table: {e}")))?
        } else {
            conn.create_empty_table(CHUNK_TABLE_NAME, Arc::new(chunk_schema(dim)))
                .execute()
                .await
                .map_err(|e| crate::Error::Storage(format!("lance create chunk table: {e}")))?
        };

        Ok(Self {
            conn,
            table,
            dim,
            opts,
            base_path: path.to_path_buf(),
            scalar_indexes_ready: std::sync::atomic::AtomicBool::new(false),
            // Start dirty so the first search builds the index even when the
            // startup compact heuristic didn't run for this kb.
            fts_needs_build: std::sync::atomic::AtomicBool::new(true),
            vector_needs_build: std::sync::atomic::AtomicBool::new(true),
            fts_build_count: std::sync::atomic::AtomicU64::new(0),
            vector_build_count: std::sync::atomic::AtomicU64::new(0),
            last_fts_build: std::sync::Mutex::new(None),
            last_vector_build: std::sync::Mutex::new(None),
            chunk_table,
            chunk_vector_needs_build: std::sync::atomic::AtomicBool::new(true),
            chunk_vector_build_count: std::sync::atomic::AtomicU64::new(0),
            decode_skips: DecodeSkipCounter::default(),
        })
    }

    /// Force-create the FTS index over the text columns. Idempotent — if the
    /// index already exists, lance returns OK. Should be called once after
    /// the first batch of rows lands (FTS index requires data).
    pub async fn ensure_fts_index(&self) -> Result<()> {
        use std::sync::atomic::Ordering;
        // Fast path: the FTS indexes already cover the current row-set. Every
        // upsert/delete re-dirties this (see `mark_search_indexes_dirty`), so a
        // skip here can never serve stale results — the next search after a
        // change rebuilds. This is the whole optimization: a full `create_index`
        // re-train (lancedb default `replace=true`) across all `FTS_COLUMNS`
        // costs ~hundreds of ms; doing it once per change instead of once per
        // query is the ≈500 ms search win.
        if !self.fts_needs_build.load(Ordering::Relaxed) {
            return Ok(());
        }
        // Fix 2 — throttle the full retrain. Without this gate a high-churn
        // corpus (transcripts rewritten every turn, watcher debounce 400 ms,
        // recall/search hooks firing several times a minute) paid one
        // `replace=true` retrain of all 5 FTS columns every search — measured
        // in production as a ~217 MB index rebuild every ~3.7 minutes. Serving
        // with the stale index for up to `index_rebuild_min_secs` is SAFE for
        // freshness: lance 4.0.0's `Scanner::plan_match_query`
        // (src/dataset/scanner.rs:3286-3345) unions the FTS index over indexed
        // fragments with a `FlatMatchQueryExec` flat BM25 scan over fragments
        // newer than the index, so newly-written rows still match — nothing is
        // silently missed and nothing errors. (Only `fast_search = true` would
        // drop the unindexed fragments, and kb never sets it; lancedb's
        // `Query.fast_search` defaults to false.) The dirty flag stays set, so
        // the first search after the window rebuilds.
        if self.rebuild_throttled(&self.last_fts_build) {
            return Ok(());
        }
        // `any_built` flips true once at least one column indexes — i.e. the
        // dataset has data. It stays false on a still-empty dataset so the gate
        // remains dirty and a later search retries cheaply once rows land (FTS,
        // unlike vector, has no brute-force fallback).
        let mut any_built = false;
        for col in FTS_COLUMNS {
            let result = self
                .table
                .create_index(&[col], Index::FTS(FtsIndexBuilder::default()))
                .execute()
                .await;
            match result {
                Ok(()) => any_built = true,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("Index already exists") {
                        any_built = true;
                        continue;
                    }
                    // This column has no data yet (e.g. no doc carries a
                    // `prompt`); skip it — a later rebuild picks it up once it
                    // gains data, which always arrives via a dirtying upsert.
                    if msg.contains("empty") || msg.contains("no rows") || msg.contains("zero") {
                        continue;
                    }
                    return Err(crate::Error::Storage(format!("FTS index {col}: {e}")));
                }
            }
        }
        // Also ensure the scalar BTree indexes for the exact-match filter
        // columns. Folded in here so every existing post-data call site
        // (`indexer`, search/recall routes) creates them too, without new
        // storage-actor plumbing. (Itself gated by `scalar_indexes_ready`.)
        self.ensure_scalar_indexes().await?;
        if any_built {
            self.fts_build_count.fetch_add(1, Ordering::Relaxed);
            self.fts_needs_build.store(false, Ordering::Relaxed);
            *self
                .last_fts_build
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
            // Fix 3 — a successful `replace=true` retrain supersedes the
            // previous build's `_indices/<uuid>` dirs, which lance never
            // reaps. Best-effort; GC failures never fail the search.
            self.gc_orphan_index_dirs().await;
        }
        Ok(())
    }

    /// Create scalar BTree indexes on `SCALAR_INDEX_COLUMNS`. Idempotent
    /// ("Index already exists" → skip) and tolerant of empty / all-null
    /// columns (lance refuses to index those — we skip and a later
    /// post-data pass retries once rows land). lancedb 0.27.2 exposes
    /// `Index::BTree`; the index turns `path = '…'` / `kb_session = '…'`
    /// filters from full scans into seeks.
    async fn ensure_scalar_indexes(&self) -> Result<()> {
        use std::sync::atomic::Ordering;
        // Build at most once per process. lancedb's `create_index` defaults
        // to `replace=true` (a full re-train, NOT a skip-if-current), so
        // without this gate every search/recall — which calls
        // `ensure_fts_index` — would rebuild both BTree indexes, adding pure
        // hot-path latency through the single-writer storage actor. Scan
        // fallback covers rows added after the build; `compact_all`
        // reconciles the index.
        if self.scalar_indexes_ready.load(Ordering::Relaxed) {
            return Ok(());
        }
        let mut all_built = true;
        for col in SCALAR_INDEX_COLUMNS {
            let result = self
                .table
                .create_index(&[col], Index::BTree(BTreeIndexBuilder::default()))
                .execute()
                .await;
            if let Err(e) = result {
                let lower = e.to_string().to_lowercase();
                if lower.contains("index already exists") {
                    continue;
                }
                if lower.contains("empty")
                    || lower.contains("no rows")
                    || lower.contains("zero")
                    || lower.contains("not enough")
                    || lower.contains("at least")
                {
                    // Table isn't populated enough to index yet — don't mark
                    // ready, so a later post-data call retries once rows land.
                    all_built = false;
                    continue;
                }
                return Err(crate::Error::Storage(format!("scalar index {col}: {e}")));
            }
        }
        if all_built {
            self.scalar_indexes_ready.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Upsert documents by `id` via lance `merge_insert` — one atomic
    /// operation (matched rows updated in place, unmatched rows inserted),
    /// so a concurrent reader never observes a row vanish mid-write. Since
    /// v0.7 the id is the path hash (stable across content edits), so an
    /// edited file re-indexes onto its existing row.
    ///
    /// v0.7.1 H1: this replaced a non-atomic delete-by-id-then-add, whose
    /// window let a `get_by_id` racing a reindex see zero rows for an id
    /// that exists before and after.
    pub async fn upsert_docs(&self, docs: &[Doc]) -> Result<()> {
        if docs.is_empty() {
            return Ok(());
        }
        let batches = docs_to_batches(docs, self.dim)?;
        let reader = arrow::record_batch::RecordBatchIterator::new(
            batches.into_iter().map(Ok),
            Arc::new(schema(self.dim)),
        );
        let mut merge = self.table.merge_insert(&["id"]);
        merge
            .when_matched_update_all(None)
            .when_not_matched_insert_all();
        merge
            .execute(Box::new(reader))
            .await
            .map_err(|e| crate::Error::Storage(format!("lance merge_insert: {e}")))?;
        // New / updated rows aren't in the FTS + vector indexes yet — the next
        // search rebuilds to pick them up.
        self.mark_search_indexes_dirty();
        Ok(())
    }

    /// Delete a single artifact by id.
    pub async fn delete_by_id(&self, id: &str) -> Result<()> {
        self.table
            .delete(&filter_eq("id", id))
            .await
            .map_err(|e| crate::Error::Storage(format!("lance delete: {e}")))?;
        self.mark_search_indexes_dirty();
        // SQ5 — cascade the doc's chunks.
        self.delete_chunks_for_doc(id).await;
        Ok(())
    }

    /// Delete every artifact whose `path` column matches. Used by
    /// `process_delete` on `watch.delete` — the file is gone, so there's
    /// no current content to derive an id from. The `watch.modify` path
    /// no longer needs this: since v0.7 the id is path-stable, so
    /// `upsert_docs`'s `merge_insert` re-indexes an edited file onto its
    /// existing row in one atomic step.
    pub async fn delete_by_path(&self, path: &str) -> Result<()> {
        // SQ5 — capture the ids at this path BEFORE deleting so we can
        // cascade their chunks (chunks key on doc_id, not path).
        let ids = self.ids_for_filter(&filter_eq("path", path)).await;
        self.table
            .delete(&filter_eq("path", path))
            .await
            .map_err(|e| crate::Error::Storage(format!("lance delete by path: {e}")))?;
        self.mark_search_indexes_dirty();
        for id in ids {
            self.delete_chunks_for_doc(&id).await;
        }
        Ok(())
    }

    /// Total row count.
    pub async fn count_rows(&self) -> Result<u64> {
        let n = self
            .table
            .count_rows(None)
            .await
            .map_err(|e| crate::Error::Storage(format!("lance count_rows: {e}")))?;
        Ok(n as u64)
    }

    /// Columns every ranked-search arm projects (bm25 / vector / hybrid /
    /// chunk-resolve). A superset of the slim search shape: it carries the
    /// rich gallery-card metadata so `?detail=full` can surface it in one
    /// round-trip (track F — the full search page), AND the memory-recall
    /// metas (`kb_salience` / `kb_decay` / `kb_supersedes` / `kb_session`)
    /// that recall reads off search hits (root invariant #10).
    /// `batches_to_summaries` decodes any unprojected column to `None`, so
    /// widening here is the only change needed to populate the rich fields;
    /// pinning all four arms to this one const keeps them in lock-step (a
    /// drifting arm would silently return empty rich fields for that mode —
    /// e.g. chunked kbs). Cost: ~16 extra narrow columns per candidate row,
    /// read off already-scanned fragments — negligible vs. kNN/FTS/embed.
    const SEARCH_PROJECTION: &'static [&'static str] = &[
        "id",
        "title",
        "path",
        "kb_category",
        "kb_status",
        "kb_severity",
        // body excerpt → `DocSummary.summary`; also the reranker's doc text.
        "body_text_excerpt",
        "mtime_unix",
        "indexed_at_unix",
        "created_unix",
        "word_count",
        "longread",
        "svg_count",
        "has_canvas",
        "has_form",
        "has_animation",
        "has_details",
        "has_math",
        "has_drag",
        "js_loc",
        "css_loc",
        "table_count",
        "code_block_count",
        "tags_csv",
        // memory-recall metas — recall re-rank reads these off search hits
        // (root invariant #10); must stay projected.
        "kb_salience",
        "kb_decay",
        "kb_supersedes",
        "kb_session",
        // RA4 — surfaced on recall hits as the memory's one-line gloss.
        "kb_summary",
        // MI-W3.3a / MI-W3.4 — surfaced on recall hits (display, never a
        // scoring input — see `RecallHit`, which carries neither field).
        "kb_memory_type",
        "kb_source",
        // CT-A1 (U3 parse-back) — highlight provenance, surfaced on recall
        // hits the same pass-through way as `kb_memory_type`/`kb_source`.
        "kb_author",
        "kb_source_kb",
        "kb_source_artifact",
        "kb_source_anchor",
    ];

    /// BM25 query over the FTS-indexed text columns. Returns top `limit`
    /// matches as `DocSummary`. Topic 11 v0.1: callers can also use
    /// `vector_query` (semantic) or `hybrid_query` (BM25 + vector via RRF).
    ///
    /// GC-D1 — `typo_tolerance` (default off; wired from `[kb.*.search]
    /// typo_tolerance`) is a FALLBACK, not a blanket rewrite: the exact
    /// query always runs first, unchanged; only when it comes back EMPTY
    /// does a second, fuzzy pass (max edit distance 1, [`Self::fuzzy_query`])
    /// run and supply the result instead. Bench evidence
    /// (docs/research/typo-tolerance-spike-2026-07.html) is why it's built
    /// this way and not as an always-on rewrite: fuzzing every term
    /// (including ones that already have an exact hit) floods the ranked
    /// set with weakly-related noise and measurably wrecks Recall@1/MRR
    /// even on well-formed, typo-free queries — a straight regression, not
    /// a wash. Gating on "the exact arm found nothing" keeps every
    /// already-working query (the overwhelming majority) byte-identical to
    /// `false`, and only spends the extra fuzzy round-trip on queries that
    /// would otherwise return nothing at all (kb's own "corpus-gap signal"
    /// territory — GC-B3 zero-hit tracking already treats this case as
    /// notable, not routine).
    ///
    /// `false` issues the exact same single query as before GC-D1 (byte-
    /// identical), so determinism (GC-B1, below) is untouched when off —
    /// and, by construction, whenever the exact arm finds anything at all
    /// with `true` too.
    ///
    /// GC-B1: lance's FTS scorer returns exactly-tied scores (common on
    /// short keyword queries, or docs sharing identical text) in physical
    /// scan order, which rides fragment layout and moves as fragments
    /// merge/compact — a determinism-audit finding (docs/research/
    /// search-determinism-settling-window-2026-07.html §3). `rank_sort`
    /// pins the tie-break to artifact id so the same corpus always returns
    /// the same order regardless of on-disk layout — true for both the
    /// exact and fuzzy query shapes below.
    pub async fn bm25_query(
        &self,
        q: &str,
        limit: u32,
        typo_tolerance: bool,
    ) -> Result<Vec<DocSummary>> {
        let exact = self
            .run_fts_query(FullTextSearchQuery::new(q.into()), limit)
            .await?;
        if typo_tolerance && exact.is_empty() {
            let fuzzy = self.fuzzy_query(q).await?;
            return self.run_fts_query(fuzzy, limit).await;
        }
        Ok(exact)
    }

    /// Shared executor for [`Self::bm25_query`]'s exact and fuzzy arms —
    /// run the given FTS query, collect, and apply the same tie-break
    /// (GC-B1) either way.
    async fn run_fts_query(
        &self,
        query: FullTextSearchQuery,
        limit: u32,
    ) -> Result<Vec<DocSummary>> {
        let stream = self
            .table
            .query()
            .full_text_search(query)
            .select(Select::columns(Self::SEARCH_PROJECTION))
            .limit(limit as usize)
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance bm25 query: {e}")))?;

        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;

        let mut hits = batches_to_summaries(&batches, &self.decode_skips);
        rank_sort(&mut hits);
        Ok(hits)
    }

    /// GC-D1 — build the fuzzy (edit-distance-1) variant of `bm25_query`'s
    /// FTS query, across every `FTS_COLUMNS` entry that currently has a
    /// live FTS index built.
    ///
    /// A naive `FullTextSearchQuery::new_fuzzy(q, Some(1))` (column
    /// unset, letting lance auto-fill the indexed columns) turns out to
    /// silently revert to an EXACT match on any kb with more than one
    /// FTS-indexed column — title+body already qualifies. The auto-fill
    /// (`fill_fts_query_column`, triggered whenever a `MatchQuery`'s
    /// `column` is `None`) rebuilds a fresh `MatchQuery::new(..)` per
    /// column when there's more than one, which resets `fuzziness` to its
    /// exact-match default instead of cloning the caller's query — so the
    /// `with_fuzziness(Some(1))` we asked for never survives the fan-out.
    /// This method does that fan-out itself, setting fuzziness on each
    /// per-column leaf explicitly, over whichever columns the table
    /// currently reports as FTS-indexed (`list_indices`, the same source
    /// of truth lance's own auto-fill consults, so a column with no data
    /// yet — e.g. no doc carries a `prompt` — is skipped exactly like the
    /// exact-match path already skips it in `ensure_fts_index`).
    async fn fuzzy_query(&self, q: &str) -> Result<FullTextSearchQuery> {
        let indexed: Vec<String> = self
            .table
            .list_indices()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance list_indices: {e}")))?
            .into_iter()
            .filter(|idx| idx.index_type == IndexType::FTS)
            .flat_map(|idx| idx.columns)
            .filter(|c| FTS_COLUMNS.contains(&c.as_str()))
            .collect();

        let fts_query = match indexed.len() {
            // No FTS index built yet — fall through with the same
            // column-less shape the exact-match arm would use; the
            // executor surfaces whatever empty-index behavior it already
            // does for that case.
            0 => FtsQuery::Match(MatchQuery::new(q.to_string()).with_fuzziness(Some(1))),
            1 => FtsQuery::Match(
                MatchQuery::new(q.to_string())
                    .with_fuzziness(Some(1))
                    .with_column(Some(indexed[0].clone())),
            ),
            _ => {
                let mut multi = MultiMatchQuery::try_new(q.to_string(), indexed)
                    .map_err(|e| crate::Error::Storage(format!("lance fuzzy multi-match: {e}")))?;
                multi.match_queries = multi
                    .match_queries
                    .into_iter()
                    .map(|m| m.with_fuzziness(Some(1)))
                    .collect();
                FtsQuery::MultiMatch(multi)
            }
        };
        Ok(FullTextSearchQuery::new_query(fts_query))
    }

    /// Force-create the IVF-PQ vector index on the embedding column.
    /// Idempotent + tolerant of "not enough rows" (lance requires a minimum
    /// row count to build a meaningful PQ index; under that, vector queries
    /// fall back to brute-force scan, which is fine for small kbs).
    pub async fn ensure_vector_index(&self) -> Result<()> {
        use std::sync::atomic::Ordering;
        // Fast path — same gate as `ensure_fts_index`: the IVF-PQ index already
        // covers the current row-set (re-dirtied by upsert/delete/clear).
        if !self.vector_needs_build.load(Ordering::Relaxed) {
            return Ok(());
        }
        // Fix 2 — same throttle as `ensure_fts_index`. Stale-window serving is
        // safe here by the long-standing design: vector queries brute-force-
        // scan fragments the IVF-PQ index doesn't cover, so freshness never
        // depends on the rebuild having happened.
        if self.rebuild_throttled(&self.last_vector_build) {
            return Ok(());
        }
        let result = self
            .table
            .create_index(&["embedding"], Index::IvfPq(IvfPqIndexBuilder::default()))
            .execute()
            .await;
        if let Err(e) = result {
            let msg = e.to_string();
            if msg.contains("Index already exists") {
                self.vector_needs_build.store(false, Ordering::Relaxed);
                return Ok(());
            }
            // Lance refuses small datasets with various phrasings; fall
            // through gracefully — brute-force scan still works.
            // (Observed: "Not enough rows to train PQ. Requires 256 rows
            // but only 1 available".)
            let lower = msg.to_lowercase();
            if lower.contains("not enough")
                || lower.contains("empty")
                || lower.contains("no rows")
                || lower.contains("zero")
                || lower.contains("at least")
            {
                // Too few rows to train PQ — vector queries brute-force-scan,
                // which is correct. Clear the gate so we don't retry the failing
                // build on every search; growth past the threshold arrives via a
                // dirtying upsert, and the next vector search builds for real.
                self.vector_needs_build.store(false, Ordering::Relaxed);
                return Ok(());
            }
            return Err(crate::Error::Storage(format!("vector index: {e}")));
        }
        self.vector_build_count.fetch_add(1, Ordering::Relaxed);
        self.vector_needs_build.store(false, Ordering::Relaxed);
        *self
            .last_vector_build
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        // Fix 3 — same orphan `_indices` sweep as after an FTS retrain.
        self.gc_orphan_index_dirs().await;
        Ok(())
    }

    /// GC-B7 — retention window for the old-manifest prune step below.
    /// Lance's own `OptimizeAction::All` hardcodes 7 DAYS *and*
    /// `delete_unverified: false` (which additionally protects any
    /// unreferenced data file younger than 7 days even once its owning
    /// manifest is pruned) — a sane default for an untrusted multi-writer
    /// environment, but wildly conservative for kb: the storage actor is
    /// the dataset's SOLE writer AND sole compactor for a given kb (root
    /// invariant #2 / architecture invariant #17's sibling on the storage
    /// side), so nothing else is ever mid-transaction against these files.
    /// The only real hazard is an in-flight READ (a search/gallery query
    /// already scanning a manifest's fragments) losing a file mid-scan if a
    /// concurrent compaction prunes it out from under that read — every
    /// read measured in the 2026-07-11 20k-doc scale test finished in well
    /// under a second even under heavy fragmentation (worst recorded
    /// ~157ms facets p50), so a few-minutes window is a generous multiple
    /// of any realistic in-flight read duration while still reclaiming
    /// disk same-session instead of after a week.
    const PRUNE_RETENTION_MINUTES: i64 = 5;

    /// Run compaction + a physical old-version reclaim, using kb's own
    /// (safe-for-single-writer) retention policy — see
    /// `PRUNE_RETENTION_MINUTES` and `compact_all_with_retention`.
    ///
    /// Why we need it: every `upsert_docs` is a `merge_insert` that
    /// commits a fresh manifest + a new data fragment. The indexer used to
    /// run one upsert per file (GC-B7 batches that — see
    /// `indexer::flush_prepared_batch`), so a corpus that's been reindexed
    /// a few times still accumulates fragments and stale manifest
    /// versions over time. Hybrid search then degrades from ~50 ms to many
    /// seconds because every query reads every fragment. The lance dataset
    /// docs call this out under "many small files hurt read perf";
    /// compaction is the official remediation for the fragments, and
    /// pruning is the remediation for the disk the stale versions leave
    /// behind (measured: 319 MB state for 1,619 docs, ~20x raw source,
    /// `old_versions_removed=0` every cycle under lance's own 7-day/
    /// unverified=false default).
    pub async fn compact_all(&self) -> Result<CompactStats> {
        self.compact_all_with_retention(Self::PRUNE_RETENTION_MINUTES, true)
            .await
    }

    /// Same as `compact_all` but with an explicit retention window +
    /// `delete_unverified` flag, so callers (and tests) can exercise the
    /// physical-reclaim path deterministically — production always goes
    /// through `compact_all`, which pins the safe defaults above. Runs
    /// compaction and pruning as TWO separate `optimize` calls (rather than
    /// `OptimizeAction::All`, which hardcodes lance's 7-day/
    /// `delete_unverified: false` prune policy with no way to override
    /// either), plus a best-effort index optimize — matching what `All`
    /// used to do end to end. Never runs concurrently with itself: the
    /// storage actor parks any other Lance-mutating message
    /// (`defers_during_compact`, `storage/actor.rs`) while a compaction is
    /// in flight, so at most one physical reclaim pass per compaction
    /// cycle.
    pub async fn compact_all_with_retention(
        &self,
        retention_minutes: i64,
        delete_unverified: bool,
    ) -> Result<CompactStats> {
        let compaction = self
            .table
            .optimize(OptimizeAction::Compact {
                options: Default::default(),
                remap_options: None,
            })
            .await
            .map_err(|e| crate::Error::Storage(format!("lance compact: {e}")))?;
        let prune = self
            .table
            .optimize(OptimizeAction::Prune {
                older_than: Some(Duration::minutes(retention_minutes)),
                delete_unverified: Some(delete_unverified),
                error_if_tagged_old_versions: None,
            })
            .await
            .map_err(|e| crate::Error::Storage(format!("lance prune: {e}")))?;
        // Best-effort: unindexed rows still get picked up by the next
        // search's scan fallback (`ensure_fts_index` / vector build), so a
        // failure here isn't fatal to correctness, just a missed
        // opportunistic speedup.
        if let Err(e) = self
            .table
            .optimize(OptimizeAction::Index(Default::default()))
            .await
        {
            tracing::debug!(error = %e, "compact_all: index optimize failed (non-fatal)");
        }
        let mut stats = CompactStats::from(compaction);
        let pruned = CompactStats::from(prune);
        stats.bytes_pruned = pruned.bytes_pruned;
        stats.old_versions_removed = pruned.old_versions_removed;
        // SQ5 — keep the chunk table compact too (best-effort; it
        // accumulates fragments per upsert just like the doc table). Left
        // on lance's own default prune policy — chunking is opt-in and off
        // by default, so this table's disk growth is a lesser concern.
        let _ = self.chunk_table.optimize(OptimizeAction::All).await;
        // Fix 3 — lance's prune reaps old manifest versions but never the
        // superseded `_indices/<uuid>` dirs those versions referenced
        // (production: `index_files_removed: 0` across thousands of orphans,
        // 21 GB of 23 GB total). Sweep them here too. Best-effort.
        self.gc_orphan_index_dirs().await;
        Ok(stats)
    }

    /// Fix 3 — grace window for the orphan `_indices` GC below: a uuid dir
    /// younger than this is never deleted, so a just-finished (or still
    /// uploading) index build is never reaped even if a stale manifest read
    /// doesn't reference it yet. The storage actor is the dataset's sole
    /// writer (invariant #2), so an hour is far wider than any real lag.
    const INDEX_ORPHAN_GRACE_SECS: u64 = 60 * 60;

    /// Fix 3 — sweep orphaned `<table>.lance/_indices/<uuid>` dirs for BOTH
    /// tables (doc + chunk), using the production grace window. Best-effort:
    /// every failure is logged (WARN) and swallowed — the GC never fails its
    /// caller. Returns what was reclaimed (for the INFO log + tests).
    pub async fn gc_orphan_index_dirs(&self) -> OrphanGcStats {
        self.gc_orphan_index_dirs_with_grace(Self::INDEX_ORPHAN_GRACE_SECS)
            .await
    }

    /// Same as [`gc_orphan_index_dirs`](Self::gc_orphan_index_dirs) but with
    /// an explicit grace window, so tests can exercise the delete path
    /// deterministically with `0` — the `compact_all_with_retention(0, true)`
    /// precedent.
    pub async fn gc_orphan_index_dirs_with_grace(&self, grace_secs: u64) -> OrphanGcStats {
        let mut total = OrphanGcStats::default();
        for (table, name) in [
            (&self.table, TABLE_NAME),
            (&self.chunk_table, CHUNK_TABLE_NAME),
        ] {
            let indices_dir = self
                .base_path
                .join(format!("{name}.lance"))
                .join("_indices");
            let stats = Self::gc_orphan_index_dirs_for(table, &indices_dir, grace_secs).await;
            total.dirs_removed += stats.dirs_removed;
            total.bytes_removed += stats.bytes_removed;
        }
        total
    }

    /// GC one table's `_indices` dir. The live set is the uuid of every index
    /// the dataset's CURRENT manifest references (`load_indices` — includes
    /// the system frag-reuse index); any uuid-shaped dir NOT in that set and
    /// older than the grace window is a superseded build lance forgot to
    /// reap. Anything else on disk (non-uuid names, files, un-ageable
    /// entries) is left strictly alone.
    async fn gc_orphan_index_dirs_for(
        table: &LanceTable,
        indices_dir: &Path,
        grace_secs: u64,
    ) -> OrphanGcStats {
        use lance::index::DatasetIndexExt;

        let mut stats = OrphanGcStats::default();
        let live: std::collections::HashSet<String> = 'live: {
            let Some(wrapper) = table.dataset() else {
                // Non-native (remote) table — never in kb, but don't GC blind.
                break 'live Default::default();
            };
            match wrapper.get().await {
                Ok(dataset) => match dataset.load_indices().await {
                    Ok(indices) => indices.iter().map(|m| m.uuid.to_string()).collect(),
                    Err(e) => {
                        tracing::warn!(error = %e, dir = %indices_dir.display(),
                            "orphan index GC: load_indices failed; skipping this table");
                        return stats;
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, dir = %indices_dir.display(),
                        "orphan index GC: dataset handle failed; skipping this table");
                    return stats;
                }
            }
        };
        if live.is_empty() {
            // No live indices → nothing on disk can be orphaned-but-deletable
            // safely distinguishable from "manifest read broke", and the
            // `None` dataset case above lands here too. Bail conservatively.
            return stats;
        }
        let Ok(entries) = std::fs::read_dir(indices_dir) else {
            return stats; // no _indices dir yet — nothing to do
        };
        let now = std::time::SystemTime::now();
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !is_uuid_dir_name(name) || live.contains(name) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            let Ok(mtime) = meta.modified() else {
                continue; // can't age it — never delete blind
            };
            let age = now.duration_since(mtime).unwrap_or_default();
            if age < std::time::Duration::from_secs(grace_secs) {
                continue;
            }
            let bytes = dir_size_bytes(&entry.path());
            match std::fs::remove_dir_all(entry.path()) {
                Ok(()) => {
                    stats.dirs_removed += 1;
                    stats.bytes_removed += bytes;
                }
                Err(e) => {
                    tracing::warn!(error = %e, dir = %entry.path().display(),
                        "orphan index GC: remove failed (continuing)");
                }
            }
        }
        if stats.dirs_removed > 0 {
            tracing::info!(
                dir = %indices_dir.display(),
                dirs_removed = stats.dirs_removed,
                bytes_removed = stats.bytes_removed,
                "orphan index GC reclaimed superseded _indices dirs"
            );
        }
        stats
    }

    /// Cheap snapshot of dataset shape used by the startup auto-compact
    /// heuristic (`fragments > 8 × rows || versions > 200`). Reads via
    /// `Table::stats()` + `Table::list_versions()` — no full scans.
    pub async fn dataset_stats(&self) -> Result<DatasetStats> {
        let stats = self
            .table
            .stats()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stats: {e}")))?;
        let versions = self
            .table
            .list_versions()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance list_versions: {e}")))?;
        Ok(DatasetStats {
            rows: stats.num_rows as u64,
            fragments: stats.fragment_stats.num_fragments as u64,
            small_fragments: stats.fragment_stats.num_small_fragments as u64,
            indices: stats.num_indices as u64,
            versions: versions.len() as u64,
        })
    }

    /// Vector-only semantic query — top-`limit` rows nearest to the query
    /// vector by cosine distance (lance default for FixedSizeList<Float32>).
    /// Skips rows whose embedding column is null (v0.0.1 leftovers).
    pub async fn vector_query(&self, query_vec: &[f32], limit: u32) -> Result<Vec<DocSummary>> {
        // Guard the empty-corpus case: `nearest_to` on a zero-row table
        // panicked in an older lance (the brute-force path indexed
        // `batches[0]` with no batches present); not reproducible on the
        // pinned 0.27.2, so this is cheap defensive insurance. An empty
        // corpus has no hits anyway, and `count_rows` reads manifest
        // metadata (no scan), so the populated-path cost is negligible.
        if self.count_rows().await? == 0 {
            return Ok(Vec::new());
        }
        let stream = self
            .table
            .query()
            .nearest_to(query_vec.to_vec())
            .map_err(|e| crate::Error::Storage(format!("lance nearest_to: {e}")))?
            .select(Select::columns(Self::SEARCH_PROJECTION))
            .limit(limit as usize)
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance vector query: {e}")))?;

        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;

        // GC-B1 — same tie-break discipline as `bm25_query`: two rows at
        // an identical distance (duplicate/near-duplicate embeddings) must
        // not resolve via physical scan order.
        let mut hits = batches_to_summaries(&batches, &self.decode_skips);
        rank_sort(&mut hits);
        Ok(hits)
    }

    /// Hybrid query: BM25 over text columns + vector over the embedding
    /// column, fused via lance's RRF (k=60 default per topic 01 §Decisions).
    /// Both signals run; lance handles the merge.
    pub async fn hybrid_query(
        &self,
        q: &str,
        query_vec: &[f32],
        limit: u32,
    ) -> Result<Vec<DocSummary>> {
        // Same empty-corpus guard as `vector_query` — the hybrid path also
        // calls `nearest_to`, which has panicked on a zero-row table.
        if self.count_rows().await? == 0 {
            return Ok(Vec::new());
        }
        let stream = self
            .table
            .query()
            .full_text_search(FullTextSearchQuery::new(q.into()))
            .nearest_to(query_vec.to_vec())
            .map_err(|e| crate::Error::Storage(format!("lance hybrid nearest_to: {e}")))?
            .select(Select::columns(Self::SEARCH_PROJECTION))
            .limit(limit as usize)
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance hybrid query: {e}")))?;

        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;

        // GC-B1 — same tie-break as the two single-signal arms above; the
        // `rrf_fuse` (kb-core::fusion) tie-break relies on deterministic
        // arm order, so this arm must itself be canonical on ties.
        let mut hits = batches_to_summaries(&batches, &self.decode_skips);
        rank_sort(&mut hits);
        Ok(hits)
    }

    // ---- SQ5 — passage/chunk table -------------------------------------

    /// Replace a document's chunks: delete its existing chunk rows, then
    /// insert the new ones. Dirties ONLY the chunk-vector index (not the
    /// doc FTS/vector indexes) and never bumps the gallery generation —
    /// chunks are a search-only sidecar, invisible to the doc row-set
    /// (root invariant #15).
    pub async fn upsert_chunks(&self, doc_id: &str, chunks: &[ChunkDoc]) -> Result<()> {
        use std::sync::atomic::Ordering;
        self.chunk_table
            .delete(&filter_eq("doc_id", doc_id))
            .await
            .map_err(|e| crate::Error::Storage(format!("lance chunk delete: {e}")))?;
        if !chunks.is_empty() {
            let batches = chunks_to_batches(chunks, self.dim)?;
            let reader = arrow::record_batch::RecordBatchIterator::new(
                batches.into_iter().map(Ok),
                Arc::new(chunk_schema(self.dim)),
            );
            let mut merge = self.chunk_table.merge_insert(&["chunk_id"]);
            merge
                .when_matched_update_all(None)
                .when_not_matched_insert_all();
            merge
                .execute(Box::new(reader))
                .await
                .map_err(|e| crate::Error::Storage(format!("lance chunk merge: {e}")))?;
        }
        self.chunk_vector_needs_build.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Cascade helper: drop all chunks for a doc id (called on doc delete).
    /// Takes the RAW id and escapes internally via [`filter_eq`] — callers no
    /// longer pre-escape (a footgun the old `escaped_id` contract invited).
    async fn delete_chunks_for_doc(&self, doc_id: &str) {
        let _ = self.chunk_table.delete(&filter_eq("doc_id", doc_id)).await;
        self.chunk_vector_needs_build
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Collect the `id`s matching a lance filter (for delete cascades).
    /// Best-effort: returns empty on any query error.
    async fn ids_for_filter(&self, filter: &str) -> Vec<String> {
        let Ok(stream) = self
            .table
            .query()
            .only_if(filter)
            .select(Select::columns(&["id"]))
            .execute()
            .await
        else {
            return Vec::new();
        };
        let batches: Vec<arrow::record_batch::RecordBatch> =
            stream.try_collect().await.unwrap_or_default();
        let mut ids = Vec::new();
        for batch in &batches {
            if let Some(col) = batch.column_by_name("id") {
                let arr = col.as_string::<i32>();
                for i in 0..batch.num_rows() {
                    ids.push(arr.value(i).to_string());
                }
            }
        }
        ids
    }

    /// Build the IVF-PQ index on the chunk embedding column. Same gate +
    /// small-table tolerance as `ensure_vector_index`, but its OWN dirty
    /// flag so chunk writes never rebuild the doc index.
    pub async fn ensure_chunk_vector_index(&self) -> Result<()> {
        use std::sync::atomic::Ordering;
        if !self.chunk_vector_needs_build.load(Ordering::Relaxed) {
            return Ok(());
        }
        let result = self
            .chunk_table
            .create_index(&["embedding"], Index::IvfPq(IvfPqIndexBuilder::default()))
            .execute()
            .await;
        if let Err(e) = result {
            let lower = e.to_string().to_lowercase();
            if lower.contains("already exists")
                || lower.contains("not enough")
                || lower.contains("empty")
                || lower.contains("no rows")
                || lower.contains("zero")
                || lower.contains("at least")
            {
                self.chunk_vector_needs_build
                    .store(false, Ordering::Relaxed);
                return Ok(());
            }
            return Err(crate::Error::Storage(format!("chunk vector index: {e}")));
        }
        self.chunk_vector_build_count
            .fetch_add(1, Ordering::Relaxed);
        self.chunk_vector_needs_build
            .store(false, Ordering::Relaxed);
        Ok(())
    }

    /// Number of real chunk-vector index rebuilds since open (observability).
    pub fn chunk_vector_rebuild_count(&self) -> u64 {
        self.chunk_vector_build_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Vector search over passage chunks, max-pooled to a per-document
    /// ranking — the SQ5 fix for whole-body embedding truncation. Over-fetch
    /// `over_fetch` chunks, keep the nearest chunk per doc, take the top
    /// `limit` docs, and resolve them to `DocSummary` (score =
    /// `1/(1+distance)`). Chunks whose doc no longer exists (path-delete
    /// orphans) drop at the resolve step. Empty table → empty (no panic).
    pub async fn chunk_vector_query(
        &self,
        query_vec: &[f32],
        over_fetch: u32,
        limit: u32,
    ) -> Result<Vec<DocSummary>> {
        if self
            .chunk_table
            .count_rows(None)
            .await
            .map_err(|e| crate::Error::Storage(format!("lance chunk count: {e}")))?
            == 0
        {
            return Ok(Vec::new());
        }
        let stream = self
            .chunk_table
            .query()
            .nearest_to(query_vec.to_vec())
            .map_err(|e| crate::Error::Storage(format!("lance chunk nearest_to: {e}")))?
            .select(Select::columns(&["doc_id"]))
            .limit(over_fetch as usize)
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance chunk vector query: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance chunk stream: {e}")))?;

        // Max-pool: keep the best (min) distance per doc_id.
        let mut best: Vec<(String, f32)> = Vec::new();
        let mut pos: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for batch in &batches {
            // R1b — typed decode: a missing/drifted `doc_id` column used to
            // `.unwrap().as_string()`-panic on the storage-actor task. This fn
            // returns Result, so surface it as a storage error instead.
            let Some(doc_ids) = batch
                .column_by_name("doc_id")
                .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
            else {
                return Err(crate::Error::Storage(
                    "lance chunk_vector_query: `doc_id` column missing or not a string array"
                        .into(),
                ));
            };
            let dists = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<arrow::array::Float32Array>());
            for i in 0..batch.num_rows() {
                let id = doc_ids.value(i).to_string();
                let dist = dists.map(|d| d.value(i)).unwrap_or(0.0);
                match pos.get(&id) {
                    Some(&p) => {
                        if dist < best[p].1 {
                            best[p].1 = dist;
                        }
                    }
                    None => {
                        pos.insert(id.clone(), best.len());
                        best.push((id, dist));
                    }
                }
            }
        }
        // GC-B1 — tie-break by doc id: two chunks landing at the exact
        // same pooled distance (duplicate passages) must not resolve via
        // the KNN stream's physical order, which is what decides which
        // survives `truncate` below.
        best.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        best.truncate(limit as usize);
        if best.is_empty() {
            return Ok(Vec::new());
        }

        // Resolve winning doc_ids to DocSummary.
        let in_list = best
            .iter()
            .map(|(id, _)| format!("'{}'", escape_literal(id)))
            .collect::<Vec<_>>()
            .join(",");
        let stream = self
            .table
            .query()
            .only_if(format!("id IN ({in_list})"))
            .select(Select::columns(Self::SEARCH_PROJECTION))
            .limit(best.len())
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance chunk resolve: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance chunk resolve stream: {e}")))?;
        let mut summaries = batches_to_summaries(&batches, &self.decode_skips);

        // Order by chunk rank; set score = 1/(1+best_distance).
        let order: std::collections::HashMap<&str, (usize, f32)> = best
            .iter()
            .enumerate()
            .map(|(rank, (id, dist))| (id.as_str(), (rank, *dist)))
            .collect();
        summaries.sort_by_key(|s| {
            order
                .get(s.id.as_str())
                .map(|(r, _)| *r)
                .unwrap_or(usize::MAX)
        });
        for s in &mut summaries {
            if let Some((_, dist)) = order.get(s.id.as_str()) {
                s.score = Some(1.0 / (1.0 + dist));
            }
        }
        Ok(summaries)
    }

    /// Look up a single doc by exact id. Returns `Ok(None)` if no row
    /// matches. Used by the `/api/kb/{kb}/docs/{id}` endpoint and by the
    /// artifact subdomain handler when the host id is a content hash
    /// rather than a file stem.
    pub async fn get_by_id(&self, id: &str) -> Result<Option<DocSummary>> {
        // Lance SQL filter — `id = '<hex>'`. Hex is alphanumeric so no
        // escaping needed; we still reject anything non-alnum to be safe.
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Ok(None);
        }
        let stream = self
            .table
            .query()
            .only_if(format!("id = '{id}'"))
            .select(Select::columns(&[
                "id",
                "title",
                "path",
                "kb_category",
                "kb_status",
                "kb_severity",
                // AS — origin session id, so artifact→sessions can flag the
                // session the artifact was born in (`authored`). Opt-in column;
                // `batches_to_summaries` leaves it `None` when not selected.
                "kb_session",
                "kb_summary",
                "mtime_unix",
                "indexed_at_unix",
                "body_text_excerpt",
                "svg_count",
                "has_canvas",
                "has_form",
                "has_animation",
                "has_details",
                "has_math",
                "has_drag",
                "js_loc",
                "css_loc",
                "table_count",
                "code_block_count",
                "word_count",
                "longread",
                "tags_csv",
                "created_unix",
                "task_done",
                "task_total",
            ]))
            .limit(1)
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance get_by_id: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        Ok(batches_to_summaries(&batches, &self.decode_skips)
            .into_iter()
            .next())
    }

    /// F3a — load the full [`Doc`] row for relocate (id rekey): every column
    /// that `docs_to_batches` writes, including the embedding vector and
    /// body/code/prompt text. Returns `Ok(None)` when the id is absent.
    /// Atlas coordinates live in separate `add_columns` fields and are NOT
    /// part of [`Doc`] — a relocate leaves them behind on the old id (atlas
    /// recomputes). Chunks are a sibling table; use
    /// [`Self::list_chunks_for_doc`].
    pub async fn get_full_doc(&self, id: &str) -> Result<Option<Doc>> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Ok(None);
        }
        let cols = [
            "id",
            "path",
            "title",
            "body",
            "headings",
            "code",
            "prompt",
            "body_text_excerpt",
            "embedding",
            "kb_category",
            "prompt_size_bytes",
            "size_kb",
            "js_loc",
            "css_loc",
            "svg_count",
            "has_svg",
            "has_form",
            "has_canvas",
            "has_animation",
            "has_details",
            "has_script",
            "has_drag",
            "has_math",
            "mtime_unix",
            "indexed_at_unix",
            "table_count",
            "code_block_count",
            "word_count",
            "longread",
            "tags_csv",
            "kb_status",
            "kb_severity",
            "kb_salience",
            "kb_decay",
            "kb_supersedes",
            "kb_session",
            "created_unix",
            "content_hash",
            "task_done",
            "task_total",
            "kb_summary",
            "kb_memory_type",
            "kb_source",
            "kb_author",
            "kb_source_kb",
            "kb_source_artifact",
            "kb_source_anchor",
        ];
        let stream = self
            .table
            .query()
            .only_if(format!("id = '{id}'"))
            .select(Select::columns(&cols))
            .limit(1)
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance get_full_doc: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance get_full_doc stream: {e}")))?;
        Ok(batch_to_full_docs(&batches).into_iter().next())
    }

    /// F3a — every passage chunk for `doc_id`, including stored embeddings.
    /// Empty when the doc has no chunks (never chunk-indexed, or already
    /// re-keyed away). Used by relocate to re-derive `chunk_id =
    /// "{new_id}#{idx}"` without re-embedding.
    pub async fn list_chunks_for_doc(&self, doc_id: &str) -> Result<Vec<ChunkDoc>> {
        if doc_id.is_empty() {
            return Ok(Vec::new());
        }
        let stream = self
            .chunk_table
            .query()
            .only_if(filter_eq("doc_id", doc_id))
            .select(Select::columns(&[
                "chunk_id",
                "doc_id",
                "chunk_idx",
                "text",
                "embedding",
            ]))
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance list_chunks_for_doc: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance list_chunks stream: {e}")))?;
        Ok(batches_to_chunk_docs(&batches))
    }

    /// Batch variant of [`get_by_id`](Self::get_by_id): resolve many exact ids
    /// in a single `id IN (…)` scan instead of one round-trip per id. Returns
    /// the matched summaries in unspecified order (the caller keys them by
    /// `id`); ids that don't exist are simply absent. Used by the fleet inbox
    /// to resolve title/source-path for every commented artifact in one query
    /// — and (W2.3a) by the true-neighbors route to resolve a small
    /// (≤`SIMILAR_MAX_LIMIT`) neighbor id set's title/path/atlas coords in
    /// one round-trip. `atlas_x`/`atlas_y`/`atlas_cluster` are included in
    /// the projection (unlike `get_by_id`'s single-doc twin) so that route
    /// never needs a second, full-corpus `list_docs_with_atlas` scan just to
    /// find three columns for a handful of ids; the three extra narrow
    /// columns are negligible for every other caller (same reasoning as
    /// `SEARCH_PROJECTION`'s memory-recall metas above).
    pub async fn get_by_ids(&self, ids: &[String]) -> Result<Vec<DocSummary>> {
        // Same defensive id shape as get_by_id (hex/stem ids are alnum or '-').
        let in_list = ids
            .iter()
            .filter(|id| {
                !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(",");
        if in_list.is_empty() {
            return Ok(Vec::new());
        }
        let stream = self
            .table
            .query()
            .only_if(format!("id IN ({in_list})"))
            .select(Select::columns(&[
                "id",
                "title",
                "path",
                "kb_category",
                "kb_status",
                "kb_severity",
                "kb_session",
                "kb_summary",
                "mtime_unix",
                "indexed_at_unix",
                "body_text_excerpt",
                "svg_count",
                "has_canvas",
                "has_form",
                "has_animation",
                "has_details",
                "has_math",
                "has_drag",
                "js_loc",
                "css_loc",
                "table_count",
                "code_block_count",
                "word_count",
                "longread",
                "tags_csv",
                "created_unix",
                "task_done",
                "task_total",
                "atlas_x",
                "atlas_y",
                "atlas_cluster",
            ]))
            .limit(ids.len())
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance get_by_ids: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance get_by_ids collect: {e}")))?;
        Ok(batches_to_summaries(&batches, &self.decode_skips))
    }

    /// Q-track (board B1) — batch FULL-body lookup for the search route's
    /// match-context snippet extraction. Unlike `get_by_ids` (which
    /// projects `body_text_excerpt`, capped ~400 chars — `DocSummary::
    /// summary`), this projects the raw `body` column so the snippet
    /// extractor can find a match anywhere in the document, not just its
    /// opening excerpt. Same id-allowlist defense as `get_by_ids` (ids
    /// outside `[A-Za-z0-9-]` are dropped from the IN-list rather than
    /// escaped — a deliberate allowlist, see `escape_literal`'s doc
    /// comment). Returns `(id, body)` pairs in unspecified order; ids that
    /// don't exist (or fail the allowlist) are simply absent — never an
    /// error, so one bad id can't fail the whole page's snippet pass.
    pub async fn get_bodies_by_ids(&self, ids: &[String]) -> Result<Vec<(String, String)>> {
        let in_list = ids
            .iter()
            .filter(|id| {
                !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(",");
        if in_list.is_empty() {
            return Ok(Vec::new());
        }
        let stream = self
            .table
            .query()
            .only_if(format!("id IN ({in_list})"))
            .select(Select::columns(&["id", "body"]))
            .limit(ids.len())
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance get_bodies_by_ids: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        let mut out = Vec::new();
        for batch in &batches {
            let (Some(id_col), Some(body_col)) =
                (batch.column_by_name("id"), batch.column_by_name("body"))
            else {
                continue;
            };
            let ids_arr = id_col.as_string::<i32>();
            let bodies_arr = body_col.as_string::<i32>();
            for i in 0..batch.num_rows() {
                if ids_arr.is_null(i) || bodies_arr.is_null(i) {
                    continue;
                }
                out.push((
                    ids_arr.value(i).to_string(),
                    bodies_arr.value(i).to_string(),
                ));
            }
        }
        Ok(out)
    }

    /// W2.3a — fetch just the embedding vector for one id. Mirrors
    /// `get_by_id`'s shape (same defensive id allowlist, same `only_if("id =
    /// '<id>'")` scan) but projects `["id", "embedding"]` instead of the slim
    /// summary columns: the true-neighbors route's seed vector never needs
    /// the doc's other fields. Returns `Ok(None)` both when the id doesn't
    /// exist AND when the row's embedding column is null (never indexed with
    /// one, or a v0.0.1-era row) — the caller can't distinguish the two from
    /// this fn alone, which is fine, since both mean "no seed vector to
    /// query with" (`routes::atlas::similar` renders that as an honest
    /// `{neighbors: [], reason: "no-embedding"}`, not a 404).
    pub async fn embedding_by_id(&self, id: &str) -> Result<Option<Vec<f32>>> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Ok(None);
        }
        let stream = self
            .table
            .query()
            .only_if(format!("id = '{id}'"))
            .select(Select::columns(&["id", "embedding"]))
            .limit(1)
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance embedding_by_id: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance embedding_by_id stream: {e}")))?;
        Ok(batches_to_embeddings(&batches, &self.decode_skips)
            .into_iter()
            .next()
            .map(|(_, v)| v))
    }

    /// W2.3a — batch variant of [`embedding_by_id`](Self::embedding_by_id):
    /// resolve many ids' embedding vectors in one bounded `id IN (…)` scan
    /// (mirrors [`get_bodies_by_ids`](Self::get_bodies_by_ids)'s shape). Used
    /// by the true-neighbors route to fetch the handful of neighbor vectors
    /// (`limit`, capped small — never a corpus-wide pull like
    /// `list_embeddings`) it needs for an in-route cosine computation —
    /// lance's own `_distance` is never decoded onto the wire (see
    /// `vector_query`'s doc comment). Same allowlist-drop-not-escape
    /// discipline as `get_by_ids`; ids that don't exist (or have a null
    /// embedding) are simply absent.
    pub async fn embeddings_by_ids(&self, ids: &[String]) -> Result<Vec<EmbeddingPair>> {
        let in_list = ids
            .iter()
            .filter(|id| {
                !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(",");
        if in_list.is_empty() {
            return Ok(Vec::new());
        }
        let stream = self
            .table
            .query()
            .only_if(format!("id IN ({in_list})"))
            .select(Select::columns(&["id", "embedding"]))
            .limit(ids.len())
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance embeddings_by_ids: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance embeddings_by_ids collect: {e}")))?;
        Ok(batches_to_embeddings(&batches, &self.decode_skips))
    }

    /// W2.11 — fetch just the stored generation prompt + its (index-time,
    /// 8 KiB-)capped byte size for one id. Mirrors `embedding_by_id`'s shape
    /// (same defensive id allowlist, same `only_if("id = '<id>'")` scan) but
    /// projects `["id", "prompt", "prompt_size_bytes"]` — the prompt-browse
    /// route (`GET /api/kb/{kb}/artifacts/{id}/prompt`) needs nothing else.
    /// Returns `Ok(None)` both when the id doesn't exist AND when the row's
    /// `prompt` column is null (no `<template id="kb-prompt">` in the
    /// source) — same "can't distinguish, and that's fine" contract as
    /// `embedding_by_id`: the caller (`kb_server::routes::prompt::get`) runs
    /// its own `get_by_id` existence check first (for the 404 case), so by
    /// the time this is called a `None` unambiguously means "no prompt to
    /// show" rather than "unknown id".
    pub async fn prompt_by_id(&self, id: &str) -> Result<Option<(String, u32)>> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Ok(None);
        }
        let stream = self
            .table
            .query()
            .only_if(format!("id = '{id}'"))
            .select(Select::columns(&["id", "prompt", "prompt_size_bytes"]))
            .limit(1)
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance prompt_by_id: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance prompt_by_id stream: {e}")))?;
        for batch in &batches {
            let (Some(prompt_col), Some(size_col)) = (
                batch.column_by_name("prompt"),
                batch.column_by_name("prompt_size_bytes"),
            ) else {
                continue;
            };
            let prompts = prompt_col.as_string::<i32>();
            let Some(sizes) = size_col
                .as_any()
                .downcast_ref::<arrow::array::UInt32Array>()
            else {
                continue;
            };
            for i in 0..batch.num_rows() {
                if prompts.is_null(i) {
                    continue;
                }
                return Ok(Some((prompts.value(i).to_string(), sizes.value(i))));
            }
        }
        Ok(None)
    }

    /// Look up a single doc by exact `path` (the absolute file path the
    /// indexer stored at upsert time — see `indexer.rs` where
    /// `path.to_string_lossy()` populates the row). Returns `Ok(None)`
    /// if no row matches. Used by the artifact subdomain handler's
    /// cross-artifact relative-link fallback: when a sub-path on
    /// `<id>.artifacts.localhost/<rel>` canonicalises to a file under
    /// the kb's source root, this lookup decides whether that file is
    /// itself an indexed artifact (→ trampoline) or a raw asset (→ 404).
    pub async fn get_by_source_path(&self, path: &str) -> Result<Option<DocSummary>> {
        if path.is_empty() {
            return Ok(None);
        }
        // `filter_eq` builds `path = 'literal'`, doubling embedded
        // apostrophes. Path strings may contain spaces and unicode —
        // Lance treats those as literal bytes inside the quoted form.
        let stream = self
            .table
            .query()
            .only_if(filter_eq("path", path))
            .select(Select::columns(&[
                "id",
                "title",
                "path",
                "kb_category",
                "kb_status",
                "kb_severity",
                "mtime_unix",
                "indexed_at_unix",
                "body_text_excerpt",
                "svg_count",
                "has_canvas",
                "has_form",
                "has_animation",
                "has_details",
                "has_math",
                "has_drag",
                "js_loc",
                "css_loc",
                "table_count",
                "code_block_count",
                "word_count",
                "longread",
                "tags_csv",
                "created_unix",
                "task_done",
                "task_total",
                "kb_summary",
            ]))
            .limit(1)
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance get_by_source_path: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        Ok(batches_to_summaries(&batches, &self.decode_skips)
            .into_iter()
            .next())
    }

    /// Batch variant of [`get_by_source_path`](Self::get_by_source_path):
    /// resolve many exact stored paths in one `path IN (…)` scan instead of
    /// one actor round-trip per path (the `path` column carries a scalar
    /// BTree index, so each stays a seek). Returns matched summaries in
    /// unspecified order (callers key them by `path`); paths with no row
    /// are simply absent. Used by the edge-record enrichment hook to
    /// resolve a hub page's relative hrefs in one query.
    pub async fn get_by_source_paths(&self, paths: &[String]) -> Result<Vec<DocSummary>> {
        // Same escaping as get_by_source_path: double embedded apostrophes;
        // spaces/unicode are literal bytes inside the quoted form.
        let in_list = paths
            .iter()
            .filter(|p| !p.is_empty())
            .map(|p| format!("'{}'", escape_literal(p)))
            .collect::<Vec<_>>()
            .join(",");
        if in_list.is_empty() {
            return Ok(Vec::new());
        }
        let stream = self
            .table
            .query()
            .only_if(format!("path IN ({in_list})"))
            .select(Select::columns(&[
                "id",
                "title",
                "path",
                "kb_category",
                "kb_status",
                "kb_severity",
                "mtime_unix",
                "indexed_at_unix",
                "body_text_excerpt",
                "svg_count",
                "has_canvas",
                "has_form",
                "has_animation",
                "has_details",
                "has_math",
                "has_drag",
                "js_loc",
                "css_loc",
                "table_count",
                "code_block_count",
                "word_count",
                "longread",
                "tags_csv",
                "created_unix",
                "task_done",
                "task_total",
                "kb_summary",
            ]))
            .limit(paths.len())
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance get_by_source_paths: {e}")))?;
        let batches: Vec<_> = stream.try_collect().await.map_err(|e| {
            crate::Error::Storage(format!("lance get_by_source_paths collect: {e}"))
        })?;
        Ok(batches_to_summaries(&batches, &self.decode_skips))
    }

    /// Unfiltered scan returning up to `limit` doc summaries. Used by the
    /// SPA gallery's catch-all view (no search query, just enumerate).
    /// Lance has no offset on `query()` in 4.0.0, so pagination is "fetch
    /// up to N at once" — v0.1 caps at a few hundred which is enough for
    /// typical kbs. Larger corpora should use search.
    ///
    /// v0.3: defaults to NOT include atlas coords (saves response bytes
    /// + scan effort). Use `list_docs_with_atlas` to opt in.
    pub async fn list_docs(&self, limit: u32) -> Result<Vec<DocSummary>> {
        self.list_docs_inner(limit, false).await
    }

    /// v0.3: same as `list_docs` but also reads the atlas_x/atlas_y/
    /// atlas_cluster columns. Used by `?include=atlas` on the docs route.
    pub async fn list_docs_with_atlas(&self, limit: u32) -> Result<Vec<DocSummary>> {
        self.list_docs_inner(limit, true).await
    }

    async fn list_docs_inner(&self, limit: u32, include_atlas: bool) -> Result<Vec<DocSummary>> {
        // v0.6 B1: every list_docs query carries the card-feeding fields
        // so the SPA gallery doesn't need a second round-trip per doc.
        // The columns are all present in the existing schema; this is
        // a SELECT widening, not a migration.
        let mut cols: Vec<&str> = vec![
            "id",
            "title",
            "path",
            "kb_category",
            "kb_status",
            "kb_severity",
            "mtime_unix",
            "indexed_at_unix",
            "body_text_excerpt",
            "svg_count",
            "has_canvas",
            "has_form",
            "has_animation",
            "has_details",
            "has_math",
            "has_drag",
            "js_loc",
            "css_loc",
            "table_count",
            "code_block_count",
            "word_count",
            "longread",
            "tags_csv",
            // v0.9 M7 — memory metas, so the recall endpoint's empty-query
            // timeline path (which uses list_docs) carries real salience /
            // decay instead of defaulting. Harmless for the gallery.
            "kb_salience",
            "kb_decay",
            "kb_supersedes",
            // v0.14 S1 — origin session id, used by
            // `/api/sessions/{sid}/memories` to filter the recall stream.
            "kb_session",
            // RA4 — one-line summary for the recall empty-query timeline.
            "kb_summary",
            // MI-W3.3a / MI-W3.4 — surfaced on census + the empty-query
            // recall timeline.
            "kb_memory_type",
            "kb_source",
            // CT-A1 (U3 parse-back) — surfaced on census + the empty-query
            // recall timeline, same as `kb_memory_type`/`kb_source` above.
            "kb_author",
            "kb_source_kb",
            "kb_source_artifact",
            "kb_source_anchor",
            // v0.15 — filesystem btime, used by the gallery's "created"
            // sort. Cheap to carry; NULL on rows indexed before v0.15.
            "created_unix",
            // N-track — task-list progress (NULL on non-task artifacts /
            // rows indexed before v17); the notes views read these.
            "task_done",
            "task_total",
        ];
        if include_atlas {
            cols.extend_from_slice(&["atlas_x", "atlas_y", "atlas_cluster"]);
        }
        // Fetch the full slim projection (no embeddings, no body), then
        // sort by `indexed_at_unix DESC` in memory before truncating to
        // `limit`. Lance's `Query::limit` returns rows in arbitrary
        // storage order, which made the dropped tail nondeterministic —
        // entire folders could vanish from the SPA gallery when the kb
        // grew past the SPA's request cap. The projection is small (no
        // body/embedding columns), so the full scan stays cheap up to
        // tens of thousands of rows.
        let stream = self
            .table
            .query()
            .select(Select::columns(&cols))
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance list_docs: {e}")))?;

        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;

        let mut summaries = batches_to_summaries(&batches, &self.decode_skips);
        // GC-B1 — id tiebreak: a bulk reindex commonly lands many rows at
        // the same `indexed_at_unix` second, and this order IS the served
        // gallery row-set (`gallery_snapshot` memoizes it verbatim, root
        // invariant #15) — an unbroken tie rides physical scan order and
        // can reorder the gallery across restarts/compaction.
        summaries.sort_by(|a, b| {
            b.indexed_at_unix
                .unwrap_or(i64::MIN)
                .cmp(&a.indexed_at_unix.unwrap_or(i64::MIN))
                .then_with(|| a.id.cmp(&b.id))
        });
        summaries.truncate(limit as usize);
        Ok(summaries)
    }

    /// Schema-evolution helper. Topic 01 §Decisions: `add_columns` is the
    /// safe path for new nullable facets. Spike-lance confirmed bug #3136
    /// is not present in lance 4.0.0. Not used in v0.0.1 — exposed for v0.1+.
    #[allow(dead_code)]
    pub async fn add_nullable_string_column(&self, name: &str) -> Result<()> {
        self.table
            .add_columns()
            .transform(NewColumnTransform::SqlExpressions(vec![(
                name.to_string(),
                "CAST(NULL AS STRING)".to_string(),
            )]))
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance add_columns: {e}")))?;
        Ok(())
    }

    /// v0.3: write atlas coordinates for a batch of artifacts. Per-row
    /// `update` is used (lance 4.0.0 doesn't expose `merge_insert` as
    /// a typed builder we can drive with arrays of literals). Cost
    /// scales linearly with rows; for ≤1000 row recomputes (the v0.3
    /// debounce target) the total is sub-second on local SSD.
    ///
    /// M2: rows whose `x` or `y` aren't finite (NaN / ±Inf) are
    /// SKIPPED. The pre-fix code formatted them as `CAST(NaN AS FLOAT)`
    /// or `CAST(inf AS FLOAT)` and either failed mid-batch (aborting the
    /// whole atlas update, leaving stale coords for the rest) or wrote
    /// the literal into lance and broke downstream SQL filters.
    /// `compute_layout` SHOULD never produce non-finite outputs from
    /// finite inputs; this is a defensive guard for embeddings that
    /// were themselves NaN (rare, but documented in fastembed for
    /// empty inputs).
    pub async fn update_atlas(&self, rows: &[(String, f32, f32, i16)]) -> Result<()> {
        for (id, x, y, cluster) in rows {
            if !x.is_finite() || !y.is_finite() {
                tracing::warn!(
                    artifact_id = %id,
                    x = %x,
                    y = %y,
                    "atlas update: skipping non-finite coords"
                );
                continue;
            }
            self.table
                .update()
                .only_if(filter_eq("id", id))
                .column("atlas_x", format!("CAST({x} AS FLOAT)"))
                .column("atlas_y", format!("CAST({y} AS FLOAT)"))
                .column("atlas_cluster", format!("CAST({cluster} AS SMALLINT)"))
                .execute()
                .await
                .map_err(|e| crate::Error::Storage(format!("lance update atlas: {e}")))?;
        }
        Ok(())
    }

    /// Heal a stale `mtime_unix` for one row WITHOUT touching its
    /// embedding, content, or any other column. The reconcile producer-side
    /// dedup (indexer) skips re-emitting a file only when its on-disk mtime
    /// equals this stored value; a file touched-but-unchanged (mtime bumped,
    /// bytes identical — e.g. a `git checkout` in the source repo) otherwise
    /// has disk_mtime != stored, so reconcile emits a `watch.modify` for it
    /// on EVERY pass — and the content-hash gate then drops the re-index
    /// without rewriting the Doc, so the mismatch never heals and the SSE
    /// event ring floods (the SPA pegs the CPU draining the replay).
    /// Persisting the disk mtime here is self-limiting: the next reconcile
    /// pass sees disk == stored and stops emitting. Same `table.update()`
    /// by-id surface as [`Self::update_atlas`], so the embedding column is
    /// preserved.
    pub async fn touch_mtime(&self, id: &str, mtime_unix: i64) -> Result<()> {
        self.table
            .update()
            .only_if(filter_eq("id", id))
            .column("mtime_unix", format!("CAST({mtime_unix} AS BIGINT)"))
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance touch_mtime: {e}")))?;
        Ok(())
    }

    /// v0.3 G4 — set the embedding column to NULL for every row in the
    /// table. The indexer's content-hash gate normally skips re-embeds
    /// when the file content hasn't changed; clearing embeddings forces
    /// the next reindex pass to repopulate them. Used by `kb model set
    /// --in-place` for same-dim model swaps so we don't have to drop +
    /// re-create the lance table.
    ///
    /// Limitation: same-dim swaps only. Different-dim model swaps mean
    /// the new model embeds at a different width than `self.dim`; the
    /// `--in-place` CLI rejects this and `Storage::open` would refuse
    /// to re-open the kb anyway (dim-mismatch guard). For a different-
    /// dim swap, stand up a new kb against the same source dir — that
    /// pattern is what the bake-off bench is built around.
    pub async fn clear_embeddings(&self) -> Result<()> {
        self.table
            .update()
            .column("embedding", "NULL")
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance clear_embeddings: {e}")))?;
        // Embeddings changed — the IVF-PQ index is stale (FTS columns untouched).
        self.vector_needs_build
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// S5 admin — wipe every row from the lance table. Atlas coords +
    /// embeddings + capability flags + edges-in-lance all vanish. The
    /// table itself stays (schema preserved), so a subsequent reindex
    /// re-populates it on the existing layout without a re-open.
    /// `delete_by_path`/`delete_by_id` use the same `table.delete()`
    /// surface; a tautology predicate matches every row.
    pub async fn delete_all_rows(&self) -> Result<()> {
        self.table
            .delete("id IS NOT NULL")
            .await
            .map_err(|e| crate::Error::Storage(format!("lance delete_all: {e}")))?;
        self.mark_search_indexes_dirty();
        Ok(())
    }

    /// v0.3: read all (id, embedding) pairs for atlas recompute. Skips
    /// rows where embedding is null (the indexer hasn't reached them
    /// yet, or the kb runs without an embedder).
    ///
    /// GC-B1: sorted by id before returning. This is a plain `select`
    /// with no `only_if`/order clause — lance returns physical scan
    /// order, which is unsorted WalkDir/merge_insert insertion order, not
    /// id- or path-sorted (docs/research/
    /// atlas-input-order-determinism-2026-07.html §2). `compute_layout`
    /// (the sole consumer, via `atlas::recompute_for_kb_with` /
    /// `recluster_for_kb_with`) is proven order-sensitive — a reverse
    /// permutation moves every point — so this fn's own contract is now
    /// "id-ascending", not "whatever lance handed back".
    pub async fn list_embeddings(&self) -> Result<Vec<EmbeddingPair>> {
        let stream = self
            .table
            .query()
            .select(Select::columns(&["id", "embedding"]))
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance list_embeddings: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        let mut pairs = batches_to_embeddings(&batches, &self.decode_skips);
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(pairs)
    }

    /// v0.16 — every non-null `(id, content_hash, mtime_unix)` triple
    /// across the table. The indexer pre-populates its in-memory dedup
    /// cache from this at startup so `watch.create` envelopes from the
    /// watcher's initial walk skip the embed pipeline when the on-disk
    /// bytes match what was last indexed. Rows indexed before v0.16 carry
    /// NULL and are filtered out (they'll get a hash on their next
    /// embed pass).
    ///
    /// v0.24 SC1 — the stored `mtime_unix` rides along (nullable — rows
    /// may predate the column) so the dedup pre-gate's mtime heal can
    /// compare disk vs stored and fire ONLY on a genuine touch. The
    /// unconditional heal turned every duplicate emission of an unchanged
    /// file (restart initial-walk, reconcile backlog twins) into a
    /// full-table-scan `touch_mtime` UPDATE + manifest commit — the
    /// restart-with-backlog quadratic storm the 20k scale lab measured.
    pub async fn list_content_hashes(&self) -> Result<Vec<(String, String, Option<i64>)>> {
        use arrow::array::Int64Array;
        let stream = self
            .table
            .query()
            .select(Select::columns(&["id", "content_hash", "mtime_unix"]))
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance list_content_hashes: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        let mut out = Vec::new();
        for batch in &batches {
            let (Some(id_col), Some(hash_col)) = (
                batch.column_by_name("id"),
                batch.column_by_name("content_hash"),
            ) else {
                continue;
            };
            let ids = id_col.as_string::<i32>();
            let hashes = hash_col.as_string::<i32>();
            let mtimes = batch
                .column_by_name("mtime_unix")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            for i in 0..batch.num_rows() {
                if hashes.is_null(i) || ids.is_null(i) {
                    continue;
                }
                let h = hashes.value(i).trim();
                if h.is_empty() {
                    continue;
                }
                let mt = mtimes.and_then(|a| (!a.is_null(i)).then(|| a.value(i)));
                out.push((ids.value(i).to_string(), h.to_string(), mt));
            }
        }
        Ok(out)
    }

    /// v0.33 X2 — narrow projection for the `doc_first_seen` bring-up seed:
    /// `(id, created_unix, mtime_unix, indexed_at_unix)`. The seed uses
    /// `coalesce(created, mtime, indexed_at)` so existing corpora keep their
    /// current created-sort order; new docs get true first-index time from
    /// the indexer tail.
    pub async fn list_first_seen_seed_rows(
        &self,
    ) -> Result<Vec<(String, Option<i64>, Option<i64>, Option<i64>)>> {
        use arrow::array::Int64Array;
        let stream = self
            .table
            .query()
            .select(Select::columns(&[
                "id",
                "created_unix",
                "mtime_unix",
                "indexed_at_unix",
            ]))
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance list_first_seen_seed_rows: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        let mut out = Vec::new();
        for batch in &batches {
            let Some(id_col) = batch.column_by_name("id") else {
                continue;
            };
            let ids = id_col.as_string::<i32>();
            let created = batch
                .column_by_name("created_unix")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            let mtimes = batch
                .column_by_name("mtime_unix")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            let indexed = batch
                .column_by_name("indexed_at_unix")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            for i in 0..batch.num_rows() {
                if ids.is_null(i) {
                    continue;
                }
                let c = created.and_then(|a| (!a.is_null(i)).then(|| a.value(i)));
                let m = mtimes.and_then(|a| (!a.is_null(i)).then(|| a.value(i)));
                let ix = indexed.and_then(|a| (!a.is_null(i)).then(|| a.value(i)));
                out.push((ids.value(i).to_string(), c, m, ix));
            }
        }
        Ok(out)
    }

    /// Reconcile projection — every row's `(path, mtime_unix)` and nothing
    /// else. The periodic reconcile safety net only needs the stored path
    /// (to drive the vanished-file delete pass) and the stored mtime (to
    /// feed the producer-side dedup map). Selecting just these two columns
    /// avoids widening the scan to the full `list_docs` slim projection
    /// (title/tags/body-excerpt/atlas, ~33 columns) and the in-memory sort
    /// that `list_docs` does — neither of which reconcile consumes — so the
    /// per-pass actor occupancy stays proportional to the row count, not the
    /// payload width. `mtime_unix` is nullable (rows may predate the column);
    /// reconcile treats a `None` as "no dedup mtime", which degrades to
    /// "emit anyway", never a wrong skip.
    pub async fn list_reconcile_rows(&self) -> Result<Vec<(String, String, Option<i64>)>> {
        use arrow::array::Int64Array;
        let stream = self
            .table
            .query()
            .select(Select::columns(&["id", "path", "mtime_unix"]))
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance list_reconcile_rows: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        let mut out = Vec::new();
        for batch in &batches {
            let Some(path_col) = batch.column_by_name("path") else {
                continue;
            };
            let Some(id_col) = batch.column_by_name("id") else {
                continue;
            };
            let ids = id_col.as_string::<i32>();
            let paths = path_col.as_string::<i32>();
            let mtimes = batch
                .column_by_name("mtime_unix")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            for i in 0..batch.num_rows() {
                if paths.is_null(i) || ids.is_null(i) {
                    continue;
                }
                let mt = mtimes.and_then(|a| (!a.is_null(i)).then(|| a.value(i)));
                out.push((ids.value(i).to_string(), paths.value(i).to_string(), mt));
            }
        }
        Ok(out)
    }

    /// v0.9 M3 — every non-null, non-empty `kb_supersedes` value across
    /// the table (the ids some memory claims to replace). Recall turns
    /// these into a tombstone set so a superseded memory drops from
    /// results even when the superseding memory wasn't retrieved in the
    /// hit window. Memory corpora are small; the scalar scan is cheap.
    pub async fn list_supersede_targets(&self) -> Result<Vec<String>> {
        let stream = self
            .table
            .query()
            .select(Select::columns(&["kb_supersedes"]))
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance list_supersede_targets: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        let mut out = Vec::new();
        for batch in &batches {
            if let Some(col) = batch.column_by_name("kb_supersedes") {
                let arr = col.as_string::<i32>();
                for i in 0..batch.num_rows() {
                    if !arr.is_null(i) {
                        let v = arr.value(i).trim();
                        if !v.is_empty() {
                            out.push(v.to_string());
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// MI-W2.4a — `kb memory log <id>`'s lineage-walk projection: just
    /// enough to render one hop of a supersede chain (title, the forward
    /// pointer, the tombstone flag, and the two timestamps a chain node
    /// needs) without pulling the FULL `get_by_id` shape (body excerpt,
    /// caps, tag csv, …) that a lineage walk never renders. `path` and
    /// `kb_category` MUST stay in this list even though the lineage walk
    /// itself never reads them — `batches_to_summaries` hard-requires all
    /// four of `id`/`title`/`path`/`kb_category` to decode ANY row at all
    /// (a narrower projection silently decodes to zero rows, counted as a
    /// decode-skip, not an error).
    const LINEAGE_PROJECTION: &'static [&'static str] = &[
        "id",
        "title",
        "path",
        "kb_category",
        "kb_status",
        "kb_supersedes",
        "created_unix",
        "mtime_unix",
    ];

    /// MI-W2.4a — one hop of the FORWARD direction: what does `id` itself
    /// say it supersedes (`kb_supersedes`)? A sibling of `get_by_id`
    /// narrowed to [`Self::LINEAGE_PROJECTION`] rather than widening the
    /// widely-used general lookup.
    pub async fn lineage_by_id(&self, id: &str) -> Result<Option<DocSummary>> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Ok(None);
        }
        let stream = self
            .table
            .query()
            .only_if(format!("id = '{id}'"))
            .select(Select::columns(Self::LINEAGE_PROJECTION))
            .limit(1)
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance lineage_by_id: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        Ok(batches_to_summaries(&batches, &self.decode_skips)
            .into_iter()
            .next())
    }

    /// MI-W2.4a — one hop of the REVERSE direction: which row(s) claim
    /// `kb_supersedes == target_id`? Ordinarily exactly one (a memory
    /// supersedes at most one predecessor by convention), but nothing
    /// enforces that, so this returns every match rather than silently
    /// picking one — ambiguous-chain handling is the CALLER's decision.
    /// `routes::memory::lineage` walks the first entry after sorting by
    /// `created_unix` ascending (nulls first) then `id`, so a genuine fork
    /// is at least a DETERMINISTIC choice rather than depending on lance's
    /// physical row order.
    pub async fn find_superseded_by(&self, target_id: &str) -> Result<Vec<DocSummary>> {
        if target_id.is_empty()
            || !target_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Ok(Vec::new());
        }
        let escaped = escape_literal(target_id);
        let stream = self
            .table
            .query()
            .only_if(format!("kb_supersedes = '{escaped}'"))
            .select(Select::columns(Self::LINEAGE_PROJECTION))
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance find_superseded_by: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        let mut out = batches_to_summaries(&batches, &self.decode_skips);
        out.sort_by(|a, b| {
            a.created_unix
                .cmp(&b.created_unix)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(out)
    }

    /// v0.14 S3 — count rows whose `kb_session` matches a session id,
    /// **excluding the memory-session transcript itself**. Backs the
    /// `memory_count` field on /api/sessions: callers want "how many
    /// agent-curated memories did this session produce", not "how many
    /// docs reference this session id" (which would always include the
    /// transcript row that carries the meta verbatim). Single SQL
    /// filter through lance's `count_rows`; the session id is
    /// ASCII-safe (UUID-shaped) but we still escape embedded
    /// apostrophes defensively against operator-supplied ids.
    pub async fn count_docs_with_kb_session(&self, session_id: &str) -> Result<u64> {
        if session_id.is_empty() {
            return Ok(0);
        }
        let escaped = escape_literal(session_id);
        let n = self
            .table
            .count_rows(Some(format!(
                "kb_session = '{escaped}' AND \
                 (kb_category IS NULL OR kb_category != 'memory-session')"
            )))
            .await
            .map_err(|e| crate::Error::Storage(format!("lance count by kb_session: {e}")))?;
        Ok(n as u64)
    }

    /// Perf sweep 2026-07 — batched sibling of
    /// `count_docs_with_kb_session`: group-count every requested session
    /// id in ONE narrow projection scan instead of one `count_rows`
    /// filter scan per id. Backs the /api/sessions list page, where the
    /// per-row × per-corpus count fan-out dominated latency. Same
    /// semantics as the single-id count: the memory-session transcript
    /// rows themselves are excluded, and counts are capture-independent
    /// (#11 — `kb_session` is a doc column, not a capture row). Mirrors
    /// the `list_content_hashes` / `list_reconcile_rows` shape: predicate
    /// pushdown + a two-column projection, then in-memory grouping. Ids
    /// with zero matching docs are simply absent from the map (callers
    /// default to 0).
    pub async fn count_docs_by_kb_session(
        &self,
        session_ids: &[String],
    ) -> Result<std::collections::HashMap<String, u64>> {
        let mut out = std::collections::HashMap::new();
        if session_ids.is_empty() {
            return Ok(out);
        }
        let wanted: std::collections::HashSet<&str> = session_ids
            .iter()
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .collect();
        if wanted.is_empty() {
            return Ok(out);
        }
        let stream = self
            .table
            .query()
            .only_if(
                "kb_session IS NOT NULL AND \
                 (kb_category IS NULL OR kb_category != 'memory-session')",
            )
            .select(Select::columns(&["kb_session"]))
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance count_docs_by_kb_session: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        for batch in &batches {
            let Some(col) = batch.column_by_name("kb_session") else {
                continue;
            };
            let arr = col.as_string::<i32>();
            for i in 0..batch.num_rows() {
                if arr.is_null(i) {
                    continue;
                }
                let sid = arr.value(i);
                if wanted.contains(sid) {
                    *out.entry(sid.to_string()).or_insert(0) += 1;
                }
            }
        }
        Ok(out)
    }

    /// CT-F5 — every distinct non-empty `kb_session` value in this corpus,
    /// with how many docs carry it. Backs the orphan-kb_session SLO
    /// indicator: the caller asks each kb's `sessions` table which of these
    /// ids it knows, and the docs behind the ids nobody knows are the
    /// orphans.
    ///
    /// TWO deliberate differences from [`Self::count_docs_by_kb_session`]:
    ///
    /// 1. It takes NO wanted-set — the whole point is to discover ids the
    ///    caller does not already have.
    /// 2. It does NOT exclude `memory-session` transcripts. The sibling reads
    ///    exclude them because they answer "how many MEMORIES did this
    ///    session produce" and the transcript would always self-count. Here a
    ///    transcript whose own `kb_session` resolves to nothing is precisely
    ///    the signal wanted: either its capture never landed, or the hint is
    ///    dirty (invariant #11's truncated-meta failure mode, which once
    ///    broke `claude -r` corpus-wide and was invisible from every other
    ///    surface). Both are a doc claiming an origin nothing can produce.
    ///
    /// Same predicate-pushdown + one-column projection shape as
    /// `count_docs_by_kb_session`; grouping happens in memory.
    pub async fn kb_session_doc_counts(&self) -> Result<std::collections::HashMap<String, u64>> {
        let mut out = std::collections::HashMap::new();
        let stream = self
            .table
            .query()
            .only_if("kb_session IS NOT NULL")
            .select(Select::columns(&["kb_session"]))
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance kb_session_doc_counts: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        for batch in &batches {
            let Some(col) = batch.column_by_name("kb_session") else {
                continue;
            };
            let arr = col.as_string::<i32>();
            for i in 0..batch.num_rows() {
                if arr.is_null(i) {
                    continue;
                }
                let sid = arr.value(i);
                // An empty string is not a session claim — the capture hook's
                // known failure mode writes one, and counting it as an orphan
                // would report a doc that never named a session at all.
                if sid.is_empty() {
                    continue;
                }
                *out.entry(sid.to_string()).or_insert(0) += 1;
            }
        }
        Ok(out)
    }

    /// v0.14 S3 — list rows whose `kb_session` matches a session id,
    /// **excluding the memory-session transcript itself** (see
    /// `count_docs_with_kb_session` for why). Returns the same slim
    /// `DocSummary` projection the recall + list routes use, so the
    /// /api/sessions/{sid}/memories endpoint can reuse the existing
    /// rendering shape.
    pub async fn list_docs_with_kb_session(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<DocSummary>> {
        if session_id.is_empty() {
            return Ok(Vec::new());
        }
        let escaped = escape_literal(session_id);
        let stream = self
            .table
            .query()
            .only_if(format!(
                "kb_session = '{escaped}' AND \
                 (kb_category IS NULL OR kb_category != 'memory-session')"
            ))
            .select(Select::columns(&[
                "id",
                "title",
                "path",
                "kb_category",
                "kb_status",
                "kb_severity",
                "mtime_unix",
                "indexed_at_unix",
                "body_text_excerpt",
                "tags_csv",
                "kb_salience",
                "kb_decay",
                "kb_supersedes",
                "kb_session",
                "kb_summary",
            ]))
            .limit(limit as usize)
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance list by kb_session: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        Ok(batches_to_summaries(&batches, &self.decode_skips))
    }

    /// CT-A1 (U3 parse-back) — the reverse of `MemoryProvenance`: list rows
    /// whose `kb_source_kb`/`kb_source_artifact` name a given origin
    /// artifact. Mirrors `list_docs_with_kb_session`'s shape (a single
    /// equality filter, slim `DocSummary` projection). Filters on BOTH
    /// `source_kb` and `source_artifact` — an artifact id alone is only
    /// unique WITHIN its own kb's source root (invariant #27), so the pair
    /// is what actually identifies an origin doc uniquely across the
    /// daemon. Backs `GET /api/kb/{kb}/docs/{id}/memories-from`.
    pub async fn list_docs_with_kb_source_artifact(
        &self,
        source_kb: &str,
        source_artifact: &str,
        limit: u32,
    ) -> Result<Vec<DocSummary>> {
        if source_kb.is_empty() || source_artifact.is_empty() {
            return Ok(Vec::new());
        }
        let kb_escaped = escape_literal(source_kb);
        let id_escaped = escape_literal(source_artifact);
        let stream = self
            .table
            .query()
            .only_if(format!(
                "kb_source_kb = '{kb_escaped}' AND kb_source_artifact = '{id_escaped}'"
            ))
            .select(Select::columns(&[
                "id",
                "title",
                "path",
                "kb_category",
                "body_text_excerpt",
                "mtime_unix",
                "indexed_at_unix",
                "created_unix",
                "kb_summary",
                "kb_author",
                "kb_source_kb",
                "kb_source_artifact",
                "kb_source_anchor",
            ]))
            .limit(limit as usize)
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance list by kb_source_artifact: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        Ok(batches_to_summaries(&batches, &self.decode_skips))
    }

    /// N-track — list rows with `kb_category = 'note'`, slim projection
    /// (incl. `task_done`/`task_total`). Backs the per-kb + cross-kb notes
    /// list so the route filters server-side instead of scanning the whole
    /// gallery memo. Newest-first by `mtime_unix` is left to the route
    /// (it has the source root to derive folder + the cross-kb merge).
    pub async fn list_notes(&self, limit: u32) -> Result<Vec<DocSummary>> {
        let stream = self
            .table
            .query()
            .only_if("kb_category = 'note'")
            .select(Select::columns(&[
                "id",
                "title",
                "path",
                "kb_category",
                "kb_status",
                "kb_severity",
                "mtime_unix",
                "indexed_at_unix",
                "body_text_excerpt",
                "tags_csv",
                "task_done",
                "task_total",
            ]))
            .limit(limit as usize)
            .execute()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance list_notes: {e}")))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| crate::Error::Storage(format!("lance stream collect: {e}")))?;
        // A note is Markdown + `kb-category: note`; the `kb_category='note'`
        // filter above is the cheap lance prefilter, but `note` is also a
        // legit content category on HTML artifacts, so post-filter to true
        // (Markdown) notes — see `notes::is_note`. Note volume is tiny.
        Ok(batches_to_summaries(&batches, &self.decode_skips)
            .into_iter()
            .filter(|d| crate::indexer::is_markdown(std::path::Path::new(&d.path)))
            .collect())
    }

    /// Direct connection accessor, for the storage actor's read-only paths.
    #[allow(dead_code)]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

/// GC-B1 — stable secondary sort for the ranked read paths (bm25/vector/
/// hybrid/chunk-resolve). Higher score wins; on an exact tie (common —
/// identical text, duplicate embeddings, or the empty-query/zero-score
/// case) the artifact id breaks it ascending. `DocSummary::id` is a
/// content-hash derived from the source-relative path (root invariant
/// #27), so this tie-break is itself corpus-content-derived and
/// reproducible across machines — unlike the lance physical scan order it
/// replaces (docs/research/search-determinism-settling-window-2026-07.html).
/// `sort_by` (not `sort_unstable_by`) so a caller that already produced a
/// meaningfully-ordered `score: None` sequence (there are none on these
/// paths today, but cheap insurance) keeps its relative order too.
fn rank_sort(hits: &mut [DocSummary]) {
    hits.sort_by(|a, b| {
        b.score
            .unwrap_or(f32::MIN)
            .partial_cmp(&a.score.unwrap_or(f32::MIN))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn batches_to_summaries(
    batches: &[arrow::record_batch::RecordBatch],
    skips: &DecodeSkipCounter,
) -> Vec<DocSummary> {
    use arrow::array::{Float32Array, Int16Array, Int64Array, StringArray, UInt32Array};
    let mut hits = Vec::new();
    for batch in batches {
        // R1b — typed decode of the four REQUIRED string columns. Previously a
        // `.unwrap().as_string()` on any of them panicked (missing column or a
        // type drift) and killed the storage-actor task. This is an infallible
        // summarizer (returns a plain Vec), so a malformed batch is skipped with
        // a warn rather than aborting the whole read — matching the tolerant
        // opt-in idiom the rest of this fn already uses for optional columns.
        let (Some(ids), Some(titles), Some(paths), Some(categories)) = (
            batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>()),
            batch
                .column_by_name("title")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>()),
            batch
                .column_by_name("path")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>()),
            batch
                .column_by_name("kb_category")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>()),
        ) else {
            skips.note(
                "batches_to_summaries: a batch is missing a required string column \
                 (id/title/path/kb_category) or one drifted type; skipping the batch",
            );
            continue;
        };
        // Atlas columns may be absent (caller didn't ask) — handle both.
        let atlas_x = batch
            .column_by_name("atlas_x")
            .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
        let atlas_y = batch
            .column_by_name("atlas_y")
            .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
        let atlas_cluster = batch
            .column_by_name("atlas_cluster")
            .and_then(|c| c.as_any().downcast_ref::<Int16Array>());

        // v0.6 B1 — same opt-in pattern as the atlas columns: read if
        // present, leave None otherwise. Older callers (search hits)
        // still get the slim DocSummary they expect.
        let mtime = batch
            .column_by_name("mtime_unix")
            .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
        let indexed_at = batch
            .column_by_name("indexed_at_unix")
            .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
        let summary = batch
            .column_by_name("body_text_excerpt")
            .map(|c| c.as_string::<i32>());
        let svg_count = batch
            .column_by_name("svg_count")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let has_canvas = batch
            .column_by_name("has_canvas")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let has_form = batch
            .column_by_name("has_form")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let has_animation = batch
            .column_by_name("has_animation")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let has_details = batch
            .column_by_name("has_details")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let has_math = batch
            .column_by_name("has_math")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let has_drag = batch
            .column_by_name("has_drag")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let js_loc = batch.column_by_name("js_loc").map(|c| c.as_string::<i32>());
        let css_loc = batch
            .column_by_name("css_loc")
            .map(|c| c.as_string::<i32>());
        let table_count = batch
            .column_by_name("table_count")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let code_block_count = batch
            .column_by_name("code_block_count")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let word_count = batch
            .column_by_name("word_count")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let longread = batch
            .column_by_name("longread")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let tags_csv = batch
            .column_by_name("tags_csv")
            .map(|c| c.as_string::<i32>());
        // v0.7 S1 — optional like the rest: callers that didn't select
        // them (or rows from before the migration) leave them None.
        let kb_statuses = batch
            .column_by_name("kb_status")
            .map(|c| c.as_string::<i32>());
        let kb_severities = batch
            .column_by_name("kb_severity")
            .map(|c| c.as_string::<i32>());
        // v0.9 M1 — memory metas. Same opt-in pattern: present only when
        // the caller's projection selected them.
        let kb_saliences = batch
            .column_by_name("kb_salience")
            .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
        let kb_decays = batch
            .column_by_name("kb_decay")
            .map(|c| c.as_string::<i32>());
        let kb_supersedes_col = batch
            .column_by_name("kb_supersedes")
            .map(|c| c.as_string::<i32>());
        // v0.14 S1 — origin session id. Same opt-in pattern.
        let kb_sessions = batch
            .column_by_name("kb_session")
            .map(|c| c.as_string::<i32>());
        // RA4 — one-line summary. Same opt-in pattern.
        let kb_summaries = batch
            .column_by_name("kb_summary")
            .map(|c| c.as_string::<i32>());
        // MI-W3.3a / MI-W3.4 — memory type + trust source. Same opt-in
        // pattern: present only when the caller's projection selected them.
        let kb_memory_types = batch
            .column_by_name("kb_memory_type")
            .map(|c| c.as_string::<i32>());
        let kb_sources = batch
            .column_by_name("kb_source")
            .map(|c| c.as_string::<i32>());
        // CT-A1 (U3 parse-back) — highlight provenance. Same opt-in pattern:
        // present only when the caller's projection selected them.
        let kb_authors = batch
            .column_by_name("kb_author")
            .map(|c| c.as_string::<i32>());
        let kb_source_kbs = batch
            .column_by_name("kb_source_kb")
            .map(|c| c.as_string::<i32>());
        let kb_source_artifacts = batch
            .column_by_name("kb_source_artifact")
            .map(|c| c.as_string::<i32>());
        let kb_source_anchors = batch
            .column_by_name("kb_source_anchor")
            .map(|c| c.as_string::<i32>());
        // v0.15 — filesystem btime. Same opt-in pattern; nullable
        // because the column is itself nullable (btime-less filesystems,
        // pre-v15 rows).
        let created_unixes = batch
            .column_by_name("created_unix")
            .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
        // N-track — task-list progress. Same opt-in pattern: present only
        // when the caller's projection selected them (the notes list path).
        let task_dones = batch
            .column_by_name("task_done")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let task_totals = batch
            .column_by_name("task_total")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        // SQ1 — relevance score columns. lance auto-projects `_score`
        // (BM25, higher=better) and `_distance` (vector cosine,
        // lower=better) on the respective arms even under an explicit
        // Select; the combined hybrid path yields `_relevance_score`
        // (higher=better). All opt-in: absent on list/gallery
        // projections, so `score` stays None there.
        let relevance_score = batch
            .column_by_name("_relevance_score")
            .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
        let bm25_score = batch
            .column_by_name("_score")
            .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
        let vec_distance = batch
            .column_by_name("_distance")
            .and_then(|c| c.as_any().downcast_ref::<Float32Array>());

        for i in 0..batch.num_rows() {
            hits.push(DocSummary {
                id: ids.value(i).to_string(),
                title: titles.value(i).to_string(),
                path: paths.value(i).to_string(),
                kb_category: if categories.is_null(i) {
                    None
                } else {
                    Some(categories.value(i).to_string())
                },
                atlas_x: atlas_x.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                atlas_y: atlas_y.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                atlas_cluster: atlas_cluster.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                // X1 — guard the null slot like every other nullable
                // column below; a bare `.value(i)` reads a null as 0
                // (epoch 1970) instead of None, which then sorts as a real
                // 1970 timestamp rather than "missing" and renders as
                // "55y ago" instead of blank.
                mtime_unix: mtime.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                indexed_at_unix: indexed_at.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                summary: summary.and_then(|a| {
                    if a.is_null(i) {
                        None
                    } else {
                        let s = a.value(i);
                        if s.is_empty() {
                            None
                        } else {
                            Some(s.to_string())
                        }
                    }
                }),
                svg_count: svg_count.map(|a| a.value(i)),
                has_canvas: has_canvas.map(|a| a.value(i)),
                has_form: has_form.map(|a| a.value(i)),
                has_animation: has_animation.map(|a| a.value(i)),
                has_details: has_details.map(|a| a.value(i)),
                has_math: has_math.map(|a| a.value(i)),
                has_drag: has_drag.map(|a| a.value(i)),
                js_loc: js_loc.and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                css_loc: css_loc.and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                table_count: table_count.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                code_block_count: code_block_count
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                word_count: word_count.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                longread: longread.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                tags: tags_csv
                    .and_then(|a| {
                        if a.is_null(i) {
                            None
                        } else {
                            Some(
                                a.value(i)
                                    .split(',')
                                    .map(|s| s.trim())
                                    .filter(|s| !s.is_empty())
                                    .map(|s| s.to_string())
                                    .collect::<Vec<_>>(),
                            )
                        }
                    })
                    .unwrap_or_default(),
                kb_status: kb_statuses
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                kb_severity: kb_severities
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                kb_salience: kb_saliences.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                kb_decay: kb_decays.and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                kb_supersedes: kb_supersedes_col
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                kb_session: kb_sessions
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                kb_summary: kb_summaries
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                kb_memory_type: kb_memory_types
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                kb_source: kb_sources.and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                kb_author: kb_authors.and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                kb_source_kb: kb_source_kbs
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                kb_source_artifact: kb_source_artifacts
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                kb_source_anchor: kb_source_anchors
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i).to_string())),
                created_unix: created_unixes.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                task_done: task_dones.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                task_total: task_totals.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                // Prefer the fused hybrid score; else the BM25 score;
                // else map vector distance to a (0,1] similarity so the
                // exposed `score` is consistently higher=better.
                score: relevance_score
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i)))
                    .or_else(|| bm25_score.and_then(|a| (!a.is_null(i)).then(|| a.value(i))))
                    .or_else(|| {
                        vec_distance.and_then(|a| (!a.is_null(i)).then(|| 1.0 / (1.0 + a.value(i))))
                    }),
            });
        }
    }
    hits
}

fn batches_to_embeddings(
    batches: &[arrow::record_batch::RecordBatch],
    skips: &DecodeSkipCounter,
) -> Vec<EmbeddingPair> {
    use arrow::array::{FixedSizeListArray, Float32Array, StringArray};
    let mut out = Vec::new();
    for batch in batches {
        // R1b — typed decode of the required `id` column. Infallible collector
        // (returns a plain Vec), so skip a malformed batch with a warn, matching
        // the let-else `continue` idiom the embedding column below already uses.
        let Some(ids) = batch
            .column_by_name("id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        else {
            skips.note(
                "batches_to_embeddings: batch missing the required `id` string column \
                 or it drifted type; skipping the batch",
            );
            continue;
        };
        let Some(emb_col) = batch.column_by_name("embedding") else {
            continue;
        };
        let Some(emb) = emb_col.as_any().downcast_ref::<FixedSizeListArray>() else {
            continue;
        };
        let value_arr = emb.values();
        let Some(floats) = value_arr.as_any().downcast_ref::<Float32Array>() else {
            continue;
        };
        let value_len = emb.value_length() as usize;
        for i in 0..batch.num_rows() {
            if emb.is_null(i) {
                continue;
            }
            let start = i * value_len;
            let mut v = Vec::with_capacity(value_len);
            for j in 0..value_len {
                v.push(floats.value(start + j));
            }
            out.push((ids.value(i).to_string(), v));
        }
    }
    out
}

/// Decode full [`Doc`] rows from a `get_full_doc` projection. Best-effort:
/// a batch missing a required non-null column is skipped.
fn batch_to_full_docs(batches: &[arrow::record_batch::RecordBatch]) -> Vec<Doc> {
    use arrow::array::{
        BooleanArray, FixedSizeListArray, Float32Array, Int64Array, StringArray, UInt32Array,
    };
    let mut out = Vec::new();
    for batch in batches {
        let id = batch.column_by_name("id").map(|c| c.as_string::<i32>());
        let path = batch.column_by_name("path").map(|c| c.as_string::<i32>());
        let title = batch.column_by_name("title").map(|c| c.as_string::<i32>());
        let body = batch.column_by_name("body").map(|c| c.as_string::<i32>());
        let headings = batch
            .column_by_name("headings")
            .map(|c| c.as_string::<i32>());
        let code = batch.column_by_name("code").map(|c| c.as_string::<i32>());
        let (Some(id), Some(path), Some(title), Some(body), Some(headings), Some(code)) =
            (id, path, title, body, headings, code)
        else {
            continue;
        };
        let prompt = batch.column_by_name("prompt").map(|c| c.as_string::<i32>());
        let body_excerpt = batch
            .column_by_name("body_text_excerpt")
            .map(|c| c.as_string::<i32>());
        let emb_col = batch
            .column_by_name("embedding")
            .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>());
        let kb_category = batch
            .column_by_name("kb_category")
            .map(|c| c.as_string::<i32>());
        let prompt_size = batch
            .column_by_name("prompt_size_bytes")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let size_kb = batch
            .column_by_name("size_kb")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let js_loc = batch.column_by_name("js_loc").map(|c| c.as_string::<i32>());
        let css_loc = batch
            .column_by_name("css_loc")
            .map(|c| c.as_string::<i32>());
        let svg_count = batch
            .column_by_name("svg_count")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let has_svg = batch
            .column_by_name("has_svg")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let has_form = batch
            .column_by_name("has_form")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let has_canvas = batch
            .column_by_name("has_canvas")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let has_animation = batch
            .column_by_name("has_animation")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let has_details = batch
            .column_by_name("has_details")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let has_script = batch
            .column_by_name("has_script")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let has_drag = batch
            .column_by_name("has_drag")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let has_math = batch
            .column_by_name("has_math")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let mtime = batch
            .column_by_name("mtime_unix")
            .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
        let indexed_at = batch
            .column_by_name("indexed_at_unix")
            .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
        let table_count = batch
            .column_by_name("table_count")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let code_block_count = batch
            .column_by_name("code_block_count")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let word_count = batch
            .column_by_name("word_count")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let longread = batch
            .column_by_name("longread")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>());
        let tags_csv = batch
            .column_by_name("tags_csv")
            .map(|c| c.as_string::<i32>());
        let kb_status = batch
            .column_by_name("kb_status")
            .map(|c| c.as_string::<i32>());
        let kb_severity = batch
            .column_by_name("kb_severity")
            .map(|c| c.as_string::<i32>());
        let kb_salience = batch
            .column_by_name("kb_salience")
            .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
        let kb_decay = batch
            .column_by_name("kb_decay")
            .map(|c| c.as_string::<i32>());
        let kb_supersedes = batch
            .column_by_name("kb_supersedes")
            .map(|c| c.as_string::<i32>());
        let kb_session = batch
            .column_by_name("kb_session")
            .map(|c| c.as_string::<i32>());
        let created_unix = batch
            .column_by_name("created_unix")
            .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
        let content_hash = batch
            .column_by_name("content_hash")
            .map(|c| c.as_string::<i32>());
        let task_done = batch
            .column_by_name("task_done")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let task_total = batch
            .column_by_name("task_total")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        let kb_summary = batch
            .column_by_name("kb_summary")
            .map(|c| c.as_string::<i32>());
        let kb_memory_type = batch
            .column_by_name("kb_memory_type")
            .map(|c| c.as_string::<i32>());
        let kb_source = batch
            .column_by_name("kb_source")
            .map(|c| c.as_string::<i32>());
        let kb_author = batch
            .column_by_name("kb_author")
            .map(|c| c.as_string::<i32>());
        let kb_source_kb = batch
            .column_by_name("kb_source_kb")
            .map(|c| c.as_string::<i32>());
        let kb_source_artifact = batch
            .column_by_name("kb_source_artifact")
            .map(|c| c.as_string::<i32>());
        let kb_source_anchor = batch
            .column_by_name("kb_source_anchor")
            .map(|c| c.as_string::<i32>());

        let emb_floats = emb_col.and_then(|e| {
            e.values()
                .as_any()
                .downcast_ref::<Float32Array>()
                .map(|f| (e, f))
        });

        for i in 0..batch.num_rows() {
            let embedding = emb_floats.and_then(|(emb, floats)| {
                if emb.is_null(i) {
                    return None;
                }
                let value_len = emb.value_length() as usize;
                let start = i * value_len;
                let mut v = Vec::with_capacity(value_len);
                for j in 0..value_len {
                    v.push(floats.value(start + j));
                }
                Some(v)
            });
            let str_opt = |col: Option<&StringArray>| {
                col.and_then(|a| {
                    if a.is_null(i) {
                        None
                    } else {
                        Some(a.value(i).to_string())
                    }
                })
            };
            out.push(Doc {
                id: id.value(i).to_string(),
                path: path.value(i).to_string(),
                title: title.value(i).to_string(),
                body: body.value(i).to_string(),
                headings: headings.value(i).to_string(),
                code: code.value(i).to_string(),
                prompt: str_opt(prompt),
                body_text_excerpt: body_excerpt
                    .map(|a| {
                        if a.is_null(i) {
                            String::new()
                        } else {
                            a.value(i).to_string()
                        }
                    })
                    .unwrap_or_default(),
                embedding,
                kb_category: str_opt(kb_category),
                prompt_size_bytes: prompt_size.map(|a| a.value(i)).unwrap_or(0),
                size_kb: size_kb.map(|a| a.value(i)).unwrap_or(0),
                js_loc: js_loc
                    .map(|a| {
                        if a.is_null(i) {
                            "static".into()
                        } else {
                            a.value(i).to_string()
                        }
                    })
                    .unwrap_or_else(|| "static".into()),
                css_loc: css_loc
                    .map(|a| {
                        if a.is_null(i) {
                            "static".into()
                        } else {
                            a.value(i).to_string()
                        }
                    })
                    .unwrap_or_else(|| "static".into()),
                svg_count: svg_count.map(|a| a.value(i)).unwrap_or(0),
                has_svg: has_svg.map(|a| a.value(i)).unwrap_or(false),
                has_form: has_form.map(|a| a.value(i)).unwrap_or(false),
                has_canvas: has_canvas.map(|a| a.value(i)).unwrap_or(false),
                has_animation: has_animation.map(|a| a.value(i)).unwrap_or(false),
                has_details: has_details.map(|a| a.value(i)).unwrap_or(false),
                has_script: has_script.map(|a| a.value(i)).unwrap_or(false),
                has_drag: has_drag.map(|a| a.value(i)).unwrap_or(false),
                has_math: has_math.map(|a| a.value(i)).unwrap_or(false),
                mtime_unix: mtime.map(|a| a.value(i)).unwrap_or(0),
                indexed_at_unix: indexed_at.map(|a| a.value(i)).unwrap_or(0),
                table_count: table_count
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i)))
                    .unwrap_or(0),
                code_block_count: code_block_count
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i)))
                    .unwrap_or(0),
                word_count: word_count
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i)))
                    .unwrap_or(0),
                longread: longread
                    .and_then(|a| (!a.is_null(i)).then(|| a.value(i)))
                    .unwrap_or(false),
                tags_csv: tags_csv
                    .and_then(|a| {
                        if a.is_null(i) {
                            None
                        } else {
                            Some(a.value(i).to_string())
                        }
                    })
                    .unwrap_or_default(),
                kb_status: str_opt(kb_status),
                kb_severity: str_opt(kb_severity),
                kb_salience: kb_salience.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                kb_decay: str_opt(kb_decay),
                kb_supersedes: str_opt(kb_supersedes),
                kb_session: str_opt(kb_session),
                created_unix: created_unix.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                content_hash: str_opt(content_hash),
                task_done: task_done.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                task_total: task_total.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
                kb_summary: str_opt(kb_summary),
                kb_memory_type: str_opt(kb_memory_type),
                kb_source: str_opt(kb_source),
                kb_author: str_opt(kb_author),
                kb_source_kb: str_opt(kb_source_kb),
                kb_source_artifact: str_opt(kb_source_artifact),
                kb_source_anchor: str_opt(kb_source_anchor),
            });
        }
    }
    out
}

/// Decode [`ChunkDoc`] rows (text + embedding) from a chunk-table projection.
fn batches_to_chunk_docs(batches: &[arrow::record_batch::RecordBatch]) -> Vec<ChunkDoc> {
    use arrow::array::{FixedSizeListArray, Float32Array, StringArray, UInt32Array};
    let mut out = Vec::new();
    for batch in batches {
        let Some(chunk_ids) = batch
            .column_by_name("chunk_id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        else {
            continue;
        };
        let Some(doc_ids) = batch
            .column_by_name("doc_id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        else {
            continue;
        };
        let Some(idxs) = batch
            .column_by_name("chunk_idx")
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>())
        else {
            continue;
        };
        let Some(texts) = batch
            .column_by_name("text")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        else {
            continue;
        };
        let emb_col = batch
            .column_by_name("embedding")
            .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>());
        let emb_floats = emb_col.and_then(|e| {
            e.values()
                .as_any()
                .downcast_ref::<Float32Array>()
                .map(|f| (e, f))
        });
        for i in 0..batch.num_rows() {
            let embedding = emb_floats.and_then(|(emb, floats)| {
                if emb.is_null(i) {
                    return None;
                }
                let value_len = emb.value_length() as usize;
                let start = i * value_len;
                let mut v = Vec::with_capacity(value_len);
                for j in 0..value_len {
                    v.push(floats.value(start + j));
                }
                Some(v)
            });
            out.push(ChunkDoc {
                chunk_id: chunk_ids.value(i).to_string(),
                doc_id: doc_ids.value(i).to_string(),
                chunk_idx: idxs.value(i),
                text: texts.value(i).to_string(),
                embedding,
            });
        }
    }
    out
}

/// Bake-off A1: read the `embedding` column's `FixedSizeList<_, N>` width
/// from an opened lance table. Returns the dim as `i32` to match
/// `schema(dim)` / `docs_to_batches(_, dim)`. Returns `Error::Storage` if
/// the embedding column is missing or has an unexpected type — a sanity
/// guard against opening a table that wasn't built by this codebase.
async fn read_embedding_dim(table: &LanceTable) -> Result<i32> {
    let s = table
        .schema()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance schema: {e}")))?;
    let field = s.field_with_name("embedding").map_err(|e| {
        crate::Error::Storage(format!(
            "lance table missing `embedding` column (not built by this codebase?): {e}"
        ))
    })?;
    match field.data_type() {
        arrow::datatypes::DataType::FixedSizeList(_, n) => Ok(*n),
        other => Err(crate::Error::Storage(format!(
            "lance `embedding` column is {other:?}, expected FixedSizeList"
        ))),
    }
}

/// v0.3: lazy schema migration. Lance `add_columns` is metadata-only
/// when the table is empty; on populated tables it scales with row
/// count but stays bounded (per spike-lance findings, lance 4.0.0 is
/// not affected by #3136). Idempotent — checks the schema first and
/// no-ops when all three columns are already present.
async fn ensure_atlas_columns(table: &LanceTable) -> Result<()> {
    let existing = table
        .schema()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance schema: {e}")))?;
    let names: std::collections::HashSet<&str> = existing
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    let mut to_add: Vec<(String, String)> = Vec::new();
    if !names.contains("atlas_x") {
        to_add.push(("atlas_x".into(), "CAST(NULL AS FLOAT)".into()));
    }
    if !names.contains("atlas_y") {
        to_add.push(("atlas_y".into(), "CAST(NULL AS FLOAT)".into()));
    }
    if !names.contains("atlas_cluster") {
        to_add.push(("atlas_cluster".into(), "CAST(NULL AS SMALLINT)".into()));
    }
    if to_add.is_empty() {
        return Ok(());
    }
    table
        .add_columns()
        .transform(NewColumnTransform::SqlExpressions(to_add))
        .execute()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance atlas migration: {e}")))?;
    Ok(())
}

/// v0.6 I1 — add the gallery card counter columns. Idempotent (no-op
/// when all four columns are already present). The columns are
/// nullable so rows indexed before v0.6 stay valid; a `kb reindex`
/// pass populates them on rewrite.
async fn ensure_v6_columns(table: &LanceTable) -> Result<()> {
    let existing = table
        .schema()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance schema: {e}")))?;
    let names: std::collections::HashSet<&str> = existing
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    let mut to_add: Vec<(String, String)> = Vec::new();
    if !names.contains("table_count") {
        to_add.push(("table_count".into(), "CAST(NULL AS INT UNSIGNED)".into()));
    }
    if !names.contains("code_block_count") {
        to_add.push((
            "code_block_count".into(),
            "CAST(NULL AS INT UNSIGNED)".into(),
        ));
    }
    if !names.contains("word_count") {
        to_add.push(("word_count".into(), "CAST(NULL AS INT UNSIGNED)".into()));
    }
    if !names.contains("longread") {
        to_add.push(("longread".into(), "CAST(NULL AS BOOLEAN)".into()));
    }
    if !names.contains("tags_csv") {
        to_add.push(("tags_csv".into(), "CAST(NULL AS STRING)".into()));
    }
    if to_add.is_empty() {
        return Ok(());
    }
    table
        .add_columns()
        .transform(NewColumnTransform::SqlExpressions(to_add))
        .execute()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance v6 migration: {e}")))?;
    Ok(())
}

/// v0.7 S1 — add the `kb_status` and `kb_severity` columns. Idempotent.
/// Both are nullable strings, populated via `<meta name="kb-status">` /
/// `<meta name="kb-severity">` on next index of each artifact.
async fn ensure_v7_kb_meta_columns(table: &LanceTable) -> Result<()> {
    let existing = table
        .schema()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance schema: {e}")))?;
    let names: std::collections::HashSet<&str> = existing
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    let mut to_add: Vec<(String, String)> = Vec::new();
    if !names.contains("kb_status") {
        to_add.push(("kb_status".into(), "CAST(NULL AS STRING)".into()));
    }
    if !names.contains("kb_severity") {
        to_add.push(("kb_severity".into(), "CAST(NULL AS STRING)".into()));
    }
    if to_add.is_empty() {
        return Ok(());
    }
    table
        .add_columns()
        .transform(NewColumnTransform::SqlExpressions(to_add))
        .execute()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance v7 kb-meta migration: {e}")))?;
    Ok(())
}

/// v0.9 M1 — add the memory metas (`kb_salience` Float32, `kb_decay` +
/// `kb_supersedes` Utf8). Idempotent. Nullable, so rows indexed before
/// v0.9 stay valid; a `kb reindex` pass populates them on rewrite.
async fn ensure_v9_memory_columns(table: &LanceTable) -> Result<()> {
    let existing = table
        .schema()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance schema: {e}")))?;
    let names: std::collections::HashSet<&str> = existing
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    let mut to_add: Vec<(String, String)> = Vec::new();
    if !names.contains("kb_salience") {
        to_add.push(("kb_salience".into(), "CAST(NULL AS FLOAT)".into()));
    }
    if !names.contains("kb_decay") {
        to_add.push(("kb_decay".into(), "CAST(NULL AS STRING)".into()));
    }
    if !names.contains("kb_supersedes") {
        to_add.push(("kb_supersedes".into(), "CAST(NULL AS STRING)".into()));
    }
    if to_add.is_empty() {
        return Ok(());
    }
    table
        .add_columns()
        .transform(NewColumnTransform::SqlExpressions(to_add))
        .execute()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance v9 memory migration: {e}")))?;
    Ok(())
}

/// v0.14 S1 — add the origin-session id column (`kb_session` Utf8).
/// Idempotent. Nullable, so rows indexed before v0.14 stay valid; a
/// `kb reindex` pass populates them on rewrite from the parser's
/// `<meta name="kb-session">` extraction.
async fn ensure_v14_session_column(table: &LanceTable) -> Result<()> {
    let existing = table
        .schema()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance schema: {e}")))?;
    let names: std::collections::HashSet<&str> = existing
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    if names.contains("kb_session") {
        return Ok(());
    }
    table
        .add_columns()
        .transform(NewColumnTransform::SqlExpressions(vec![(
            "kb_session".into(),
            "CAST(NULL AS STRING)".into(),
        )]))
        .execute()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance v14 session migration: {e}")))?;
    Ok(())
}

/// v0.16 — add the persisted-content-hash column (`content_hash` Utf8).
/// Idempotent. Nullable: rows indexed before v0.16 stay NULL until a
/// reindex repopulates them. Used by the indexer's startup dedup cache
/// so `watch.create` envelopes from the watcher's initial walk no
/// longer re-embed unchanged artifacts on every daemon restart.
async fn ensure_v16_content_hash_column(table: &LanceTable) -> Result<()> {
    let existing = table
        .schema()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance schema: {e}")))?;
    let names: std::collections::HashSet<&str> = existing
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    if names.contains("content_hash") {
        return Ok(());
    }
    table
        .add_columns()
        .transform(NewColumnTransform::SqlExpressions(vec![(
            "content_hash".into(),
            "CAST(NULL AS STRING)".into(),
        )]))
        .execute()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance v16 content_hash migration: {e}")))?;
    Ok(())
}

/// v0.15 — add the filesystem-btime column (`created_unix` Int64).
/// Idempotent. Nullable: rows on btime-less filesystems and rows
/// indexed before v0.15 stay NULL until a reindex captures
/// `metadata.created()` for them.
async fn ensure_v15_created_column(table: &LanceTable) -> Result<()> {
    let existing = table
        .schema()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance schema: {e}")))?;
    let names: std::collections::HashSet<&str> = existing
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    if names.contains("created_unix") {
        return Ok(());
    }
    table
        .add_columns()
        .transform(NewColumnTransform::SqlExpressions(vec![(
            "created_unix".into(),
            "CAST(NULL AS BIGINT)".into(),
        )]))
        .execute()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance v15 created migration: {e}")))?;
    Ok(())
}

/// N-track — add the GFM task-list progress columns (`task_done`,
/// `task_total`, both nullable UInt32). Idempotent. Old rows stay NULL
/// until a reindex counts their checkboxes; notes get real counts on their
/// first index.
async fn ensure_v17_task_columns(table: &LanceTable) -> Result<()> {
    let existing = table
        .schema()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance schema: {e}")))?;
    let names: std::collections::HashSet<&str> = existing
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    let mut to_add: Vec<(String, String)> = Vec::new();
    if !names.contains("task_done") {
        to_add.push(("task_done".into(), "CAST(NULL AS INT UNSIGNED)".into()));
    }
    if !names.contains("task_total") {
        to_add.push(("task_total".into(), "CAST(NULL AS INT UNSIGNED)".into()));
    }
    if to_add.is_empty() {
        return Ok(());
    }
    table
        .add_columns()
        .transform(NewColumnTransform::SqlExpressions(to_add))
        .execute()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance v17 task migration: {e}")))?;
    Ok(())
}

/// RA4 — add the one-line memory summary column (`kb_summary` Utf8).
/// Idempotent. Nullable, so rows indexed before RA4 stay valid; a
/// `kb reindex` pass populates them on rewrite from the parser's
/// `<meta name="kb-summary">` extraction.
async fn ensure_v18_summary_column(table: &LanceTable) -> Result<()> {
    let existing = table
        .schema()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance schema: {e}")))?;
    let names: std::collections::HashSet<&str> = existing
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    if names.contains("kb_summary") {
        return Ok(());
    }
    table
        .add_columns()
        .transform(NewColumnTransform::SqlExpressions(vec![(
            "kb_summary".into(),
            "CAST(NULL AS STRING)".into(),
        )]))
        .execute()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance v18 summary migration: {e}")))?;
    Ok(())
}

/// MI-W3.3a / MI-W3.4 — add the optional memory-type + trust-source
/// columns (`kb_memory_type`, `kb_source`, both Utf8). Idempotent, same
/// bundled-columns shape as `ensure_v9_memory_columns`. Nullable, so rows
/// indexed before this migration stay valid; a `kb reindex` pass populates
/// them on rewrite from the parser's `<meta name="kb-memory-type">` /
/// `<meta name="kb-source">` extraction.
async fn ensure_mi_w3_memory_type_and_source_columns(table: &LanceTable) -> Result<()> {
    let existing = table
        .schema()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance schema: {e}")))?;
    let names: std::collections::HashSet<&str> = existing
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    let mut to_add: Vec<(String, String)> = Vec::new();
    if !names.contains("kb_memory_type") {
        to_add.push(("kb_memory_type".into(), "CAST(NULL AS STRING)".into()));
    }
    if !names.contains("kb_source") {
        to_add.push(("kb_source".into(), "CAST(NULL AS STRING)".into()));
    }
    if to_add.is_empty() {
        return Ok(());
    }
    table
        .add_columns()
        .transform(NewColumnTransform::SqlExpressions(to_add))
        .execute()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance memory-type/source migration: {e}")))?;
    Ok(())
}

/// CT-A1 (U3 parse-back) — add the optional highlight-provenance columns
/// (`kb_author`, `kb_source_kb`, `kb_source_artifact`, `kb_source_anchor`,
/// all Utf8). Idempotent, same bundled-columns shape as
/// `ensure_mi_w3_memory_type_and_source_columns`. Nullable, so rows indexed
/// before this migration stay valid; a `kb reindex` pass populates them on
/// rewrite from the parser's `<meta name="kb-author">` /
/// `<meta name="kb-source-kb">` / `<meta name="kb-source-artifact">` /
/// `<meta name="kb-source-anchor">` extraction.
async fn ensure_u3_provenance_columns(table: &LanceTable) -> Result<()> {
    let existing = table
        .schema()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance schema: {e}")))?;
    let names: std::collections::HashSet<&str> = existing
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    let mut to_add: Vec<(String, String)> = Vec::new();
    if !names.contains("kb_author") {
        to_add.push(("kb_author".into(), "CAST(NULL AS STRING)".into()));
    }
    if !names.contains("kb_source_kb") {
        to_add.push(("kb_source_kb".into(), "CAST(NULL AS STRING)".into()));
    }
    if !names.contains("kb_source_artifact") {
        to_add.push(("kb_source_artifact".into(), "CAST(NULL AS STRING)".into()));
    }
    if !names.contains("kb_source_anchor") {
        to_add.push(("kb_source_anchor".into(), "CAST(NULL AS STRING)".into()));
    }
    if to_add.is_empty() {
        return Ok(());
    }
    table
        .add_columns()
        .transform(NewColumnTransform::SqlExpressions(to_add))
        .execute()
        .await
        .map_err(|e| crate::Error::Storage(format!("lance U3 provenance migration: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::Doc;

    fn fixture_doc(id: &str, title: &str, body: &str) -> Doc {
        let mut d = Doc::placeholder(id, format!("/tmp/{id}.html"));
        d.title = title.into();
        d.body = body.into();
        d
    }

    // Q-track (board B1) — `get_bodies_by_ids` projects the FULL `body`
    // column, distinct from `get_by_ids`'s capped `body_text_excerpt`
    // (`DocSummary::summary`). Confirms both the projection and the
    // id-allowlist (a non-alnum/dash id is dropped, not errored).
    #[tokio::test]
    async fn get_bodies_by_ids_projects_full_body_not_the_excerpt() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let long_body = "word ".repeat(200); // far past the ~400-char excerpt cap
        s.upsert_docs(&[
            fixture_doc("a", "Doc A", &long_body),
            fixture_doc("b", "Doc B", "short body b"),
        ])
        .await
        .unwrap();

        let pairs = s
            .get_bodies_by_ids(&["a".to_string(), "b".to_string(), "missing".to_string()])
            .await
            .unwrap();
        assert_eq!(
            pairs.len(),
            2,
            "unknown id must be silently absent: {pairs:?}"
        );
        let by_id: std::collections::HashMap<_, _> = pairs.into_iter().collect();
        assert_eq!(
            by_id.get("a").unwrap(),
            &long_body,
            "full body, not truncated"
        );
        assert_eq!(by_id.get("b").unwrap(), "short body b");

        // An id outside the `[A-Za-z0-9-]` allowlist is dropped, not an error.
        let pairs = s
            .get_bodies_by_ids(&["a; DROP TABLE artifacts".to_string()])
            .await
            .unwrap();
        assert!(pairs.is_empty());
    }

    // W2.3a — `embedding_by_id` / `embeddings_by_ids`.

    #[tokio::test]
    async fn embedding_by_id_returns_the_vector_and_none_for_unknown_or_bad_id() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[doc_with_embedding("a", "Doc A", "alpha content")])
            .await
            .unwrap();

        let v = s.embedding_by_id("a").await.unwrap();
        assert_eq!(v.as_ref().map(|v| v.len()), Some(384));

        assert!(s.embedding_by_id("missing").await.unwrap().is_none());
        // Outside the `[A-Za-z0-9-]` allowlist — dropped, not an error.
        assert!(s
            .embedding_by_id("a; DROP TABLE artifacts")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn embeddings_by_ids_resolves_many_in_one_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[
            doc_with_embedding("a", "Doc A", "alpha content"),
            doc_with_embedding("b", "Doc B", "beta content"),
        ])
        .await
        .unwrap();

        let pairs = s
            .embeddings_by_ids(&["a".to_string(), "b".to_string(), "missing".to_string()])
            .await
            .unwrap();
        assert_eq!(pairs.len(), 2, "unknown id must be silently absent");
        let by_id: std::collections::HashMap<_, _> = pairs.into_iter().collect();
        assert_eq!(by_id.get("a").unwrap().len(), 384);
        assert_eq!(by_id.get("b").unwrap().len(), 384);
        assert_ne!(
            by_id.get("a").unwrap(),
            by_id.get("b").unwrap(),
            "distinct docs embed to distinct vectors"
        );

        // Empty input never scans.
        assert!(s.embeddings_by_ids(&[]).await.unwrap().is_empty());
    }

    // W2.11 — `prompt_by_id`.

    #[tokio::test]
    async fn prompt_by_id_projects_prompt_and_size_and_is_honest_about_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let mut with_prompt = fixture_doc("a", "Doc A", "alpha content");
        with_prompt.prompt = Some("write a haiku about rust".into());
        with_prompt.prompt_size_bytes = with_prompt.prompt.as_ref().unwrap().len() as u32;
        let without_prompt = fixture_doc("b", "Doc B", "beta content"); // prompt: None by default
        s.upsert_docs(&[with_prompt, without_prompt]).await.unwrap();

        let (text, size) = s.prompt_by_id("a").await.unwrap().unwrap();
        assert_eq!(text, "write a haiku about rust");
        assert_eq!(size, text.len() as u32);

        // A doc that exists but never had a `<template id="kb-prompt">` —
        // the caller's own `get_by_id` existence check (not this fn)
        // distinguishes this from an unknown id; both surface `None` here.
        assert!(s.prompt_by_id("b").await.unwrap().is_none());
        assert!(s.prompt_by_id("missing").await.unwrap().is_none());
        // Outside the `[A-Za-z0-9-]` allowlist — dropped, not an error.
        assert!(s
            .prompt_by_id("a; DROP TABLE artifacts")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn open_creates_dataset_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::open(tmp.path(), None).await.unwrap();
        assert_eq!(storage.count_rows().await.unwrap(), 0);
        // No `embedding_model` configured → default-model dim (384) bakes
        // into the fresh schema so the column is well-formed.
        assert_eq!(storage.dim(), 384);
    }

    // ---- SQ5: passage/chunk table round-trip ----

    fn unit_vec(dim: usize, hot: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dim];
        v[hot] = 1.0;
        v
    }

    fn chunk(id: &str, idx: u32, emb: Vec<f32>) -> crate::storage::schema::ChunkDoc {
        crate::storage::schema::ChunkDoc {
            chunk_id: format!("{id}#{idx}"),
            doc_id: id.into(),
            chunk_idx: idx,
            text: format!("{id} passage {idx}"),
            embedding: Some(emb),
        }
    }

    #[tokio::test]
    async fn chunk_upsert_and_vector_query_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), Some(384)).await.unwrap();
        s.upsert_docs(&[
            fixture_doc("d1", "Doc One", "alpha"),
            fixture_doc("d2", "Doc Two", "beta"),
        ])
        .await
        .unwrap();
        let v1 = unit_vec(384, 0);
        let v2 = unit_vec(384, 1);
        s.upsert_chunks("d1", &[chunk("d1", 0, v1.clone())])
            .await
            .unwrap();
        s.upsert_chunks("d2", &[chunk("d2", 0, v2)]).await.unwrap();

        // Query near d1's chunk → d1 ranks first, carries a score.
        let hits = s.chunk_vector_query(&v1, 50, 10).await.unwrap();
        assert!(!hits.is_empty(), "chunk query returned no hits");
        assert_eq!(hits[0].id, "d1", "nearest chunk's doc should rank first");
        assert!(hits[0].score.is_some(), "chunk hit should carry a score");
    }

    // SC2 (verify) — `finish_indexed_doc` calls `upsert_chunks` once PER
    // DOC (never batched across an ingest group, unlike GC-B7's
    // `upsert_docs`), and `upsert_chunks` itself does one `merge_insert`
    // per call. Confirms the `compact_all`'s own comment ("it accumulates
    // fragments per upsert just like the doc table") with a real fragment
    // count: N per-doc chunk upserts commit N chunk-table fragments,
    // mirroring the pre-GC-B7 doc-table growth
    // (`compact_all_round_trip`'s "each upsert creates its own fragment").
    // Reachability check, not a regression test — chunking is opt-in
    // (`chunked_embeddings` defaults to `false`, config.rs) and no kb in
    // the fleet's `kb.toml` turns it on, so this growth is currently
    // unexercised in prod; kept as a `#[ignore]`d instrument, not a gate.
    #[tokio::test]
    #[ignore = "instrumentation, not a regression gate — see SC2 phase notes"]
    async fn chunk_table_fragments_grow_one_per_doc_upsert() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), Some(384)).await.unwrap();
        const N: usize = 40;
        let docs: Vec<Doc> = (0..N)
            .map(|i| fixture_doc(&format!("d{i}"), &format!("Doc {i}"), &format!("body {i}")))
            .collect();
        // Doc rows land in ONE merge_insert (GC-B7 already batches this).
        s.upsert_docs(&docs).await.unwrap();
        // Chunk writes mirror `finish_indexed_doc`: one `upsert_chunks`
        // call per doc, each its own merge_insert.
        for i in 0..N {
            let id = format!("d{i}");
            let v = unit_vec(384, i % 384);
            s.upsert_chunks(&id, &[chunk(&id, 0, v)]).await.unwrap();
        }
        let chunk_stats = s
            .chunk_table
            .stats()
            .await
            .expect("chunk table stats")
            .fragment_stats;
        println!(
            "SC2 evidence: {N} per-doc upsert_chunks calls -> chunk_table fragments={} (rows={})",
            chunk_stats.num_fragments,
            s.chunk_table.count_rows(None).await.unwrap_or(0),
        );
        assert!(
            chunk_stats.num_fragments as usize >= N,
            "expected >= {N} chunk-table fragments (one per per-doc upsert_chunks call), got {}",
            chunk_stats.num_fragments
        );
    }

    // GC-B1 — the max-pooled `best` list (line ~1030) is built by iterating
    // the KNN stream and sorted by distance only; two docs whose nearest
    // chunk sits at the EXACT same pooled distance (identical chunk
    // embeddings here) must not resolve via the stream's physical order,
    // which decides who survives `truncate`.
    #[test]
    fn chunk_vector_tie_order_is_insertion_order_independent() {
        use crate::test_support::assert_order_independent;
        let rows = vec!["z_tie", "b_tie"];
        assert_order_independent(&rows, 6, |doc_ids| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let tmp = tempfile::tempdir().unwrap();
                let s = Storage::open(tmp.path(), Some(384)).await.unwrap();
                let v = unit_vec(384, 0);
                for id in &doc_ids {
                    s.upsert_docs(&[fixture_doc(id, id, id)]).await.unwrap();
                    s.upsert_chunks(id, &[chunk(id, 0, v.clone())])
                        .await
                        .unwrap();
                }
                s.chunk_vector_query(&v, 50, 10)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|h| h.id)
                    .collect::<Vec<_>>()
            })
        });
    }

    #[tokio::test]
    async fn chunk_query_empty_table_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), Some(384)).await.unwrap();
        assert!(s
            .chunk_vector_query(&unit_vec(384, 0), 50, 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn delete_by_id_cascades_chunks() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), Some(384)).await.unwrap();
        s.upsert_docs(&[fixture_doc("d1", "T", "b")]).await.unwrap();
        let v = unit_vec(384, 0);
        s.upsert_chunks("d1", &[chunk("d1", 0, v.clone())])
            .await
            .unwrap();
        assert!(!s.chunk_vector_query(&v, 50, 10).await.unwrap().is_empty());
        s.delete_by_id("d1").await.unwrap();
        assert!(
            s.chunk_vector_query(&v, 50, 10).await.unwrap().is_empty(),
            "delete_by_id must cascade the doc's chunks"
        );
    }

    #[tokio::test]
    async fn reupsert_chunks_replaces_not_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), Some(384)).await.unwrap();
        s.upsert_docs(&[fixture_doc("d1", "T", "b")]).await.unwrap();
        let v = unit_vec(384, 0);
        // First: 3 chunks. Then re-upsert with 1 chunk → old 3 must be gone.
        s.upsert_chunks(
            "d1",
            &[
                chunk("d1", 0, v.clone()),
                chunk("d1", 1, v.clone()),
                chunk("d1", 2, v.clone()),
            ],
        )
        .await
        .unwrap();
        s.upsert_chunks("d1", &[chunk("d1", 0, v.clone())])
            .await
            .unwrap();
        // Only one doc resolves; no stale chunks blow up the resolve.
        let hits = s.chunk_vector_query(&v, 50, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "d1");
    }

    // ---- Bake-off A1: dim guard / multi-dim round-trip ----

    #[tokio::test]
    async fn open_creates_768_dim_dataset_when_configured() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::open(tmp.path(), Some(768)).await.unwrap();
        assert_eq!(storage.dim(), 768);
        // Schema must reflect the configured dim, not the default.
        let s = storage.table.schema().await.unwrap();
        let f = s.field_with_name("embedding").unwrap();
        match f.data_type() {
            arrow::datatypes::DataType::FixedSizeList(_, n) => assert_eq!(*n, 768),
            other => panic!("expected FixedSizeList(_, 768), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn upsert_with_768_dim_doc_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), Some(768)).await.unwrap();
        let mut d = fixture_doc("d768", "Hello", "world");
        d.embedding = Some(vec![0.1; 768]);
        s.upsert_docs(&[d]).await.unwrap();
        assert_eq!(s.count_rows().await.unwrap(), 1);
        let pairs = s.list_embeddings().await.unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].1.len(), 768);
    }

    #[tokio::test]
    async fn upsert_with_1024_dim_doc_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), Some(1024)).await.unwrap();
        let mut d = fixture_doc("d1024", "Hello", "world");
        d.embedding = Some(vec![0.2; 1024]);
        s.upsert_docs(&[d]).await.unwrap();
        assert_eq!(s.count_rows().await.unwrap(), 1);
        let pairs = s.list_embeddings().await.unwrap();
        assert_eq!(pairs[0].1.len(), 1024);
    }

    #[tokio::test]
    async fn open_rejects_when_disk_dim_mismatches_config_dim() {
        let tmp = tempfile::tempdir().unwrap();
        // Create at 384 first.
        {
            let _ = Storage::open(tmp.path(), Some(384)).await.unwrap();
        }
        // Reopening with a different config dim must refuse — the dataset
        // would silently corrupt on the next upsert if we let it through.
        // `Storage` doesn't implement Debug, so unwrap the Result by hand.
        let err = match Storage::open(tmp.path(), Some(768)).await {
            Ok(_) => panic!("dim mismatch should have been rejected"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("384") && msg.contains("768"),
            "error must surface both dims, got: {msg}"
        );
        assert!(
            matches!(err, crate::Error::Config(_)),
            "expected Error::Config, got {err:?}"
        );
    }

    #[tokio::test]
    async fn open_accepts_when_config_dim_is_none_regardless_of_disk_dim() {
        let tmp = tempfile::tempdir().unwrap();
        // Create at 768.
        {
            let _ = Storage::open(tmp.path(), Some(768)).await.unwrap();
        }
        // Reopening with None must trust the disk (the embed-less kb path
        // where the operator hasn't wired an embedder yet).
        let s = Storage::open(tmp.path(), None).await.unwrap();
        assert_eq!(s.dim(), 768);
    }

    #[tokio::test]
    async fn upsert_with_wrong_dim_returns_storage_error_not_panic() {
        // The kb is 384-dim on disk; passing a 768-dim doc must surface
        // as Error::Storage from `docs_to_batches`, NOT crash the actor.
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), Some(384)).await.unwrap();
        let mut d = fixture_doc("oops", "Hello", "world");
        d.embedding = Some(vec![0.0; 768]);
        let err = match s.upsert_docs(&[d]).await {
            Ok(_) => panic!("wrong-dim upsert should have failed"),
            Err(e) => e,
        };
        assert!(matches!(err, crate::Error::Storage(_)));
        let msg = err.to_string();
        assert!(msg.contains("oops"), "must name the offending doc: {msg}");
    }

    #[tokio::test]
    async fn open_then_reopen_preserves_rows() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let s = Storage::open(tmp.path(), None).await.unwrap();
            s.upsert_docs(&[fixture_doc("a", "Hello", "world")])
                .await
                .unwrap();
            assert_eq!(s.count_rows().await.unwrap(), 1);
        }
        let s2 = Storage::open(tmp.path(), None).await.unwrap();
        assert_eq!(s2.count_rows().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn upsert_replaces_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[fixture_doc("a", "First", "v1")])
            .await
            .unwrap();
        s.upsert_docs(&[fixture_doc("a", "Second", "v2")])
            .await
            .unwrap();
        assert_eq!(s.count_rows().await.unwrap(), 1, "upsert, not append");
        // merge_insert updates the matched row in place — the surviving
        // row carries the v2 content, not a stale or missing one.
        let hit = s.get_by_id("a").await.unwrap().expect("row a present");
        assert_eq!(hit.title, "Second");
    }

    #[tokio::test]
    async fn upsert_mixed_insert_and_update_in_one_call() {
        // merge_insert: a batch that both updates an existing id and
        // inserts a new one lands atomically as one operation.
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[fixture_doc("a", "A1", "v1")])
            .await
            .unwrap();
        s.upsert_docs(&[fixture_doc("a", "A2", "v2"), fixture_doc("b", "B1", "v1")])
            .await
            .unwrap();
        assert_eq!(s.count_rows().await.unwrap(), 2);
        assert_eq!(
            s.get_by_id("a").await.unwrap().expect("a").title,
            "A2",
            "existing id updated"
        );
        assert_eq!(
            s.get_by_id("b").await.unwrap().expect("b").title,
            "B1",
            "new id inserted"
        );
    }

    #[tokio::test]
    async fn delete_by_id_removes_row() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[
            fixture_doc("a", "A", "alpha"),
            fixture_doc("b", "B", "beta"),
        ])
        .await
        .unwrap();
        s.delete_by_id("a").await.unwrap();
        assert_eq!(s.count_rows().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn get_by_source_path_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[
            fixture_doc("a", "A", "alpha"),
            fixture_doc("b", "B", "beta"),
        ])
        .await
        .unwrap();
        let hit = s.get_by_source_path("/tmp/b.html").await.unwrap();
        assert_eq!(hit.map(|d| d.id), Some("b".into()));
    }

    #[tokio::test]
    async fn get_by_source_path_misses_when_no_row() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[fixture_doc("a", "A", "alpha")])
            .await
            .unwrap();
        assert!(s
            .get_by_source_path("/tmp/missing.html")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn get_by_source_path_escapes_apostrophes() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let mut d = fixture_doc("q", "Quoted", "alpha");
        d.path = "/tmp/it's-fine.html".into();
        s.upsert_docs(&[d]).await.unwrap();
        let hit = s.get_by_source_path("/tmp/it's-fine.html").await.unwrap();
        assert_eq!(hit.map(|d| d.id), Some("q".into()));
    }

    // ---- R4: centralized lance-filter literal escaping ----

    #[test]
    fn escape_literal_doubles_single_quotes() {
        assert_eq!(escape_literal("plain"), "plain");
        assert_eq!(escape_literal("it's"), "it''s");
        // Already-doubled input doubles again (it's a byte transform, not an
        // un-escape): two quotes → four.
        assert_eq!(escape_literal("a''b"), "a''''b");
        assert_eq!(escape_literal("'"), "''");
        assert_eq!(escape_literal(""), "");
        // Unicode + spaces pass through untouched — only the quote is special.
        assert_eq!(escape_literal("caffè d'oro"), "caffè d''oro");
        // A classic injection attempt is neutralised into an inert literal.
        assert_eq!(escape_literal("x' OR '1'='1"), "x'' OR ''1''=''1");
    }

    #[test]
    fn filter_eq_builds_escaped_equality() {
        assert_eq!(filter_eq("id", "abc"), "id = 'abc'");
        assert_eq!(
            filter_eq("path", "/tmp/it's.html"),
            "path = '/tmp/it''s.html'"
        );
        // The column name is hardcoded by callers and interpolated verbatim.
        assert_eq!(filter_eq("kb_session", "s'1"), "kb_session = 's''1'");
    }

    #[tokio::test]
    async fn delete_by_path_with_apostrophe_deletes_only_that_row() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let mut quoted = fixture_doc("q", "Quoted", "alpha");
        quoted.path = "/tmp/it's-fine.html".into();
        let mut plain = fixture_doc("p", "Plain", "beta");
        plain.path = "/tmp/plain.html".into();
        s.upsert_docs(&[quoted, plain]).await.unwrap();
        assert_eq!(s.count_rows().await.unwrap(), 2);

        // The escaped predicate must match EXACTLY the quote-containing row —
        // not error, not match everything, not miss.
        s.delete_by_path("/tmp/it's-fine.html").await.unwrap();
        assert_eq!(s.count_rows().await.unwrap(), 1);
        assert!(s
            .get_by_source_path("/tmp/it's-fine.html")
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            s.get_by_source_path("/tmp/plain.html")
                .await
                .unwrap()
                .map(|d| d.id),
            Some("p".into())
        );
    }

    #[tokio::test]
    async fn bm25_returns_match() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[
            fixture_doc(
                "a",
                "Borrow Checker",
                "Drag and resolve lifetime conflicts in real time.",
            ),
            fixture_doc(
                "b",
                "Cost of Abstraction",
                "Iterators and closures cost nothing at runtime.",
            ),
            fixture_doc("c", "Field Guide to Errors", "Panics are not errors."),
        ])
        .await
        .unwrap();
        s.ensure_fts_index().await.unwrap();

        let hits = s.bm25_query("borrow", 5, false).await.unwrap();
        assert!(!hits.is_empty(), "expected at least one hit for 'borrow'");
        assert_eq!(hits[0].id, "a");
        assert_eq!(hits[0].title, "Borrow Checker");
    }

    #[tokio::test]
    async fn bm25_returns_empty_on_no_match() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[fixture_doc("a", "Hello", "world")])
            .await
            .unwrap();
        s.ensure_fts_index().await.unwrap();
        let hits = s
            .bm25_query("zxcvbnm-no-such-token", 5, false)
            .await
            .unwrap();
        assert!(hits.is_empty());
    }

    // GC-D1 — the flag's whole reason to exist: a single-letter typo the
    // exact-match arm can't find at all becomes findable once fuzzy
    // matching is switched on.
    #[tokio::test]
    async fn bm25_typo_tolerance_off_misses_a_single_letter_typo() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[fixture_doc(
            "a",
            "Borrow Checker",
            "Drag and resolve lifetime conflicts in real time.",
        )])
        .await
        .unwrap();
        s.ensure_fts_index().await.unwrap();

        // "borow" (one dropped letter) — exact match finds nothing.
        let exact = s.bm25_query("borow", 5, false).await.unwrap();
        assert!(exact.is_empty(), "exact-match arm should miss the typo");
    }

    #[tokio::test]
    async fn bm25_typo_tolerance_on_finds_the_single_letter_typo() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[fixture_doc(
            "a",
            "Borrow Checker",
            "Drag and resolve lifetime conflicts in real time.",
        )])
        .await
        .unwrap();
        s.ensure_fts_index().await.unwrap();

        let fuzzy = s.bm25_query("borow", 5, true).await.unwrap();
        assert!(
            fuzzy.iter().any(|h| h.id == "a"),
            "fuzzy arm should find the typo'd doc, got {fuzzy:?}"
        );
    }

    // GC-D1 determinism: with the flag OFF, the query issued is byte-
    // identical to pre-GC-D1 behaviour — an exact query for an existing
    // term returns the exact same hit set/order it always did (GC-B1's
    // tie-break discipline is untouched).
    #[tokio::test]
    async fn bm25_typo_tolerance_off_is_unchanged_for_exact_queries() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[
            fixture_doc(
                "a",
                "Borrow Checker",
                "Drag and resolve lifetime conflicts in real time.",
            ),
            fixture_doc(
                "b",
                "Cost of Abstraction",
                "Iterators and closures cost nothing at runtime.",
            ),
        ])
        .await
        .unwrap();
        s.ensure_fts_index().await.unwrap();

        let off_ids: Vec<_> = s
            .bm25_query("borrow", 5, false)
            .await
            .unwrap()
            .into_iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(off_ids, vec!["a".to_string()]);
    }

    // GC-D1 — the regression the bench run caught: an EARLIER version of
    // this flag ran the fuzzy query unconditionally (fuzzing every term,
    // even ones with a perfect exact hit already), which measurably
    // wrecked Recall@1/MRR on ordinary, typo-free queries by diluting the
    // ranked set with weakly-related noise. `typo_tolerance` must only
    // ever change the OUTCOME when the exact arm found nothing — so an
    // exact query that already has a hit is identical whether the flag is
    // on or off.
    #[tokio::test]
    async fn bm25_typo_tolerance_on_does_not_alter_results_when_exact_arm_already_hits() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[
            fixture_doc(
                "a",
                "Borrow Checker",
                "Drag and resolve lifetime conflicts in real time.",
            ),
            fixture_doc(
                "b",
                "Cost of Abstraction",
                "Iterators and closures cost nothing at runtime.",
            ),
        ])
        .await
        .unwrap();
        s.ensure_fts_index().await.unwrap();

        let off = s.bm25_query("borrow", 5, false).await.unwrap();
        let on = s.bm25_query("borrow", 5, true).await.unwrap();
        let off_ids: Vec<_> = off.iter().map(|h| h.id.clone()).collect();
        let on_ids: Vec<_> = on.iter().map(|h| h.id.clone()).collect();
        assert_eq!(
            off_ids, on_ids,
            "typo_tolerance must not touch a query the exact arm already answered"
        );
    }

    // GC-B1 — determinism-audit finding: `bm25_query` handed back lance's
    // top-k in physical scan order with no id tie-break, so two docs at an
    // EXACT score tie could swap rank across a compaction (docs/research/
    // search-determinism-settling-window-2026-07.html §3). Two docs here
    // carry byte-identical scored text (title+body), guaranteeing an
    // exact BM25 tie; `assert_order_independent` inserts them in every
    // shuffled order and asserts the returned id sequence is always the
    // same canonical (id-ascending) order — i.e. independent of insertion/
    // scan order, not just "stable run to run".
    #[test]
    fn bm25_tie_order_is_insertion_order_independent() {
        use crate::test_support::assert_order_independent;
        let rows = vec![
            (
                "z_tie",
                "Borrow Checker",
                "Drag and resolve lifetime conflicts.",
            ),
            (
                "b_tie",
                "Borrow Checker",
                "Drag and resolve lifetime conflicts.",
            ),
        ];
        assert_order_independent(&rows, 6, |docs| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let tmp = tempfile::tempdir().unwrap();
                let s = Storage::open(tmp.path(), None).await.unwrap();
                let fixtures: Vec<Doc> = docs
                    .into_iter()
                    .map(|(id, title, body)| fixture_doc(id, title, body))
                    .collect();
                s.upsert_docs(&fixtures).await.unwrap();
                s.ensure_fts_index().await.unwrap();
                let hits = s.bm25_query("lifetime", 5, false).await.unwrap();
                hits.into_iter().map(|h| h.id).collect::<Vec<_>>()
            })
        });
    }

    #[tokio::test]
    async fn ensure_fts_index_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[fixture_doc("a", "Hello", "world")])
            .await
            .unwrap();
        s.ensure_fts_index().await.unwrap();
        assert_eq!(s.fts_rebuild_count(), 1, "first ensure builds the index");
        // No mutation in between → the gate skips the expensive `replace=true`
        // rebuild. This skip is the whole optimization.
        s.ensure_fts_index().await.unwrap();
        s.ensure_fts_index().await.unwrap();
        assert_eq!(
            s.fts_rebuild_count(),
            1,
            "subsequent ensures on an unchanged row-set skip the rebuild"
        );
    }

    #[tokio::test]
    async fn ensure_fts_gate_rebuilds_after_mutation_and_stays_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[fixture_doc("a", "Borrow checker", "lifetime conflicts")])
            .await
            .unwrap();
        s.ensure_fts_index().await.unwrap();
        assert_eq!(s.fts_rebuild_count(), 1);
        assert!(
            s.bm25_query("borrow", 10, false)
                .await
                .unwrap()
                .iter()
                .any(|h| h.id == "a"),
            "first doc is searchable"
        );

        // A NEW doc must become searchable: the upsert re-dirties the gate, so
        // the next ensure rebuilds and incorporates it. This is the correctness
        // guarantee — gating the rebuild must NOT serve stale results.
        s.upsert_docs(&[fixture_doc("b", "Garbage collector", "tracing pauses")])
            .await
            .unwrap();
        s.ensure_fts_index().await.unwrap();
        assert_eq!(
            s.fts_rebuild_count(),
            2,
            "a mutation forces exactly one rebuild"
        );
        assert!(
            s.bm25_query("garbage", 10, false)
                .await
                .unwrap()
                .iter()
                .any(|h| h.id == "b"),
            "newly-upserted doc is searchable after the rebuild"
        );
        assert!(
            s.bm25_query("borrow", 10, false)
                .await
                .unwrap()
                .iter()
                .any(|h| h.id == "a"),
            "the original doc is still searchable after the rebuild"
        );

        // A search with no intervening mutation does not rebuild again.
        s.ensure_fts_index().await.unwrap();
        assert_eq!(s.fts_rebuild_count(), 2, "a clean search skips the rebuild");
    }

    // --- Vector + hybrid query tests ---------------------------------------
    //
    // Use a deterministic toy embedding (hash → xorshift → L2-normalise) so
    // tests don't need a real embedder. Same shape as spike-lance; lifted
    // into a private test helper here.

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

    fn doc_with_embedding(id: &str, title: &str, body: &str) -> Doc {
        let mut d = fixture_doc(id, title, body);
        d.embedding = Some(toy_embed(&format!("{title} {body}")));
        d
    }

    #[tokio::test]
    async fn vector_query_finds_exact_match_top_one() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let docs = vec![
            doc_with_embedding("a", "Borrow Checker", "lifetime conflicts"),
            doc_with_embedding("b", "Cost of Abstraction", "iterators and closures"),
            doc_with_embedding("c", "Field Guide to Errors", "panics are not errors"),
        ];
        s.upsert_docs(&docs).await.unwrap();

        // Query with the exact embedding of doc "a" — it must rank top-1.
        let qvec = toy_embed("Borrow Checker lifetime conflicts");
        let hits = s.vector_query(&qvec, 3).await.unwrap();
        assert!(!hits.is_empty(), "expected at least one hit");
        assert_eq!(hits[0].id, "a", "exact-match embedding should rank top-1");
    }

    #[tokio::test]
    async fn vector_query_returns_three_when_three_indexed() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[
            doc_with_embedding("a", "alpha", "first"),
            doc_with_embedding("b", "beta", "second"),
            doc_with_embedding("c", "gamma", "third"),
        ])
        .await
        .unwrap();

        let q = toy_embed("anything");
        let hits = s.vector_query(&q, 10).await.unwrap();
        assert_eq!(hits.len(), 3, "limit=10 against 3 docs returns all 3");
    }

    #[tokio::test]
    async fn vector_query_empty_when_no_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let q = toy_embed("anything");
        let hits = s.vector_query(&q, 5).await.unwrap();
        assert!(hits.is_empty());
    }

    // GC-B1 — same audit finding as `bm25_tie_order_is_insertion_order_
    // independent`, for the vector arm: two rows with a BYTE-IDENTICAL
    // embedding (same title+body text fed through `toy_embed`) are
    // exactly equidistant from any query vector, so their relative order
    // rode lance's physical scan order pre-fix.
    #[test]
    fn vector_tie_order_is_insertion_order_independent() {
        use crate::test_support::assert_order_independent;
        let rows = vec![
            ("z_tie", "Borrow Checker", "lifetime conflicts"),
            ("b_tie", "Borrow Checker", "lifetime conflicts"),
        ];
        assert_order_independent(&rows, 6, |docs| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let tmp = tempfile::tempdir().unwrap();
                let s = Storage::open(tmp.path(), None).await.unwrap();
                let fixtures: Vec<Doc> = docs
                    .into_iter()
                    .map(|(id, title, body)| doc_with_embedding(id, title, body))
                    .collect();
                s.upsert_docs(&fixtures).await.unwrap();
                let qvec = toy_embed("Borrow Checker lifetime conflicts");
                let hits = s.vector_query(&qvec, 5).await.unwrap();
                hits.into_iter().map(|h| h.id).collect::<Vec<_>>()
            })
        });
    }

    #[tokio::test]
    async fn hybrid_query_returns_results_combining_signals() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[
            doc_with_embedding(
                "a",
                "Borrow Checker",
                "Drag and resolve lifetime conflicts in real time.",
            ),
            doc_with_embedding(
                "b",
                "Cost of Abstraction",
                "Iterators and closures cost nothing at runtime.",
            ),
            doc_with_embedding("c", "Field Guide to Errors", "Panics are not errors."),
        ])
        .await
        .unwrap();
        s.ensure_fts_index().await.unwrap();

        let qvec = toy_embed("Borrow Checker lifetime conflicts");
        let hits = s.hybrid_query("borrow", &qvec, 5).await.unwrap();
        assert!(!hits.is_empty(), "hybrid query must return some hits");
        // The borrow-checker doc should be at the top — both BM25 and vector
        // signals point at it.
        assert_eq!(hits[0].id, "a");
    }

    #[tokio::test]
    async fn ensure_vector_index_tolerates_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        // No rows yet; should not error.
        s.ensure_vector_index().await.unwrap();
    }

    #[tokio::test]
    async fn ensure_vector_index_tolerates_small_dataset() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[doc_with_embedding("a", "x", "y")])
            .await
            .unwrap();
        // 1 row is below IVF-PQ's training threshold; method must swallow.
        s.ensure_vector_index().await.unwrap();
    }

    // --- v0.3: atlas migration + update + read round-trip --------------

    #[tokio::test]
    async fn open_creates_atlas_columns_on_empty_dataset() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let schema = s.table.schema().await.unwrap();
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert!(names.contains(&"atlas_x"));
        assert!(names.contains(&"atlas_y"));
        assert!(names.contains(&"atlas_cluster"));
    }

    #[tokio::test]
    async fn open_is_idempotent_on_atlas_columns() {
        let tmp = tempfile::tempdir().unwrap();
        // First open creates the table + adds atlas columns.
        let s1 = Storage::open(tmp.path(), None).await.unwrap();
        s1.upsert_docs(&[doc_with_embedding("a", "x", "y")])
            .await
            .unwrap();
        drop(s1);
        // Second open re-runs the migration; should no-op + not error.
        let s2 = Storage::open(tmp.path(), None).await.unwrap();
        let count = s2.count_rows().await.unwrap();
        assert_eq!(count, 1);
    }

    // --- v0.9 M1: memory-meta migration + projection round-trip --------

    #[tokio::test]
    async fn open_creates_memory_columns_on_empty_dataset() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let schema = s.table.schema().await.unwrap();
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert!(names.contains(&"kb_salience"));
        assert!(names.contains(&"kb_decay"));
        assert!(names.contains(&"kb_supersedes"));
    }

    #[tokio::test]
    async fn memory_metas_read_back_through_search_projection() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let mut d = doc_with_embedding("a", "alpha memory", "body text");
        d.kb_salience = Some(0.8);
        d.kb_decay = Some("fast".into());
        d.kb_supersedes = Some("dead00beef00".into());
        d.mtime_unix = 1_700_000_000;
        s.upsert_docs(&[d]).await.unwrap();

        // The widened search projection (M1) carries the memory metas +
        // mtime so `memory::rerank` can read them off a search hit.
        let qvec = toy_embed("alpha memory body text");
        let hits = s.vector_query(&qvec, 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kb_salience, Some(0.8));
        assert_eq!(hits[0].kb_decay.as_deref(), Some("fast"));
        assert_eq!(hits[0].kb_supersedes.as_deref(), Some("dead00beef00"));
        assert_eq!(hits[0].mtime_unix, Some(1_700_000_000));
    }

    #[tokio::test]
    async fn open_creates_u3_provenance_columns_on_empty_dataset() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let schema = s.table.schema().await.unwrap();
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert!(names.contains(&"kb_author"));
        assert!(names.contains(&"kb_source_kb"));
        assert!(names.contains(&"kb_source_artifact"));
        assert!(names.contains(&"kb_source_anchor"));
    }

    // CT-A1 — storage round-trip: write a highlight-provenance memory
    // fixture through the same `upsert_docs` path the indexer uses, then
    // read it back through the widened `SEARCH_PROJECTION` (what recall
    // actually queries) AND `get_full_doc` (what relocate/F3a reads).
    #[tokio::test]
    async fn u3_provenance_read_back_through_search_projection_and_full_doc() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let mut d = doc_with_embedding("h1", "Highlighted claim", "the selection, verbatim");
        d.kb_author = Some("you".into());
        d.kb_source_kb = Some("kb-docs".into());
        d.kb_source_artifact = Some("a1b2c3d4e5f6".into());
        d.kb_source_anchor = Some(r#"{"kind":"section","id":"intro"}"#.into());
        s.upsert_docs(&[d]).await.unwrap();

        let qvec = toy_embed("Highlighted claim the selection, verbatim");
        let hits = s.vector_query(&qvec, 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kb_author.as_deref(), Some("you"));
        assert_eq!(hits[0].kb_source_kb.as_deref(), Some("kb-docs"));
        assert_eq!(hits[0].kb_source_artifact.as_deref(), Some("a1b2c3d4e5f6"));
        assert_eq!(
            hits[0].kb_source_anchor.as_deref(),
            Some(r#"{"kind":"section","id":"intro"}"#)
        );

        let full = s.get_full_doc("h1").await.unwrap().expect("h1 exists");
        assert_eq!(full.kb_author.as_deref(), Some("you"));
        assert_eq!(full.kb_source_kb.as_deref(), Some("kb-docs"));
        assert_eq!(full.kb_source_artifact.as_deref(), Some("a1b2c3d4e5f6"));
        assert_eq!(
            full.kb_source_anchor.as_deref(),
            Some(r#"{"kind":"section","id":"intro"}"#)
        );
    }

    #[tokio::test]
    async fn list_docs_with_kb_source_artifact_matches_the_kb_and_id_pair() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let mut lifted = fixture_doc("mem1", "Lifted claim", "body");
        lifted.kb_source_kb = Some("kb-docs".into());
        lifted.kb_source_artifact = Some("a1b2c3d4e5f6".into());
        // Same artifact id, but from a DIFFERENT source kb — must not match
        // (invariant #27: an id alone isn't unique across kbs).
        let mut other_kb = fixture_doc("mem2", "Different origin kb", "body");
        other_kb.kb_source_kb = Some("kb-other".into());
        other_kb.kb_source_artifact = Some("a1b2c3d4e5f6".into());
        // Same source kb, but a different artifact id — must not match.
        let mut other_id = fixture_doc("mem3", "Different origin id", "body");
        other_id.kb_source_kb = Some("kb-docs".into());
        other_id.kb_source_artifact = Some("deadbeefcafe".into());
        // No provenance at all — an ordinary `kb remember` — must not match.
        let plain = fixture_doc("mem4", "No provenance", "body");
        s.upsert_docs(&[lifted, other_kb, other_id, plain])
            .await
            .unwrap();

        let rows = s
            .list_docs_with_kb_source_artifact("kb-docs", "a1b2c3d4e5f6", 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "mem1");
        assert_eq!(rows[0].kb_source_kb.as_deref(), Some("kb-docs"));
        assert_eq!(rows[0].kb_source_artifact.as_deref(), Some("a1b2c3d4e5f6"));
    }

    #[tokio::test]
    async fn list_docs_with_kb_source_artifact_empty_inputs_yield_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        assert!(s
            .list_docs_with_kb_source_artifact("", "a1b2c3d4e5f6", 10)
            .await
            .unwrap()
            .is_empty());
        assert!(s
            .list_docs_with_kb_source_artifact("kb-docs", "", 10)
            .await
            .unwrap()
            .is_empty());
    }

    // --- MI-W2.4a: lineage-walk lance methods ---------------------------

    #[tokio::test]
    async fn lineage_by_id_reads_forward_pointer_and_status() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let mut old = fixture_doc("old1", "Old Fact", "body");
        old.kb_status = Some("forgotten".into());
        let mut new = fixture_doc("new1", "New Fact", "body");
        new.kb_supersedes = Some("old1".into());
        s.upsert_docs(&[old, new]).await.unwrap();

        let forward = s.lineage_by_id("new1").await.unwrap().expect("new1 exists");
        assert_eq!(forward.kb_supersedes.as_deref(), Some("old1"));

        let target = s.lineage_by_id("old1").await.unwrap().expect("old1 exists");
        assert_eq!(target.kb_status.as_deref(), Some("forgotten"));
        assert_eq!(target.kb_supersedes, None);

        assert!(s.lineage_by_id("doesnotexist0").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn find_superseded_by_reverse_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let old = fixture_doc("old2", "Old", "body");
        let mut new = fixture_doc("new2", "New", "body");
        new.kb_supersedes = Some("old2".into());
        s.upsert_docs(&[old, new]).await.unwrap();

        let matches = s.find_superseded_by("old2").await.unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "new2");

        // A memory nothing supersedes → empty, not an error.
        assert!(s.find_superseded_by("new2").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn find_superseded_by_sorts_ambiguous_matches_deterministically() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        // Two DIFFERENT memories both (unusually) claim to supersede the
        // same target — an ambiguous fork nothing prevents at write time.
        let old = fixture_doc("old3", "Old", "body");
        let mut a = fixture_doc("zzz-later", "A", "body");
        a.kb_supersedes = Some("old3".into());
        a.created_unix = Some(200);
        let mut b = fixture_doc("aaa-earlier", "B", "body");
        b.kb_supersedes = Some("old3".into());
        b.created_unix = Some(100);
        s.upsert_docs(&[old, a, b]).await.unwrap();

        let matches = s.find_superseded_by("old3").await.unwrap();
        assert_eq!(matches.len(), 2, "both branches returned, none dropped");
        // Sorted by created_unix ascending — the earlier claim first,
        // regardless of id/insertion order.
        assert_eq!(matches[0].id, "aaa-earlier");
        assert_eq!(matches[1].id, "zzz-later");
    }

    #[tokio::test]
    async fn update_atlas_writes_then_list_with_atlas_reads_back() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[
            doc_with_embedding("a", "alpha", "x"),
            doc_with_embedding("b", "beta", "y"),
        ])
        .await
        .unwrap();
        s.update_atlas(&[("a".into(), 0.25, 0.75, 1), ("b".into(), 0.6, 0.4, 2)])
            .await
            .unwrap();
        let mut docs = s.list_docs_with_atlas(10).await.unwrap();
        docs.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(docs.len(), 2);
        let a = &docs[0];
        assert_eq!(a.id, "a");
        assert!((a.atlas_x.unwrap() - 0.25).abs() < 1e-5);
        assert!((a.atlas_y.unwrap() - 0.75).abs() < 1e-5);
        assert_eq!(a.atlas_cluster, Some(1));
        let b = &docs[1];
        assert_eq!(b.atlas_cluster, Some(2));
    }

    #[tokio::test]
    async fn list_docs_default_omits_atlas_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[doc_with_embedding("a", "x", "y")])
            .await
            .unwrap();
        s.update_atlas(&[("a".into(), 0.1, 0.2, 3)]).await.unwrap();
        let docs = s.list_docs(10).await.unwrap();
        // The slim shape doesn't request the columns, so the helper
        // returns None for them.
        assert_eq!(docs.len(), 1);
        assert!(docs[0].atlas_x.is_none());
        assert!(docs[0].atlas_y.is_none());
        assert!(docs[0].atlas_cluster.is_none());
    }

    #[tokio::test]
    async fn list_docs_returns_most_recently_indexed_when_truncated() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let mut docs: Vec<Doc> = (0..5)
            .map(|i| {
                let mut d = doc_with_embedding(
                    &format!("d{i}"),
                    &format!("title-{i}"),
                    &format!("body-{i}"),
                );
                d.indexed_at_unix = 1_000 + i as i64;
                d
            })
            .collect();
        // Insert in reverse-time order so lance storage order can't
        // accidentally coincide with the indexed-time order.
        docs.reverse();
        s.upsert_docs(&docs).await.unwrap();

        let truncated = s.list_docs(2).await.unwrap();
        assert_eq!(truncated.len(), 2);
        assert_eq!(truncated[0].id, "d4");
        assert_eq!(truncated[1].id, "d3");

        let full = s.list_docs(10).await.unwrap();
        let ids: Vec<&str> = full.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["d4", "d3", "d2", "d1", "d0"]);
    }

    // GC-B1 — a bulk reindex commonly lands many rows at the SAME
    // `indexed_at_unix` second; `gallery_snapshot` (kb-server) memoizes
    // `list_docs`'s row order verbatim as the served gallery (root
    // invariant #15), so an unbroken tie must not ride physical scan
    // order. All 3 docs here share one timestamp; the tie resolves
    // id-ascending regardless of insertion order.
    #[test]
    fn list_docs_tie_break_is_insertion_order_independent() {
        use crate::test_support::assert_order_independent;
        let rows = vec!["z", "a", "m"];
        assert_order_independent(&rows, 8, |ids| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let tmp = tempfile::tempdir().unwrap();
                let s = Storage::open(tmp.path(), None).await.unwrap();
                let docs: Vec<Doc> = ids
                    .into_iter()
                    .map(|id| {
                        let mut d = doc_with_embedding(id, id, id);
                        d.indexed_at_unix = 1_000; // identical for every row
                        d
                    })
                    .collect();
                s.upsert_docs(&docs).await.unwrap();
                s.list_docs(10)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|d| d.id)
                    .collect::<Vec<_>>()
            })
        });
    }

    #[tokio::test]
    async fn list_embeddings_returns_id_and_vector_pairs() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[
            doc_with_embedding("a", "alpha", "x"),
            doc_with_embedding("b", "beta", "y"),
        ])
        .await
        .unwrap();
        let embs = s.list_embeddings().await.unwrap();
        assert_eq!(embs.len(), 2);
        for (id, v) in &embs {
            assert!(id == "a" || id == "b");
            assert_eq!(v.len(), 384);
        }
    }

    #[tokio::test]
    async fn list_embeddings_is_sorted_by_id() {
        // GC-B1 — `list_embeddings` is a plain `select` with no ORDER BY;
        // lance returns physical scan order (unsorted WalkDir / merge_insert
        // insertion order — docs/research/
        // atlas-input-order-determinism-2026-07.html §2). It is the sole
        // input to the order-sensitive `compute_layout` (atlas recompute/
        // recluster), so its OWN contract is now "id-ascending", not
        // "whatever lance handed back". Insert out of id order and check.
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[
            doc_with_embedding("z", "zeta", "z"),
            doc_with_embedding("a", "alpha", "a"),
            doc_with_embedding("m", "mu", "m"),
        ])
        .await
        .unwrap();
        let pairs = s.list_embeddings().await.unwrap();
        let ids: Vec<&str> = pairs.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["a", "m", "z"],
            "list_embeddings must be id-ascending"
        );
    }

    // GC-B1 — the atlas input, over N seeded insertion orders: regardless
    // of which order the 3 docs are upserted in (each upsert is its own
    // tiny fragment/merge_insert commit), `list_embeddings` must always
    // return the same id-ascending sequence.
    #[test]
    fn list_embeddings_order_is_insertion_order_independent() {
        use crate::test_support::assert_order_independent;
        let rows = vec![
            ("z", "zeta", "z"),
            ("a", "alpha", "a"),
            ("m", "mu", "m"),
            ("q", "qux", "q"),
        ];
        assert_order_independent(&rows, 8, |docs| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let tmp = tempfile::tempdir().unwrap();
                let s = Storage::open(tmp.path(), None).await.unwrap();
                let fixtures: Vec<Doc> = docs
                    .into_iter()
                    .map(|(id, title, body)| doc_with_embedding(id, title, body))
                    .collect();
                s.upsert_docs(&fixtures).await.unwrap();
                s.list_embeddings()
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>()
            })
        });
    }

    #[tokio::test]
    async fn clear_embeddings_nulls_all_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[
            doc_with_embedding("a", "alpha", "x"),
            doc_with_embedding("b", "beta", "y"),
        ])
        .await
        .unwrap();
        assert_eq!(s.list_embeddings().await.unwrap().len(), 2);
        s.clear_embeddings().await.unwrap();
        // After clearing, list_embeddings (which skips nulls) returns
        // zero rows even though the rows themselves still exist.
        assert_eq!(s.list_embeddings().await.unwrap().len(), 0);
        assert_eq!(s.count_rows().await.unwrap(), 2);
    }

    // --- N-track: notes list + task columns ----------------------------

    #[tokio::test]
    async fn open_creates_task_columns_on_empty_dataset() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let schema = s.table.schema().await.unwrap();
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert!(names.contains(&"task_done"));
        assert!(names.contains(&"task_total"));
    }

    #[tokio::test]
    async fn list_notes_filters_to_category_note_with_task_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let mut note = fixture_doc("n", "Deploy", "tasks");
        note.path = "/tmp/n.md".into(); // a real note is Markdown
        note.kb_category = Some("note".into());
        note.task_done = Some(1);
        note.task_total = Some(3);
        let mut research = fixture_doc("r", "Research", "prose");
        research.kb_category = Some("research".into());
        // Regression (the kb.example.com bug): an HTML artifact merely tagged
        // `kb-category: note` is NOT an editable note and must be filtered out
        // (else it pollutes /notes + vanishes from the gallery). `fixture_doc`
        // already gives it a `.html` path.
        let mut html_note = fixture_doc("h", "HTML write-up", "<p>prose</p>");
        html_note.kb_category = Some("note".into());
        s.upsert_docs(&[note, research, html_note]).await.unwrap();

        let notes = s.list_notes(u32::MAX).await.unwrap();
        assert_eq!(
            notes.len(),
            1,
            "only the Markdown kb_category=note row returns (HTML note excluded)"
        );
        assert_eq!(notes[0].id, "n");
        assert_eq!(notes[0].task_done, Some(1));
        assert_eq!(notes[0].task_total, Some(3));
    }

    #[tokio::test]
    async fn count_docs_by_kb_session_groups_in_one_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();

        // Empty input → empty map, no scan semantics to observe.
        assert!(s.count_docs_by_kb_session(&[]).await.unwrap().is_empty());

        let mut a = fixture_doc("a", "Memory A", "body");
        a.kb_session = Some("s1".into());
        let mut b = fixture_doc("b", "Memory B", "body");
        b.kb_session = Some("s1".into());
        let mut c = fixture_doc("c", "Memory C", "body");
        c.kb_session = Some("s2".into());
        // The memory-session transcript row itself is excluded (same
        // semantics as the single-id `count_docs_with_kb_session`).
        let mut t = fixture_doc("t", "Transcript", "jsonl digest");
        t.kb_session = Some("s1".into());
        t.kb_category = Some("memory-session".into());
        // A session nobody asked about — must not leak into the map.
        let mut d = fixture_doc("d", "Memory D", "body");
        d.kb_session = Some("s3".into());
        // No session at all — ignored.
        let plain = fixture_doc("p", "Plain", "body");
        s.upsert_docs(&[a, b, c, t, d, plain]).await.unwrap();

        let ids = vec!["s1".to_string(), "s2".to_string(), "s0".to_string()];
        let counts = s.count_docs_by_kb_session(&ids).await.unwrap();
        assert_eq!(counts.get("s1"), Some(&2), "transcript row excluded");
        assert_eq!(counts.get("s2"), Some(&1));
        assert!(
            !counts.contains_key("s0"),
            "id with zero docs is absent (caller defaults to 0)"
        );
        assert!(!counts.contains_key("s3"), "unrequested id never surfaces");
        assert_eq!(counts.len(), 2);

        // Parity with the single-id filter count.
        assert_eq!(s.count_docs_with_kb_session("s1").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn task_columns_back_compat_null() {
        // A row written without task counts reads back None (not 0), so the
        // additive columns are backward-compatible with pre-v17 rows.
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        let mut note = fixture_doc("n", "Old note", "x");
        note.path = "/tmp/n.md".into(); // a real note is Markdown
        note.kb_category = Some("note".into());
        // task_done/total left as None (placeholder default).
        s.upsert_docs(&[note]).await.unwrap();
        let notes = s.list_notes(u32::MAX).await.unwrap();
        assert_eq!(notes[0].task_done, None);
        assert_eq!(notes[0].task_total, None);
    }

    // ---- GC-B2: decode-skip observability ----

    fn malformed_batch() -> arrow::record_batch::RecordBatch {
        use arrow::array::Int64Array;
        use arrow::datatypes::{DataType, Field, Schema};
        // None of `id`/`title`/`path`/`kb_category` present — every required
        // string column is missing, so the typed decode must skip it.
        let schema = Arc::new(Schema::new(vec![Field::new(
            "unrelated_col",
            DataType::Int64,
            false,
        )]));
        arrow::record_batch::RecordBatch::try_new(schema, vec![Arc::new(Int64Array::from(vec![1]))])
            .unwrap()
    }

    #[test]
    fn batches_to_summaries_skips_malformed_batch_and_counts_it() {
        let skips = DecodeSkipCounter::default();
        let hits = batches_to_summaries(&[malformed_batch()], &skips);
        assert!(hits.is_empty(), "malformed batch contributes no rows");
        assert_eq!(skips.count.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn batches_to_embeddings_skips_malformed_batch_and_counts_it() {
        let skips = DecodeSkipCounter::default();
        let pairs = batches_to_embeddings(&[malformed_batch()], &skips);
        assert!(pairs.is_empty(), "malformed batch contributes no rows");
        assert_eq!(skips.count.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn decode_skip_counter_warns_once_but_counts_every_skip() {
        use std::sync::atomic::Ordering;
        let skips = DecodeSkipCounter::default();
        assert!(!skips.warned.load(Ordering::Relaxed));
        skips.note("first skip");
        assert_eq!(skips.count.load(Ordering::Relaxed), 1);
        assert!(
            skips.warned.load(Ordering::Relaxed),
            "latch flips on first skip"
        );
        // A second (and third) skip keeps incrementing the count; the latch
        // just stays flipped (no re-warn, but this test only proves the
        // counter — the "only one WARN emitted" half is a log side-effect
        // this unit test doesn't capture).
        skips.note("second skip");
        skips.note("third skip");
        assert_eq!(skips.count.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn storage_decode_skip_count_starts_zero_and_reflects_the_counter() {
        // Wired through `Storage::decode_skip_count()` — the getter the
        // `/api/stats` route reads. A fresh dataset has decoded nothing
        // malformed yet, so it starts at 0; bumping the internal counter
        // (as the two skip sites above do) is reflected immediately.
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        assert_eq!(s.decode_skip_count(), 0);
        s.decode_skips.note("synthetic test skip");
        assert_eq!(s.decode_skip_count(), 1);
    }

    /// GC-B7 — `compact_all` (production defaults) never physically reclaims
    /// disk inside a fast test: the 5-minute retention window is a deliberate
    /// safety margin, and every version here is milliseconds old. This test
    /// exercises the actual reclaim mechanism via `compact_all_with_retention`
    /// with a zero window + `delete_unverified: true` — the knobs `compact_all`
    /// can't expose without breaking that safety margin — proving the
    /// mechanism itself (not just that it's gated) actually frees old-version
    /// bytes instead of the `old_versions_removed=0` the 2026-07-11 scale test
    /// measured under lance's own default policy.
    #[tokio::test]
    async fn compact_all_with_retention_reclaims_old_versions() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();

        // One `upsert_docs` call per doc (worst case: one manifest version
        // per doc, exactly the pre-GC-B7 ingest pattern) plus a second pass
        // that re-upserts every doc with a bigger body, so each id's FIRST
        // version becomes superseded — that's the disk this test proves gets
        // reclaimed.
        for i in 0..20 {
            let d = fixture_doc(&format!("r{i}"), &format!("doc {i}"), "small body");
            s.upsert_docs(std::slice::from_ref(&d)).await.unwrap();
        }
        for i in 0..20 {
            let d = fixture_doc(
                &format!("r{i}"),
                &format!("doc {i} revised"),
                &"revised body ".repeat(200),
            );
            s.upsert_docs(std::slice::from_ref(&d)).await.unwrap();
        }

        let before = s.dataset_stats().await.unwrap();
        assert_eq!(before.rows, 20);
        assert!(
            before.versions >= 40,
            "expected ≥40 manifest versions from 40 individual upserts, got {}",
            before.versions
        );

        let stats = s
            .compact_all_with_retention(0, true)
            .await
            .expect("compact_all_with_retention should succeed");
        assert!(
            stats.old_versions_removed > 0,
            "a 0-minute retention window with delete_unverified=true must reclaim \
             SOME old manifest versions, got stats: {stats:?}"
        );

        let after = s.dataset_stats().await.unwrap();
        assert_eq!(after.rows, 20, "row data must survive the reclaim intact");
        assert!(
            after.versions < before.versions,
            "expected fewer manifest versions after reclaim ({} → {})",
            before.versions,
            after.versions
        );

        // Every doc is still resolvable with its revised (post-supersede)
        // content — the reclaim pruned OLD versions, not live rows.
        let hit = s.get_by_id("r0").await.unwrap();
        assert!(
            hit.is_some_and(|d| d.title == "doc 0 revised"),
            "surviving row must be the latest (post-supersede) content"
        );
    }

    // ---- Fix 1/2/3: cache caps, rebuild throttle, orphan _indices GC ----

    /// Fix 1 — the capped-cache session path (`open_with_options` with a
    /// custom `lancedb::Session`) opens, writes, and searches exactly like
    /// the legacy uncapped path. 1 MiB caps prove a small-but-nonzero value
    /// works (lance evicts under pressure rather than erroring).
    #[tokio::test]
    async fn open_with_capped_caches_searches_normally() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = LanceOptions {
            index_cache_mb: 1,
            metadata_cache_mb: 1,
            index_rebuild_min_secs: 0,
        };
        let s = Storage::open_with_options(tmp.path(), None, opts)
            .await
            .unwrap();
        s.upsert_docs(&[fixture_doc("a", "Borrow Checker", "lifetime conflicts")])
            .await
            .unwrap();
        s.ensure_fts_index().await.unwrap();
        let hits = s.bm25_query("borrow", 5, false).await.unwrap();
        assert!(hits.iter().any(|h| h.id == "a"));
    }

    /// Fix 2 — the `index_rebuild_min_secs` throttle: the first build after
    /// open is immediate, a dirty-triggered rebuild inside the window is
    /// suppressed (dirty flag left set), and — the correctness half — the
    /// suppressed window still finds the new rows because lance 4.0.0 unions
    /// the stale FTS index with a flat BM25 scan over unindexed fragments
    /// (`Scanner::plan_match_query`). Once the window elapses (simulated by
    /// aging the last-build stamp), the next ensure rebuilds.
    #[tokio::test]
    async fn index_rebuild_throttle_suppresses_rebuild_within_interval() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = LanceOptions {
            index_cache_mb: 0,
            metadata_cache_mb: 0,
            index_rebuild_min_secs: 3600,
        };
        let s = Storage::open_with_options(tmp.path(), None, opts)
            .await
            .unwrap();
        s.upsert_docs(&[fixture_doc("a", "Borrow checker", "lifetime conflicts")])
            .await
            .unwrap();
        s.ensure_fts_index().await.unwrap();
        assert_eq!(
            s.fts_rebuild_count(),
            1,
            "first build after open is immediate"
        );

        // A mutation re-dirties, but the rebuild is inside the throttle
        // window → suppressed.
        s.upsert_docs(&[fixture_doc("b", "Garbage collector", "tracing pauses")])
            .await
            .unwrap();
        s.ensure_fts_index().await.unwrap();
        assert_eq!(
            s.fts_rebuild_count(),
            1,
            "a second rebuild within the interval is suppressed"
        );
        assert!(
            s.bm25_query("garbage", 10, false)
                .await
                .unwrap()
                .iter()
                .any(|h| h.id == "b"),
            "throttled window still finds the new row via lance's flat-scan union"
        );

        // Window elapsed → the dirty flag (never cleared) triggers the build.
        *s.last_fts_build.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(3700));
        s.ensure_fts_index().await.unwrap();
        assert_eq!(
            s.fts_rebuild_count(),
            2,
            "the first ensure after the window rebuilds"
        );
    }

    /// Fix 2 — `index_rebuild_min_secs = 0` opts out: every dirty flag
    /// rebuilds, byte-for-byte the pre-throttle behavior (same shape as
    /// `ensure_fts_gate_rebuilds_after_mutation_and_stays_fresh`).
    #[tokio::test]
    async fn index_rebuild_throttle_zero_disables_throttling() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open_with_options(tmp.path(), None, LanceOptions::unbounded())
            .await
            .unwrap();
        s.upsert_docs(&[fixture_doc("a", "Borrow checker", "lifetime conflicts")])
            .await
            .unwrap();
        s.ensure_fts_index().await.unwrap();
        s.upsert_docs(&[fixture_doc("b", "Garbage collector", "tracing pauses")])
            .await
            .unwrap();
        s.ensure_fts_index().await.unwrap();
        assert_eq!(
            s.fts_rebuild_count(),
            2,
            "zero interval = rebuild per dirty"
        );
    }

    /// Fix 3 — the orphan `_indices` GC removes an unreferenced uuid dir
    /// (past the grace window), keeps manifest-referenced dirs, keeps dirs
    /// inside the grace window, and never touches non-uuid-shaped entries.
    #[tokio::test]
    async fn orphan_index_gc_removes_unreferenced_and_keeps_live_and_recent() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Storage::open(tmp.path(), None).await.unwrap();
        s.upsert_docs(&[fixture_doc("a", "Hello", "world")])
            .await
            .unwrap();
        s.ensure_fts_index().await.unwrap();

        let indices_dir = tmp
            .path()
            .join(format!("{TABLE_NAME}.lance"))
            .join("_indices");
        let live_dirs: Vec<PathBuf> = std::fs::read_dir(&indices_dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        assert!(
            !live_dirs.is_empty(),
            "the FTS build should have produced live index dirs"
        );

        // An orphan: uuid-shaped, referenced by no manifest entry, with a
        // payload so reclaimed bytes are observable.
        let orphan = indices_dir.join("00000000-0000-0000-0000-000000000000");
        std::fs::create_dir_all(orphan.join("nested")).unwrap();
        std::fs::write(orphan.join("nested/part.bin"), vec![0u8; 4096]).unwrap();
        // Decoys the GC must never touch: a non-uuid dir and a non-uuid file.
        let decoy_dir = indices_dir.join("not-a-uuid");
        std::fs::create_dir_all(&decoy_dir).unwrap();
        let decoy_file = indices_dir.join("11111111-1111-1111-1111-111111111111.tmp");
        std::fs::write(&decoy_file, b"x").unwrap();

        // Production grace window: everything here is seconds old → nothing
        // is deletable, orphan included.
        let stats = s.gc_orphan_index_dirs().await;
        assert_eq!(
            stats,
            OrphanGcStats::default(),
            "dirs inside the grace window are never deleted"
        );
        assert!(orphan.exists());

        // Zero grace (the `compact_all_with_retention(0, ..)` precedent):
        // the orphan is reclaimed; live dirs + decoys survive.
        let stats = s.gc_orphan_index_dirs_with_grace(0).await;
        assert_eq!(stats.dirs_removed, 1, "exactly the orphan is removed");
        assert_eq!(stats.bytes_removed, 4096);
        assert!(!orphan.exists());
        assert!(decoy_dir.exists(), "non-uuid dir is never touched");
        assert!(decoy_file.exists(), "non-uuid file is never touched");
        for d in &live_dirs {
            assert!(d.exists(), "live index dir preserved: {}", d.display());
        }
    }

    /// Fix 3 — the uuid-shape gate itself: anything that isn't `8-4-4-4-12`
    /// hex is never a GC candidate.
    #[test]
    fn is_uuid_dir_name_accepts_only_canonical_uuid_shape() {
        assert!(is_uuid_dir_name("00000000-0000-0000-0000-000000000000"));
        assert!(is_uuid_dir_name("01234567-89ab-cdef-0123-456789abcdef"));
        assert!(is_uuid_dir_name("FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF"));
        for bad in [
            "",
            "not-a-uuid",
            "00000000-0000-0000-0000-00000000000",   // 35 chars
            "00000000-0000-0000-0000-0000000000000", // 37 chars
            "00000000_0000-0000-0000-000000000000",  // wrong separator
            "00000000-0000-0000-0000-00000000000g",  // non-hex
            "00000000-0000-0000-0000-000000000000.tmp", // suffix
            " 00000000-0000-0000-0000-000000000000", // leading space
        ] {
            assert!(!is_uuid_dir_name(bad), "must reject: {bad:?}");
        }
    }
}
