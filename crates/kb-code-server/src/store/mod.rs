//! kb-code's own per-daemon SQLite store — blob-hash-keyed derived data
//! (ADR-2: symbols/highlights are keyed by `(blob_hash, salt)`, never by
//! path, so unchanged content is never re-parsed and a branch switch
//! re-derives nothing). Lives at `<state>/kb-code/index.db`
//! (`KbPaths::new("kb-code").state.join("index.db")`), a file entirely
//! separate from kb's own per-kb `index.db` files.
//!
//! This MIRRORS kb-core's `storage/sqlite.rs` idioms (refinery migrations
//! embedded from `./migrations`, WAL journal mode, `busy_timeout`,
//! `foreign_keys = ON`) rather than importing it — kb-code owns its own
//! schema (`repos`/`files`/`symbols`/`highlights`), which has nothing to do
//! with kb's lance-backed `Doc` schema. See `crates/kb-code-server/
//! migrations/V0001__initial.sql` for the exact table shapes and the
//! rationale for each column.
//!
//! **Single-writer discipline**: `Store` wraps one `rusqlite::Connection`
//! behind a `parking_lot::Mutex` — fine for this Wave's read-mostly,
//! single-ingest-at-a-time workload (`ingest::index_repo_working_tree`
//! walks one repo at a time, in-process). kb-core's storage actor (an
//! mpsc-backed single-writer task per kb, with read/write priority lanes —
//! see `crates/kb-core/CLAUDE.md` invariant 2) is the documented upgrade
//! path if/when kb-code needs concurrent multi-repo ingest or read
//! throughput under a write backlog; not needed yet, and deliberately not
//! built ahead of a real need.
//!
//! **Async-context access MUST go through [`StoreBlocking::run_blocking`]**
//! (2026-08-31 prod incident, v0.40 deploy): a route handler that calls a
//! `Store` method inline blocks its tokio async-worker thread for the whole
//! mutex-wait + query. During a heavy sink reconcile burst (a large repo's
//! first deep index, cold page cache, disk-bound host) every store-touching
//! probe parks a worker for seconds-to-minutes; enough concurrent probes
//! saturate ALL async workers and the runtime can no longer poll ANY task —
//! including `/healthz`, which touches no store state at all. That is how
//! kbc.example.com went dark three deploy attempts in a row while the process sat
//! at near-zero CPU: not a deadlock, worker starvation. The old convention
//! ("store reads are fast, no `spawn_blocking` needed" — routes.rs) is
//! REVERSED: every `Store` call reachable from async context (route
//! handlers, background async tasks like the on-boot backfill) runs inside
//! `run_blocking`, which parks the wait on tokio's blocking pool (hundreds
//! of threads) instead. Sync contexts — the sink worker's own
//! `spawn_blocking` closures, `ingest::*` called from them, boot code before
//! the runtime serves — keep calling `Store` methods directly; wrapping
//! those would just double-hop the blocking pool.
//!
//! Second half of the same incident: the connection mutex is
//! `parking_lot::Mutex`, NOT `std::sync::Mutex`. std's unfair handoff let
//! the sink's tight per-file lock/unlock loop re-acquire immediately every
//! time, starving a parked reader for an entire reconcile burst (observed
//! live on v0.39: `/api/identity` >90 s while individual sink messages were
//! ~11 s). parking_lot's eventual-fairness forces a fair handoff under
//! sustained contention, bounding a reader's wait to roughly one sink
//! message. Both halves are needed: `run_blocking` keeps the RUNTIME alive
//! regardless of how long the wait is; fairness keeps the wait SHORT.

pub(in crate::store) use parking_lot::Mutex;
pub(in crate::store) use rusqlite::{params, Connection, OptionalExtension, Transaction};
pub(in crate::store) use std::collections::HashMap;
pub(in crate::store) use std::path::Path;
pub(in crate::store) use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub(in crate::store) use crate::extract::Symbol;
pub(in crate::store) use crate::highlight::Span;
pub(in crate::store) use crate::transcripts::indexer::IndexedTurn;

// Method groups AND the row types, constants and row-mapping helpers they
// own live in child modules, so this file stays the connection surface
// (`Store`, `StoreError`, migrations). `crate::store::*` paths are
// unchanged: every moved public item is re-exported below.
mod analytics;
mod annotations;
mod behavioral;
mod bookmarks;
mod canvas;
mod claims;
mod comments;
mod doclens;
mod entities;
mod files;
mod findings;
mod lanes;
mod mutations;
mod rails;
mod reading;
mod recipes;
mod review_docs;
mod review_stores;
mod reviews;
mod scip_runs;
mod symbols;
#[cfg(test)]
mod tests;
mod trails;
mod transcripts;
mod workspace;

