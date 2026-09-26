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

// Method groups live in child modules so this file stays the type
// and connection surface. `crate::store::*` paths are unchanged.
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
    /// default to guess. `Relaxed` is right because the write lands
    /// during boot, before any request-serving task can reach
    /// `GitCtx::for_repo`.
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

    pub(crate) fn set_review_store_readable(&self, readable: bool) {
        self.review_store_readable.store(readable, Ordering::Relaxed);
    }

    pub(crate) fn review_store_readable(&self) -> bool {
        self.review_store_readable.load(Ordering::Relaxed)
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

/// V70-A2 — one row to append to the `mutations` ledger. A distinct input
/// type from [`MutationRow`] because `id` is the store's own concern (same
/// convention as `ScipOccurrenceIn` vs the row it becomes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationIn {
    pub ts_unix: i64,
    pub route: String,
    pub method: String,
    /// `"loopback"` | `"bearer"` | `"review_gate"` — the CHECK-constrained
    /// vocabulary of `V0027__mutations_audit.sql`.
    pub admission: String,
    pub repo: Option<String>,
    pub target: Option<String>,
    pub blob_before: Option<String>,
    pub blob_after: Option<String>,
    pub request_id: String,
    pub outcome: String,
}

/// V70-A2 — one `mutations` row as read back by `GET /api/audit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationRow {
    pub id: i64,
    pub ts_unix: i64,
    pub route: String,
    pub method: String,
    pub admission: String,
    pub repo: Option<String>,
    pub target: Option<String>,
    pub blob_before: Option<String>,
    pub blob_after: Option<String>,
    pub request_id: String,
    pub outcome: String,
}

/// V71-G0 — one `entity_defs` row as READ, joined against the live
/// `files` row for the same path so the caller can tell a fresh claim from
/// a stale one. Carries no trust class: that is computed per request by
/// `crate::entities::class_for` and is never stored (root invariant #2's
/// posture, and design §P8's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityDefRow {
    pub worktree: String,
    pub path: String,
    /// The FQN the tree proved (literal `class`/`module` nesting, plus any
    /// compact scope recovered from the source line).
    pub fqn: String,
    pub kind: String,
    /// `lexical` | `ambiguous` — how completely the tree determines `fqn`
    /// (`crate::entities::NESTING_*`). An input to `class_for`, not a
    /// class.
    pub nesting: String,
    pub line_start: i64,
    pub line_end: i64,
    /// The constant the path convention derives, when this row carries one.
    pub zeitwerk_fqn: Option<String>,
    /// What the Zeitwerk read was worth when the claim was made.
    pub zeitwerk_state: String,
    /// The blob the claim was derived from.
    pub blob_hash: String,
    /// The blob currently at this path, or `None` when no `files` row
    /// exists any more. `!= blob_hash` ⇒ the claim is stale.
    pub live_blob_hash: Option<String>,
}

/// V71-G0 — one kbc-seq/1 projection row, resolved out of whichever table
/// still owns it (`crate::seq`). `source` names that table: the layer is
/// honest about being a layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeqProjectionRow {
    pub projection: String,
    pub id: String,
    pub name: String,
    /// `None` when the projection's element count is genuinely unknown to
    /// this daemon (a board's opaque payload) — never a placeholder 0.
    pub size: Option<i64>,
    pub ref_label: Option<String>,
    pub workspace_id: Option<String>,
    pub source: &'static str,
    pub updated_at: i64,
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

/// One `doc_lens_pins` row (DCB W1.C) — see migration V0020's doc for why
/// `repo` is a NAME and `repo_root` is recorded beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocLensPin {
    pub kb: String,
    pub doc_id: String,
    pub repo: String,
    pub repo_root: String,
    pub doc_hash: Option<String>,
    pub pinned_at: i64,
}

/// Reads the 6-column order every `doc_lens_pins` SELECT above uses.
fn doc_lens_pin_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<DocLensPin> {
    Ok(DocLensPin {
        kb: r.get(0)?,
        doc_id: r.get(1)?,
        repo: r.get(2)?,
        repo_root: r.get(3)?,
        doc_hash: r.get(4)?,
        pinned_at: r.get(5)?,
    })
}

/// One `doc_refs` row as READ BACK (`doc_refs_for_path`). DCB W3.A — a
/// CLAIM, never a cached verdict: `resolved_path` is what the resolution
/// found AT SYNC TIME and is re-validated against the live `files` table on
/// every read (`doclens::sync::doc_refs_for`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocRefRow {
    pub kb: String,
    pub doc_id: String,
    pub ordinal: i64,
    pub kind: String,
    pub raw_hint: String,
    pub resolved_path: String,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
    pub line_state: Option<String>,
    pub group_key: Option<String>,
    pub group_label: Option<String>,
    pub doc_title: String,
    pub doc_path: String,
    pub doc_hash: Option<String>,
    pub head_sha: Option<String>,
    pub dirty: bool,
    pub seen_at: i64,
}

/// The per-DOCUMENT half of one [`Store::replace_doc_refs`] write — split
/// from [`NewDocRef`] because these columns are denormalized onto every row
/// of the same doc, so passing them per-ref would invite them to disagree.
#[derive(Debug, Clone, Copy)]
pub struct DocRefWrite<'a> {
    pub kb: &'a str,
    pub doc_id: &'a str,
    pub repo_id: i64,
    pub doc_title: &'a str,
    pub doc_path: &'a str,
    pub doc_hash: Option<&'a str>,
    /// The resolving repo's head_sha + dirty flag at sync time (amendment
    /// 11's "resolving head_sha / worktree label").
    pub head_sha: Option<&'a str>,
    pub dirty: bool,
    pub seen_at: i64,
}

/// The per-REFERENCE half of one [`Store::replace_doc_refs`] write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDocRef {
    /// The ref's own `coderef/1` ordinal — never reassigned.
    pub ordinal: i64,
    pub kind: String,
    pub raw_hint: String,
    pub resolved_path: String,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
    pub line_state: Option<String>,
    pub group_key: Option<String>,
    pub group_label: Option<String>,
}

/// One `doclens_sync_cursors` row (DCB W3.A) — per-kb feed progress.
/// `cursor` is OPAQUE here: the store persists and returns it verbatim and
/// never parses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoclensSyncCursor {
    pub kb: String,
    pub cursor: Option<String>,
    pub last_run_at: i64,
    pub last_error: Option<String>,
}

/// V70-A3X — a `WITH cur(salt) AS (VALUES (?),(?),...)` CTE fragment
/// listing every entry of [`crate::lang::ALL_LANGS`] (the CURRENT salt for
/// every registered language), for a repo-wide derived-table read
/// (`symbols_for_repo`, the occurrence `*_in_repo` fns) to restrict itself
/// to current-salt rows. Returns `(cte_sql, salts)` — bind `salts` FIRST
/// (the `VALUES` placeholders come before the rest of the query's own
/// params in source order).
///
/// The restriction has a deliberate FALLBACK: a caller applies it as
/// `s.salt IN (SELECT salt FROM cur) OR NOT EXISTS (SELECT 1 FROM <table>
/// t2 WHERE t2.blob_hash = <table>.blob_hash AND t2.salt IN (SELECT salt
/// FROM cur))` — i.e. "prefer the current-salt row, but if NO row for this
/// blob_hash has a current salt at all, show everything for it." A strict
/// `s.salt IN (SELECT salt FROM cur)`-only filter would silently empty out
/// every fixture across this crate's test suite that seeds symbols/
/// occurrences under a short ad hoc salt like `"rust@1"` (a deliberate,
/// widespread test convention, decoupled from `lang.rs`'s exact pinned
/// version strings) — this fallback keeps every one of those
/// byte-identical while still closing the real production bug: once a
/// GENUINE salt bump leaves an old-salt derivation coexisting with a new
/// one for the SAME blob, a current-salt sibling now exists, so the
/// fallback doesn't fire and the stale generation is correctly hidden.
///
/// V72-H2b: the set is per-FAMILY. `symbols`/`occurrences` are read
/// against the SYMBOL salts, `highlights` against the HIGHLIGHT ones —
/// passing the wrong family declares every current row of the other
/// family stale, which for the sweep would mean deleting it.
fn current_salt_cte(family: crate::lang::SaltFamily) -> (String, Vec<&'static str>) {
    let salts = crate::lang::current_salts(family);
    let values = salts.iter().map(|_| "(?)").collect::<Vec<_>>().join(",");
    (format!("WITH cur(salt) AS (VALUES {values})"), salts)
}

/// Per-table row counts pruned by [`Store::sweep_stale_salt_derived`]
/// (V70-A3X).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StaleSaltSweepCounts {
    pub symbols: u64,
    pub highlights: u64,
    pub occurrences: u64,
    /// V72-H2b — the per-family derivation markers, swept against their
    /// OWN family's salt set (see [`SWEEP_TABLES`]).
    pub derived_status: u64,
}

impl StaleSaltSweepCounts {
    /// `true` if every table's count is zero — the common case, so the
    /// caller can skip logging a no-op sweep (mirrors `prune_stale_pins`'s
    /// `Ok(0) => {}` boot-log convention).
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub fn total(&self) -> u64 {
        self.symbols + self.highlights + self.occurrences + self.derived_status
    }
}

/// How many distinct `files.blob_hash` values one
/// [`Store::sweep_stale_salt_page`] covers (V72-B0). Small on purpose: this
/// is the unit of write-mutex hold time, and on a cold production store
/// every blob costs a handful of random index seeks (~50/s on spinning
/// disks), so a page is seconds, not hours.
pub const STALE_SALT_SWEEP_PAGE: usize = 128;