// Row types, constants and shared helpers now live beside the domain
// that owns them; every `crate::store::X` path is re-exported unchanged.
pub use self::analytics::{AnalyticsFindingRow, RecurrenceRow, RECURRENCE_MIN_REVIEWS};
#[cfg(test)]
use self::annotations::annotation_row_from;
use self::annotations::{insert_annotation_on, update_annotation_on};
pub use self::annotations::{
    AnnotationOpReport, AnnotationRow, AnnotationSuggestionRow, PreparedAnnotationOp,
    PreparedSuggestionWrite,
};
pub use self::behavioral::{AuthorStatsRow, BehavioralMetaRow, PathStatsRow, SessionSignalsRow};
pub use self::bookmarks::{BookmarkRow, TodoItemRow};
pub use self::canvas::{
    CanvasApplyOutcome, CanvasBoardRow, CanvasBoardSummaryRow, CanvasEdgeRow, CanvasNodeRow,
    CanvasSetRow, CanvasSetSummaryRow, CanvasStepRow, NewCanvasBoard, NewCanvasEdge, NewCanvasNode,
    NewCanvasStep,
};
pub use self::claims::{ClaimFilter, ClaimRow};
pub use self::comments::{CommentRow, NewComment};
pub use self::doclens::{DocLensPin, DocRefRow, DocRefWrite, DoclensSyncCursor, NewDocRef};
pub use self::entities::EntityDefRow;
use self::findings::reconcile_findings_import_on;
pub use self::findings::{
    derive_finding_anchor, is_valid_disposition, is_valid_finding_origin, is_valid_location_kind,
    is_valid_severity, location_lines_json, slug_ordinal, AdoptedReviewFinding, ComposeOutcome,
    DerivedFindingAnchor, FindingIdentity, FindingsImportMode, FindingsImportOutcome,
    ImportedFinding, NewReviewFinding, OtherReviewJudgement, ReviewFindingRow, DISPOSITIONS,
    DISPOSITION_AGREE, DISPOSITION_DISPUTE, DISPOSITION_FIX_LATER, DISPOSITION_WAIVE,
    FINDING_ORIGINS, FINDING_ORIGIN_IMPORT, FINDING_ORIGIN_MANUAL, LOCATION_KINDS,
    LOCATION_KIND_MULTI, LOCATION_KIND_RANGE, LOCATION_KIND_SINGLE, LOCATION_KIND_WHOLE_FILE,
    SEVERITIES, SEVERITY_BLOCKER, SEVERITY_CONCERN, SEVERITY_OK, SUPERSEDED_REASON_NOT_IN_REIMPORT,
    SUPERSEDED_REASON_REPLACED,
};
pub use self::lanes::{
    LaneFactIn, LaneFactRow, LaneGcCounts, LaneRunIn, LaneStatRow, LaneSummaryRow, LANE_GC_PAGE,
    LANE_SUMMARY_SCAN_CAP,
};
pub use self::mutations::{MutationIn, MutationRow};
pub use self::reading::{NewReadingSetSpan, ReadingSetRow, ReadingSetSpanRow};
pub use self::recipes::{RecipeRunRow, RecipeServerRow, RecipeTrustRow};
pub use self::review_docs::{ComposeDocOutcome, NewReviewDoc, ReviewDocRow};
pub use self::review_stores::{PatchsetBaseFields, RepoStoreRow, ReviewBaseRow, ReviewStoreRow};
pub use self::reviews::{
    NewReviewBase, NewReviewPr, ReviewHunkViewedRow, ReviewPatchsetRow, ReviewPrBinding,
    ReviewReport, ReviewRow, ReviewViewedRow,
};
pub use self::scip_runs::ScipRunRow;
pub(crate) use self::sweep::BILL_TABLES;
use self::sweep::{
    current_salt_cte, lang_prefix_pattern, mark_derived_in, sweep_stale_salt_table, SWEEP_TABLES,
};
pub use self::sweep::{CensusPage, StaleSaltSweepCounts, STALE_SALT_SWEEP_PAGE};
pub use self::symbols::{ScipOccurrenceIn, SeqProjectionRow};
pub use self::trails::{
    NewTrail, NewTrailStep, TrailAggregateRow, TrailNoteRow, TrailStepRow, TrailSummaryRow,
    TRAIL_GC_PAGE,
};
pub use self::transcripts::{
    CommitSessionRow, TranscriptFileRow, TranscriptPathHit, TranscriptSearchRow, TranscriptStats,
    TranscriptTurnRow,
};
pub use self::workspace::WorkspaceRow;
mod embedded {
    refinery::embed_migrations!("./migrations");
}

/// This binary's kb-code schema epoch — the highest version among the
/// migrations EMBEDDED in it (`crates/kb-code-server/migrations/`, a set
/// entirely separate from kb's own). Surfaced on `GET /api/identity` as
/// `schema_epoch` (kb-sibling/1, `kb_core::sibling`) and compared against
/// the volume's own epoch by [`Store::open`].
pub fn schema_epoch() -> u32 {
    kb_core::sibling::binary_epoch(&embedded::migrations::runner())
}