/// One table's worth of ONE [`Store::sweep_stale_salt_page`] — see that
/// fn's doc for the exact delete condition and why paging the driver cannot
/// change the result set. Free fn (not a `Store` method): takes an open
/// `Transaction` so all three tables' deletes share one tx.
///
/// `blobs` is this page's driver set, bound as parameters — the V72-B0 fix
/// for the un-paged `blob_hash IN (SELECT blob_hash FROM files)`, whose
/// per-statement cost was O(every live blob) regardless of how few rows
/// actually needed deleting.
fn sweep_stale_salt_table(
    tx: &Transaction<'_>,
    cte: &str,
    salts: &[&'static str],
    blobs: &[String],
    table: &str,
    family_value: Option<&str>,
) -> Result<u64> {
    let blob_slots = blobs.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    // V72-H2b — `derived_status` holds BOTH families in one table, so its
    // two passes each add `family = ?`. The predicate rides the
    // correlated EXISTS too: a blob's SYMBOL marker must never count as
    // the current-salt sibling that authorises deleting its HIGHLIGHT one.
    let (family_pred, sibling_pred) = match family_value {
        Some(_) => (
            format!("AND {table}.family = ?"),
            format!("AND t2.family = {table}.family"),
        ),
        None => (String::new(), String::new()),
    };
    let sql = format!(
        "{cte}
         DELETE FROM {table}
         WHERE blob_hash IN ({blob_slots})
           {family_pred}
           AND salt NOT IN (SELECT salt FROM cur)
           AND EXISTS (
                 SELECT 1 FROM {table} t2
                 WHERE t2.blob_hash = {table}.blob_hash AND t2.salt IN (SELECT salt FROM cur)
                 {sibling_pred}
               )"
    );
    let mut stmt = tx.prepare(&sql)?;
    // Bind order matches the SQL: the `cur` CTE's salts are written first
    // (`{cte}` opens the statement), then this page's blob hashes, then
    // the optional family.
    let mut bind: Vec<Box<dyn rusqlite::ToSql>> = salts.iter().map(|s| Box::new(*s) as _).collect();
    for b in blobs {
        bind.push(Box::new(b.clone()));
    }
    if let Some(f) = family_value {
        bind.push(Box::new(f.to_string()));
    }
    let n = stmt.execute(rusqlite::params_from_iter(bind.iter()))?;
    Ok(n as u64)
}

/// Every table [`Store::sweep_stale_salt_page`] sweeps, with the salt
/// FAMILY that keys it and (for the one table holding both families) the
/// `family` value to restrict to. V72-H2b: the family column is what makes
/// "a family is stale only when ITS salt moved" a property of the SQL
/// rather than of the caller's memory. Adding a derived table means adding
/// a row here — an omission is a table that accumulates stale rows
/// forever, which is the defect V70-A3X shipped this sweep for.
/// Every blob-keyed derived table the re-extract bill counts (V72-H2b).
/// Deliberately WIDER than [`SWEEP_TABLES`]: the bill prices what a salt
/// bump re-derives, and `import_specs`/`call_sites`/`type_relations` ride
/// the SYMBOL salt even though the V70-A3X sweep never learned to prune
/// them (a known, named gap — see `crates/kb-code-server/CLAUDE.md`
/// invariant 11).
/// One page of [`Store::derived_row_census_page`] — a named struct rather
/// than a three-tuple so the bill's loop reads as what it is.
#[derive(Debug, Clone)]
pub struct CensusPage {
    /// Per-table row counts for THIS page's blobs, in [`BILL_TABLES`] order.
    pub counts: Vec<(&'static str, u64)>,
    /// Distinct blob hashes this page actually covered.
    pub blobs: usize,
    /// Cursor to resume from; `None` once the last page has been counted.
    pub next: Option<String>,
}

pub(crate) const BILL_TABLES: &[&str] = &[
    "symbols",
    "highlights",
    "occurrences",
    "import_specs",
    "call_sites",
    "type_relations",
    "derived_status",
];

const SWEEP_TABLES: &[(&str, crate::lang::SaltFamily, Option<&str>)] = &[
    ("symbols", crate::lang::SaltFamily::Symbol, None),
    ("occurrences", crate::lang::SaltFamily::Symbol, None),
    ("highlights", crate::lang::SaltFamily::Highlight, None),
    (
        "derived_status",
        crate::lang::SaltFamily::Symbol,
        Some("symbols"),
    ),
    (
        "derived_status",
        crate::lang::SaltFamily::Highlight,
        Some("highlights"),
    ),
];

/// V70-A3X — a `LIKE` pattern matching every salt of `salt`'s OWN language
/// (e.g. `salt = "rust@0.24.2+q3"` → `"rust@%"`), used to purge a blob's
/// STALE-salt `symbols`/`highlights`/`occurrences` rows before writing a
/// fresh derivation under a NEW salt (a grammar/query version bump — see
/// `lang.rs`'s module doc). Deliberately narrower than a bare `blob_hash`
/// match: a degenerate/empty file can share one `blob_hash` across
/// DIFFERENT languages (different extensions detecting to different
/// `LangInfo`s over identical bytes), and this must never purge a sibling
/// language's CURRENT rows for that same blob. Salt ids are a closed,
/// alphanumeric set (`lang::ALL_LANGS`), so no `LIKE`-wildcard-escaping
/// concern for the prefix itself.
fn lang_prefix_pattern(salt: &str) -> String {
    format!("{}@%", salt.split('@').next().unwrap_or(salt))
}

/// Write the `(blob_hash, family, salt)` derivation marker inside an
/// already-open transaction, purging every OTHER salt of this blob's
/// language FOR THIS FAMILY first (invariant 11's purge-on-write rule,
/// applied to the marker table). Scoped by `family` as well as by language
/// prefix: a symbol-salt bump must not erase the highlight marker, which is
/// the entire point of V72-H2b's split.
fn mark_derived_in(
    tx: &Transaction<'_>,
    blob_hash: &str,
    family: crate::lang::SaltFamily,
    salt: &str,
    rows: usize,
) -> Result<()> {
    tx.execute(
        "DELETE FROM derived_status \
         WHERE blob_hash = ?1 AND family = ?2 AND salt LIKE ?3 AND salt != ?4",
        params![blob_hash, family.as_str(), lang_prefix_pattern(salt), salt],
    )?;
    tx.execute(
        "INSERT INTO derived_status (blob_hash, family, salt, rows) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(blob_hash, family, salt) DO UPDATE SET rows = excluded.rows",
        params![blob_hash, family.as_str(), salt, rows as i64],
    )?;
    Ok(())
}

/// Read a `Symbol` starting at column `offset` (ordinal … param_max).
fn symbol_from_row(r: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Symbol> {
    let param_min: Option<i64> = r.get(offset + 10)?;
    let param_max: Option<i64> = r.get(offset + 11)?;
    Ok(Symbol {
        ordinal: r.get(offset)?,
        name: r.get(offset + 1)?,
        kind: r.get(offset + 2)?,
        line_start: r.get(offset + 3)?,
        line_end: r.get(offset + 4)?,
        col_start: r.get(offset + 5)?,
        col_end: r.get(offset + 6)?,
        container: r.get(offset + 7)?,
        signature: r.get(offset + 8)?,
        doc: r.get(offset + 9)?,
        param_min: param_min.map(|n| n as u32),
        param_max: param_max.map(|n| n as u32),
    })
}

fn occurrence_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<crate::occurrences::Occurrence> {
    occurrence_row_from_offset(r, 0)
}

fn occurrence_row_from_offset(
    r: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<crate::occurrences::Occurrence> {
    let local_def_ordinal: Option<i64> = r.get(offset + 7)?;
    Ok(crate::occurrences::Occurrence {
        ordinal: r.get(offset)?,
        name: r.get(offset + 1)?,
        role: r.get(offset + 2)?,
        line: r.get(offset + 3)?,
        col_start: r.get(offset + 4)?,
        col_end: r.get(offset + 5)?,
        source: r.get(offset + 6)?,
        local_def_ordinal: local_def_ordinal.map(|n| n as u32),
    })
}

/// One SCIP-derived occurrence row `crate::scip`'s ingest route hands to
/// [`Store::replace_scip_occurrences`] — `ordinal`/`source` are the STORE's
/// own concern there (continues the shared ordinal space, tags
/// `source='scip'`), so this input type deliberately carries neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScipOccurrenceIn {
    pub name: String,
    /// `"def"` | `"ref"` — see `crate::scip`'s module doc for the SCIP
    /// `SymbolRole` bit this is mapped from.
    pub role: String,
    pub line: u32,
    pub col_start: u32,
    pub col_end: u32,
}

/// Reads exactly the 17-column order `list_annotations`/`get_annotation`/
/// `list_open_annotations`/`list_review_annotations`/`list_annotations_by_
/// set` all SELECT in (see those methods' doc) — positional, not by name,
/// so it's fine to hand this to a query that SELECTs extra trailing
/// columns of its own (`list_open_annotations`' `reply_count`, read
/// separately by its caller at index 17 — V70-A10 pushed it from 16 to 17
/// when `set_id` was appended at index 16, just before it).
fn annotation_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<AnnotationRow> {
    Ok(AnnotationRow {
        id: r.get(0)?,
        repo_id: r.get(1)?,
        path: r.get(2)?,
        anchor: r.get(3)?,
        anchor_kind: r.get(4)?,
        anchor2: r.get(5)?,
        parent_id: r.get(6)?,
        intent: r.get(7)?,
        body: r.get(8)?,
        author: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        resolved: r.get::<_, i64>(12)? != 0,
        review_id: r.get(13)?,
        ps_number: r.get(14)?,
        side: r.get(15)?,
        set_id: r.get(16)?,
        trail_id: r.get(17)?,
    })
}

/// V73-K3 — reads the 14-column order every `claims` SELECT in this file
/// uses. One mapper, so a column added to the table cannot be picked up by
/// one read and missed by another.
fn claim_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ClaimRow> {
    Ok(ClaimRow {
        id: r.get(0)?,
        repo_id: r.get(1)?,
        subject_kind: r.get(2)?,
        subject: r.get(3)?,
        subject_path: r.get(4)?,
        review_id: r.get(5)?,
        kind: r.get(6)?,
        body_md: r.get(7)?,
        confidence: r.get(8)?,
        evidence_json: r.get(9)?,
        session_id: r.get(10)?,
        model: r.get(11)?,
        blob_sha: r.get(12)?,
        created_at: r.get(13)?,
    })
}

/// V4.C1 — reads the 9-column order `get_annotation_suggestion` SELECTs in.
fn annotation_suggestion_row_from(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<AnnotationSuggestionRow> {
    Ok(AnnotationSuggestionRow {
        annotation_id: r.get(0)?,
        replacement: r.get(1)?,
        original: r.get(2)?,
        base_blob_sha: r.get(3)?,
        applied: r.get::<_, i64>(4)? != 0,
        applied_at: r.get(5)?,
        applied_head_sha: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

/// Tx-scoped twins of the annotation / suggestion writers. Used only by
/// [`Store::apply_annotation_ops`] so the whole batch is one commit.
fn get_annotation_on(tx: &Transaction<'_>, id: &str) -> Result<Option<AnnotationRow>> {
    tx.query_row(
        "SELECT id, repo_id, path, anchor, anchor_kind, anchor2, parent_id, intent,
                body, author, created_at, updated_at, resolved,
                review_id, ps_number, side, set_id, trail_id
         FROM annotations WHERE id = ?1",
        params![id],
        annotation_row_from,
    )
    .optional()
    .map_err(Into::into)
}

fn insert_annotation_on(tx: &Transaction<'_>, row: &AnnotationRow) -> Result<()> {
    tx.execute(
        "INSERT INTO annotations
            (id, repo_id, path, anchor, anchor_kind, anchor2, parent_id, intent,
             body, author, created_at, updated_at, resolved,
             review_id, ps_number, side, set_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            row.id,
            row.repo_id,
            row.path,
            row.anchor,
            row.anchor_kind,
            row.anchor2,
            row.parent_id,
            row.intent,
            row.body,
            row.author,
            row.created_at,
            row.updated_at,
            row.resolved as i64,
            row.review_id,
            row.ps_number,
            row.side,
            row.set_id,
        ],
    )?;
    Ok(())
}

fn update_annotation_on(
    tx: &Transaction<'_>,
    id: &str,
    body: Option<&str>,
    resolved: Option<bool>,
    intent: Option<&str>,
    updated_at: i64,
) -> Result<bool> {
    let n = tx.execute(
        "UPDATE annotations SET
            body = COALESCE(?2, body),
            resolved = COALESCE(?3, resolved),
            intent = COALESCE(?4, intent),
            updated_at = ?5
         WHERE id = ?1",
        params![id, body, resolved.map(|b| b as i64), intent, updated_at],
    )?;
    Ok(n > 0)
}

/// Tx-scoped twin of [`Store::update_annotation_review_scope`] — V80-M0's
/// `BindReview`/`UnbindReview` batch ops. See that method's doc.
fn update_annotation_review_scope_on(
    tx: &Transaction<'_>,
    id: &str,
    scope: Option<(i64, i64, &str)>,
    updated_at: i64,
) -> Result<bool> {
    let (review_id, ps_number, side) = match scope {
        Some((r, p, s)) => (Some(r), Some(p), Some(s)),
        None => (None, None, None),
    };
    let n = tx.execute(
        "UPDATE annotations SET
            review_id = ?2, ps_number = ?3, side = ?4, updated_at = ?5
         WHERE id = ?1",
        params![id, review_id, ps_number, side, updated_at],
    )?;
    Ok(n > 0)
}

fn delete_annotation_on(tx: &Transaction<'_>, id: &str) -> Result<bool> {
    tx.execute(
        "DELETE FROM annotation_suggestions
         WHERE annotation_id = ?1
            OR annotation_id IN (SELECT id FROM annotations WHERE parent_id = ?1)",
        params![id],
    )?;
    tx.execute("DELETE FROM annotations WHERE parent_id = ?1", params![id])?;
    let n = tx.execute("DELETE FROM annotations WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

fn upsert_suggestion_on(
    tx: &Transaction<'_>,
    annotation_id: &str,
    replacement: &str,
    original: &str,
    base_blob_sha: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO annotation_suggestions
            (annotation_id, replacement, original, base_blob_sha,
             applied, applied_at, applied_head_sha, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 0, NULL, NULL, ?5, ?5)
         ON CONFLICT(annotation_id) DO UPDATE SET
            replacement = excluded.replacement,
            original = excluded.original,
            base_blob_sha = excluded.base_blob_sha,
            applied = 0,
            applied_at = NULL,
            applied_head_sha = NULL,
            updated_at = excluded.updated_at",
        params![annotation_id, replacement, original, base_blob_sha, now],
    )?;
    Ok(())
}

fn delete_suggestion_on(tx: &Transaction<'_>, annotation_id: &str) -> Result<bool> {
    let n = tx.execute(
        "DELETE FROM annotation_suggestions WHERE annotation_id = ?1",
        params![annotation_id],
    )?;
    Ok(n > 0)
}

/// Phase E3 — reads the 14-column order `list_reading_sets`/`get_reading_set`
/// both SELECT in (DCB-W3.C widened this from 6 to 10 — the four
/// `source_*` provenance columns appended after `updated_at`; V70-A10
/// widens it again from 10 to 14 — `kind`/`desk_json`/`ref`/
/// `description_md`, `V0028__workspaces.sql`, appended LAST after the
/// `source_*` four). Positional, not by name, so `list_reading_sets` can
/// SELECT further trailing computed columns (`span_count`/`note_count`) of
/// its own without disturbing this read.
fn reading_set_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReadingSetRow> {
    Ok(ReadingSetRow {
        id: r.get(0)?,
        repo_id: r.get(1)?,
        name: r.get(2)?,
        description: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        source_kb: r.get(6)?,
        source_doc_id: r.get(7)?,
        source_doc_path: r.get(8)?,
        source_doc_hash: r.get(9)?,
        kind: r.get(10)?,
        desk_json: r.get(11)?,
        ref_label: r.get(12)?,
        description_md: r.get(13)?,
        workspace_id: r.get(14)?,
    })
}

/// Phase E3 — reads the 6-column order `reading_set_spans` SELECTs in.
fn reading_set_span_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReadingSetSpanRow> {
    Ok(ReadingSetSpanRow {
        ordinal: r.get(0)?,
        path: r.get(1)?,
        line_start: r.get(2)?,
        line_end: r.get(3)?,
        git_ref: r.get(4)?,
        note: r.get(5)?,
    })
}

/// Phase N — reads the 8-column order `list_bookmarks`/`get_bookmark`
/// both SELECT in.
fn bookmark_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<BookmarkRow> {
    Ok(BookmarkRow {
        id: r.get(0)?,
        repo: r.get(1)?,
        path: r.get(2)?,
        line: r.get(3)?,
        mnemonic: r.get(4)?,
        note: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
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

/// One `transcript_files` tail-state row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptFileRow {
    pub id: i64,
    pub inode: i64,
    pub byte_offset: i64,
    pub mtime: i64,
}

/// One `search_transcripts` hit — a `transcript_turns` row joined to its
/// `transcript_files` parent for `project_dir`/`src_file` (the latter is
/// what the search route's snippet builder needs to re-open the raw JSONL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptSearchRow {
    pub session_id: String,
    pub uuid: String,
    pub ts: i64,
    pub kind: String,
    pub tool_name: Option<String>,
    pub project_dir: String,
    pub src_file: String,
    pub byte_offset: i64,
    pub byte_len: i64,
    pub is_sidechain: bool,
}

/// One `transcript_turns_for_session` row — see that method's doc for how
/// this differs from [`TranscriptSearchRow`] (whole-session narrative walk,
/// not an FTS5 hit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptTurnRow {
    pub session_id: String,
    pub uuid: String,
    pub parent_uuid: Option<String>,
    pub ts: i64,
    pub kind: String,
    pub tool_name: Option<String>,
    pub file_paths: Vec<String>,
    pub is_sidechain: bool,
    pub project_dir: String,
    pub src_file: String,
    pub byte_offset: i64,
    pub byte_len: i64,
}

/// `kb-code transcripts status` / `GET /api/transcripts/status`'s payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TranscriptStats {
    pub files: u64,
    pub turns: u64,
    pub indexed_bytes: u64,
}

/// One `transcript_sessions_touching_path` hit (W3.4) — just enough to
/// dedupe-and-order into `provenance::why`'s `session_ids` list; the raw
/// turn text never leaves the store for this query (see that method's doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptPathHit {
    pub session_id: String,
    /// Unix milliseconds (`transcript_turns.ts`).
    pub ts: i64,
}

/// One `commit_sessions` row (W3.2, migration V0005) — the join ladder's
/// precompute cache. Plain strings for `confidence`/`via` at this layer
/// (the store has no opinion on `join::ladder::Confidence`'s 4-valued
/// enum — that conversion lives in `join::ladder`, mirroring how this
/// module already keeps its row types free of `search`/`semantic` concerns
/// elsewhere). See the migration's doc for the freshness contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitSessionRow {
    pub confidence: String,
    pub via: String,
    pub session_id: Option<String>,
    pub kb: Option<String>,
    pub display_name: Option<String>,
    pub started_at: Option<i64>,
    pub resolved_at: i64,
}

/// One `annotations` row (W4.6 migration V0006; D-server migration V0008
/// adds `anchor_kind`/`anchor2`/`parent_id`/`intent`; V4.C1 / V0023 adds
/// `review_id`/`ps_number`/`side`). `anchor`/`anchor2` are RAW JSON
/// exactly as stored — callers (`crate::annotations`) deserialize them,
/// never this module (the store stays free of kb-core's review types,
/// same "row types don't know about other modules' concerns" convention
/// `CommitSessionRow` follows above).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationRow {
    pub id: String,
    pub repo_id: i64,
    pub path: String,
    /// `None` only for a REPLY (`parent_id.is_some()`) — every top-level
    /// annotation always has one. A JSON-encoded `kb_core::review::Anchor`.
    pub anchor: Option<String>,
    /// "line" | "range" | "symbol" | "diff" — vocab validated at the route
    /// boundary (`crate::annotations::is_valid_anchor_kind`), never here.
    /// Meaningless (left at its `'line'` DEFAULT) on a reply row.
    pub anchor_kind: String,
    /// Nullable, kind-dependent JSON payload — `None` for `line` and for a
    /// reply; see `crate::annotations`'s module doc for what each OTHER
    /// kind stores here.
    pub anchor2: Option<String>,
    /// `Some` for a REPLY — the annotation it's nested under. One level of
    /// nesting only, enforced in code (`routes::create_annotation`), not a
    /// SQL FK — see the migration's doc.
    pub parent_id: Option<String>,
    /// "note" | "question" | "todo" | "flag-for-agent" | "tour-stop" |
    /// "claim" (V72-J2) — vocab validated at the route boundary
    /// (`crate::annotations::is_valid_intent`).
    pub intent: String,
    pub body: String,
    pub author: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub resolved: bool,
    /// V4.C1 — `reviews.id` when this row is review-scoped. `None` for
    /// ordinary working-tree annotations (every pre-V0023 row). No SQL
    /// FK; cascade is `delete_review`'s job.
    pub review_id: Option<i64>,
    /// Patchset the comment was CREATED against (not the target it is
    /// later resolved against — that is computed per request).
    pub ps_number: Option<i64>,
    /// `"old"` | `"new"` — which side of the patchset the anchor was
    /// built from. Route-validated, never here.
    pub side: Option<String>,
    /// V70-A10 ("Workspaces v0") — `reading_sets.id` when this row is a
    /// workspace note (general path-less, `annotations::ANCHOR_KIND_SET`,
    /// OR an ordinary code-anchored comment scoped to a workspace via
    /// `set_id` alongside its normal `line`/`range`/`symbol`/`diff`
    /// anchor). `None` for every annotation not scoped to a workspace
    /// (every pre-V0028 row). TEXT (matches `reading_sets.id`'s own TEXT
    /// PK — see `V0028__workspaces.sql`'s doc), no SQL FK; cascade is
    /// `delete_reading_set`'s job (same `review_id`/`delete_review`
    /// precedent above). A reply inherits its parent's `set_id`
    /// (`routes::assemble_reply_annotation`), so this is never a case
    /// where a top-level row and its own reply disagree.
    pub set_id: Option<String>,
    /// V74-L3b (`kbc-trail/1`) — `trails.id` when this row is a DISSENT
    /// note on an agent-AUTHORED trail (D12: "the human walks `]`/`[` and
    /// dissents inline"). `None` for every annotation that is not one
    /// (every pre-V0039 row). TEXT (matches `trails.id`'s own TEXT PK),
    /// no SQL FK; a reply inherits it through the SAME
    /// `routes::inherit_scope_field` ladder `set_id`/`review_id` use.
    ///
    /// Deliberately NOT removed by a trail purge: a note is the human's
    /// own authored words, and invariant 23(a) rules that authored
    /// content is not derived data. A note whose trail was purged reads
    /// back saying so rather than vanishing with it.
    pub trail_id: Option<String>,
}

/// One `annotation_suggestions` row (V4.C1 / V0023). V4.C2 owns the
/// upsert/delete accessors; apply is S1. `applied` is SQLite's 0/1
/// boolean, mapped to `bool` here like `AnnotationRow.resolved`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationSuggestionRow {
    pub annotation_id: String,
    pub replacement: String,
    pub original: String,
    pub base_blob_sha: String,
    pub applied: bool,
    pub applied_at: Option<i64>,
    pub applied_head_sha: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// V4.C2 — one already-validated suggestion write. The route captures
/// `original` / `base_blob_sha` from the blob BEFORE taking the store
/// lock; this type just carries those bytes into the tx.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSuggestionWrite {
    pub annotation_id: String,
    pub replacement: String,
    pub original: String,
    pub base_blob_sha: String,
}

/// V4.C2 — one op ready for [`Store::apply_annotation_ops`]. Named
/// variants (not a pile of Options) so clippy's `type_complexity` stays
/// quiet — same rationale as `routes::InheritedReviewScope`.
#[derive(Debug, Clone)]
pub enum PreparedAnnotationOp {
    Insert {
        row: Box<AnnotationRow>,
        suggestion: Option<PreparedSuggestionWrite>,
    },
    EditBody {
        id: String,
        body: String,
    },
    SetIntent {
        id: String,
        intent: String,
    },
    SetResolved {
        id: String,
        resolved: bool,
    },
    Delete {
        id: String,
    },
    UpsertSuggestion(PreparedSuggestionWrite),
    ClearSuggestion {
        annotation_id: String,
    },
    /// V80-M0 — bind/rebind an EXISTING top-level annotation's review
    /// scope. `(review_id, ps_number, side)` is already fully resolved
    /// (existence/repo-match/open-state validated) by the route.
    BindReview {
        id: String,
        review_id: i64,
        ps_number: i64,
        side: String,
    },
    /// V80-M0 — clear an EXISTING annotation's review scope.
    UnbindReview {
        id: String,
    },
}

/// Outcome of [`Store::apply_annotation_ops`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnnotationOpReport {
    pub applied: usize,
    pub created_ids: Vec<String>,
    pub changed: bool,
}

/// One `reading_sets` row (Phase E3) — see the migration's doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingSetRow {
    pub id: String,
    pub repo_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// DCB-W3.C — doc-materialization provenance
    /// (`V0022__reading_sets_doc_provenance.sql`). All four `None` on every
    /// set NOT created via `POST /api/sets/from-doc`.
    pub source_kb: Option<String>,
    pub source_doc_id: Option<String>,
    pub source_doc_path: Option<String>,
    pub source_doc_hash: Option<String>,
    /// V70-A10 — `"set"` (the pre-existing default) | `"workspace"`.
    /// Validated at the route boundary (`reading_sets::is_valid_set_kind`),
    /// never here.
    pub kind: String,
    /// V70-A10 — the workspace's opaque `DeskState` snapshot (JSON, ≤ 64
    /// KiB, `V0028__workspaces.sql`). `None` on a plain `'set'` row and on
    /// a `'workspace'` row saved before a client sent one. Stored VERBATIM
    /// — never parsed server-side.
    pub desk_json: Option<String>,
    /// V70-A10 — the SQL column is literally `ref` (same unquoted-keyword
    /// precedent `reading_set_spans.ref`/`SpanOut::git_ref` already use);
    /// named `ref_label` here since `ref` is a Rust keyword. An optional
    /// branch/ref label a workspace groups under (`GET /api/sets?kind=
    /// workspace&group=ref`) — shape-validated only
    /// (`reviews::reject_user_ref`), never checked for existence.
    pub ref_label: Option<String>,
    /// V70-A10 — free-text Markdown description (≤ 64 KiB), separate from
    /// the pre-existing short `description` column.
    pub description_md: Option<String>,
    /// V71-G0 (D26) — the kbc-seq/1 workspace this projection is bound to
    /// (another `reading_sets.id`, of kind `'workspace'`), or `None` when
    /// it is unbound. Written only by [`Store::set_reading_set_workspace`]
    /// (a direct assignment, so unbinding is expressible — see that
    /// method's doc).
    pub workspace_id: Option<String>,
}

/// One `reading_set_spans` row, as read back (already carries its
/// `ordinal`) — contrast [`NewReadingSetSpan`], the caller-supplied shape
/// with no ordinal yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingSetSpanRow {
    pub ordinal: i64,
    pub path: String,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
    pub git_ref: Option<String>,
    pub note: Option<String>,
}

/// One span as supplied by a caller, BEFORE the store assigns its
/// `ordinal` — slice POSITION for `create_reading_set`/
/// `replace_reading_set_spans`, `MAX(ordinal) + 1` for
/// `append_reading_set_span`. `reading_sets::validate_span` is what
/// produces these from the wire `SpanInput` shape (route-boundary
/// validation happens there, never in this store-layer type).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewReadingSetSpan {
    pub path: String,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
    pub git_ref: Option<String>,
    pub note: Option<String>,
}

/// One `bookmarks` row (Phase N) — see migration V0011's doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkRow {
    pub id: i64,
    pub repo: String,
    pub path: String,
    pub line: i64,
    pub mnemonic: Option<String>,
    pub note: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One todo list row — the `GET /api/todos` view over `comments/1`
/// (V72-J1). The shape is unchanged from the deleted `todo_items`
/// implementation so every existing consumer (the route, `kb-code todos`,
/// `tree::sources`' two decoration lanes) reads it verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItemRow {
    pub path: String,
    pub line: i64,
    pub marker: String,
    pub text: String,
}

/// One `comments` row (V72-J1, migration V0032).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentRow {
    pub path: String,
    pub blob_sha: String,
    pub comments_version: String,
    pub ordinal: i64,
    pub kind: String,
    pub keyword: Option<String>,
    pub keyword_text: Option<String>,
    /// `keywords::SmartTodoFields` as JSON, or `None`.
    pub fields_json: Option<String>,
    pub line_start: i64,
    pub line_end: i64,
    pub text: String,
    pub text_truncated: bool,
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub symbol_line_start: Option<i64>,
    pub symbol_line_end: Option<i64>,
    pub directive_tool: Option<String>,
    /// `None` for a magic comment / build tag (nothing to justify);
    /// `Some` only for a suppression directive.
    pub directive_has_reason: Option<bool>,
}

/// One comment block to write via [`Store::replace_comments`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewComment {
    pub ordinal: i64,
    pub kind: String,
    pub keyword: Option<String>,
    pub keyword_text: Option<String>,
    pub fields_json: Option<String>,
    pub line_start: i64,
    pub line_end: i64,
    pub text: String,
    pub text_truncated: bool,
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub symbol_line_start: Option<i64>,
    pub symbol_line_end: Option<i64>,
    pub directive_tool: Option<String>,
    pub directive_has_reason: Option<bool>,
}

/// The ONE row mapper every `comments` SELECT shares — the column list is
/// identical across four queries, so a reordering can never desync one of
/// them from the others.
fn comment_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<CommentRow> {
    Ok(CommentRow {
        path: r.get(0)?,
        blob_sha: r.get(1)?,
        comments_version: r.get(2)?,
        ordinal: r.get(3)?,
        kind: r.get(4)?,
        keyword: r.get(5)?,
        keyword_text: r.get(6)?,
        fields_json: r.get(7)?,
        line_start: r.get(8)?,
        line_end: r.get(9)?,
        text: r.get(10)?,
        text_truncated: r.get::<_, i64>(11)? != 0,
        symbol_name: r.get(12)?,
        symbol_kind: r.get(13)?,
        symbol_line_start: r.get(14)?,
        symbol_line_end: r.get(15)?,
        directive_tool: r.get(16)?,
        directive_has_reason: r.get::<_, Option<i64>>(17)?.map(|v| v != 0),
    })
}

/// One `reviews` row (V3.R1 / V0014; V4.C1 / V0023 adds the four
/// `verdict*` columns — NULL until V4.C2's write routes stamp them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRow {
    pub id: i64,
    pub repo: String,
    pub title: Option<String>,
    pub base_ref: String,
    pub head_ref: String,
    pub session_id: Option<String>,
    pub state: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// `"comment"` | `"approve"` | `"request-changes"` when set.
    pub verdict: Option<String>,
    pub verdict_note: Option<String>,
    pub verdict_at: Option<i64>,
    pub verdict_ps: Option<i64>,
}

/// One `review_patchsets` row (V3.R1 / V0014).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPatchsetRow {
    pub id: i64,
    pub review_id: i64,
    pub ps_number: i64,
    pub tip_sha: String,
    pub base_sha: String,
    pub captured_at: i64,
}

/// One `review_viewed` row (V3.R1 / V0014).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewViewedRow {
    pub review_id: i64,
    pub path: String,
    pub blob_sha: String,
    pub viewed_at: i64,
}

/// One `review_hunk_viewed` row (V73-K2a / V0031). `hunk_id` is the
/// SPA's own `kbc-hunkid/1` content address — opaque here by design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewHunkViewedRow {
    pub review_id: i64,
    pub hunk_id: String,
    pub path: String,
    pub viewed_at: i64,
}

// --- kbc-canvas/1 boards (V74-L1, migration V0036) -------------------------

/// One `canvas_boards` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasBoardRow {
    pub id: i64,
    pub repo_id: i64,
    pub slug: String,
    pub title: String,
    pub description_md: String,
    pub status: String,
    pub authored_ref: Option<String>,
    pub content_hash: String,
    pub revision: i64,
    pub created_unix: i64,
    pub updated_unix: i64,
}

/// A board summary for `GET /api/boards` — the parent row plus the three
/// child counts, so a list never has to fetch children to say how big a
/// board is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasBoardSummaryRow {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub status: String,
    pub revision: i64,
    pub updated_unix: i64,
    pub nodes: i64,
    pub edges: i64,
    pub steps: i64,
}