/// V72-B1 — the name refinery recorded for the V3 migration (`transcripts`,
/// from `V0003__transcripts.sql`). Shared by the repair below and its tests.
const V3_TRANSCRIPTS_NAME: &str = "transcripts";

/// V72-B1 — the checksum an archive-era binary wrote into
/// `refinery_schema_history` for V3 `transcripts`, before this repo went
/// public. Refinery's checksum hashes a migration's exact file content
/// (version + name + full SQL text — `refinery_core::Migration::
/// unapplied`), and the public-repo scrub anonymised an EXAMPLE PATH inside
/// a comment in that file (not executed SQL — see `migrations/
/// V0003__transcripts.sql`'s header), which changed the checksum. Read
/// from a copy of a real production kb-code state volume's
/// `refinery_schema_history` row (migrated by an archive-era binary) and
/// independently reproduced by hashing the archive-era file's exact
/// content; cross-checked against [`V3_TRANSCRIPTS_PUBLIC_CHECKSUM`] below
/// and the `migrations.checksums.json` golden — see `tests::v72_b1` for
/// all three. Dated 2026-09.
const V3_TRANSCRIPTS_ARCHIVE_CHECKSUM: &str = "17561702661079640667";

/// V72-B1 — the checksum this binary computes today for V3 `transcripts`
/// from the current (public-tree) migration file — i.e. what
/// `embedded::migrations::runner()` embeds. Also pinned as one row of the
/// `migrations.checksums.json` golden (`tests::v72_b1::
/// every_embedded_migration_checksum_matches_the_golden`), so a future
/// accidental edit to an APPLIED migration fails CI rather than silently
/// drifting again.
const V3_TRANSCRIPTS_PUBLIC_CHECKSUM: &str = "6341265312121235865";