/// One `canvas_nodes` row. `ref_json` is this daemon's OWN typed reference
/// (`boards::RefFields`), parsed on every read — see V0036's header for why
/// that is the deliberate opposite of `reading_sets.desk_json`.
#[derive(Debug, Clone, PartialEq)]
pub struct CanvasNodeRow {
    pub node_id: String,
    pub ordinal: i64,
    pub kind: String,
    pub title: Option<String>,
    pub body_md: Option<String>,
    pub ref_json: String,
    pub group_id: Option<String>,
    pub thread_id: Option<String>,
    pub anchor_snippet: Option<String>,
    pub pin_x: Option<f64>,
    pub pin_y: Option<f64>,
}

/// One `canvas_edges` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasEdgeRow {
    pub from_node: String,
    pub to_node: String,
    pub kind: String,
    pub label: Option<String>,
    pub provenance: String,
    pub trust: Option<String>,
}

/// One `canvas_steps` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasStepRow {
    pub node_id: String,
    pub caption: Option<String>,
    /// V74-L3b — the per-step CAMERA, JSON, `None` on every board step.
    /// Parsed by `tours::Camera`, never by this module.
    pub camera_json: Option<String>,
}

/// The board half of an apply payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCanvasBoard {
    /// V74-L3b — `board` or `tour` (`tours::BOARD_KINDS`). Required, never
    /// defaulted: see [`Store::list_canvas_boards`].
    pub kind: String,
    pub slug: String,
    pub title: String,
    pub description_md: String,
    pub status: String,
    pub authored_ref: Option<String>,
    pub content_hash: String,
}

/// One node of an apply payload. `ordinal` is NOT a field: it is the
/// position in the slice, so a caller cannot hand in a document whose
/// declared order and actual order disagree.
#[derive(Debug, Clone, PartialEq)]
pub struct NewCanvasNode {
    pub node_id: String,
    pub kind: String,
    pub title: Option<String>,
    pub body_md: Option<String>,
    pub ref_json: String,
    pub group_id: Option<String>,
    pub thread_id: Option<String>,
    pub anchor_snippet: Option<String>,
    pub pin_x: Option<f64>,
    pub pin_y: Option<f64>,
}

/// One edge of an apply payload (see [`NewCanvasNode`] on `ordinal`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCanvasEdge {
    pub from_node: String,
    pub to_node: String,
    pub kind: String,
    pub label: Option<String>,
    pub provenance: String,
    pub trust: Option<String>,
}

/// One step of an apply payload (see [`NewCanvasNode`] on `ordinal`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCanvasStep {
    pub node_id: String,
    pub caption: Option<String>,
    /// V74-L3b — the per-step camera, already serialized by the caller
    /// AFTER the lint validated it.
    pub camera_json: Option<String>,
}

// --- V74-L3b: kbc-trail/1 rows -------------------------------------------

/// One step of an ingest batch, AFTER the route derived and quantised its
/// dwell. There is no `left_at` field: it was an input to `dwell_secs`, not
/// a fact worth keeping (see `trails`'s module doc, property 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTrailStep {
    pub via: String,
    pub path: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub symbol: Option<String>,
    pub blob_sha: Option<String>,
    pub entered_at: i64,
    pub dwell_secs: i64,
    pub day: String,
    pub note: Option<String>,
}

/// An explicitly created trail — AUTHORED or a FORK. Both carry a NULL
/// `day`, which is what exempts them from the one-recorded-trail-per-day
/// unique index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTrail {
    pub origin: String,
    pub title: Option<String>,
    pub parent_id: Option<String>,
    pub parent_ordinal: Option<i64>,
    pub session_hint: Option<String>,
}

/// One `trails` row plus its two derived counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailSummaryRow {
    pub id: String,
    pub origin: String,
    pub title: Option<String>,
    pub day: Option<String>,
    pub parent_id: Option<String>,
    pub parent_ordinal: Option<i64>,
    pub created_unix: i64,
    pub updated_unix: i64,
    pub steps: i64,
    pub dwell_secs: i64,
}

/// One `trail_steps` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailStepRow {
    pub ordinal: i64,
    pub via: String,
    pub path: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub symbol: Option<String>,
    pub blob_sha: Option<String>,
    pub entered_at: i64,
    pub dwell_secs: i64,
    pub day: String,
    pub note: Option<String>,
}

/// One row of the aggregate read. No timestamp finer than a day exists on
/// this struct, and that is the privacy contract rather than an oversight —
/// `trails::tests::an_aggregate_row_carries_no_timestamp_finer_than_a_day`
/// pins the wire shape it feeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailAggregateRow {
    pub path: Option<String>,
    pub symbol: Option<String>,
    pub steps: i64,
    pub dwell_secs: i64,
    pub days: i64,
    pub first_day: String,
    pub last_day: String,
}

/// One dissent note on an AUTHORED trail — an ordinary `annotations` row,
/// read back by its `trail_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailNoteRow {
    pub id: String,
    pub parent_id: Option<String>,
    pub author: String,
    pub intent: String,
    pub body: String,
    pub path: String,
    pub resolved: bool,
    pub created_at: i64,
}

/// What an apply DID — every field a caller needs to render the outcome
/// without a second read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasApplyOutcome {
    pub board_id: i64,
    pub created: bool,
    /// `true` when the document's content hash already matched: nothing was
    /// written and `revision` did not move.
    pub unchanged: bool,
    pub revision: i64,
    pub status: String,
    /// `true` when a changed apply moved the board off `accepted`/
    /// `archived` — D21's ruling, surfaced rather than silent.
    pub status_reset: bool,
}

fn canvas_board_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<CanvasBoardRow> {
    Ok(CanvasBoardRow {
        id: r.get(0)?,
        repo_id: r.get(1)?,
        slug: r.get(2)?,
        title: r.get(3)?,
        description_md: r.get(4)?,
        status: r.get(5)?,
        authored_ref: r.get(6)?,
        content_hash: r.get(7)?,
        revision: r.get(8)?,
        created_unix: r.get(9)?,
        updated_unix: r.get(10)?,
    })
}

/// One `claims` row (V73-K3 / V0035, `kbc-claim/1`). There is deliberately
/// no `trust`/`state` column: the Ladder is computed per request by
/// `crate::claims::ladder_state` from `blob_sha` against the file's live
/// blob — root invariant #2's "kb-code mints classes, nothing is cached",
/// applied to the agent's own prose. `evidence_json` is RAW JSON exactly as
/// stored (a JSON array of kbc refs), the same "row types don't parse other
/// modules' JSON" convention `AnnotationRow` follows.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimRow {
    pub id: String,
    pub repo_id: i64,
    pub subject_kind: String,
    pub subject: String,
    pub subject_path: Option<String>,
    pub review_id: Option<i64>,
    pub kind: String,
    pub body_md: String,
    /// The AGENT'S declared confidence, verbatim. Never a ranking term.
    pub confidence: Option<f64>,
    pub evidence_json: String,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub blob_sha: Option<String>,
    pub created_at: i64,
}

/// The `claims` read filter. Every field is AND-ed; `None` means "no
/// constraint". One struct rather than five query variants so
/// [`Store::count_claims`] and [`Store::list_claims`] cannot drift apart —
/// a `total` computed under a different predicate than the page it
/// describes is the classic paging lie.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimFilter {
    pub repo_id: i64,
    pub subject: Option<String>,
    pub subject_kind: Option<String>,
    pub subject_path: Option<String>,
    pub review_id: Option<i64>,
    pub kind: Option<String>,
}

impl ClaimFilter {
    /// The shared `WHERE` text + its bound values. ONE builder, so the
    /// count and the page are the same predicate by construction.
    fn where_clause(&self) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut sql = String::from("repo_id = ?1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(self.repo_id)];
        let optional: [(&str, Option<Box<dyn rusqlite::ToSql>>); 5] = [
            (
                "subject",
                self.subject
                    .clone()
                    .map(|v| Box::new(v) as Box<dyn rusqlite::ToSql>),
            ),
            (
                "subject_kind",
                self.subject_kind
                    .clone()
                    .map(|v| Box::new(v) as Box<dyn rusqlite::ToSql>),
            ),
            (
                "subject_path",
                self.subject_path
                    .clone()
                    .map(|v| Box::new(v) as Box<dyn rusqlite::ToSql>),
            ),
            (
                "review_id",
                self.review_id
                    .map(|v| Box::new(v) as Box<dyn rusqlite::ToSql>),
            ),
            (
                "kind",
                self.kind
                    .clone()
                    .map(|v| Box::new(v) as Box<dyn rusqlite::ToSql>),
            ),
        ];
        // The placeholder number IS the argument's position, so the two can
        // never drift the way a separately-incremented counter can.
        for (col, value) in optional {
            if let Some(v) = value {
                args.push(v);
                sql.push_str(&format!(" AND {col} = ?{}", args.len()));
            }
        }
        (sql, args)
    }
}

/// V3.2-B1 — `behavioral_meta` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehavioralMetaRow {
    pub repo_id: i64,
    pub last_commit_sha: Option<String>,
    pub updated_at: i64,
}

/// V3.2-B1 — one `path_stats` row (repo-addressed history counters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathStatsRow {
    pub path: String,
    pub revisions: i64,
    pub lines_added: i64,
    pub lines_deleted: i64,
    pub first_seen_unix: Option<i64>,
    pub last_touch_unix: Option<i64>,
}

/// V3.2-B1 — one `author_stats` row for a path.
/// V3.2-B2 adds `first_seen_unix` (nullable until rebuild).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorStatsRow {
    pub author: String,
    pub commits: i64,
    pub first_seen_unix: Option<i64>,
}

/// V3.2-B2 — one `session_signals` row (pain evidence).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSignalsRow {
    pub session_id: String,
    pub fail_count: i64,
    pub error_count: i64,
    pub duration_secs: i64,
    pub captured_at: i64,
}

/// V3.4-C1 — full `canvas_sets` row (incl. opaque payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasSetRow {
    pub id: i64,
    pub repo_id: i64,
    pub name: String,
    pub review_id: Option<i64>,
    pub payload: String,
    pub created_unix: i64,
    pub updated_unix: i64,
}

/// V3.4-C1 — list-row (no payload body; `payload_bytes` only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasSetSummaryRow {
    pub id: i64,
    pub name: String,
    pub review_id: Option<i64>,
    pub updated_unix: i64,
    pub payload_bytes: i64,
}

fn canvas_set_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<CanvasSetRow> {
    Ok(CanvasSetRow {
        id: r.get(0)?,
        repo_id: r.get(1)?,
        name: r.get(2)?,
        review_id: r.get(3)?,
        payload: r.get(4)?,
        created_unix: r.get(5)?,
        updated_unix: r.get(6)?,
    })
}

fn review_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewRow> {
    Ok(ReviewRow {
        id: r.get(0)?,
        repo: r.get(1)?,
        title: r.get(2)?,
        base_ref: r.get(3)?,
        head_ref: r.get(4)?,
        session_id: r.get(5)?,
        state: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        verdict: r.get(9)?,
        verdict_note: r.get(10)?,
        verdict_at: r.get(11)?,
        verdict_ps: r.get(12)?,
    })
}

fn review_patchset_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewPatchsetRow> {
    Ok(ReviewPatchsetRow {
        id: r.get(0)?,
        review_id: r.get(1)?,
        ps_number: r.get(2)?,
        tip_sha: r.get(3)?,
        base_sha: r.get(4)?,
        captured_at: r.get(5)?,
    })
}

fn review_viewed_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewViewedRow> {
    Ok(ReviewViewedRow {
        review_id: r.get(0)?,
        path: r.get(1)?,
        blob_sha: r.get(2)?,
        viewed_at: r.get(3)?,
    })
}

// ── PRR-R1: PR binding + findings ──────────────────────────────────────
//
// kb v0.39 "The PR Room" (T2), unit R1 — Phase 1 of design-server.md: schema
// (migration V0024, already applied above) + the store-layer CRUD/
// reconciliation core. No routes, nothing reachable from HTTP yet — the
// NEXT phase (R2/R3) wires these through `router.rs`. Severity/disposition
// vocab per the milestone plan's arbitrations (blocker|concern|ok,
// agree|dispute|waive|fix-later), not the design doc's original sketch.
//
// PR-binding / report / verdict-publish are plain columns on `reviews`
// (design doc §1.2 — a snapshot, not a live mirror). Rather than widen the
// existing `ReviewRow`/`review_row_from`/`get_review` surface (shared,
// high-traffic, and edited by sibling units this same wave), these get
// their OWN small targeted get/set pairs below, each doing its own
// SELECT/UPDATE over just the columns it owns — additive-only, zero risk
// of colliding with another unit's edit to the existing review CRUD.
//
// Findings (`review_findings`, design doc §1.3) are a sibling table 1:1 on
// `annotation_id`, mirroring `annotation_suggestions`'s existing shape.
// [`derive_finding_anchor`] implements the §1.4 location-kind ladder
// (single|range|multi|whole_file -> annotations anchor_kind/anchor/anchor2/
// side) as a PURE function — no git I/O — so it is unit-testable without a
// repo; a later phase's import route supplies the real per-line text read
// from the target patchset's pinned git blob. [`Store::reconcile_findings_
// import`] is the §4.3 reconciliation core: new slug -> create; existing
// slug present again -> refresh volatile fields + stamp
// `content_updated_at` + un-supersede, but NEVER touch disposition/thread
// (a human's prior agree/dispute/waive call survives a re-review
// untouched); existing slug absent -> soft-supersede
// (`superseded_reason="not_in_reimport"`), never hard-deleted, mirroring
// invariant #10's MI-W2.3 precedent. A slug whose refreshed fields are
// BYTE-IDENTICAL to what is already stored (and was not previously
// superseded) is reported "unchanged" rather than "updated" — no spurious
// `content_updated_at` bump or watch-loop noise on a true no-op re-review,
// the same "compare before writing" discipline `set_review_verdict` already
// uses for the verdict columns.
//
// Design fill-ins not spelled out verbatim in the spec (flagged here, and
// in the unit's own report, for a later phase to revisit if wrong):
//   - A finding's linked `annotations.body` mirrors its `title` (the
//     annotation table's `body` is NOT NULL and needs *something*
//     human-readable for `/comments`-style thread rendering; `rationale`
//     stays the long-form field, `review_findings`-only).
//   - A finding's linked `annotations.author` is whatever the import call
//     supplies (the generator agent's identity, e.g. "claude") — plain
//     pass-through, not defaulted here.
//   - `side` is always written explicitly ("old" when `location_removed`,
//     else "new") — never a bare `NULL`, matching
//     `resolve_review_create_scope`'s existing explicit-string convention.
//   - Un-superseding a slug whose CONTENT happens to be unchanged still
//     stamps `content_updated_at`/`updated_at` (it is itself a real,
//     narratively meaningful write) and is bucketed "updated", not
//     "unchanged".