/// V72-B1 — repair the ONE `refinery_schema_history` row the 2026-09
/// public-repo scrub diverged (see the constants above for the full
/// story). This is a ONE-TIME, NARROWLY-TARGETED fix, never a general
/// divergent-checksum bypass: refinery's own `abort_divergent` default
/// stays ON, and only a V3 row named `transcripts` whose checksum is
/// EXACTLY [`V3_TRANSCRIPTS_ARCHIVE_CHECKSUM`] is ever rewritten — to
/// EXACTLY [`V3_TRANSCRIPTS_PUBLIC_CHECKSUM`], never anything computed at
/// runtime. Any OTHER checksum at V3 (including one that's already
/// public, or a REAL divergence unrelated to this scrub) is left
/// untouched, so refinery's own guard still fires exactly as designed.
/// Idempotent: a volume already repaired (or already public, e.g. a fresh
/// volume this binary itself created) simply doesn't match the archive
/// value and this is a no-op.
///
/// Must run AFTER `kb_core::sibling::refuse_if_volume_ahead` (a stale
/// checksum must never be confused with a forward-migrated volume) and
/// BEFORE the refinery runner (which would otherwise abort on it first) —
/// see the call site in [`Store::open`].
fn repair_v3_transcripts_checksum(conn: &mut Connection) -> rusqlite::Result<()> {
    let history_table_exists: bool = conn.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'refinery_schema_history'",
        [],
        |row| row.get(0),
    )?;
    if !history_table_exists {
        // Brand-new volume — refinery creates the table itself on first run.
        return Ok(());
    }

    let tx = conn.transaction()?;
    let current: Option<String> = tx
        .query_row(
            "SELECT checksum FROM refinery_schema_history WHERE version = 3 AND name = ?1",
            params![V3_TRANSCRIPTS_NAME],
            |row| row.get(0),
        )
        .optional()?;
    if current.as_deref() != Some(V3_TRANSCRIPTS_ARCHIVE_CHECKSUM) {
        // Not the known archive-era value: no V3 row at all, already the
        // public value, already repaired, or a real divergence refinery
        // must still refuse. Touch nothing either way.
        return Ok(());
    }
    tx.execute(
        "UPDATE refinery_schema_history SET checksum = ?1 WHERE version = 3 AND name = ?2",
        params![V3_TRANSCRIPTS_PUBLIC_CHECKSUM, V3_TRANSCRIPTS_NAME],
    )?;
    tx.commit()?;
    tracing::info!(
        "migration checksum repaired: V3__transcripts (2026-09 public-scrub comment change)"
    );
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("kb-code store sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("kb-code store migration error: {0}")]
    Migration(String),
    /// kb-sibling/1 — the volume was forward-migrated by a NEWER binary.
    /// Distinct from [`StoreError::Migration`] on purpose: nothing was
    /// attempted, and the remediation is a deploy/restore, not a repair.
    /// Boot-only — no request handler can produce it.
    #[error("{0}")]
    SchemaEpoch(String),
    #[error("kb-code store io error: {0}")]
    Io(#[from] std::io::Error),
    /// V75-M1 — the pre-migration snapshot the Workspace-re-key epoch
    /// crossing requires could not be written, and no override was set.
    /// Boot-only, like [`StoreError::SchemaEpoch`], and for the same
    /// reason: an epoch is a one-way door and its only remedy is a
    /// restore, so migrating without a snapshot is the outage, not the
    /// refusal.
    #[error("{0}")]
    BackupRequired(String),
    #[error("kb-code store encoding error: {0}")]
    Encoding(#[from] serde_json::Error),
    /// Phase E3 — a `reading_sets` `(repo_id, name)` UNIQUE-constraint
    /// violation, caught at the sqlite layer (`name_conflict_or`) rather
    /// than surfacing as an opaque [`StoreError::Sqlite`]; `routes.rs`'s
    /// `impl From<StoreError> for ApiError` maps this one variant to `409`,
    /// everything else to `500`.
    #[error("a reading set named {0:?} already exists in this repo")]
    NameConflict(String),
    /// V4.C2 — an `apply_annotation_ops` target vanished between
    /// validation and the write tx (or a caller skipped validation).
    /// Mapped to `400` so a batch never 500s on an unknown id.
    #[error("{0} not found")]
    NotFound(String),
    /// V74-L3b — `canvas_boards`' `UNIQUE (repo_id, slug)` spans BOTH
    /// kinds (a tour is a board row, migration V0039), so an apply can
    /// collide with a slug the caller cannot see from its own family's
    /// list. Mapped to `409` beside [`StoreError::NameConflict`] rather
    /// than surfacing as an opaque sqlite constraint error.
    #[error("the slug {slug:?} is already taken in this repo by a {kind}")]
    SlugTakenByOtherKind { slug: String, kind: String },
    /// V80-M5 — a finding-ADOPTION insert (`Store::insert_review_finding_
    /// adopting`) lost a race against another finding already claiming the
    /// SAME `annotation_id` (`review_findings.annotation_id`'s own UNIQUE
    /// index, V0024: one annotation backs at most one finding). Caught at
    /// the sqlite layer (`annotation_finding_conflict_or`) rather than
    /// surfacing as an opaque [`StoreError::Sqlite`] 500 — see that
    /// function's own doc for why a constraint hit here is a client error
    /// (409), never a panic.
    #[error("annotation {0:?} is already linked to a finding — a thread backs at most one")]
    AnnotationAlreadyFinding(String),
    /// A7-2 — a bind/rebind/unbind targeted an annotation that IS a
    /// finding's thread (`review_findings.annotation_id` points at it).
    /// The finding's review and patchset live on that row, so moving it
    /// would detach the thread from the finding. Mapped to `409`.
    #[error("annotation {0:?} backs a finding — its review scope is the finding's; resolve or delete the finding instead")]
    AnnotationIsFinding(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// One row of the `files` table — the current working-tree state mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    pub path: String,
    pub blob_hash: String,
    pub lang: String,
    pub size: u64,
    /// V77-P1 — filesystem mtime (unix seconds) as last observed by one of
    /// the `std::fs`-reading sink paths (`sink::handle_upsert`/
    /// `handle_full_reconcile`), via `Store::upsert_file_with_mtime`. `0`
    /// means "unknown" (every ODB tree-walk write goes through the plain
    /// `Store::upsert_file`, which never sets this) and must never be
    /// treated as a match by a fingerprint comparison — see that fn's doc.
    pub mtime: u64,
}

pub struct Store {
    conn: Mutex<Connection>,
    /// W2.1 — bumped on every mutating call (`upsert_file`/`delete_file`/
    /// `replace_symbols`/`bump_file_open`). The search lanes' in-memory
    /// per-repo caches (`search::files::FileIndex`, `search::symbols::
    /// SymbolIndex`) compare their cached snapshot's generation against
    /// `self.generation()` on every call and rebuild only when it has
    /// moved — a lazy, pull-based invalidation rather than a subscribed
    /// bus listener: every search-lane call already holds a `&Store`
    /// (from the request handler), so checking one atomic counter per call
    /// is simpler than wiring a background `EventBus` subscriber into
    /// `bind_and_spawn`, with no task lifetime to manage. `Relaxed` is
    /// deliberate, not an oversight: every sqlite read/write already goes
    /// through the SAME single `Mutex<Connection>` (never a per-reader
    /// connection), so a query always sees whatever the connection's own
    /// state is regardless of this counter's memory ordering — the counter
    /// only decides whether an in-memory CACHE gets rebuilt, and the worst
    /// case of a relaxed race is one extra query cycle served from a
    /// microseconds-stale cache, not a wrong answer.
    generation: AtomicU64,
    /// V70-A3X — a SEPARATE monotonic counter for `bump_file_open` ONLY.
    /// Before this split, every `GET /api/file` bumped the SAME
    /// `generation` a real content mutation does, which meant every file
    /// open invalidated the files/symbols search lanes' ENTIRE in-memory
    /// path/symbol snapshot caches (`search::files::FileIndex`, `search::
    /// symbols::SymbolIndex`) — a read-only action paying a write's cache
    /// cost. Only `search::files::FileIndex`'s own recency snapshot (the
    /// per-repo `last_opened_map` used by its frecency blend) keys on this
    /// counter; `generation` keeps its original meaning ("the files/symbols
    /// candidate SET changed") untouched.
    opens_generation: AtomicU64,
    /// RS-U4 — reads a `GitCtx` served from a user repo instead of a ready
    /// review store (`crate::git::roots`). Shared (`Arc`) because every
    /// `GitCtx` built against this store carries a handle to bump it.
    git_fallbacks: std::sync::Arc<crate::git::roots::GitFallbackStats>,
    /// RS — boot-published "may a READ resolve a store root for this
    /// boot?". `git::roots::resolve_ready_store` sees only `&Store`, so
    /// the `review_store::StoreSettings::disabled` verdict (and the
    /// store git spawner being unbuildable) is pushed here once, by
    /// `bind_and_spawn`, instead of pulled. The `true` default is
    /// deliberate: a `Store` opened with no `ReviewStores` at all — the
    /// CLI, benches, fixtures — keeps the pre-existing read behaviour
    /// exactly, and a disabled boot is a *configured* refusal, not a
    /// default to guess.
    ///
    /// `Release`/`Acquire`, NOT the `Relaxed` used by the counters
    /// above: there a stale read costs one cache rebuild, but here it
    /// is a GATE — a stale `true` serves a read from a store root the
    /// operator refused for this boot, which is the whole defect this
    /// flag exists to close.
    ///
    /// Defence in depth, NOT the mechanism. The flag is a lone
    /// `AtomicBool` that publishes no other memory, so the Acquire load
    /// synchronises with nothing an observer could act on. The edge
    /// that actually holds is SPAWN ORDERING: the release store in
    /// `bind_and_spawn` runs before any task that can construct a
    /// `GitCtx` exists — the `[backfill] on_boot` spawn, the serve
    /// spawn, the RS boot job, the maintenance worker, and
    /// `run_blocking`'s dispatch of a route handler onto the blocking
    /// pool. The load-bearing change was RELOCATING that backfill
    /// block to after the publish (it used to sit up with the other
    /// boot-time spawns, ahead of it). The stronger ordering is kept
    /// because it costs nothing — one fence per boot, one acquire per
    /// `GitCtx` construction, once per route entry and not once per git
    /// subprocess — and because it stops the guarantee from depending
    /// on that spawn order surviving the next edit.
    review_store_readable: AtomicBool,
}

/// The ONE sanctioned way to touch the store from async context — see the
/// module doc's 2026-08-31 incident note. An extension trait on
/// `Arc<Store>` (rather than an inherent `self: Arc<Self>` method) so call
/// sites read `state.store.run_blocking(move |s| …).await` without a clone:
/// the closure runs on tokio's blocking pool, so the mutex wait can never
/// park an async worker thread.
pub trait StoreBlocking {
    /// Run `f` against the store on the blocking pool and await its result.
    /// The closure must be `'static` (it outlives the calling stack frame) —
    /// move owned copies of whatever it needs in.
    fn run_blocking<T, F>(&self, f: F) -> impl std::future::Future<Output = T> + Send
    where
        F: FnOnce(&Store) -> T + Send + 'static,
        T: Send + 'static;
}

impl StoreBlocking for std::sync::Arc<Store> {
    async fn run_blocking<T, F>(&self, f: F) -> T
    where
        F: FnOnce(&Store) -> T + Send + 'static,
        T: Send + 'static,
    {
        let store = std::sync::Arc::clone(self);
        tokio::task::spawn_blocking(move || f(&store))
            .await
            // A JoinError here is a panic inside the closure (store code
            // panicked) or runtime shutdown — same abort semantics the old
            // inline call had, just surfaced through the join.
            .expect("kb-code store blocking task panicked")
    }
}

impl Store {
    /// Open (or create) the database at `path` and run all pending
    /// migrations. Idempotent across processes via refinery's own
    /// `refinery_schema_history` bookkeeping table — same convention as
    /// `kb_core::storage::sqlite::Db::open`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)?;

        conn.pragma_update(None, "journal_mode", "WAL")?;
        // synchronous=NORMAL is the standard durable-enough pairing with
        // WAL — see kb-core's `Db::open` doc comment for the rationale;
        // same tradeoff applies here (derived data is cheaply
        // re-computable from the blob anyway, so this side is even less
        // durability-sensitive than kb-core's).
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        // PF-K1 — rusqlite's default prepared-statement cache capacity (16)
        // is far smaller than this store's distinct hot-path statement
        // texts; mirrors kb-core's own `Db::open` (`crates/kb-core/src/
        // storage/sqlite.rs`), same rationale: a cache miss is only ever a
        // re-prepare (correctness-identical), so this is sizing, not
        // semantics.
        conn.set_prepared_statement_cache_capacity(128);

        // RS-U9 — the review-store restore guard's automatic detector reads
        // the volume's epoch BEFORE anything below touches it (the same
        // point `refuse_if_volume_ahead`/`ensure_for_epoch_crossing` read
        // it from, for the same reason: after the migration runner below,
        // every volume reads back at `schema_epoch()` regardless of
        // whether THIS boot's starting point was a restored older
        // snapshot — see `review_store::maint::restore_guard`'s module
        // doc). A read-only probe; never itself an error.
        let volume_epoch_at_boot = kb_core::sibling::volume_epoch(&conn).ok().flatten();

        // kb-sibling/1 — HARD schema-epoch guard, BEFORE the migration run
        // (same posture, same helper, as `kb_core::storage::sqlite::Db::
        // open`): refinery only ever migrates FORWARD, so an older binary
        // pointed at a volume a newer one already migrated boots green and
        // then fails at request time on columns it doesn't know about — the
        // 13.5 h kbc outage. This daemon has ONE store, so this fires once,
        // and the error propagates out of `bind_and_spawn`, refusing boot.
        kb_core::sibling::refuse_if_volume_ahead(&conn, path, schema_epoch())
            .map_err(|e| StoreError::SchemaEpoch(e.to_string()))?;

        // V75-M1 — the pre-migration backup gate. Ordered deliberately:
        // AFTER the epoch guard (a volume this binary must refuse is never
        // snapshotted) and BEFORE the checksum repair below, which is
        // itself a WRITE — a snapshot taken after it would not be the
        // pre-migration state an operator would roll back to. Returns
        // `None` (and touches nothing) on every boot that is not a
        // crossing, which is all of them once a volume is past
        // `backup::REKEY_EPOCH`.
        // The receipt is held so the prune below can spare the snapshot
        // THIS boot just took. Retention itself runs only after the runner
        // succeeds — a failed migration must still have its rollback target.
        let pre_migration = crate::backup::ensure_for_epoch_crossing(&conn, path, schema_epoch())
            .map_err(|e| StoreError::BackupRequired(e.to_string()))?;

        // RS-U9 — the restore guard's automatic detector: filesystem only
        // (a sentinel read/write), no git, no network. Should-fix review
        // finding — "NO git I/O on the boot path": the actual bundle
        // backup this MAY warrant (a gated-epoch snapshot or a freshly
        // detected restore, design-internal-store.md §8 / README §5.4's
        // "on a gated-epoch snapshot … or a restore") is deferred to
        // `spawn_boot_bundle_backup` (`lib.rs`, spawned AFTER the daemon
        // binds) via a marker file — this function only ever WRITES that
        // marker, never runs git itself.
        {
            let state_dir = path.parent().unwrap_or_else(|| Path::new("."));
            let guard_path = crate::review_store::maint::restore_guard::path_for(state_dir);
            let guard = crate::review_store::maint::restore_guard::observe_boot_epoch(
                &guard_path,
                volume_epoch_at_boot,
                chrono::Utc::now().timestamp(),
            );
            if guard.just_flagged {
                tracing::warn!(
                    reason = ?guard.flagged_reason,
                    "kb-code: review-store restore guard flagged — a real GC apply stays \
                     refused per-store until an operator runs `kb-code store gc --repo R --yes`"
                );
            }
            if pre_migration.is_some() || guard.just_flagged {
                let reason = if pre_migration.is_some() {
                    "gated-epoch-snapshot"
                } else {
                    "restore-detected"
                };
                if let Err(e) =
                    crate::review_store::maint::mark_boot_backup_pending(state_dir, reason)
                {
                    tracing::warn!(
                        error = %e,
                        "kb-code: could not mark a boot-time review-store bundle backup pending"
                    );
                }
            }
        }

        // V72-B1 — one-time, narrowly-targeted repair for the ONE migration
        // checksum a 2026-09 public-repo scrub diverged. MUST run after the
        // schema-epoch guard above (a stale checksum is never a
        // forward-migrated volume) and before the runner below (which would
        // otherwise abort on it first).
        repair_v3_transcripts_checksum(&mut conn)?;

        embedded::migrations::runner()
            .run(&mut conn)
            .map_err(|e| StoreError::Migration(e.to_string()))?;
        // ch-10 — successful boot at epoch N reaps `*.pre-V<e>.bak` for
        // e < N-1. The snapshot taken above (if this boot was a crossing)
        // is spared; the next boot reaps it if it is older than N-1. A
        // stuck file is a warning, not a boot refusal.
        if let Err(e) = crate::backup::prune_snapshots(
            path,
            schema_epoch(),
            pre_migration
                .as_ref()
                .map(|r| std::path::Path::new(&r.backup_path)),
        ) {
            tracing::warn!(
                error = %e,
                "kb-code: could not reap pre-migration snapshots older than the previous epoch"
            );
        }

        Ok(Self {
            conn: Mutex::new(conn),
            generation: AtomicU64::new(0),
            opens_generation: AtomicU64::new(0),
            git_fallbacks: Default::default(),
            review_store_readable: AtomicBool::new(true),
        })
    }

    /// RS-U4 — read-only view of the review-store fallback counters (the
    /// Phase-1 gate "0 fallback hits after ready" reads `odb_miss`).
    pub fn git_fallback_stats(&self) -> crate::git::roots::GitFallbackSnapshot {
        self.git_fallbacks.snapshot()
    }

    pub(crate) fn git_fallbacks_handle(
        &self,
    ) -> std::sync::Arc<crate::git::roots::GitFallbackStats> {
        std::sync::Arc::clone(&self.git_fallbacks)
    }

    /// Publish the boot's read verdict. The `Release` store pairs with
    /// the `Acquire` load in [`Self::review_store_readable`]; see the
    /// field doc for why a gate cannot be `Relaxed`.
    pub fn set_review_store_readable(&self, readable: bool) {
        self.review_store_readable
            .store(readable, Ordering::Release);
    }

    /// `pub(crate)`: every read-side consumer is in-crate
    /// (`git::roots::resolve_ready_store`). The setter above is `pub`
    /// only because out-of-crate `AppState` builders — integration
    /// tests, embedding crates — construct their own `ReviewStores` and
    /// would otherwise silently keep the permissive `true` default while
    /// their own `AppState` refuses the store.
    pub(crate) fn review_store_readable(&self) -> bool {
        self.review_store_readable.load(Ordering::Acquire)
    }

    fn lock(&self) -> parking_lot::MutexGuard<'_, Connection> {
        // parking_lot, not std — eventual fairness is load-bearing here
        // (see the Cargo.toml dep comment + the module doc's incident
        // note): under a sink reconcile burst the writer re-locks in a
        // tight per-file loop, and std's unfair handoff let a waiting
        // reader starve for the whole burst. No poisoning in parking_lot,
        // so the old `PoisonError::into_inner` recovery is moot.
        self.conn.lock()
    }

    /// TEST-ONLY (compiled unconditionally so INTEGRATION tests — a separate
    /// crate, where `#[cfg(test)]` items are invisible — can reach it): hold
    /// the store's one connection mutex for `dur`, simulating a slow sink
    /// write. Exists solely for the async-worker-starvation regression test
    /// (see the module doc's 2026-08-31 incident note); never call it from
    /// non-test code — it does exactly what the incident did on purpose.
    #[doc(hidden)]
    pub fn hold_lock_for_test(&self, dur: std::time::Duration) {
        let _guard = self.lock();
        std::thread::sleep(dur);
    }

    /// TEST-ONLY (compiled unconditionally so INTEGRATION tests can reach
    /// it, like [`Store::hold_lock_for_test`]): migrate the volume at
    /// `path` only as far as `version`, leaving it deliberately BEHIND
    /// this binary's own epoch.
    ///
    /// The only way to build a volume that predates a migration this
    /// binary embeds, which is what the V75-M1 backup gate and the
    /// rehearsal verb both need a fixture for. Uses refinery's own
    /// `Target::Version`, so the result is a genuinely migrated volume
    /// with a genuine history — never a fabricated `refinery_schema_history`
    /// row.
    #[doc(hidden)]
    pub fn migrate_to_for_test(path: &Path, version: u32) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // refinery 0.9 widened Target::Version to i32 (int8-versions prep);
        // kb's epochs are small positives, so the conversion cannot fail.
        let version = i32::try_from(version)
            .map_err(|e| StoreError::Migration(format!("epoch {version} out of range: {e}")))?;
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(version))
            .run(&mut conn)
            .map_err(|e| StoreError::Migration(e.to_string()))?;
        Ok(())
    }

    /// TEST-ONLY: drops the `symbols` table out from under this `Store`, so
    /// a caller elsewhere in the crate can exercise "the store failed" for a
    /// consumer that has no other way to force a real `StoreError` (e.g.
    /// doc-lens's C2 fix — a poisoned store must surface as an explicit
    /// degrade, not silently read back as "no symbols found"). Every
    /// subsequent query against `symbols` errors with "no such table";
    /// `files`-only reads (`list_files`, etc.) are unaffected.
    #[cfg(test)]
    pub(crate) fn drop_symbols_table_for_test(&self) {
        self.lock()
            .execute_batch("DROP TABLE symbols")
            .expect("drop symbols table");
    }

    /// Current generation — see the field doc. Monotonically increasing for
    /// the lifetime of this `Store`; never resets.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    fn bump_generation(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Current opens-generation — see the field doc (V70-A3X). Monotonically
    /// increasing for the lifetime of this `Store`, independent of
    /// [`Self::generation`]; never resets.
    pub fn opens_generation(&self) -> u64 {
        self.opens_generation.load(Ordering::Relaxed)
    }

    fn bump_opens_generation(&self) {
        self.opens_generation.fetch_add(1, Ordering::Relaxed);
    }

    fn comment_group_counts(
        &self,
        repo_id: i64,
        column: &str,
        skip_null: bool,
    ) -> Result<Vec<(String, i64)>> {
        // `column` is one of two crate-internal literals, never caller
        // input — the same posture `docs_query` takes for its own ORDER BY.
        let sql = format!(
            "SELECT {column}, COUNT(*) FROM comments
             WHERE repo_id = ?1 {}
             GROUP BY {column} ORDER BY {column} ASC",
            if skip_null {
                "AND keyword IS NOT NULL"
            } else {
                ""
            }
        );
        let conn = self.lock();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn upsert_author_stat(
        tx: &rusqlite::Transaction<'_>,
        repo_id: i64,
        path: &str,
        author: &str,
        commit_unix: i64,
    ) -> Result<()> {
        tx.execute(
            "INSERT INTO author_stats (repo_id, path, author, commits, first_seen_unix)
             VALUES (?1, ?2, ?3, 1, ?4)
             ON CONFLICT(repo_id, path, author) DO UPDATE SET
               commits = commits + 1,
               first_seen_unix = MIN(
                   COALESCE(first_seen_unix, excluded.first_seen_unix),
                   excluded.first_seen_unix)",
            params![repo_id, path, author, commit_unix],
        )?;
        Ok(())
    }

    fn entity_defs_query(
        &self,
        repo_id: i64,
        worktree: Option<&str>,
        name: &str,
        suffix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EntityDefRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT d.worktree, d.path, d.fqn, d.kind, d.nesting, d.line_start,
                    d.line_end, d.zeitwerk_fqn, d.zeitwerk_state, d.blob_hash, f.blob_hash
             FROM entity_defs d
             LEFT JOIN files f ON f.repo_id = d.repo_id AND f.path = d.path
             WHERE d.repo_id = ?1
               AND (?2 IS NULL OR d.worktree = ?2)
               AND (
                     (?4 IS NULL AND (d.fqn = ?3 OR d.zeitwerk_fqn = ?3))
                  OR (?4 IS NOT NULL AND (d.fqn LIKE ?4 ESCAPE '\\'
                                          OR d.zeitwerk_fqn LIKE ?4 ESCAPE '\\'))
                   )
             ORDER BY d.fqn ASC, d.path ASC, d.ordinal ASC
             LIMIT ?5",
        )?;
        let rows = stmt
            .query_map(
                params![repo_id, worktree, name, suffix, limit as i64],
                |r| {
                    Ok(EntityDefRow {
                        worktree: r.get(0)?,
                        path: r.get(1)?,
                        fqn: r.get(2)?,
                        kind: r.get(3)?,
                        nesting: r.get(4)?,
                        line_start: r.get(5)?,
                        line_end: r.get(6)?,
                        zeitwerk_fqn: r.get(7)?,
                        zeitwerk_state: r.get(8)?,
                        blob_hash: r.get(9)?,
                        live_blob_hash: r.get(10)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// Escape a value for use inside a `LIKE` pattern with `ESCAPE '\\'`. Both
/// SQL wildcards (`%`, `_`) and the escape character itself. `_` is the
/// one that actually bites here: a Ruby constant may contain one
/// (`Order_v2`), and unescaped it would match any character.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '%' || c == '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Phase E3 — maps a `reading_sets(repo_id, name)` UNIQUE-constraint
/// violation to [`StoreError::NameConflict`]; any OTHER sqlite error passes
/// through as [`StoreError::Sqlite`] unchanged. Shared by
/// `create_reading_set` and `update_reading_set_meta` — both catch the
/// specific constraint at the sqlite layer rather than a separate
/// check-then-write round trip (see `create_reading_set`'s own doc for why
/// that's not a TOCTOU fix, just fewer statements per call).
fn name_conflict_or(e: rusqlite::Error, name: &str) -> StoreError {
    if e.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) {
        StoreError::NameConflict(name.to_string())
    } else {
        StoreError::Sqlite(e)
    }
}