/// PR-binding columns on `reviews` (V0024), as read back. Every field is
/// `None` on a pre-V0024 (or never-bound) review — see
/// `legacy_review_row_reads_back_with_pr_binding_report_and_verdict_
/// publish_columns_null`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReviewPrBinding {
    pub pr_number: Option<i64>,
    pub pr_repo_slug: Option<String>,
    pub pr_head_sha: Option<String>,
    pub pr_meta_json: Option<String>,
    pub pr_meta_fetched_at: Option<i64>,
    pub artifact_hint_kb: Option<String>,
    pub artifact_hint_id: Option<String>,
}

/// The agent-authored review report (V0024's `report_json`/
/// `report_updated_at`), as read back.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReviewReport {
    pub report_json: Option<String>,
    pub report_updated_at: Option<i64>,
}

/// One `review_findings` row (V0024), as read back. `location_lines` and
/// `anchor`/`anchor2` (on the linked `annotations` row, not here) are RAW
/// JSON exactly as stored — same "row types don't parse other modules'
/// JSON" convention `AnnotationRow` already follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFindingRow {
    pub id: i64,
    pub review_id: i64,
    pub annotation_id: String,
    pub slug: String,
    /// "blocker" | "concern" | "ok" — route-validated, never here (see
    /// [`is_valid_severity`]).
    pub severity: String,
    pub category: String,
    /// "single" | "range" | "multi" | "whole_file" — see
    /// [`derive_finding_anchor`].
    pub location_kind: String,
    pub location_path: String,
    pub location_lines: Option<String>,
    pub location_removed: bool,
    pub title: String,
    pub rationale: String,
    pub recommendation: Option<String>,
    pub evidence_lang: Option<String>,
    pub evidence_source: Option<String>,
    /// PRR-R1 scope extension (operator-ratified mid-build, human-authored
    /// findings) — "import" | "manual", route-validated (see
    /// [`is_valid_finding_origin`]). Gates
    /// [`Store::reconcile_findings_import`]'s supersede step: a "manual"
    /// row is NEVER superseded by a re-import.
    pub origin: String,
    /// Identity string of whoever created this finding row — independent
    /// of the linked `annotations.author`. `None` is acceptable (and
    /// typical) for `origin = "import"` in v1.
    pub author: Option<String>,
    /// "agree" | "dispute" | "waive" | "fix-later" | `None` (undecided) —
    /// route-validated, never here (see [`is_valid_disposition`]).
    pub disposition: Option<String>,
    pub disposition_note: Option<String>,
    pub disposition_by: Option<String>,
    pub disposition_at: Option<i64>,
    pub content_updated_at: Option<i64>,
    /// "unpublished" | "published".
    pub published_state: String,
    pub published_at: Option<i64>,
    pub published_url: Option<String>,
    pub superseded: bool,
    pub superseded_at: Option<i64>,
    pub superseded_reason: Option<String>,
    pub import_batch_id: String,
    pub created_at: i64,
    pub updated_at: i64,
    // --- findings v2 (V73-K1, migration V0034) ---------------------------
    /// The SPEECH-ACT axis (`review_doc::ACTS`) — route-validated on the
    /// `compose` path, `'issue'` by DEFAULT on every pre-V0034 row so an
    /// existing finding reads back exactly as it always meant.
    pub act: String,
    /// The reviewer's OWN call, deliberately not derived from `severity`:
    /// "a blocker that is not blocking this PR" is a real thing to say.
    pub blocking: bool,
    /// SECONDARY refs, raw JSON exactly as stored (same "row types don't
    /// parse other modules' JSON" convention the rest of this struct
    /// follows). The PRIMARY location is still `annotation_id`.
    pub cites_json: Option<String>,
    /// The CHANGE DETECTOR (`review_doc::fingerprint`). `None` on every
    /// pre-V0034 row and never backfilled — nothing computed one for those
    /// rows, and inventing one would let a re-compose silently adopt a
    /// finding it did not write.
    pub fingerprint: Option<String>,
    /// The slug that REPLACED this one, when the composing author declared
    /// the supersession. Never inferred.
    pub superseded_by: Option<String>,
}

/// A finding ready to persist — the caller has already: (a) validated
/// severity/location_kind/disposition against the closed vocabs below, and
/// (b) derived the annotation anchor fields (typically via
/// [`derive_finding_anchor`], fed with real line text read from the target
/// patchset's pinned git blob — I/O this store layer never does itself).
/// Mirrors `PreparedSuggestionWrite`'s "caller does I/O before the lock"
/// shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReviewFinding {
    pub review_id: i64,
    pub repo_id: i64,
    pub ps_number: i64,
    pub slug: String,
    pub severity: String,
    pub category: String,
    pub location_kind: String,
    pub location_path: String,
    pub location_lines: Option<String>,
    pub location_removed: bool,
    pub title: String,
    pub rationale: String,
    pub recommendation: Option<String>,
    pub evidence_lang: Option<String>,
    pub evidence_source: Option<String>,
    pub anchor_kind: String,
    pub anchor: String,
    pub anchor2: Option<String>,
    pub side: Option<String>,
    /// The linked `annotations` row's author (the review-comment thread
    /// identity) — distinct from `finding_author` below.
    pub author: String,
    pub import_batch_id: String,
    /// PRR-R1 scope extension — "import" | "manual" (see
    /// [`is_valid_finding_origin`]); written to `review_findings.origin`.
    pub origin: String,
    /// `review_findings.author` — the finding record's own creator
    /// identity, independent of `author` above (which is the linked
    /// annotation's). `None` is acceptable (and typical) for
    /// `origin = "import"` in v1.
    pub finding_author: Option<String>,
    // --- findings v2 (V73-K1) --------------------------------------------
    /// `review_doc::ACTS`; `"issue"` reproduces the v1 meaning exactly.
    pub act: String,
    pub blocking: bool,
    pub cites_json: Option<String>,
    pub fingerprint: Option<String>,
}

/// V80-M5 — a finding that ADOPTS an already-existing top-level, review-
/// bound `annotations` row as its thread, rather than minting a fresh
/// annotation the way [`NewReviewFinding`] does. Same shape as
/// [`NewReviewFinding`] minus every anchor field (`anchor_kind`/`anchor`/
/// `anchor2`/`side`) and the linked annotation's own `author` — those all
/// come from the annotation being adopted, verbatim, never re-derived —
/// plus `cites_json`/`fingerprint`, which findings v2 gives no route to set
/// on an adoption (a promoted comment has no document to fingerprint
/// against). See `review_findings.rs`'s module doc for the full adoption
/// contract (the OWNED item this unit resolves).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedReviewFinding {
    pub review_id: i64,
    /// The `annotations.id` being adopted — MUST already exist, be
    /// top-level (`parent_id IS NULL`), and be bound to `review_id`
    /// (`review_findings.rs`'s route boundary validates all three before
    /// this ever reaches the store; this struct trusts its caller).
    pub annotation_id: String,
    pub slug: String,
    pub severity: String,
    pub category: String,
    pub location_kind: String,
    pub location_path: String,
    pub location_lines: Option<String>,
    pub location_removed: bool,
    pub title: String,
    pub rationale: String,
    pub recommendation: Option<String>,
    pub evidence_lang: Option<String>,
    pub evidence_source: Option<String>,
    pub import_batch_id: String,
    pub finding_author: Option<String>,
    pub act: String,
    pub blocking: bool,
}

/// One finding inside a `findings/import` batch — same shape as
/// [`NewReviewFinding`] minus the fields that are constant for the WHOLE
/// batch (`review_id`/`repo_id`/`ps_number`/`author`/`import_batch_id`),
/// which [`Store::reconcile_findings_import`] takes once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedFinding {
    pub slug: String,
    pub severity: String,
    pub category: String,
    pub location_kind: String,
    pub location_path: String,
    pub location_lines: Option<String>,
    pub location_removed: bool,
    pub title: String,
    pub rationale: String,
    pub recommendation: Option<String>,
    pub evidence_lang: Option<String>,
    pub evidence_source: Option<String>,
    pub anchor_kind: String,
    pub anchor: String,
    pub anchor2: Option<String>,
    pub side: Option<String>,
    // --- findings v2 (V73-K1) --------------------------------------------
    /// `review_doc::ACTS`. The v1 `findings/import` route sends
    /// `"issue"` for every row, which is exactly what those rows have
    /// always meant.
    pub act: String,
    pub blocking: bool,
    pub cites_json: Option<String>,
    /// `Some` only on the `compose` (document) path — it is what
    /// [`FindingIdentity::Fingerprint`] matches on. The v1 path sends
    /// `None` and keeps matching on the slug.
    pub fingerprint: Option<String>,
    /// Slugs this finding declares it REPLACES. Written to the replaced
    /// row's `superseded_by` during the supersede step. Never inferred.
    pub supersedes: Vec<String>,
}

impl ImportedFinding {
    /// A v1 (`kbc-findings/1`) import item's v2 defaults — one place, so
    /// the two call sites (`findings/import` and `compose` v0) cannot
    /// drift apart on what a v1 row means in the v2 columns.
    pub fn v1_defaults() -> (String, bool, Option<String>, Option<String>, Vec<String>) {
        ("issue".to_string(), false, None, None, Vec::new())
    }
}

/// What makes two findings THE SAME finding across a re-import.
///
/// `Slug` is `kbc-findings/1`'s rule and the only one the low-level
/// `findings import` twin has ever used: the author supplies a stable slug
/// and owns it. `Fingerprint` is `kbc-review/1`'s (V73-K1, design D9): the
/// slug is IDENTITY but is MINTED by this daemon, so a re-compose that
/// re-words a finding must still land on the same row — the content
/// fingerprint is what says so. Under `Fingerprint`, an EXPLICIT slug still
/// wins (an author who names a slug means that row), and a slug is never
/// reused for a different finding, not even after a tombstone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingIdentity {
    Slug,
    Fingerprint,
}

/// Outcome of [`Store::reconcile_findings_import`] — the four slug buckets
/// `POST /api/reviews/{id}/findings/import` (a later phase) reports
/// verbatim in its response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindingsImportOutcome {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub superseded: Vec<String>,
    pub unchanged: Vec<String>,
}

/// The `annotations` anchor fields [`derive_finding_anchor`] produces for
/// one finding location — ready to drop into [`NewReviewFinding`]/
/// [`ImportedFinding`]'s `anchor_kind`/`anchor`/`anchor2`/`side`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedFindingAnchor {
    pub anchor_kind: String,
    pub anchor: String,
    pub anchor2: Option<String>,
    pub side: Option<String>,
}

pub const SEVERITY_BLOCKER: &str = "blocker";
pub const SEVERITY_CONCERN: &str = "concern";
pub const SEVERITY_OK: &str = "ok";
/// The full `review_findings.severity` vocabulary — arbitrated by the
/// milestone plan (the generator's real 3-set), overriding the design
/// doc's original nit/praise/info sketch (deferred to a future
/// `kbc-findings/2`).
pub const SEVERITIES: [&str; 3] = [SEVERITY_BLOCKER, SEVERITY_CONCERN, SEVERITY_OK];

pub fn is_valid_severity(s: &str) -> bool {
    SEVERITIES.contains(&s)
}

pub const DISPOSITION_AGREE: &str = "agree";
pub const DISPOSITION_DISPUTE: &str = "dispute";
pub const DISPOSITION_WAIVE: &str = "waive";
pub const DISPOSITION_FIX_LATER: &str = "fix-later";
/// The full `review_findings.disposition` vocabulary — arbitrated by the
/// milestone plan.
pub const DISPOSITIONS: [&str; 4] = [
    DISPOSITION_AGREE,
    DISPOSITION_DISPUTE,
    DISPOSITION_WAIVE,
    DISPOSITION_FIX_LATER,
];

pub fn is_valid_disposition(s: &str) -> bool {
    DISPOSITIONS.contains(&s)
}

pub const LOCATION_KIND_SINGLE: &str = "single";
pub const LOCATION_KIND_RANGE: &str = "range";
pub const LOCATION_KIND_MULTI: &str = "multi";
pub const LOCATION_KIND_WHOLE_FILE: &str = "whole_file";
/// The full `review_findings.location_kind` vocabulary — design doc §1.4.
pub const LOCATION_KINDS: [&str; 4] = [
    LOCATION_KIND_SINGLE,
    LOCATION_KIND_RANGE,
    LOCATION_KIND_MULTI,
    LOCATION_KIND_WHOLE_FILE,
];

pub fn is_valid_location_kind(s: &str) -> bool {
    LOCATION_KINDS.contains(&s)
}

pub const FINDING_ORIGIN_IMPORT: &str = "import";
pub const FINDING_ORIGIN_MANUAL: &str = "manual";
/// PRR-R1 scope extension (operator-ratified mid-build) — the full
/// `review_findings.origin` vocabulary. "import" = created via
/// `findings/import` (the generator agent's batch route); "manual" =
/// created via a later phase's single-finding create route, authored
/// directly by a human in the browser.
pub const FINDING_ORIGINS: [&str; 2] = [FINDING_ORIGIN_IMPORT, FINDING_ORIGIN_MANUAL];

pub fn is_valid_finding_origin(s: &str) -> bool {
    FINDING_ORIGINS.contains(&s)
}

/// PRR-R1 scope extension — `findings/import`'s (`kbc-findings/1`, a later
/// phase) optional top-level `mode` field. [`Full`](FindingsImportMode::Full)
/// is today's reconciling semantics: any `origin = "import"` row absent
/// from this batch is soft-superseded (the §4.3 rule); a `manual` row is
/// NEVER touched by this step regardless of mode.
/// [`Additive`](FindingsImportMode::Additive) creates/updates only the
/// slugs present in `findings` and supersedes nothing at all — for a
/// later phase's "add more findings without touching what's already
/// there" case. Default (when the payload omits `mode`) is `Full`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingsImportMode {
    Full,
    Additive,
}

/// Encode a finding's cited line numbers as the `location_lines` JSON
/// column's canonical text — a bare helper so every caller (this module's
/// own tests, and a later phase's import route) produces byte-identical
/// JSON for the same numbers rather than each hand-rolling
/// `serde_json::to_string`.
pub fn location_lines_json(lines: &[i64]) -> String {
    serde_json::to_string(lines).expect("Vec<i64> always serializes")
}

/// The §1.4 location-kind ladder: derive the `annotations` anchor a
/// finding's structured `location` produces, ready to hand to
/// [`NewReviewFinding`]/[`ImportedFinding`]. PURE — no git I/O:
/// `line_text` is a caller-supplied lookup (a later phase's import route
/// resolves it from the TARGET patchset's pinned git blob; here it is just
/// a function argument so this ladder is unit-testable without a repo).
///
/// | `kind` | `lines` | anchor written |
/// |---|---|---|
/// | `single` | `[N]` | `anchor_kind="line"`, one `Selection` at N |
/// | `range` | `[start,end]` | `anchor_kind="range"`, `Selection`s at start (`anchor`) / end (`anchor2`) |
/// | `multi` | `[a,b,c,...]` | `anchor_kind="line"` at the FIRST line only — documented approximation (design doc §1.4) |
/// | `whole_file` | (none) | `anchor_kind="whole_file"`, `anchor` = the bare `path` (NOT JSON), `anchor2=None` |
///
/// `removed` forces `side = Some("old")` regardless of kind (a deleted
/// line/file only ever existed pre-diff); otherwise `side = Some("new")` —
/// always an explicit string, matching `routes::resolve_review_create_
/// scope`'s own convention (never a bare `None` default).
///
/// Errors are plain `String`s (this is a pure validation/construction
/// helper, not a `Store` method — it never touches sqlite, so it has no
/// business returning a `store::Result`/`StoreError`); a later phase's
/// route boundary turns an `Err` into its own 400.
pub fn derive_finding_anchor(
    kind: &str,
    path: &str,
    lines: Option<&[i64]>,
    removed: bool,
    mut line_text: impl FnMut(i64) -> String,
) -> std::result::Result<DerivedFindingAnchor, String> {
    let side = Some(if removed { "old" } else { "new" }.to_string());
    match kind {
        LOCATION_KIND_WHOLE_FILE => Ok(DerivedFindingAnchor {
            anchor_kind: crate::annotations::ANCHOR_KIND_WHOLE_FILE.to_string(),
            anchor: path.to_string(),
            anchor2: None,
            side,
        }),
        LOCATION_KIND_SINGLE => {
            let n = match lines {
                Some([n]) => *n,
                _ => return Err("single location requires exactly one line".to_string()),
            };
            let anchor = crate::annotations::anchor_for_line(line_u32(n)?, &line_text(n));
            Ok(DerivedFindingAnchor {
                anchor_kind: crate::annotations::ANCHOR_KIND_LINE.to_string(),
                anchor: serde_json::to_string(&anchor).map_err(|e| e.to_string())?,
                anchor2: None,
                side,
            })
        }
        LOCATION_KIND_MULTI => {
            let n = match lines {
                Some(ls) if !ls.is_empty() => ls[0],
                _ => return Err("multi location requires at least one line".to_string()),
            };
            let anchor = crate::annotations::anchor_for_line(line_u32(n)?, &line_text(n));
            Ok(DerivedFindingAnchor {
                anchor_kind: crate::annotations::ANCHOR_KIND_LINE.to_string(),
                anchor: serde_json::to_string(&anchor).map_err(|e| e.to_string())?,
                anchor2: None,
                side,
            })
        }
        LOCATION_KIND_RANGE => {
            let (start, end) = match lines {
                Some([s, e]) => (*s, *e),
                _ => {
                    return Err("range location requires exactly two lines [start, end]".to_string())
                }
            };
            let start_anchor =
                crate::annotations::anchor_for_line(line_u32(start)?, &line_text(start));
            let end_anchor = crate::annotations::anchor_for_line(line_u32(end)?, &line_text(end));
            Ok(DerivedFindingAnchor {
                anchor_kind: crate::annotations::ANCHOR_KIND_RANGE.to_string(),
                anchor: serde_json::to_string(&start_anchor).map_err(|e| e.to_string())?,
                anchor2: Some(serde_json::to_string(&end_anchor).map_err(|e| e.to_string())?),
                side,
            })
        }
        other => Err(format!("unknown location_kind: {other:?}")),
    }
}

fn line_u32(n: i64) -> std::result::Result<u32, String> {
    u32::try_from(n).map_err(|_| format!("line number out of range: {n}"))
}

/// Reads the 31-column order every `review_findings` SELECT below uses
/// (PRR-R1 scope extension added `origin`/`author` — was 29).
fn review_finding_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewFindingRow> {
    Ok(ReviewFindingRow {
        id: r.get(0)?,
        review_id: r.get(1)?,
        annotation_id: r.get(2)?,
        slug: r.get(3)?,
        severity: r.get(4)?,
        category: r.get(5)?,
        location_kind: r.get(6)?,
        location_path: r.get(7)?,
        location_lines: r.get(8)?,
        location_removed: r.get::<_, i64>(9)? != 0,
        title: r.get(10)?,
        rationale: r.get(11)?,
        recommendation: r.get(12)?,
        evidence_lang: r.get(13)?,
        evidence_source: r.get(14)?,
        origin: r.get(15)?,
        author: r.get(16)?,
        disposition: r.get(17)?,
        disposition_note: r.get(18)?,
        disposition_by: r.get(19)?,
        disposition_at: r.get(20)?,
        content_updated_at: r.get(21)?,
        published_state: r.get(22)?,
        published_at: r.get(23)?,
        published_url: r.get(24)?,
        superseded: r.get::<_, i64>(25)? != 0,
        superseded_at: r.get(26)?,
        superseded_reason: r.get(27)?,
        import_batch_id: r.get(28)?,
        created_at: r.get(29)?,
        updated_at: r.get(30)?,
        act: r.get(31)?,
        blocking: r.get::<_, i64>(32)? != 0,
        cites_json: r.get(33)?,
        fingerprint: r.get(34)?,
        superseded_by: r.get(35)?,
    })
}

const REVIEW_FINDING_COLUMNS: &str = "id, review_id, annotation_id, slug, severity, category,
    location_kind, location_path, location_lines, location_removed,
    title, rationale, recommendation, evidence_lang, evidence_source,
    origin, author,
    disposition, disposition_note, disposition_by, disposition_at,
    content_updated_at, published_state, published_at, published_url,
    superseded, superseded_at, superseded_reason, import_batch_id,
    created_at, updated_at,
    act, blocking, cites_json, fingerprint, superseded_by";

/// Insert one finding's `annotations` row AND its `review_findings`
/// sibling, in that order, on an ALREADY-OPEN transaction — shared by
/// [`Store::insert_review_finding`] (single create) and
/// [`Store::reconcile_findings_import`]'s "new slug" branch (batch create),
/// same "public method + tx-scoped `_on` twin, both reused by a batch
/// caller" shape `insert_annotation`/`insert_annotation_on` already
/// establish. Returns `(annotation_id, review_findings.id)`.
fn insert_review_finding_on(
    tx: &Transaction<'_>,
    f: &NewReviewFinding,
    now: i64,
) -> Result<(String, i64)> {
    let annotation_id = crate::annotations::new_annotation_id();
    let ann_row = AnnotationRow {
        id: annotation_id.clone(),
        repo_id: f.repo_id,
        path: f.location_path.clone(),
        anchor: Some(f.anchor.clone()),
        anchor_kind: f.anchor_kind.clone(),
        anchor2: f.anchor2.clone(),
        parent_id: None,
        intent: crate::annotations::INTENT_FINDING.to_string(),
        body: f.title.clone(),
        author: f.author.clone(),
        created_at: now,
        updated_at: now,
        resolved: false,
        review_id: Some(f.review_id),
        ps_number: Some(f.ps_number),
        side: f.side.clone(),
        set_id: None,
        trail_id: None,
    };
    insert_annotation_on(tx, &ann_row)?;
    tx.execute(
        "INSERT INTO review_findings
            (review_id, annotation_id, slug, severity, category,
             location_kind, location_path, location_lines, location_removed,
             title, rationale, recommendation, evidence_lang, evidence_source,
             origin, author,
             disposition, disposition_note, disposition_by, disposition_at,
             content_updated_at, published_state, published_at, published_url,
             superseded, superseded_at, superseded_reason, import_batch_id,
             created_at, updated_at,
             act, blocking, cites_json, fingerprint, superseded_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16,
                 NULL, NULL, NULL, NULL,
                 NULL, 'unpublished', NULL, NULL,
                 0, NULL, NULL, ?17, ?18, ?19,
                 ?20, ?21, ?22, ?23, NULL)",
        params![
            f.review_id,
            annotation_id,
            f.slug,
            f.severity,
            f.category,
            f.location_kind,
            f.location_path,
            f.location_lines,
            f.location_removed as i64,
            f.title,
            f.rationale,
            f.recommendation,
            f.evidence_lang,
            f.evidence_source,
            f.origin,
            f.finding_author,
            f.import_batch_id,
            now,
            now,
            f.act,
            f.blocking as i64,
            f.cites_json,
            f.fingerprint,
        ],
    )?;
    let finding_id = tx.last_insert_rowid();
    Ok((annotation_id, finding_id))
}

/// V80-M5 — maps an `annotation_id` UNIQUE-constraint violation (on
/// `review_findings.annotation_id`) to [`StoreError::AnnotationAlreadyFinding`];
/// any OTHER sqlite error (including a DIFFERENT constraint on the same
/// INSERT, e.g. a `(review_id, slug)` collision) passes through as
/// [`StoreError::Sqlite`] unchanged — same "catch the specific constraint
/// at the sqlite layer" shape as [`name_conflict_or`], one column over.
/// Sqlite's own constraint-violation message names the failing
/// `table.column` (`"UNIQUE constraint failed: review_findings.annotation_id"`),
/// which is the only way to tell the two constraints apart from the error
/// alone — the caller has already validated slug availability before this
/// INSERT runs, so a slug collision here should be rare, but it must never
/// be misreported as an annotation conflict.
fn annotation_finding_conflict_or(e: rusqlite::Error, annotation_id: &str) -> StoreError {
    if e.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation)
        && e.to_string().contains("annotation_id")
    {
        StoreError::AnnotationAlreadyFinding(annotation_id.to_string())
    } else {
        StoreError::Sqlite(e)
    }
}

/// V80-M5 — the ADOPTION twin of [`insert_review_finding_on`]: writes only
/// the `review_findings` row, reusing `f.annotation_id` verbatim rather than
/// minting a new `annotations` row. No transaction of its own (a single
/// `INSERT` is already atomic) — takes `&Connection` rather than
/// `&Transaction<'_>` so [`Store::insert_review_finding_adopting`] can hand
/// it the locked connection directly.
fn insert_review_finding_adopting_on(
    conn: &Connection,
    f: &AdoptedReviewFinding,
    now: i64,
) -> Result<i64> {
    let result = conn.execute(
        "INSERT INTO review_findings
            (review_id, annotation_id, slug, severity, category,
             location_kind, location_path, location_lines, location_removed,
             title, rationale, recommendation, evidence_lang, evidence_source,
             origin, author,
             disposition, disposition_note, disposition_by, disposition_at,
             content_updated_at, published_state, published_at, published_url,
             superseded, superseded_at, superseded_reason, import_batch_id,
             created_at, updated_at,
             act, blocking, cites_json, fingerprint, superseded_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16,
                 NULL, NULL, NULL, NULL,
                 NULL, 'unpublished', NULL, NULL,
                 0, NULL, NULL, ?17, ?18, ?19,
                 ?20, ?21, NULL, NULL, NULL)",
        params![
            f.review_id,
            f.annotation_id,
            f.slug,
            f.severity,
            f.category,
            f.location_kind,
            f.location_path,
            f.location_lines,
            f.location_removed as i64,
            f.title,
            f.rationale,
            f.recommendation,
            f.evidence_lang,
            f.evidence_source,
            FINDING_ORIGIN_MANUAL,
            f.finding_author,
            f.import_batch_id,
            now,
            now,
            f.act,
            f.blocking as i64,
        ],
    );
    match result {
        Ok(_) => Ok(conn.last_insert_rowid()),
        Err(e) => Err(annotation_finding_conflict_or(e, &f.annotation_id)),
    }
}
/// [`Store::compose_review`]'s result — one field per write the
/// transaction performed.
#[derive(Debug, Clone)]
pub struct ComposeOutcome {
    pub findings: FindingsImportOutcome,
    pub report_set: bool,
    pub verdict_changed: bool,
}

/// The design doc §4.3 reconciliation core's transaction-scoped body,
/// shared by [`Store::reconcile_findings_import`] (self-locking, its own
/// commit) and [`Store::compose_review`] (one shared transaction with the
/// report + verdict writes) — see [`Store::reconcile_findings_import`]'s
/// own doc for the reconciliation rules; this free fn changes nothing
/// about them, only where the transaction boundary lives.
#[allow(clippy::too_many_arguments)]
fn reconcile_findings_import_on(
    tx: &Transaction<'_>,
    review_id: i64,
    repo_id: i64,
    ps_number: i64,
    import_batch_id: &str,
    author: &str,
    findings: &[ImportedFinding],
    mode: FindingsImportMode,
    identity: FindingIdentity,
    now: i64,
) -> Result<FindingsImportOutcome> {
    let existing_rows: Vec<ReviewFindingRow> = {
        let mut stmt = tx.prepare(&format!(
            "SELECT {REVIEW_FINDING_COLUMNS} FROM review_findings WHERE review_id = ?1"
        ))?;
        let rows = stmt
            .query_map(params![review_id], review_finding_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    let existing: HashMap<String, ReviewFindingRow> = existing_rows
        .into_iter()
        .map(|r| (r.slug.clone(), r))
        .collect();

    let mut outcome = FindingsImportOutcome::default();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // V73-K1 — under `FindingIdentity::Fingerprint` an incoming finding may
    // match an EXISTING row by content even though it carries no slug of its
    // own; `resolve_identity` is the only place that decision is made, and it
    // returns the slug to write, minting a fresh one from the ledger when
    // nothing matched. Under `Slug` it is the identity function.
    let mut by_fingerprint: HashMap<&str, &ReviewFindingRow> = HashMap::new();
    if matches!(identity, FindingIdentity::Fingerprint) {
        // Prefer a LIVE row over a tombstoned one carrying the same
        // fingerprint: re-composing a finding that was dropped and is back
        // must revive the row a human may already have disposed of, and if
        // both exist the live one is the one they are looking at.
        for row in existing.values() {
            let Some(fp) = row.fingerprint.as_deref() else {
                continue;
            };
            match by_fingerprint.get(fp) {
                Some(prev) if !prev.superseded => {}
                _ => {
                    by_fingerprint.insert(fp, row);
                }
            }
        }
    }
    let mut supersede_declarations: HashMap<String, String> = HashMap::new();

    for f in findings {
        let slug = match identity {
            FindingIdentity::Slug => f.slug.clone(),
            FindingIdentity::Fingerprint => {
                if !f.slug.is_empty() {
                    record_finding_slug_on(tx, review_id, &f.slug, now)?;
                    f.slug.clone()
                } else if let Some(hit) = f
                    .fingerprint
                    .as_deref()
                    .and_then(|fp| by_fingerprint.get(fp))
                {
                    hit.slug.clone()
                } else {
                    mint_finding_slug_on(tx, review_id, &existing, now)?
                }
            }
        };
        for replaced in &f.supersedes {
            supersede_declarations.insert(replaced.clone(), slug.clone());
        }
        seen.insert(slug.clone());
        let f = &ImportedFinding {
            slug: slug.clone(),
            ..f.clone()
        };
        match existing.get(&slug) {
            None => {
                let new_row = NewReviewFinding {
                    review_id,
                    repo_id,
                    ps_number,
                    slug: f.slug.clone(),
                    severity: f.severity.clone(),
                    category: f.category.clone(),
                    location_kind: f.location_kind.clone(),
                    location_path: f.location_path.clone(),
                    location_lines: f.location_lines.clone(),
                    location_removed: f.location_removed,
                    title: f.title.clone(),
                    rationale: f.rationale.clone(),
                    recommendation: f.recommendation.clone(),
                    evidence_lang: f.evidence_lang.clone(),
                    evidence_source: f.evidence_source.clone(),
                    anchor_kind: f.anchor_kind.clone(),
                    anchor: f.anchor.clone(),
                    anchor2: f.anchor2.clone(),
                    side: f.side.clone(),
                    author: author.to_string(),
                    import_batch_id: import_batch_id.to_string(),
                    origin: FINDING_ORIGIN_IMPORT.to_string(),
                    finding_author: None,
                    act: f.act.clone(),
                    blocking: f.blocking,
                    cites_json: f.cites_json.clone(),
                    fingerprint: f.fingerprint.clone(),
                };
                insert_review_finding_on(tx, &new_row, now)?;
                record_finding_slug_on(tx, review_id, &new_row.slug, now)?;
                outcome.created.push(f.slug.clone());
            }
            Some(cur) => {
                // PRR-R3 defense-in-depth (the OWED item flagged by
                // R1): a slug collision with an existing MANUAL
                // (human-authored) finding must never be refreshed by
                // an import, in ANY mode. The route boundary
                // (`review_findings::import_findings`) already rejects
                // such a batch WHOLESALE (400, per-index
                // `slug_conflict_manual`) before this function is ever
                // called — this guard makes the invariant true at the
                // data layer too, so a future caller that skips that
                // gate still cannot silently overwrite a human's
                // finding. Reported "unchanged": nothing is written,
                // which is the literal truth.
                if cur.origin == FINDING_ORIGIN_MANUAL {
                    outcome.unchanged.push(f.slug.clone());
                    continue;
                }
                let content_changed = cur.severity != f.severity
                    || cur.category != f.category
                    || cur.location_kind != f.location_kind
                    || cur.location_path != f.location_path
                    || cur.location_lines != f.location_lines
                    || cur.location_removed != f.location_removed
                    || cur.title != f.title
                    || cur.rationale != f.rationale
                    || cur.recommendation != f.recommendation
                    || cur.evidence_lang != f.evidence_lang
                    || cur.evidence_source != f.evidence_source
                    || cur.act != f.act
                    || cur.blocking != f.blocking
                    || cur.cites_json != f.cites_json
                    // A fingerprint the caller did not compute (the v1 twin)
                    // never counts as a change — otherwise every v1 re-import
                    // of an untouched v2 finding would clear its fingerprint
                    // and orphan it from the next compose.
                    || (f.fingerprint.is_some() && cur.fingerprint != f.fingerprint);
                if content_changed || cur.superseded {
                    tx.execute(
                        "UPDATE review_findings SET
                            severity = ?2, category = ?3, location_kind = ?4, location_path = ?5,
                            location_lines = ?6, location_removed = ?7, title = ?8, rationale = ?9,
                            recommendation = ?10, evidence_lang = ?11, evidence_source = ?12,
                            content_updated_at = ?13, superseded = 0, superseded_at = NULL,
                            superseded_reason = NULL, superseded_by = NULL, updated_at = ?14,
                            act = ?15, blocking = ?16, cites_json = ?17,
                            fingerprint = COALESCE(?18, fingerprint)
                         WHERE id = ?1",
                        params![
                            cur.id,
                            f.severity,
                            f.category,
                            f.location_kind,
                            f.location_path,
                            f.location_lines,
                            f.location_removed as i64,
                            f.title,
                            f.rationale,
                            f.recommendation,
                            f.evidence_lang,
                            f.evidence_source,
                            now,
                            now,
                            f.act,
                            f.blocking as i64,
                            f.cites_json,
                            f.fingerprint,
                        ],
                    )?;
                    // Refresh the linked annotation's display body
                    // (title) too — NEVER its anchor/resolved/intent
                    // (design doc §4.3: "the annotation's own anchor is
                    // NOT eagerly rewritten").
                    update_annotation_on(tx, &cur.annotation_id, Some(&f.title), None, None, now)?;
                    outcome.updated.push(f.slug.clone());
                } else {
                    outcome.unchanged.push(f.slug.clone());
                }
            }
        }
    }

    if matches!(mode, FindingsImportMode::Full) {
        for (slug, row) in &existing {
            if !seen.contains(slug) && !row.superseded && row.origin == FINDING_ORIGIN_IMPORT {
                // `superseded_by` is written ONLY when an incoming finding
                // declared `supersedes: [this slug]`. Never inferred — see
                // migration V0034's own comment on why guessing which new
                // finding "is really" an old one is the wrong-exact class.
                let by = supersede_declarations.get(slug);
                tx.execute(
                    "UPDATE review_findings
                     SET superseded = 1, superseded_at = ?2,
                         superseded_reason = ?3, superseded_by = ?4, updated_at = ?2
                     WHERE id = ?1",
                    params![
                        row.id,
                        now,
                        if by.is_some() {
                            SUPERSEDED_REASON_REPLACED
                        } else {
                            SUPERSEDED_REASON_NOT_IN_REIMPORT
                        },
                        by,
                    ],
                )?;
                outcome.superseded.push(slug.clone());
            }
        }
    }

    Ok(outcome)
}

// ── V73-K1: kbc-review/1 — the review document, the slug ledger ─────────

/// `review_findings.superseded_reason` — the two values a compose/import
/// tombstone can carry. `not_in_reimport` is V0024's original (and still the
/// default); `replaced` is written only when an incoming finding DECLARED
/// `supersedes: [<slug>]`, alongside `superseded_by`.
pub const SUPERSEDED_REASON_NOT_IN_REIMPORT: &str = "not_in_reimport";
pub const SUPERSEDED_REASON_REPLACED: &str = "replaced";

/// Record `slug` as TAKEN on this review, forever. `INSERT OR IGNORE` — a
/// slug already in the ledger stays with its original `minted_at`.
///
/// The `ordinal` column is the `<n>` of an `f-<n>` slug and is what
/// [`mint_finding_slug_on`] counts from; an author-supplied non-numeric slug
/// (`f-dedup-race`) is recorded with a NULL ordinal: still taken, just not
/// part of the counter.
fn record_finding_slug_on(
    tx: &Transaction<'_>,
    review_id: i64,
    slug: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO review_finding_slugs (review_id, slug, ordinal, minted_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![review_id, slug, slug_ordinal(slug), now],
    )?;
    Ok(())
}

/// The `<n>` of an `f-<n>` slug, or `None` for any other shape.
pub fn slug_ordinal(slug: &str) -> Option<i64> {
    slug.strip_prefix("f-")?.parse::<i64>().ok()
}

/// Mint the next never-before-used `f-<n>` slug for this review, and record
/// it in the ledger.
///
/// The counter is `1 + max(ledger ordinals, ordinals of slugs already on
/// `review_findings`)`. Reading BOTH is what makes the rule true for a
/// review that predates V0034 (its `f-1`/`f-2` slugs exist as rows but have
/// no ledger entry yet) as well as for one whose highest-numbered finding a
/// human deleted out from under the ledger. Both sources are monotonic and
/// neither is ever pruned, so the counter cannot walk backwards — D9's
/// "minted once per review and NEVER reused."
fn mint_finding_slug_on(
    tx: &Transaction<'_>,
    review_id: i64,
    existing: &HashMap<String, ReviewFindingRow>,
    now: i64,
) -> Result<String> {
    let ledger_max: i64 = tx.query_row(
        "SELECT COALESCE(MAX(ordinal), 0) FROM review_finding_slugs WHERE review_id = ?1",
        params![review_id],
        |r| r.get(0),
    )?;
    let rows_max = existing
        .keys()
        .filter_map(|slug| slug_ordinal(slug))
        .max()
        .unwrap_or(0);
    let next = ledger_max.max(rows_max) + 1;
    let slug = format!("f-{next}");
    record_finding_slug_on(tx, review_id, &slug, now)?;
    Ok(slug)
}

/// One `review_docs` revision, as read back. `doc_md` is the WHOLE
/// document (front matter + body) byte-for-byte as it was composed — the
/// lossless record; every other column is a denormalised copy of a parsed
/// front-matter field and the document itself wins on a disagreement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDocRow {
    pub id: i64,
    pub review_id: i64,
    pub ps_number: i64,
    pub revision: i64,
    pub schema: String,
    pub tier: String,
    pub doc_md: String,
    pub summary_md: String,
    pub risk_level: Option<String>,
    pub risk_why: Option<String>,
    /// Raw JSON array (same "row types don't parse other modules' JSON"
    /// convention `AnnotationRow`/`ReviewFindingRow` already follow).
    pub omitted_json: String,
    pub author_json: Option<String>,
    pub byte_len: i64,
    pub created_at: i64,
}

/// A revision ready to append. `revision` is assigned by the store (the
/// caller never picks one), so two concurrent composes cannot both claim
/// the same number — the UNIQUE index would reject the second anyway, and
/// this way it never gets that far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReviewDoc {
    pub review_id: i64,
    pub ps_number: i64,
    pub schema: String,
    pub tier: String,
    pub doc_md: String,
    pub summary_md: String,
    pub risk_level: Option<String>,
    pub risk_why: Option<String>,
    pub omitted_json: String,
    pub author_json: Option<String>,
}

const REVIEW_DOC_COLUMNS: &str = "id, review_id, ps_number, revision, schema, tier, doc_md,
    summary_md, risk_level, risk_why, omitted_json, author_json, byte_len, created_at";

fn review_doc_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewDocRow> {
    Ok(ReviewDocRow {
        id: r.get(0)?,
        review_id: r.get(1)?,
        ps_number: r.get(2)?,
        revision: r.get(3)?,
        schema: r.get(4)?,
        tier: r.get(5)?,
        doc_md: r.get(6)?,
        summary_md: r.get(7)?,
        risk_level: r.get(8)?,
        risk_why: r.get(9)?,
        omitted_json: r.get(10)?,
        author_json: r.get(11)?,
        byte_len: r.get(12)?,
        created_at: r.get(13)?,
    })
}

/// Append one revision on an already-open transaction. Returns the
/// revision number it was given.
fn insert_review_doc_on(tx: &Transaction<'_>, d: &NewReviewDoc, now: i64) -> Result<i64> {
    let prev: i64 = tx.query_row(
        "SELECT COALESCE(MAX(revision), 0) FROM review_docs
         WHERE review_id = ?1 AND ps_number = ?2",
        params![d.review_id, d.ps_number],
        |r| r.get(0),
    )?;
    let revision = prev + 1;
    tx.execute(
        "INSERT INTO review_docs
            (review_id, ps_number, revision, schema, tier, doc_md, summary_md,
             risk_level, risk_why, omitted_json, author_json, byte_len, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            d.review_id,
            d.ps_number,
            revision,
            d.schema,
            d.tier,
            d.doc_md,
            d.summary_md,
            d.risk_level,
            d.risk_why,
            d.omitted_json,
            d.author_json,
            d.doc_md.len() as i64,
            now,
        ],
    )?;
    Ok(revision)
}

/// [`Store::compose_review_doc`]'s result — one field per write the
/// transaction performed.
#[derive(Debug, Clone)]
pub struct ComposeDocOutcome {
    pub findings: FindingsImportOutcome,
    pub revision: i64,
    pub report_set: bool,
    pub verdict_changed: bool,
}
// ── PRR-N12: scip runs ──────────────────────────────────────────────────

/// PRR-N12 (N1) — one `scip_runs` row: the outcome of one successful
/// `POST /api/scip/ingest` call, stamped by `crate::scip::scip_ingest_route`
/// (see migration V0025's doc). `head_sha` is the repo's git HEAD AT THE
/// MOMENT of that ingest call — compared against the repo's CURRENT HEAD by
/// `routes::repos`'s `ScipStatus::fresh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScipRunRow {
    pub head_sha: String,
    pub ingested_at: i64,
    pub docs_accepted: i64,
}
// ── end PRR-N12 ──────────────────────────────────────────────────────────

// ── PRR-R9: review disposition analytics ────────────────────────────────

/// One `review_findings` row, joined to its owning review's `repo` —
/// [`Store::list_findings_for_analytics`]'s data source. Deliberately
/// narrower than [`ReviewFindingRow`] (only the columns `review_analytics`'s
/// pure `compute_analytics` needs) so that fn's fixture rows stay small and
/// the join query only pulls what it uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsFindingRow {
    pub review_id: i64,
    /// "blocker" | "concern" | "ok".
    pub severity: String,
    pub category: String,
    pub location_path: String,
    /// "agree" | "dispute" | "waive" | "fix-later" | `None` (undecided).
    pub disposition: Option<String>,
    pub disposition_at: Option<i64>,
    /// "unpublished" | "published".
    pub published_state: String,
    pub superseded: bool,
    pub created_at: i64,
}

fn analytics_finding_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<AnalyticsFindingRow> {
    Ok(AnalyticsFindingRow {
        review_id: r.get(0)?,
        severity: r.get(1)?,
        category: r.get(2)?,
        location_path: r.get(3)?,
        disposition: r.get(4)?,
        disposition_at: r.get(5)?,
        published_state: r.get(6)?,
        superseded: r.get::<_, i64>(7)? != 0,
        created_at: r.get(8)?,
    })
}

/// `(category, location_path)` seen across at least [`RECURRENCE_MIN_
/// REVIEWS`] DISTINCT reviews — [`Store::recurrence_pairs`]'s row shape.
/// `review_ids` is always sorted ascending (SQLite's `GROUP_CONCAT(DISTINCT
/// …)` element order is unspecified — sorting here is what makes repeated
/// calls over unchanged state byte-identical, same "the query result isn't
/// naturally deterministic, so the store layer imposes an order" precedent
/// as `sort_inbox_rows`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurrenceRow {
    pub category: String,
    pub location_path: String,
    pub review_count: i64,
    pub finding_count: i64,
    pub review_ids: Vec<i64>,
}

/// The addendum-2 §C default — a pair recurs once it has landed in two or
/// more distinct reviews. Exposed as a named constant (not a magic `2`
/// inline) since both `review_analytics::analytics_route` and a later
/// frontier chip (design-addendum-2 §C's own note: "this is also the
/// frontier recurring-finding query") share this threshold.
pub const RECURRENCE_MIN_REVIEWS: i64 = 2;
// ── end PRR-R9 ───────────────────────────────────────────────────────────

/// PRR-N3 — column order matches every `rails_edges` SELECT above: `kind,
/// src_path, src_line, src_symbol, dst_kind, dst_path, dst_symbol, trust,
/// extra_json`. `kind`/`trust` are CHECK-constrained at the schema level
/// (migration `V0026__rails_edges.sql`) and always written via
/// `EdgeKind::as_str()`/`Trust::as_str()`, so a decode failure here can only
/// mean the row was written by a future/foreign writer — surfaced as a
/// real `rusqlite::Error` (never silently coerced to a default variant).
fn rails_edge_row_from(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<crate::frameworks::FrameworkEdge> {
    use crate::frameworks::{EdgeKind, Trust};

    let kind_str: String = r.get(0)?;
    let kind = EdgeKind::from_str_opt(&kind_str).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(0, "kind".to_string(), rusqlite::types::Type::Text)
    })?;
    let trust_str: String = r.get(7)?;
    let trust = Trust::from_str_opt(&trust_str).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(7, "trust".to_string(), rusqlite::types::Type::Text)
    })?;
    let src_line: Option<i64> = r.get(2)?;

    Ok(crate::frameworks::FrameworkEdge {
        kind,
        src_path: r.get(1)?,
        src_line: src_line.map(|n| n as u32),
        src_symbol: r.get(3)?,
        dst_kind: r.get(4)?,
        dst_path: r.get(5)?,
        dst_symbol: r.get(6)?,
        trust,
        extra_json: r.get(8)?,
    })
}

// --- aug-lane/1 fact store (V72-H4a) -------------------------------------

/// One `lane_runs` row, as the ingest route (or a derived lane) writes it.
#[derive(Debug, Clone)]
pub struct LaneRunIn {
    pub run_id: String,
    pub lane: String,
    pub repo_id: i64,
    pub tool: String,
    pub tool_version: Option<String>,
    pub argv_redacted: Option<String>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// `"cli"` | `"daemon"` — the migration's CHECK constraint owns the
    /// vocabulary; a bad value fails the INSERT loudly rather than
    /// silently mislabelling provenance.
    pub origin: &'static str,
    pub ingested_at: i64,
}

/// One `lane_facts` row on the way in. `sha_source` is `"tool"` or
/// `"mirror_at_ingest"` (see the migration header for why that distinction
/// is what makes `exact` reachable at all).
#[derive(Debug, Clone)]
pub struct LaneFactIn {
    pub path: String,
    pub blob_sha: String,
    pub sha_source: &'static str,
    pub range_start: Option<u32>,
    pub range_end: Option<u32>,
    pub snippet: Option<String>,
    pub kind: String,
    pub value_json: String,
    pub severity: Option<String>,
    pub produced_at: i64,
}

/// One stored fact, joined to its run's provenance. Deliberately carries
/// NO trust class — that is computed per request by
/// `lanes::classing::class_for` and is never persisted.
#[derive(Debug, Clone)]
pub struct LaneFactRow {
    pub id: i64,
    pub lane: String,
    pub path: String,
    pub blob_sha: String,
    pub sha_source: String,
    pub range_start: Option<u32>,
    pub range_end: Option<u32>,
    pub snippet: Option<String>,
    pub kind: String,
    pub value_json: String,
    pub severity: Option<String>,
    pub produced_at: i64,
    pub ingested_at: i64,
    pub run_id: String,
    pub tool: String,
    pub tool_version: Option<String>,
    pub origin: String,
}

/// Per-lane rollup for `GET /api/lanes`.
#[derive(Debug, Clone, Default)]
pub struct LaneStatRow {
    pub lane: String,
    pub facts: i64,
    pub runs: i64,
    pub last_ingest_at: Option<i64>,
}

/// One `(lane, kind, severity)` bucket for `GET /api/lanes/summary`.
#[derive(Debug, Clone)]
pub struct LaneSummaryRow {
    pub lane: String,
    pub kind: String,
    pub severity: Option<String>,
    pub count: i64,
}

/// Rows removed by one [`Store::sweep_lane_retention_page`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LaneGcCounts {
    pub runs: u64,
    pub facts: u64,
}

impl LaneGcCounts {
    pub fn is_empty(&self) -> bool {
        self.runs == 0 && self.facts == 0
    }
}

/// How many expired `lane_runs` one [`Store::sweep_lane_retention_page`]
/// covers. Small for the reason [`STALE_SALT_SWEEP_PAGE`] is small: this
/// is the unit of write-mutex hold time on an IO-bound host, and a run can
/// own thousands of facts (V72-B0 rule (b) — a background pass that takes
/// one long transaction has only moved the outage, not fixed it).
pub const LANE_GC_PAGE: usize = 32;

/// V74-L3b — how many expired `trails` one
/// [`Store::sweep_trail_retention_page`] covers. Same size and the same
/// reasoning as [`LANE_GC_PAGE`]: one trail can own thousands of steps
/// ([`crate::trails::MAX_STEPS_PER_TRAIL`]), and this number is the unit
/// of write-mutex hold time on an IO-bound host.
pub const TRAIL_GC_PAGE: usize = 32;

/// Hard bound on the rows `GET /api/lanes/summary` groups over. The route
/// STATES this number and whether it was hit — a summary that silently
/// capped would be a count nobody can trust.
pub const LANE_SUMMARY_SCAN_CAP: usize = 50_000;
/// V74-L3a — one `recipe_trust` row (migration V0038).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeTrustRow {
    pub repo_id: i64,
    pub slug: String,
    pub source_path: String,
    pub content_hash: String,
    pub trusted_body: String,
    pub trusted_unix: i64,
}

/// V74-L3a — one `recipes_server` row (migration V0038).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeServerRow {
    pub slug: String,
    pub repo: Option<String>,
    pub title: String,
    pub body_json: String,
    pub created_unix: i64,
    pub updated_unix: i64,
}

/// V74-L3a — one materialised `recipe_runs` row (migration V0038).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeRunRow {
    pub id: String,
    pub repo_id: i64,
    pub slug: String,
    pub params_json: String,
    pub scope: Option<String>,
    pub generation: u64,
    pub result_json: String,
    pub created_unix: i64,
}
// ── V75-M1: the Workspace re-key ─────────────────────────────────────
//
// D13's two identities (`crate::workspace`), the paged backfill that
// stamps them onto rows that predate the V0040 triggers
// (`crate::rekey`), and the ONE read that goes through the new key: the
// per-workspace derived-row census.
//
// A separate `impl Store` block, in the same module so it still reaches
// the private connection `lock()`, kept apart so a 16k-line file gains a
// section rather than an interleaving.

/// One `workspaces` row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkspaceRow {
    pub id: String,
    pub common_dir: String,
    pub root_commit: Option<String>,
    pub created_at: i64,
}

fn worktree_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<crate::workspace::WorktreeRow> {
    Ok(crate::workspace::WorktreeRow {
        workspace_id: r.get(0)?,
        id: r.get(1)?,
        path: r.get(2)?,
        branch: r.get(3)?,
        head_sha: r.get(4)?,
        is_main: r.get::<_, i64>(5)? != 0,
        bare: r.get::<_, i64>(6)? != 0,
        detached: r.get::<_, i64>(7)? != 0,
        locked: r.get::<_, i64>(8)? != 0,
        lock_reason: r.get(9)?,
        prunable: r.get::<_, i64>(10)? != 0,
        prunable_reason: r.get(11)?,
        mounted: r.get::<_, i64>(12)? != 0,
        path_resolution: r.get(13)?,
        repo: r.get(14)?,
        created_by_daemon: r.get::<_, i64>(15)? != 0,
    })
}

// ── RS-U1: review store + base model (V0045) ───────────────────────────
//
// Row types for `review_stores`/`repo_stores` (README §5.1/§5.5, D2/D3)
// and two small row types for the new `reviews`/`review_patchsets`
// base-model columns. Methods live in `store/review_stores.rs`; this file
// stays the type surface, the same split `WorkspaceRow` above follows.
//
// `ReviewRow`/`ReviewPatchsetRow` (defined earlier in this file) are
// DELIBERATELY left untouched: both are constructed as bare struct
// literals outside this module (`reviews.rs`, `review_timeline.rs`'s test
// fixtures), and widening them here would force every one of those call
// sites to learn six new fields for a feature they don't use yet.
// `ReviewBaseRow`/`PatchsetBaseFields` are read/written through their OWN
// narrow queries instead — `Store::get_review_base`/`Store::
// set_review_base`/`Store::insert_patchset_with_base` in
// `store/review_stores.rs` — so the unit that actually wires the base
// model into review creation/capture can widen the call sites it owns
// without this migration's own tests needing to track them.

/// One `review_stores` row (V0045 / RS-U1). See the migration's own header
/// (`migrations/V0045__review_store.sql`) for what each column means and
/// why `forge_verified`/`state` are CHECK-constrained while the rest are
/// route-validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewStoreRow {
    pub id: i64,
    pub uuid: String,
    pub store_key: String,
    pub git_dir: String,
    pub base_url: Option<String>,
    pub base_url_source: Option<String>,
    pub forge_kind: Option<String>,
    pub forge_host: Option<String>,
    pub forge_slug: Option<String>,
    pub forge_verified: String,
    pub cred_kind: String,
    pub cred_reason: Option<String>,
    pub cred_account: Option<String>,
    pub key_fingerprint: Option<String>,
    pub key_read_only: Option<String>,
    pub state: String,
    pub state_json: Option<String>,
    pub created_at: i64,
}

/// One `repo_stores` row (V0045 / RS-U1) — a single member's membership in
/// its store. `repo_id` is the PRIMARY KEY (a repo belongs to exactly one
/// store); many rows can share one `store_id` (D2's shared store, NOT
/// UNIQUE on purpose — see the migration header).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoStoreRow {
    pub repo_id: i64,
    pub store_id: i64,
    pub legacy_import_json: Option<String>,
    pub legacy_refs_state: String,
}

/// The `reviews` base-model columns (V0045 / RS-U1), read/written
/// separately from [`ReviewRow`] — see this section's own doc above for
/// why. `objects_state` rides alongside since it is set by the same
/// seeding/verification pass, even though it is not itself part of the
/// base POLICY.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReviewBaseRow {
    pub base_mode: Option<String>,
    pub base_branch: Option<String>,
    pub base_member: Option<i64>,
    pub base_set_by: String,
    pub base_status: Option<String>,
    pub objects_state: Option<String>,
}

/// The two new `review_patchsets` columns (V0045 / RS-U1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PatchsetBaseFields {
    pub base_tip_sha: Option<String>,
    pub kind: Option<String>,
}

fn review_store_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewStoreRow> {
    Ok(ReviewStoreRow {
        id: r.get(0)?,
        uuid: r.get(1)?,
        store_key: r.get(2)?,
        git_dir: r.get(3)?,
        base_url: r.get(4)?,
        base_url_source: r.get(5)?,
        forge_kind: r.get(6)?,
        forge_host: r.get(7)?,
        forge_slug: r.get(8)?,
        forge_verified: r.get(9)?,
        cred_kind: r.get(10)?,
        cred_reason: r.get(11)?,
        cred_account: r.get(12)?,
        key_fingerprint: r.get(13)?,
        key_read_only: r.get(14)?,
        state: r.get(15)?,
        state_json: r.get(16)?,
        created_at: r.get(17)?,
    })
}

fn repo_store_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<RepoStoreRow> {
    Ok(RepoStoreRow {
        repo_id: r.get(0)?,
        store_id: r.get(1)?,
        legacy_import_json: r.get(2)?,
        legacy_refs_state: r.get(3)?,
    })
}
