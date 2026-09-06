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

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::extract::Symbol;
use crate::highlight::Span;
use crate::transcripts::indexer::IndexedTurn;

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
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// One row of the `files` table — the current working-tree state mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    pub path: String,
    pub blob_hash: String,
    pub lang: String,
    pub size: u64,
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

        // kb-sibling/1 — HARD schema-epoch guard, BEFORE the migration run
        // (same posture, same helper, as `kb_core::storage::sqlite::Db::
        // open`): refinery only ever migrates FORWARD, so an older binary
        // pointed at a volume a newer one already migrated boots green and
        // then fails at request time on columns it doesn't know about — the
        // 13.5 h kbc outage. This daemon has ONE store, so this fires once,
        // and the error propagates out of `bind_and_spawn`, refusing boot.
        kb_core::sibling::refuse_if_volume_ahead(&conn, path, schema_epoch())
            .map_err(|e| StoreError::SchemaEpoch(e.to_string()))?;

        embedded::migrations::runner()
            .run(&mut conn)
            .map_err(|e| StoreError::Migration(e.to_string()))?;

        Ok(Self {
            conn: Mutex::new(conn),
            generation: AtomicU64::new(0),
            opens_generation: AtomicU64::new(0),
        })
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

    // --- repos ---------------------------------------------------------

    /// Idempotent upsert: first call inserts, later calls with the same
    /// `name` update `root` if it changed. Returns the row id (stable
    /// across calls for a given `name`).
    pub fn upsert_repo(&self, name: &str, root: &str) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO repos (name, root) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET root = excluded.root",
            params![name, root],
        )?;
        let id = conn.query_row("SELECT id FROM repos WHERE name = ?1", params![name], |r| {
            r.get(0)
        })?;
        Ok(id)
    }

    pub fn repo_id(&self, name: &str) -> Result<Option<i64>> {
        let conn = self.lock();
        conn.query_row("SELECT id FROM repos WHERE name = ?1", params![name], |r| {
            r.get(0)
        })
        .optional()
        .map_err(Into::into)
    }

    /// Filesystem root path for `repo_id` (as stored by `upsert_repo`).
    pub fn repo_root(&self, repo_id: i64) -> Result<Option<String>> {
        self.lock()
            .query_row(
                "SELECT root FROM repos WHERE id = ?1",
                params![repo_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    // --- files -----------------------------------------------------------

    /// Upsert the current working-tree state for `(repo_id, path)`. Called
    /// for EVERY tracked file `ingest` visits, regardless of whether the
    /// content was parsed (an unsupported/oversized/binary file still gets
    /// a `files` row so `file_count` reflects the whole tree; only
    /// `symbols`/`highlights` are conditional on a supported language).
    pub fn upsert_file(
        &self,
        repo_id: i64,
        path: &str,
        blob_hash: &str,
        lang: &str,
        size: u64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO files (repo_id, path, blob_hash, lang, size) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(repo_id, path) DO UPDATE SET
                blob_hash = excluded.blob_hash,
                lang = excluded.lang,
                size = excluded.size",
            params![repo_id, path, blob_hash, lang, size as i64],
        )?;
        self.bump_generation();
        Ok(())
    }

    /// Every current `files` row for `repo_id`, path-ordered — the files
    /// search lane's (`search::files::FileIndex`) cache source, and the
    /// text lane's (`search::text::search_text`) working-tree walk list
    /// (see that module's doc: it reuses this table rather than a fresh
    /// filesystem walk).
    pub fn list_files(&self, repo_id: i64) -> Result<Vec<FileRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path, blob_hash, lang, size FROM files WHERE repo_id = ?1 ORDER BY path",
        )?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                Ok(FileRow {
                    path: r.get(0)?,
                    blob_hash: r.get(1)?,
                    lang: r.get(2)?,
                    size: r.get::<_, i64>(3)? as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_file(&self, repo_id: i64, path: &str) -> Result<Option<FileRow>> {
        self.lock()
            .query_row(
                "SELECT path, blob_hash, lang, size FROM files WHERE repo_id = ?1 AND path = ?2",
                params![repo_id, path],
                |r| {
                    Ok(FileRow {
                        path: r.get(0)?,
                        blob_hash: r.get(1)?,
                        lang: r.get(2)?,
                        size: r.get::<_, i64>(3)? as u64,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn file_count(&self, repo_id: i64) -> Result<u64> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM files WHERE repo_id = ?1",
            params![repo_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Delete the `(repo_id, path)` files row — a live-mirror `remove_path`
    /// observation (W1.6). Derived `symbols`/`highlights` rows are NEVER
    /// touched here: they're keyed by `blob_hash` (ADR-2), not `(repo_id,
    /// path)`, so they may still be reachable from another path pointing at
    /// the same content, or from the same path on a different branch/ref —
    /// deleting a `files` row is purely "this path no longer points at
    /// anything right now," never a statement about the blob's derived data
    /// being stale. A no-op (not an error) if the row is already gone.
    ///
    /// ALSO prunes any `file_opens` rows for the same `(repo_id, path)` —
    /// unlike symbols/highlights, that table is NOT inert once its file is
    /// gone: `search::files::FileIndex::search`'s frecency blend and
    /// `agentview::map`'s ranking both iterate LIVE paths first and only
    /// look recency up per-path, so a stray row there is harmless to them,
    /// but `recent_file_opens` (the empty-query "recent files" fallback,
    /// `FileIndex::recent`) reads `file_opens` directly with no join against
    /// `files` — an unpruned row would resurface a deleted file in that list
    /// forever (or until another path collides with the same repo/path
    /// pair). One transaction so a crash between the two deletes can't leave
    /// the tables disagreeing.
    ///
    /// ALSO prunes `rails_edges` rows for the same `(repo_id, path)` (R1
    /// fix, v70-a1). This does NOT contradict the "derived rows are
    /// blob-keyed, so leave them alone" reasoning above: `symbols`/
    /// `highlights` are read by `blob_hash`, so they're inert once no
    /// `files` row points at that blob any more. `rails_edges` reads
    /// (`rails_edges_by_src_path`/`_by_dst_path`/`_by_kind`) are `(repo_id,
    /// path/kind)`-keyed instead — that reasoning does not carry, and a
    /// deleted controller left phantom `route_action`/`render_partial` rows
    /// that `/api/usages` would report as real usages of a live partial
    /// forever (see `replace_rails_edges`'s doc for the write-side half of
    /// this fix).
    pub fn delete_file(&self, repo_id: i64, path: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM files WHERE repo_id = ?1 AND path = ?2",
            params![repo_id, path],
        )?;
        tx.execute(
            "DELETE FROM file_opens WHERE repo_id = ?1 AND path = ?2",
            params![repo_id, path],
        )?;
        tx.execute(
            "DELETE FROM rails_edges WHERE repo_id = ?1 AND src_path = ?2",
            params![repo_id, path],
        )?;
        // V71-G0 — `entity_defs` is `(repo_id, worktree, path)`-keyed, so
        // the same reasoning that puts `rails_edges` in this transaction
        // applies verbatim: its reads are path-keyed, not blob-keyed, and
        // a deleted file's rows would otherwise answer `?ent=` queries
        // with definition sites that no longer exist. Deliberately NOT
        // scoped to a worktree — a path deleted from this repo id is gone
        // from every checkout that repo id covers.
        tx.execute(
            "DELETE FROM entity_defs WHERE repo_id = ?1 AND path = ?2",
            params![repo_id, path],
        )?;
        tx.commit()?;
        self.bump_generation();
        Ok(())
    }

    // --- file_opens (frecency, W2.1) --------------------------------------

    /// Log one open EVENT for `(repo_id, path)` at `opened_at_ms` (unix
    /// milliseconds — the caller's clock, injected rather than read here so
    /// tests can pin it; `routes::file` passes `chrono::Utc::now()`). See
    /// the migration's doc: this is an append-only log, never an
    /// aggregated counter — `last_opened_map`/`recent_file_opens` derive
    /// both signals the files search lane needs (a per-path recency boost,
    /// and the empty-query "most recently opened" fallback) from it via
    /// `MAX(opened_at)`. Pruned on `delete_file` — see that fn's doc.
    ///
    /// V70-A3X: bumps [`Self::opens_generation`], NOT [`Self::
    /// bump_generation`] — a file open never changes the files/symbols
    /// candidate SET (`generation`'s actual contract), so it must not
    /// invalidate the whole-repo path/symbol snapshot caches those lanes
    /// key on `generation` for. `delete_file` still ALSO prunes
    /// `file_opens` rows without bumping `opens_generation` — harmless: the
    /// deleted path is gone from the `generation`-gated path snapshot too,
    /// so its stale recency entry (if any survives one extra cache cycle)
    /// is never looked up again.
    pub fn bump_file_open(&self, repo_id: i64, path: &str, opened_at_ms: i64) -> Result<()> {
        self.lock().execute(
            "INSERT INTO file_opens (repo_id, path, opened_at) VALUES (?1, ?2, ?3)",
            params![repo_id, path, opened_at_ms],
        )?;
        self.bump_opens_generation();
        Ok(())
    }

    /// The most recent `opened_at` (unix ms) per path in `repo_id` — the
    /// files lane's per-candidate recency lookup (`search::files::
    /// FileIndex::search`'s frecency blend). One query per search call,
    /// covering every path in the repo at once (cheap: `file_opens` is a
    /// small table, and this is already scoped to one repo).
    pub fn last_opened_map(&self, repo_id: i64) -> Result<HashMap<String, i64>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path, MAX(opened_at) FROM file_opens WHERE repo_id = ?1 GROUP BY path",
        )?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;
        Ok(rows)
    }

    /// The `limit` most recently opened `(repo_id, path)` pairs across
    /// `repo_ids`, newest first, deduped by `(repo_id, path)` — the files
    /// lane's empty-query fallback (`search::files::FileIndex::recent`).
    /// `repo_ids` is typically single-digit length (one daemon's configured
    /// repos), so a dynamically-built `IN (...)` placeholder list is simpler
    /// than a temp table and plenty fast at this scale. `path_filter`
    /// (V70-A3X, `search::unified`'s `path:` PRE-filter) is an optional
    /// case-insensitive substring applied IN SQL (`LOWER(path) LIKE`), not
    /// after `LIMIT` — a matching-but-older path can't be pushed out of the
    /// recency window by non-matching, more-recent opens before the filter
    /// ever runs.
    pub fn recent_file_opens(
        &self,
        repo_ids: &[i64],
        limit: usize,
        path_filter: Option<&str>,
    ) -> Result<Vec<(i64, String, i64)>> {
        if repo_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let placeholders = repo_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let path_clause = if path_filter.is_some() {
            " AND LOWER(path) LIKE ?"
        } else {
            ""
        };
        let sql = format!(
            "SELECT repo_id, path, MAX(opened_at) AS last_opened FROM file_opens \
             WHERE repo_id IN ({placeholders}){path_clause} GROUP BY repo_id, path \
             ORDER BY last_opened DESC, path ASC LIMIT ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        // `repo_ids` (as text, since the LIKE pattern is text too) + the
        // optional LIKE pattern + the trailing LIMIT, all bound positionally
        // via `params_from_iter` over a `Vec<Box<dyn ToSql>>` — simpler and
        // less error-prone than hand building the `&dyn ToSql` slice for a
        // dynamic-length, mixed-type placeholder list.
        let mut bind: Vec<Box<dyn rusqlite::ToSql>> =
            repo_ids.iter().map(|id| Box::new(*id) as _).collect();
        if let Some(pat) = path_filter {
            bind.push(Box::new(format!("%{}%", pat.to_lowercase())));
        }
        bind.push(Box::new(limit as i64));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bind.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- symbols (blob-keyed) -------------------------------------------

    /// `true` if `(blob_hash, salt)` already has derived symbol rows — the
    /// cache-hit check `ingest::index_file` uses to skip re-parsing.
    pub fn has_symbols(&self, blob_hash: &str, salt: &str) -> Result<bool> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM symbols WHERE blob_hash = ?1 AND salt = ?2",
            params![blob_hash, salt],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Replace the full symbol set for `(blob_hash, salt)` — a delete then
    /// re-insert inside one transaction, since a re-derive (e.g. after a
    /// salt bump) has no stable per-symbol identity to diff against.
    ///
    /// V70-A3X: the DELETE purges every STALE salt for this blob's language
    /// (see [`lang_prefix_pattern`]'s doc), not just the exact incoming
    /// `salt` — before this fix, a grammar/query salt bump left the OLD
    /// derivation's rows permanently stranded (a brand-new salt string
    /// never matches an old one, so the old-salt-only delete this replaced
    /// never touched them), and `symbols_for_repo`'s un-salted join
    /// surfaced BOTH generations as duplicate hits.
    pub fn replace_symbols(&self, blob_hash: &str, salt: &str, symbols: &[Symbol]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM symbols WHERE blob_hash = ?1 AND salt LIKE ?2",
            params![blob_hash, lang_prefix_pattern(salt)],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO symbols
                    (blob_hash, salt, ordinal, name, kind, line_start, line_end, col_start, col_end,
                     container, signature, doc, param_min, param_max)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            )?;
            for sym in symbols {
                stmt.execute(params![
                    blob_hash,
                    salt,
                    sym.ordinal,
                    sym.name,
                    sym.kind,
                    sym.line_start,
                    sym.line_end,
                    sym.col_start,
                    sym.col_end,
                    sym.container,
                    sym.signature,
                    sym.doc,
                    sym.param_min.map(|n| n as i64),
                    sym.param_max.map(|n| n as i64),
                ])?;
            }
        }
        tx.commit()?;
        // Belt-and-suspenders: `index_file` always calls `upsert_file`
        // first (which already bumps the generation), so this is a
        // redundant bump on today's only call path — kept anyway so a
        // future direct caller (e.g. a "force reindex" verb) can't forget
        // to invalidate the symbols-lane cache.
        self.bump_generation();
        Ok(())
    }

    pub fn symbols_for_blob(&self, blob_hash: &str, salt: &str) -> Result<Vec<Symbol>> {
        let conn = self.lock();
        // PF-K1 — called once per caller-group in `hierarchy::callers_at`'s
        // fan-out; `prepare_cached` (identical SQL every call) avoids a
        // full re-parse/re-plan on every group.
        let mut stmt = conn.prepare_cached(
            "SELECT ordinal, name, kind, line_start, line_end, col_start, col_end,
                    container, signature, doc, param_min, param_max
             FROM symbols WHERE blob_hash = ?1 AND salt = ?2 ORDER BY ordinal",
        )?;
        let rows = stmt
            .query_map(params![blob_hash, salt], |r| symbol_from_row(r, 0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Symbol count for `repo_id`: every `symbols` row whose `blob_hash` is
    /// reachable from at least one `files` row in this repo. Deliberately a
    /// `WHERE blob_hash IN (...)` filter rather than a `JOIN` — two files
    /// with IDENTICAL content (same `blob_hash`, e.g. a copy) share one set
    /// of `symbols` rows (ADR-2), and a join would count that shared set
    /// once per path pointing at it, inflating the total. One remaining
    /// approximation: a blob re-derived under a NEWER salt (a grammar/query
    /// version bump) leaves its OLD-salt rows on disk with nothing pruning
    /// them, so both generations get counted here — acceptable for this
    /// Wave's "cheap COUNT" identity display; a future wave that prunes
    /// stale-salt rows on ingest closes it.
    pub fn symbol_count_for_repo(&self, repo_id: i64) -> Result<u64> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM symbols
             WHERE blob_hash IN (SELECT DISTINCT blob_hash FROM files WHERE repo_id = ?1)",
            params![repo_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Every symbol reachable from `repo_id`'s current `files` rows, paired
    /// with the path that reaches it — the `GET /api/symbols?q=` substring
    /// query's data source (`routes.rs`), AND the files/symbols search
    /// lane's (`search::symbols::SymbolIndex`) own candidate source. A
    /// `JOIN` (unlike `symbol_count_for_repo`'s `WHERE ... IN (...)` — a
    /// plain count has no path to report, but a symbol LISTING needs one),
    /// so two paths sharing a `blob_hash` (a copy, or the same content
    /// reachable two ways) each get their own row here — that's correct
    /// for "where can I find this symbol," unlike the count, which would
    /// double-count identical content.
    ///
    /// V70-A3X: restricted to CURRENT-salt rows per blob (via
    /// [`current_salt_cte`]'s fallback-aware filter) — before this fix, a
    /// blob re-derived under a newer salt (a grammar/query version bump)
    /// left its old-salt rows un-pruned and BOTH generations showed up
    /// here, duplicating every symbol in `@` search / `/api/symbols` /
    /// `/api/defs`. Filtering happens in Rust
    /// (`.to_lowercase().contains(needle)`), not SQL `LIKE`, so there's no
    /// wildcard-escaping surface for a user-supplied query string. The real
    /// fuzzy-match lane (nucleo-backed ranking) is W2.1's job.
    pub fn symbols_for_repo(&self, repo_id: i64) -> Result<Vec<(String, Symbol)>> {
        let conn = self.lock();
        let (cte, salts) = current_salt_cte();
        let sql = format!(
            "{cte}
             SELECT f.path, s.ordinal, s.name, s.kind, s.line_start, s.line_end,
                    s.col_start, s.col_end, s.container, s.signature, s.doc,
                    s.param_min, s.param_max
             FROM files f
             JOIN symbols s ON s.blob_hash = f.blob_hash
             WHERE f.repo_id = ?
               AND (
                     s.salt IN (SELECT salt FROM cur)
                     OR NOT EXISTS (
                           SELECT 1 FROM symbols s2
                           WHERE s2.blob_hash = s.blob_hash AND s2.salt IN (SELECT salt FROM cur)
                         )
                   )
             ORDER BY f.path, s.ordinal"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut bind: Vec<Box<dyn rusqlite::ToSql>> =
            salts.iter().map(|s| Box::new(*s) as _).collect();
        bind.push(Box::new(repo_id));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bind.iter()), |r| {
                Ok((r.get::<_, String>(0)?, symbol_from_row(r, 1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- import graph (V3.G2) ----------------------------------------------

    /// Content-addressed cache-hit check for `import_specs`.
    pub fn has_import_specs(&self, blob_hash: &str, salt: &str) -> Result<bool> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM import_specs WHERE blob_hash = ?1 AND salt = ?2",
            params![blob_hash, salt],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Replace the full import-spec set for `(blob_hash, salt)`.
    pub fn replace_import_specs(
        &self,
        blob_hash: &str,
        salt: &str,
        specs: &[crate::import_graph::ImportSpec],
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM import_specs WHERE blob_hash = ?1 AND salt = ?2",
            params![blob_hash, salt],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO import_specs (blob_hash, salt, ordinal, raw_spec, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for s in specs {
                stmt.execute(params![blob_hash, salt, s.ordinal, s.raw_spec, s.kind])?;
            }
        }
        // Sentinel so an empty extract is still a cache hit next visit.
        if specs.is_empty() {
            tx.execute(
                "INSERT INTO import_specs (blob_hash, salt, ordinal, raw_spec, kind)
                 VALUES (?1, ?2, -1, '', 'none')",
                params![blob_hash, salt],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn import_specs_for_blob(
        &self,
        blob_hash: &str,
        salt: &str,
    ) -> Result<Vec<crate::import_graph::ImportSpec>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT ordinal, raw_spec, kind FROM import_specs
             WHERE blob_hash = ?1 AND salt = ?2 AND ordinal >= 0
             ORDER BY ordinal",
        )?;
        let rows = stmt
            .query_map(params![blob_hash, salt], |r| {
                Ok(crate::import_graph::ImportSpec {
                    ordinal: r.get::<_, i64>(0)? as u32,
                    raw_spec: r.get(1)?,
                    kind: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- call sites + type relations (V3.1-H1) ----------------------------

    pub fn has_call_sites(&self, blob_hash: &str, salt: &str) -> Result<bool> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM call_sites WHERE blob_hash = ?1 AND salt = ?2",
            params![blob_hash, salt],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn replace_call_sites(
        &self,
        blob_hash: &str,
        salt: &str,
        sites: &[crate::hierarchy::CallSite],
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM call_sites WHERE blob_hash = ?1 AND salt = ?2",
            params![blob_hash, salt],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO call_sites
                    (blob_hash, salt, ordinal, callee_name, callee_qualifier,
                     line, col, arg_count, caller_ordinal)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for s in sites {
                stmt.execute(params![
                    blob_hash,
                    salt,
                    s.ordinal,
                    s.callee_name,
                    s.callee_qualifier,
                    s.line,
                    s.col,
                    s.arg_count.map(|n| n as i64),
                    s.caller_ordinal.map(|n| n as i64),
                ])?;
            }
        }
        // Sentinel so an empty extract is still a cache hit next visit.
        if sites.is_empty() {
            tx.execute(
                "INSERT INTO call_sites
                    (blob_hash, salt, ordinal, callee_name, callee_qualifier,
                     line, col, arg_count, caller_ordinal)
                 VALUES (?1, ?2, -1, '', NULL, 0, 0, NULL, NULL)",
                params![blob_hash, salt],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn call_sites_for_blob(
        &self,
        blob_hash: &str,
        salt: &str,
    ) -> Result<Vec<crate::hierarchy::CallSite>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT ordinal, callee_name, callee_qualifier, line, col, arg_count, caller_ordinal
             FROM call_sites
             WHERE blob_hash = ?1 AND salt = ?2 AND ordinal >= 0
             ORDER BY ordinal",
        )?;
        let rows = stmt
            .query_map(params![blob_hash, salt], |r| {
                let arg_count: Option<i64> = r.get(5)?;
                let caller_ordinal: Option<i64> = r.get(6)?;
                Ok(crate::hierarchy::CallSite {
                    ordinal: r.get::<_, i64>(0)? as u32,
                    callee_name: r.get(1)?,
                    callee_qualifier: r.get(2)?,
                    line: r.get::<_, i64>(3)? as u32,
                    col: r.get::<_, i64>(4)? as u32,
                    arg_count: arg_count.map(|n| n as u32),
                    caller_ordinal: caller_ordinal.map(|n| n as u32),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Repo-wide call sites named `callee_name`, with path + blob_hash + salt.
    pub fn call_sites_by_callee_in_repo(
        &self,
        repo_id: i64,
        callee_name: &str,
    ) -> Result<Vec<(String, crate::hierarchy::CallSite, String, String)>> {
        let conn = self.lock();
        // PF-K1 — `callers_at` runs this once per invocation and
        // `review_impact`'s fan-out calls `callers_at` up to
        // `MAX_CHANGED_SYMBOLS` (20) times per request; `prepare_cached`
        // (identical SQL every call) skips the re-plan each time.
        let mut stmt = conn.prepare_cached(
            "SELECT f.path, c.ordinal, c.callee_name, c.callee_qualifier, c.line, c.col,
                    c.arg_count, c.caller_ordinal, f.blob_hash, c.salt
             FROM files f
             JOIN call_sites c ON c.blob_hash = f.blob_hash
             WHERE f.repo_id = ?1 AND c.callee_name = ?2 AND c.ordinal >= 0
             ORDER BY f.path, c.line, c.col",
        )?;
        let rows = stmt
            .query_map(params![repo_id, callee_name], |r| {
                let arg_count: Option<i64> = r.get(6)?;
                let caller_ordinal: Option<i64> = r.get(7)?;
                Ok((
                    r.get::<_, String>(0)?,
                    crate::hierarchy::CallSite {
                        ordinal: r.get::<_, i64>(1)? as u32,
                        callee_name: r.get(2)?,
                        callee_qualifier: r.get(3)?,
                        line: r.get::<_, i64>(4)? as u32,
                        col: r.get::<_, i64>(5)? as u32,
                        arg_count: arg_count.map(|n| n as u32),
                        caller_ordinal: caller_ordinal.map(|n| n as u32),
                    },
                    r.get::<_, String>(8)?,
                    r.get::<_, String>(9)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn has_type_relations(&self, blob_hash: &str, salt: &str) -> Result<bool> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM type_relations WHERE blob_hash = ?1 AND salt = ?2",
            params![blob_hash, salt],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn replace_type_relations(
        &self,
        blob_hash: &str,
        salt: &str,
        rels: &[crate::hierarchy::TypeRelation],
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM type_relations WHERE blob_hash = ?1 AND salt = ?2",
            params![blob_hash, salt],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO type_relations
                    (blob_hash, salt, ordinal, kind, subject, object, line)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for r in rels {
                stmt.execute(params![
                    blob_hash, salt, r.ordinal, r.kind, r.subject, r.object, r.line,
                ])?;
            }
        }
        if rels.is_empty() {
            tx.execute(
                "INSERT INTO type_relations
                    (blob_hash, salt, ordinal, kind, subject, object, line)
                 VALUES (?1, ?2, -1, 'none', '', '', 0)",
                params![blob_hash, salt],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn type_relations_for_blob(
        &self,
        blob_hash: &str,
        salt: &str,
    ) -> Result<Vec<crate::hierarchy::TypeRelation>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT ordinal, kind, subject, object, line FROM type_relations
             WHERE blob_hash = ?1 AND salt = ?2 AND ordinal >= 0
             ORDER BY ordinal",
        )?;
        let rows = stmt
            .query_map(params![blob_hash, salt], |r| {
                Ok(crate::hierarchy::TypeRelation {
                    ordinal: r.get::<_, i64>(0)? as u32,
                    kind: r.get(1)?,
                    subject: r.get(2)?,
                    object: r.get(3)?,
                    line: r.get::<_, i64>(4)? as u32,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Type-relation rows in `repo_id` where subject OR object equals `name`.
    pub fn type_relations_for_name_in_repo(
        &self,
        repo_id: i64,
        name: &str,
    ) -> Result<Vec<(String, crate::hierarchy::TypeRelation)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT f.path, t.ordinal, t.kind, t.subject, t.object, t.line
             FROM files f
             JOIN type_relations t ON t.blob_hash = f.blob_hash
             WHERE f.repo_id = ?1 AND t.ordinal >= 0
               AND (t.subject = ?2 OR t.object = ?2)
             ORDER BY f.path, t.line, t.ordinal",
        )?;
        let rows = stmt
            .query_map(params![repo_id, name], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    crate::hierarchy::TypeRelation {
                        ordinal: r.get::<_, i64>(1)? as u32,
                        kind: r.get(2)?,
                        subject: r.get(3)?,
                        object: r.get(4)?,
                        line: r.get::<_, i64>(5)? as u32,
                    },
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Symbol definitions named `name` in `repo_id` (path + symbol).
    pub fn symbols_named_in_repo(&self, repo_id: i64, name: &str) -> Result<Vec<(String, Symbol)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT f.path, s.ordinal, s.name, s.kind, s.line_start, s.line_end,
                    s.col_start, s.col_end, s.container, s.signature, s.doc,
                    s.param_min, s.param_max
             FROM files f
             JOIN symbols s ON s.blob_hash = f.blob_hash
             WHERE f.repo_id = ?1 AND s.name = ?2
             ORDER BY f.path, s.line_start, s.ordinal",
        )?;
        let rows = stmt
            .query_map(params![repo_id, name], |r| {
                Ok((r.get::<_, String>(0)?, symbol_from_row(r, 1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Rebuild all `import_edges` for `file_id` (delete+insert).
    /// `edges` is `(raw_spec, target_file_id)`.
    pub fn replace_import_edges(&self, file_id: i64, edges: &[(String, i64)]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM import_edges WHERE file_id = ?1",
            params![file_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO import_edges (file_id, target_file_id, raw_spec)
                 VALUES (?1, ?2, ?3)",
            )?;
            for (raw_spec, target_id) in edges {
                stmt.execute(params![file_id, target_id, raw_spec])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Repo-relative paths of files that `file_id` directly imports.
    pub fn import_target_paths(&self, file_id: i64) -> Result<Vec<String>> {
        let conn = self.lock();
        // PF-K1 — called once per caller-group in `hierarchy::callers_at`'s
        // fan-out; `prepare_cached` (identical SQL every call) avoids a
        // full re-parse/re-plan on every group.
        let mut stmt = conn.prepare_cached(
            "SELECT DISTINCT f.path
             FROM import_edges e
             JOIN files f ON f.id = e.target_file_id
             WHERE e.file_id = ?1
             ORDER BY f.path",
        )?;
        let rows = stmt
            .query_map(params![file_id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Symbols named `name` on any file whose path equals `path`
    /// (any repo — used for resolve enrichment of signature/doc/container).
    pub fn symbols_at_path_named(&self, path: &str, name: &str) -> Result<Vec<Symbol>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT s.ordinal, s.name, s.kind, s.line_start, s.line_end,
                    s.col_start, s.col_end, s.container, s.signature, s.doc,
                    s.param_min, s.param_max
             FROM files f
             JOIN symbols s ON s.blob_hash = f.blob_hash
             WHERE f.path = ?1 AND s.name = ?2
             ORDER BY s.line_start, s.ordinal",
        )?;
        let rows = stmt
            .query_map(params![path, name], |r| symbol_from_row(r, 0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Repo-relative paths of files that import `file_id` (reverse edges).
    /// Used by impact analysis's `imports` bucket (dependency footprint).
    pub fn import_source_paths(&self, file_id: i64) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT f.path
             FROM import_edges e
             JOIN files f ON f.id = e.file_id
             WHERE e.target_file_id = ?1
             ORDER BY f.path",
        )?;
        let rows = stmt
            .query_map(params![file_id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// V3.3-S1 — count of `import_edges` whose source file is in `repo_id`.
    /// Zero means import resolution has not landed (or no resolvable
    /// imports exist); callers use this for honest `inputs_missing`.
    pub fn import_edge_count_for_repo(&self, repo_id: i64) -> Result<u64> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*)
             FROM import_edges e
             JOIN files f ON f.id = e.file_id
             WHERE f.repo_id = ?1",
            params![repo_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// V3.3-S1 — count of content-addressed `import_specs` for blobs
    /// currently pointed at by `repo_id`'s files (any salt).
    pub fn import_spec_count_for_repo(&self, repo_id: i64) -> Result<u64> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*)
             FROM import_specs s
             WHERE EXISTS (
               SELECT 1 FROM files f
               WHERE f.repo_id = ?1 AND f.blob_hash = s.blob_hash
             )",
            params![repo_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// V3.3-S1 — count of non-sentinel `call_sites` for blobs currently
    /// pointed at by `repo_id`'s files.
    pub fn call_site_count_for_repo(&self, repo_id: i64) -> Result<u64> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*)
             FROM call_sites c
             WHERE c.ordinal >= 0
               AND EXISTS (
                 SELECT 1 FROM files f
                 WHERE f.repo_id = ?1 AND f.blob_hash = c.blob_hash
               )",
            params![repo_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// V3.3-S1 — direct import edges among a set of repo-relative paths
    /// (both ends must be in the set). Returns `(from_path, to_path)`
    /// ordered by (from, to) for determinism. Corpus-local, no transitive
    /// closure.
    pub fn import_edges_among_paths(
        &self,
        repo_id: i64,
        paths: &[String],
    ) -> Result<Vec<(String, String)>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        // Map path → file_id for the changed set.
        let mut id_by_path: std::collections::HashMap<String, i64> =
            std::collections::HashMap::with_capacity(paths.len());
        let mut path_by_id: std::collections::HashMap<i64, String> =
            std::collections::HashMap::with_capacity(paths.len());
        {
            let mut stmt = conn.prepare("SELECT id FROM files WHERE repo_id = ?1 AND path = ?2")?;
            for p in paths {
                if let Ok(Some(id)) = stmt
                    .query_row(params![repo_id, p], |r| r.get::<_, i64>(0))
                    .optional()
                {
                    id_by_path.insert(p.clone(), id);
                    path_by_id.insert(id, p.clone());
                }
            }
        }
        if id_by_path.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<i64> = id_by_path.values().copied().collect();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT e.file_id, e.target_file_id
             FROM import_edges e
             WHERE e.file_id IN ({placeholders})
               AND e.target_file_id IN ({placeholders})
             ORDER BY e.file_id, e.target_file_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(ids.len() * 2);
        for &id in &ids {
            params.push(rusqlite::types::Value::Integer(id));
        }
        for &id in &ids {
            params.push(rusqlite::types::Value::Integer(id));
        }
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (from_id, to_id) = row?;
            if from_id == to_id {
                continue;
            }
            let Some(from_p) = path_by_id.get(&from_id) else {
                continue;
            };
            let Some(to_p) = path_by_id.get(&to_id) else {
                continue;
            };
            if seen.insert((from_p.clone(), to_p.clone())) {
                out.push((from_p.clone(), to_p.clone()));
            }
        }
        out.sort();
        Ok(out)
    }

    /// V3.3-S1 — cheap file-pair call edges among a path set.
    ///
    /// For each changed file, every `call_sites` row whose `callee_name`
    /// is defined by a symbol on exactly one *other* changed file yields a
    /// `(from, to, class)` edge. Class is CAPPED at `likely`: this is a
    /// bare name-match heuristic with no import-reachability, arity, or
    /// dynamic-dispatch analysis, and the crate's rubric
    /// (`resolve::class_for_precision`) reserves `exact` for scip/locals
    /// proof — a repo-wide-unique name is still not proof the call binds
    /// to that definition (`dyn`/duck-typed calls break it).
    /// - `likely` when the name has a single definition in the whole repo
    /// - `candidate` when multi-def repo-wide (unique only among changed)
    ///
    /// Self-calls and unresolved names are skipped. Deterministic:
    /// ordered by (from, to), one edge per pair (best class wins:
    /// likely > candidate).
    pub fn call_edges_among_paths(
        &self,
        repo_id: i64,
        paths: &[String],
    ) -> Result<Vec<(String, String, &'static str)>> {
        use crate::resolve::{CLASS_CANDIDATE, CLASS_EXACT, CLASS_LIKELY};
        if paths.len() < 2 {
            return Ok(Vec::new());
        }
        let path_set: std::collections::HashSet<&str> = paths.iter().map(|s| s.as_str()).collect();
        // name → list of defining paths among the changed set
        let mut defs_in_changed: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        // name → total def count in repo (for class)
        let mut def_count_repo: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for p in paths {
            let Some(file) = self.get_file(repo_id, p)? else {
                continue;
            };
            let lang = crate::lang::detect(p, None);
            let salt = lang.map(|l| l.salt).unwrap_or("");
            if salt.is_empty() {
                continue;
            }
            let syms = self.symbols_for_blob(&file.blob_hash, salt)?;
            for s in syms {
                defs_in_changed
                    .entry(s.name.clone())
                    .or_default()
                    .push(p.clone());
            }
        }
        // Dedupe defining paths per name and count repo-wide defs for names
        // that appear in the changed set.
        for (name, defs) in defs_in_changed.iter_mut() {
            defs.sort();
            defs.dedup();
            let repo_defs = self.symbols_named_in_repo(repo_id, name)?;
            def_count_repo.insert(name.clone(), repo_defs.len());
        }

        // Best class per (from, to)
        let mut best: std::collections::BTreeMap<(String, String), &'static str> =
            std::collections::BTreeMap::new();
        let class_rank = |c: &str| -> u8 {
            match c {
                CLASS_EXACT => 0,
                CLASS_LIKELY => 1,
                _ => 2,
            }
        };
        for p in paths {
            let Some(file) = self.get_file(repo_id, p)? else {
                continue;
            };
            let lang = crate::lang::detect(p, None);
            let salt = lang.map(|l| l.salt).unwrap_or("");
            if salt.is_empty() {
                continue;
            }
            let sites = self.call_sites_for_blob(&file.blob_hash, salt)?;
            for site in sites {
                let Some(def_paths) = defs_in_changed.get(&site.callee_name) else {
                    continue;
                };
                // Unique other changed-file def only.
                let others: Vec<&String> = def_paths
                    .iter()
                    .filter(|dp| dp.as_str() != p.as_str())
                    .collect();
                if others.len() != 1 {
                    continue;
                }
                let to = others[0].clone();
                if !path_set.contains(to.as_str()) {
                    continue;
                }
                let repo_n = def_count_repo.get(&site.callee_name).copied().unwrap_or(0);
                let class = if repo_n == 1 {
                    CLASS_LIKELY
                } else {
                    CLASS_CANDIDATE
                };
                let key = (p.clone(), to);
                match best.get(&key) {
                    Some(prev) if class_rank(prev) <= class_rank(class) => {}
                    _ => {
                        best.insert(key, class);
                    }
                }
            }
        }
        Ok(best
            .into_iter()
            .map(|((from, to), class)| (from, to, class))
            .collect())
    }

    /// Batch: all occurrences whose name is in `names` (any role), across
    /// the repo. Used by Code Vision lenses to count usages for every
    /// declaration in one scan instead of N per-name scans.
    ///
    /// V70-A3X: same current-salt-with-fallback restriction as
    /// `symbols_for_repo` (see [`current_salt_cte`]'s doc) — a stale-salt
    /// occurrence row (from a grammar/query salt bump) no longer inflates
    /// usage counts once a current-salt derivation exists for the same
    /// blob.
    pub fn occurrences_by_names_in_repo(
        &self,
        repo_id: i64,
        names: &[String],
    ) -> Result<Vec<(String, crate::occurrences::Occurrence)>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let (cte, salts) = current_salt_cte();
        let placeholders = names.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "{cte}
             SELECT f.path, o.ordinal, o.name, o.role, o.line, o.col_start, o.col_end,
                    o.source, o.local_def_ordinal
             FROM files f
             JOIN occurrences o ON o.blob_hash = f.blob_hash
             WHERE f.repo_id = ? AND o.name IN ({placeholders})
               AND (
                     o.salt IN (SELECT salt FROM cur)
                     OR NOT EXISTS (
                           SELECT 1 FROM occurrences o2
                           WHERE o2.blob_hash = o.blob_hash AND o2.salt IN (SELECT salt FROM cur)
                         )
                   )
             ORDER BY o.name, f.path, o.line, o.col_start"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<rusqlite::types::Value> =
            Vec::with_capacity(salts.len() + 1 + names.len());
        for s in &salts {
            params.push(rusqlite::types::Value::Text((*s).to_string()));
        }
        params.push(rusqlite::types::Value::Integer(repo_id));
        for n in names {
            params.push(rusqlite::types::Value::Text(n.clone()));
        }
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params), |r| {
                Ok((r.get::<_, String>(0)?, occurrence_row_from_offset(r, 1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// All occurrence rows named `name` with role `ref` across the repo.
    ///
    /// V70-A3X: same current-salt-with-fallback restriction — see
    /// [`Self::occurrences_by_names_in_repo`]'s doc.
    pub fn ref_occurrences_by_name_in_repo(
        &self,
        repo_id: i64,
        name: &str,
    ) -> Result<Vec<(String, crate::occurrences::Occurrence)>> {
        let conn = self.lock();
        let (cte, salts) = current_salt_cte();
        let sql = format!(
            "{cte}
             SELECT f.path, o.ordinal, o.name, o.role, o.line, o.col_start, o.col_end,
                    o.source, o.local_def_ordinal
             FROM files f
             JOIN occurrences o ON o.blob_hash = f.blob_hash
             WHERE f.repo_id = ? AND o.name = ? AND o.role = 'ref'
               AND (
                     o.salt IN (SELECT salt FROM cur)
                     OR NOT EXISTS (
                           SELECT 1 FROM occurrences o2
                           WHERE o2.blob_hash = o.blob_hash AND o2.salt IN (SELECT salt FROM cur)
                         )
                   )
             ORDER BY f.path, o.line, o.col_start"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut bind: Vec<Box<dyn rusqlite::ToSql>> =
            salts.iter().map(|s| Box::new(*s) as _).collect();
        bind.push(Box::new(repo_id));
        bind.push(Box::new(name.to_string()));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bind.iter()), |r| {
                Ok((r.get::<_, String>(0)?, occurrence_row_from_offset(r, 1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Def-role occurrences named `name` across the repo.
    ///
    /// V70-A3X: same current-salt-with-fallback restriction — see
    /// [`Self::occurrences_by_names_in_repo`]'s doc.
    pub fn def_occurrences_by_name_in_repo(
        &self,
        repo_id: i64,
        name: &str,
    ) -> Result<Vec<(String, crate::occurrences::Occurrence)>> {
        let conn = self.lock();
        let (cte, salts) = current_salt_cte();
        let sql = format!(
            "{cte}
             SELECT f.path, o.ordinal, o.name, o.role, o.line, o.col_start, o.col_end,
                    o.source, o.local_def_ordinal
             FROM files f
             JOIN occurrences o ON o.blob_hash = f.blob_hash
             WHERE f.repo_id = ? AND o.name = ? AND o.role = 'def'
               AND (
                     o.salt IN (SELECT salt FROM cur)
                     OR NOT EXISTS (
                           SELECT 1 FROM occurrences o2
                           WHERE o2.blob_hash = o.blob_hash AND o2.salt IN (SELECT salt FROM cur)
                         )
                   )
             ORDER BY f.path, o.line, o.col_start"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut bind: Vec<Box<dyn rusqlite::ToSql>> =
            salts.iter().map(|s| Box::new(*s) as _).collect();
        bind.push(Box::new(repo_id));
        bind.push(Box::new(name.to_string()));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bind.iter()), |r| {
                Ok((r.get::<_, String>(0)?, occurrence_row_from_offset(r, 1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- stale-salt sweep (V70-A3X) ---------------------------------------

    /// Boot-time hygiene sweep: deletes `symbols`/`highlights`/`occurrences`
    /// rows that are BOTH (a) reachable from a live `files` row
    /// (`blob_hash IN (SELECT blob_hash FROM files)` — an orphaned blob's
    /// rows are ADR-2's own deliberate "never pruned on file delete"
    /// territory, untouched here, see `delete_file`'s doc) AND (b)
    /// genuinely SUPERSEDED: not a current salt, AND a current-salt sibling
    /// for the SAME blob_hash already exists in that table — the identical
    /// fallback-safe condition [`current_salt_cte`]'s callers read under,
    /// so this sweep can never touch a blob whose ONLY rows are under a
    /// non-current salt (an ad hoc test fixture, a downgrade, or a
    /// not-yet-recognised future language all fall through untouched).
    /// Runs [`Self::sweep_stale_salt_page`] to completion. Convenience for
    /// SMALL stores only — in-crate tests and `build_state_for_test`'s
    /// fixture. The daemon itself must NOT call this: on a production-sized
    /// store a full pass is hours of random I/O, so `bind_and_spawn` drives
    /// the paged form from a background task instead (V72-B0 — see
    /// `lib::spawn_stale_salt_sweep`).
    pub fn sweep_stale_salt_derived(&self) -> Result<StaleSaltSweepCounts> {
        let mut totals = StaleSaltSweepCounts::default();
        let mut cursor: Option<String> = None;
        loop {
            let (counts, next) =
                self.sweep_stale_salt_page(cursor.as_deref(), STALE_SALT_SWEEP_PAGE)?;
            totals.symbols += counts.symbols;
            totals.highlights += counts.highlights;
            totals.occurrences += counts.occurrences;
            match next {
                Some(c) => cursor = Some(c),
                None => return Ok(totals),
            }
        }
    }

    /// V72-B0 — ONE bounded, resumable page of the sweep above, in its own
    /// short transaction.
    ///
    /// The sweep's driver is `files.blob_hash`, so a page is "the next
    /// `page` distinct blob hashes after `after`" (an `idx_files_blob_hash`
    /// range scan — sequential and cheap), and the three DELETEs are
    /// restricted to exactly those hashes. That bounds BOTH the work and,
    /// crucially, how long this holds the store's single connection mutex.
    /// The un-paged form's `blob_hash IN (SELECT blob_hash FROM files)` made
    /// every statement O(all live blobs) random index seeks inside ONE
    /// transaction — hours on a production store, with the mutex held for
    /// all of it, so no read could proceed either.
    ///
    /// Returns the page's counts and the cursor to resume from; `None` once
    /// the last page has been swept (a SHORT page is the end).
    ///
    /// The delete PREDICATE is unchanged — partitioning the driver cannot
    /// change which rows match, because both remaining conditions (`salt NOT
    /// IN cur`, and the correlated current-salt-sibling `EXISTS`) are
    /// per-`blob_hash`, and every blob falls in exactly one page.
    pub fn sweep_stale_salt_page(
        &self,
        after: Option<&str>,
        page: usize,
    ) -> Result<(StaleSaltSweepCounts, Option<String>)> {
        let (cte, salts) = current_salt_cte();
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let blobs: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT blob_hash FROM files WHERE blob_hash > ?1 \
                 ORDER BY blob_hash LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![after.unwrap_or(""), page as i64], |r| r.get(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        if blobs.is_empty() {
            return Ok((StaleSaltSweepCounts::default(), None));
        }
        let symbols = sweep_stale_salt_table(&tx, &cte, &salts, &blobs, "symbols")?;
        let highlights = sweep_stale_salt_table(&tx, &cte, &salts, &blobs, "highlights")?;
        let occurrences = sweep_stale_salt_table(&tx, &cte, &salts, &blobs, "occurrences")?;
        tx.commit()?;
        // A SHORT page means `files` held nothing after it — stop rather
        // than pay one more empty round trip.
        let next = if blobs.len() < page {
            None
        } else {
            blobs.last().cloned()
        };
        Ok((
            StaleSaltSweepCounts {
                symbols,
                highlights,
                occurrences,
            },
            next,
        ))
    }

    // --- highlights (blob-keyed) -----------------------------------------

    pub fn has_highlights(&self, blob_hash: &str, salt: &str) -> Result<bool> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM highlights WHERE blob_hash = ?1 AND salt = ?2",
            params![blob_hash, salt],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Upsert the packed highlight spans for `(blob_hash, salt)`. Encoding:
    /// JSON (`serde_json`) of `Vec<highlight::Span>` — see `highlight.rs`'s
    /// module doc for why JSON was picked over a bespoke binary format for
    /// this Wave. Stored in an opaque `BLOB` column, so the encoding can
    /// change later without a schema migration.
    ///
    /// V70-A3X: unlike `symbols`/`occurrences`, `highlights_for_blob` is
    /// always an EXACT `(blob_hash, salt)` lookup (no un-salted repo-wide
    /// join exists for highlights, so a stale-salt row here was never
    /// VISIBLY duplicated) — but it still accumulated as dead weight
    /// forever, since the `ON CONFLICT(blob_hash, salt)` upsert only ever
    /// touches the EXACT incoming salt's row. Purge every OTHER salt of
    /// this same language for the blob first (mirrors `replace_symbols`'s
    /// fix), same hygiene, in the same transaction as the upsert.
    pub fn put_highlights(&self, blob_hash: &str, salt: &str, spans: &[Span]) -> Result<()> {
        let bytes = serde_json::to_vec(spans)?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM highlights WHERE blob_hash = ?1 AND salt LIKE ?2 AND salt != ?3",
            params![blob_hash, lang_prefix_pattern(salt), salt],
        )?;
        tx.execute(
            "INSERT INTO highlights (blob_hash, salt, spans) VALUES (?1, ?2, ?3)
             ON CONFLICT(blob_hash, salt) DO UPDATE SET spans = excluded.spans",
            params![blob_hash, salt, bytes],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn highlights_for_blob(&self, blob_hash: &str, salt: &str) -> Result<Option<Vec<Span>>> {
        let bytes: Option<Vec<u8>> = self
            .lock()
            .query_row(
                "SELECT spans FROM highlights WHERE blob_hash = ?1 AND salt = ?2",
                params![blob_hash, salt],
                |r| r.get(0),
            )
            .optional()?;
        match bytes {
            Some(b) => Ok(Some(serde_json::from_slice(&b)?)),
            None => Ok(None),
        }
    }

    // --- occurrences (blob-keyed, B2) --------------------------------------
    //
    // Same ADR-2 keying as symbols/highlights, but a SEPARATE presence check
    // (`has_occurrences`, not `has_symbols`) — see occurrences.rs's module
    // doc's "Cache key" section for why the two passes are independently
    // retry-able rather than sharing one flag.

    /// `true` if `(blob_hash, salt)` already has derived occurrence rows —
    /// `ingest::index_file`'s cache-hit check for the occurrences pass,
    /// deliberately independent of [`Store::has_symbols`].
    pub fn has_occurrences(&self, blob_hash: &str, salt: &str) -> Result<bool> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM occurrences WHERE blob_hash = ?1 AND salt = ?2",
            params![blob_hash, salt],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Replace the full TREE-SITTER (`source = 'ts'`) occurrence set for
    /// `(blob_hash, salt)` — same delete-then-re-insert-in-one-transaction
    /// shape as `replace_symbols`. Scoped to `source = 'ts'` in BOTH the
    /// DELETE and the INSERT (S1): a blob can carry `'scip'` rows from
    /// [`Store::replace_scip_occurrences`] alongside its `'ts'` ones, and a
    /// re-derive of the tree-sitter pass must never touch those — see
    /// migration `V0010__occurrences_source.sql`'s doc on why the two
    /// sources share one dense ordinal space without colliding.
    ///
    /// V70-A3X: the DELETE purges every STALE salt of this blob's language
    /// (`salt LIKE`, [`lang_prefix_pattern`]) rather than only the exact
    /// incoming `salt` — same fix, same rationale, as `replace_symbols`.
    pub fn replace_occurrences(
        &self,
        blob_hash: &str,
        salt: &str,
        occurrences: &[crate::occurrences::Occurrence],
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM occurrences WHERE blob_hash = ?1 AND salt LIKE ?2 AND source = 'ts'",
            params![blob_hash, lang_prefix_pattern(salt)],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO occurrences
                    (blob_hash, salt, ordinal, name, role, line, col_start, col_end, source,
                     local_def_ordinal)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for occ in occurrences {
                stmt.execute(params![
                    blob_hash,
                    salt,
                    occ.ordinal,
                    occ.name,
                    occ.role,
                    occ.line,
                    occ.col_start,
                    occ.col_end,
                    occ.source,
                    occ.local_def_ordinal.map(|n| n as i64),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn occurrences_for_blob(
        &self,
        blob_hash: &str,
        salt: &str,
    ) -> Result<Vec<crate::occurrences::Occurrence>> {
        let conn = self.lock();
        // PF-K1 — called once per call site inside `hierarchy::
        // is_dynamic_call`'s per-caller-group loop; `prepare_cached`
        // (identical SQL every call) skips the re-plan on every site.
        let mut stmt = conn.prepare_cached(
            "SELECT ordinal, name, role, line, col_start, col_end, source, local_def_ordinal
             FROM occurrences WHERE blob_hash = ?1 AND salt = ?2 ORDER BY ordinal",
        )?;
        let rows = stmt
            .query_map(params![blob_hash, salt], occurrence_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Look up one occurrence by its ordinal within `(blob_hash, salt)` —
    /// V3.G1 locals arm: a reference row's `local_def_ordinal` points here.
    pub fn occurrence_by_ordinal(
        &self,
        blob_hash: &str,
        salt: &str,
        ordinal: u32,
    ) -> Result<Option<crate::occurrences::Occurrence>> {
        self.lock()
            .query_row(
                "SELECT ordinal, name, role, line, col_start, col_end, source, local_def_ordinal
                 FROM occurrences
                 WHERE blob_hash = ?1 AND salt = ?2 AND ordinal = ?3
                 LIMIT 1",
                params![blob_hash, salt, ordinal],
                occurrence_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// The occurrence covering 1-based `line` / 0-based `col` (i.e.
    /// `col_start <= col < col_end`) in one blob, if any — `resolve.rs`'s
    /// position lookup. `None` when this blob has no occurrences rows at all
    /// (an old blob under a stale salt, or a language `occurrences.rs`
    /// doesn't cover yet) OR the position simply doesn't land on any
    /// identifier — the caller (`resolve::resolve_position`) distinguishes
    /// the two via [`Store::has_occurrences`] and falls back to a plain
    /// word-at-position scan in the former case.
    ///
    /// S1: when a `'ts'` row and a `'scip'` row both cover the SAME span
    /// (the common case once a repo has been `scip ingest`-ed), the `scip`
    /// row wins (`ORDER BY (source = 'scip') DESC` — SQLite evaluates a
    /// boolean expression to `1`/`0`, so this sorts `scip` rows first) —
    /// this is the whole of "scip rows also make the position lookup itself
    /// exact" (`resolve.rs`'s module doc): no separate code path, just a
    /// tie-break favoring the more precise source.
    pub fn occurrence_at(
        &self,
        blob_hash: &str,
        salt: &str,
        line: u32,
        col: u32,
    ) -> Result<Option<crate::occurrences::Occurrence>> {
        self.lock()
            .query_row(
                "SELECT ordinal, name, role, line, col_start, col_end, source, local_def_ordinal
                 FROM occurrences
                 WHERE blob_hash = ?1 AND salt = ?2 AND line = ?3
                   AND col_start <= ?4 AND col_end > ?4
                 ORDER BY (source = 'scip') DESC, ordinal
                 LIMIT 1",
                params![blob_hash, salt, line, col],
                occurrence_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Every TREE-SITTER (`source = 'ts'`) `role = "def"` occurrence named
    /// `name` in one blob — the "file-local" candidate ranking tier
    /// `resolve.rs` prefers over a symbols-table name match (see that
    /// module's doc). Scoped to `source = 'ts'` (S1) so a blob's `'scip'`
    /// def rows are ranked at their OWN, higher-precision tier instead (see
    /// [`Store::scip_def_occurrences_by_name`]) rather than double-counted
    /// at both.
    pub fn def_occurrences_by_name(
        &self,
        blob_hash: &str,
        salt: &str,
        name: &str,
    ) -> Result<Vec<crate::occurrences::Occurrence>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT ordinal, name, role, line, col_start, col_end, source, local_def_ordinal
             FROM occurrences
             WHERE blob_hash = ?1 AND salt = ?2 AND name = ?3 AND role = 'def' AND source = 'ts'
             ORDER BY ordinal",
        )?;
        let rows = stmt
            .query_map(params![blob_hash, salt, name], occurrence_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- occurrences (scip-sourced, S1) -------------------------------------

    /// Every `source = 'scip'` `role = 'def'` occurrence named `name` in one
    /// blob — `resolve.rs`'s `"scip-exact"` tier, ranked ABOVE file-local.
    pub fn scip_def_occurrences_by_name(
        &self,
        blob_hash: &str,
        salt: &str,
        name: &str,
    ) -> Result<Vec<crate::occurrences::Occurrence>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT ordinal, name, role, line, col_start, col_end, source, local_def_ordinal
             FROM occurrences
             WHERE blob_hash = ?1 AND salt = ?2 AND name = ?3 AND role = 'def' AND source = 'scip'
             ORDER BY ordinal",
        )?;
        let rows = stmt
            .query_map(params![blob_hash, salt, name], occurrence_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Replace the full SCIP (`source = 'scip'`) occurrence set for
    /// `(blob_hash, salt)` — `crate::scip`'s ingest route, called once per
    /// accepted document. Scoped to `source = 'scip'` in the DELETE (mirrors
    /// `replace_occurrences`'s own `'ts'` scoping — never touches that
    /// blob's tree-sitter rows), and continues the ordinal sequence from
    /// whatever's already there (ANY source) rather than restarting at 0 —
    /// see migration `V0010__occurrences_source.sql`'s doc on why the
    /// `(blob_hash, salt, ordinal)` primary key is shared, unwidened, across
    /// both sources.
    pub fn replace_scip_occurrences(
        &self,
        blob_hash: &str,
        salt: &str,
        occurrences: &[ScipOccurrenceIn],
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM occurrences WHERE blob_hash = ?1 AND salt = ?2 AND source = 'scip'",
            params![blob_hash, salt],
        )?;
        let next_ordinal: i64 = tx.query_row(
            "SELECT COALESCE(MAX(ordinal), -1) + 1 FROM occurrences WHERE blob_hash = ?1 AND salt = ?2",
            params![blob_hash, salt],
            |r| r.get(0),
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO occurrences
                    (blob_hash, salt, ordinal, name, role, line, col_start, col_end, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'scip')",
            )?;
            for (i, occ) in occurrences.iter().enumerate() {
                stmt.execute(params![
                    blob_hash,
                    salt,
                    next_ordinal + i as i64,
                    occ.name,
                    occ.role,
                    occ.line,
                    occ.col_start,
                    occ.col_end,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // --- chunk_status (W2.3 semantic lane) --------------------------------

    /// `true` if `(blob_hash, salt)` already has chunk+embedding rows in
    /// the semantic lance store — see `migrations/V0004__chunk_status.sql`'s
    /// doc. The cache-hit check `semantic::indexer` uses to skip re-chunking
    /// and re-embedding a blob it has already processed (ADR-2's win,
    /// applied to the expensive embed pass).
    pub fn has_chunks(&self, blob_hash: &str, salt: &str) -> Result<bool> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM chunk_status WHERE blob_hash = ?1 AND salt = ?2",
            params![blob_hash, salt],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Record that `(blob_hash, salt)` has been chunked+embedded into
    /// `chunk_count` rows in the lance chunk store. Upsert (a salt bump —
    /// a grammar/query version change — re-derives under a new cache slot,
    /// same convention as `replace_symbols`/`put_highlights`). MUST be
    /// called together with the paired `ChunkStore::upsert_chunks_for_blob`
    /// lance write — see the migration's doc on why the two stay in
    /// lockstep.
    pub fn mark_chunked(&self, blob_hash: &str, salt: &str, chunk_count: usize) -> Result<()> {
        self.lock().execute(
            "INSERT INTO chunk_status (blob_hash, salt, chunk_count) VALUES (?1, ?2, ?3)
             ON CONFLICT(blob_hash, salt) DO UPDATE SET chunk_count = excluded.chunk_count",
            params![blob_hash, salt, chunk_count as i64],
        )?;
        Ok(())
    }

    /// Drop every `chunk_status` row for `blob_hash` (any salt) — the
    /// sqlite half of the orphan-sweep pair (`semantic::indexer`'s ref-count
    /// pass). MUST be called together with the paired
    /// `ChunkStore::delete_chunks_for_blob` lance write, in either order,
    /// but both must happen — see the migration's doc.
    pub fn clear_chunk_status_for_blob(&self, blob_hash: &str) -> Result<()> {
        self.lock().execute(
            "DELETE FROM chunk_status WHERE blob_hash = ?1",
            params![blob_hash],
        )?;
        Ok(())
    }

    /// Every `(blob_hash, salt)` this daemon has ever recorded as chunked
    /// that NO current `files` row (in ANY repo — blob_hash sharing is
    /// global, ADR-2) still points at — the semantic indexer's orphan-sweep
    /// candidate list. One indexed `NOT EXISTS` query (`files.blob_hash` is
    /// already indexed — `idx_files_blob_hash`, V0001), cheap enough to run
    /// on every `reindex_repo_incremental` tick regardless of which repo
    /// triggered it (repo-agnostic by design: a blob orphaned in one repo
    /// might still be referenced by another, and `NOT EXISTS` already
    /// accounts for that).
    pub fn orphaned_chunk_blobs(&self) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT cs.blob_hash, cs.salt FROM chunk_status cs
             WHERE NOT EXISTS (SELECT 1 FROM files f WHERE f.blob_hash = cs.blob_hash)",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- commit_sessions (W3.2 join ladder cache) --------------------------
    //
    // No `bump_generation()` calls here either, same rationale as the
    // transcripts section below: `generation` only exists to invalidate the
    // files/symbols search lanes' in-memory caches, which the join ladder
    // has nothing to do with.

    /// Read-through cache lookup for `(repo_id, sha)` — `sha` MUST already be
    /// the canonical full hex sha (`join::local::resolve_local` disambiguates
    /// any short prefix before this is ever called). `None` on a cache miss;
    /// freshness (`trailer`/`exact` permanent, `fuzzy`/`none` TTL'd) is the
    /// caller's decision (`join::ladder::is_fresh`), not this method's — the
    /// store hands back whatever's on disk, stale or not.
    pub fn get_commit_session(&self, repo_id: i64, sha: &str) -> Result<Option<CommitSessionRow>> {
        self.lock()
            .query_row(
                "SELECT confidence, via, session_id, kb, display_name, started_at, resolved_at
                 FROM commit_sessions WHERE repo_id = ?1 AND sha = ?2",
                params![repo_id, sha],
                |r| {
                    Ok(CommitSessionRow {
                        confidence: r.get(0)?,
                        via: r.get(1)?,
                        session_id: r.get(2)?,
                        kb: r.get(3)?,
                        display_name: r.get(4)?,
                        started_at: r.get(5)?,
                        resolved_at: r.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Upsert one `(repo_id, sha)` resolution — always overwrites in place
    /// (a re-resolution, whether an upgrade from `none`→`fuzzy` or a
    /// same-confidence refresh that just bumps `resolved_at`, replaces every
    /// column; there is no partial-update path).
    pub fn upsert_commit_session(
        &self,
        repo_id: i64,
        sha: &str,
        row: &CommitSessionRow,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO commit_sessions
                (repo_id, sha, confidence, via, session_id, kb, display_name, started_at, resolved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(repo_id, sha) DO UPDATE SET
                confidence = excluded.confidence,
                via = excluded.via,
                session_id = excluded.session_id,
                kb = excluded.kb,
                display_name = excluded.display_name,
                started_at = excluded.started_at,
                resolved_at = excluded.resolved_at",
            params![
                repo_id,
                sha,
                row.confidence,
                row.via,
                row.session_id,
                row.kb,
                row.display_name,
                row.started_at,
                row.resolved_at,
            ],
        )?;
        Ok(())
    }

    // --- transcripts (W2.5) ------------------------------------------------
    //
    // Deliberately NO `bump_generation()` calls in this section: `generation`
    // exists to invalidate the files/symbols search lanes' in-memory caches
    // (see the field doc above), which have nothing to do with transcripts.
    // The live transcripts watcher tails frequently (every debounced flush
    // of every configured project's activity); bumping the shared counter
    // on every one of those writes would force needless cache rebuilds on
    // the UNRELATED files/symbols lanes for every daemon that has both
    // features live at once.

    /// Tail-state lookup by `src_file` (root-relative, already unique per
    /// the `V0003` schema) — `transcripts::indexer::tail_file`'s
    /// "have I seen this file before, and from where do I resume" check.
    pub fn get_transcript_file(&self, src_file: &str) -> Result<Option<TranscriptFileRow>> {
        self.lock()
            .query_row(
                "SELECT id, inode, byte_offset, mtime FROM transcript_files WHERE src_file = ?1",
                params![src_file],
                |r| {
                    Ok(TranscriptFileRow {
                        id: r.get(0)?,
                        inode: r.get(1)?,
                        byte_offset: r.get(2)?,
                        mtime: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Idempotent upsert of one file's tail state. Called BOTH to create the
    /// row on first sight (`byte_offset = 0`) and, at the end of every
    /// `tail_file` call, to record the new authoritative offset — always the
    /// SAME statement, `ON CONFLICT` decides which case applies. Returns the
    /// row id (stable across calls for a given `src_file`, since it's the
    /// table's own UNIQUE key).
    pub fn upsert_transcript_file_state(
        &self,
        project_dir: &str,
        src_file: &str,
        inode: i64,
        byte_offset: i64,
        mtime: i64,
    ) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO transcript_files (project_dir, src_file, inode, byte_offset, mtime)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(src_file) DO UPDATE SET
                inode = excluded.inode,
                byte_offset = excluded.byte_offset,
                mtime = excluded.mtime",
            params![project_dir, src_file, inode, byte_offset, mtime],
        )?;
        let id = conn.query_row(
            "SELECT id FROM transcript_files WHERE src_file = ?1",
            params![src_file],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    /// Insert every turn in `turns` for `file_id` — `transcript_turns` plus
    /// the matching `transcript_fts` row (explicit `rowid` = the freshly
    /// inserted turn's own id), in ONE transaction. See the `V0003`
    /// migration's doc for why this hand-paired insert (not a trigger) is
    /// the whole "manual insert discipline" this schema relies on. A no-op
    /// (not an error) on an empty slice.
    pub fn insert_transcript_turns(&self, file_id: i64, turns: &[IndexedTurn]) -> Result<()> {
        if turns.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        {
            let mut ins_turn = tx.prepare(
                "INSERT INTO transcript_turns
                    (file_id, session_id, uuid, parent_uuid, ts, kind, tool_name, file_paths, is_sidechain, byte_offset, byte_len)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            let mut ins_fts =
                tx.prepare("INSERT INTO transcript_fts(rowid, text) VALUES (?1, ?2)")?;
            for it in turns {
                let file_paths_json = serde_json::to_string(&it.turn.file_paths)?;
                ins_turn.execute(params![
                    file_id,
                    it.turn.session_id,
                    it.turn.uuid,
                    it.turn.parent_uuid,
                    it.turn.ts,
                    it.turn.kind,
                    it.turn.tool_name,
                    file_paths_json,
                    it.turn.is_sidechain as i64,
                    it.byte_offset,
                    it.byte_len,
                ])?;
                let turn_id = tx.last_insert_rowid();
                ins_fts.execute(params![turn_id, it.turn.text])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Delete every `transcript_turns` row (and its paired `transcript_fts`
    /// row) for `file_id` — `tail_file`'s inode-swap-forces-a-full-reparse
    /// path, always called BEFORE the fresh re-derive so no stale hit can
    /// ever be visible even momentarily. The `transcript_fts` delete runs
    /// FIRST (its subquery reads `transcript_turns` — which must still
    /// exist to resolve the id list) inside the same transaction as the
    /// `transcript_turns` delete. Legal on the `content=''` FTS5 table only
    /// because the schema sets `contentless_delete=1` — see the migration's
    /// doc.
    pub fn delete_transcript_turns_for_file(&self, file_id: i64) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM transcript_fts WHERE rowid IN (SELECT id FROM transcript_turns WHERE file_id = ?1)",
            params![file_id],
        )?;
        tx.execute(
            "DELETE FROM transcript_turns WHERE file_id = ?1",
            params![file_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// FTS5 `MATCH` over every indexed turn, optionally scoped to `session`/
    /// `kind`, newest-first (`ts DESC`, `id DESC` as a same-millisecond
    /// tie-break) — `routes::search_transcripts`' data source. `query` is
    /// passed to sqlite's FTS5 query parser verbatim (its own boolean/
    /// prefix/NEAR syntax); a malformed query surfaces as a `StoreError`
    /// the caller can inspect (`ApiError`'s `fts5` substring check maps it
    /// to 400, everything else to 500).
    pub fn search_transcripts(
        &self,
        query: &str,
        limit: usize,
        session: Option<&str>,
        kind: Option<&str>,
    ) -> Result<Vec<TranscriptSearchRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT tt.session_id, tt.uuid, tt.ts, tt.kind, tt.tool_name, tf.project_dir,
                    tf.src_file, tt.byte_offset, tt.byte_len, tt.is_sidechain
             FROM transcript_fts f
             JOIN transcript_turns tt ON tt.id = f.rowid
             JOIN transcript_files tf ON tf.id = tt.file_id
             WHERE f.text MATCH ?1
               AND (?2 IS NULL OR tt.session_id = ?2)
               AND (?3 IS NULL OR tt.kind = ?3)
             ORDER BY tt.ts DESC, tt.id DESC
             LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![query, session, kind, limit as i64], |r| {
                Ok(TranscriptSearchRow {
                    session_id: r.get(0)?,
                    uuid: r.get(1)?,
                    ts: r.get(2)?,
                    kind: r.get(3)?,
                    tool_name: r.get(4)?,
                    project_dir: r.get(5)?,
                    src_file: r.get(6)?,
                    byte_offset: r.get(7)?,
                    byte_len: r.get(8)?,
                    is_sidechain: r.get::<_, i64>(9)? != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every indexed turn for `session`, OLDEST FIRST (`ts ASC, id ASC` —
    /// the session's own NARRATIVE order, not `search_transcripts`'
    /// newest-first convention) — `sessiondiff::session_diff`'s data
    /// source (W3.5, the session diff's transcript half). Unlike
    /// `search_transcripts`, this is NOT an FTS5 `MATCH` — it returns
    /// EVERY turn for the session, matched or not, since session-diff walks
    /// the whole narrative rather than searching it. Carries `file_paths`
    /// (JSON-decoded back to a `Vec<String>` — see `insert_transcript_turns`'
    /// encode side) which `search_transcripts`/`TranscriptSearchRow` has no
    /// need for, so this is a distinct row type rather than a widened
    /// `TranscriptSearchRow`.
    pub fn transcript_turns_for_session(&self, session: &str) -> Result<Vec<TranscriptTurnRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT tt.session_id, tt.uuid, tt.parent_uuid, tt.ts, tt.kind, tt.tool_name,
                    tt.file_paths, tt.is_sidechain, tf.project_dir, tf.src_file,
                    tt.byte_offset, tt.byte_len
             FROM transcript_turns tt
             JOIN transcript_files tf ON tf.id = tt.file_id
             WHERE tt.session_id = ?1
             ORDER BY tt.ts ASC, tt.id ASC",
        )?;
        let rows = stmt
            .query_map(params![session], |r| {
                let file_paths_json: String = r.get(6)?;
                Ok((
                    TranscriptTurnRow {
                        session_id: r.get(0)?,
                        uuid: r.get(1)?,
                        parent_uuid: r.get(2)?,
                        ts: r.get(3)?,
                        kind: r.get(4)?,
                        tool_name: r.get(5)?,
                        file_paths: Vec::new(),
                        is_sidechain: r.get::<_, i64>(7)? != 0,
                        project_dir: r.get(8)?,
                        src_file: r.get(9)?,
                        byte_offset: r.get(10)?,
                        byte_len: r.get(11)?,
                    },
                    file_paths_json,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(mut row, file_paths_json)| {
                row.file_paths = serde_json::from_str(&file_paths_json).unwrap_or_default();
                row
            })
            .collect())
    }

    /// Files tracked / turns indexed / total indexed bytes — `kb-code
    /// transcripts status`'s data source (`GET /api/transcripts/status`).
    /// `indexed_bytes` sums `transcript_turns.byte_len` (the raw JSONL
    /// bytes the index POINTS AT, not the FTS5 shadow tables' own on-disk
    /// footprint — the latter needs sqlite's `dbstat` virtual table, which
    /// isn't guaranteed compiled into every `libsqlite3-sys` build; the
    /// summed byte_len is a always-available, honest proxy for "how much
    /// transcript content is covered").
    pub fn transcript_stats(&self) -> Result<TranscriptStats> {
        let conn = self.lock();
        let files: i64 =
            conn.query_row("SELECT COUNT(*) FROM transcript_files", [], |r| r.get(0))?;
        let turns: i64 =
            conn.query_row("SELECT COUNT(*) FROM transcript_turns", [], |r| r.get(0))?;
        let indexed_bytes: i64 = conn.query_row(
            "SELECT COALESCE(SUM(byte_len), 0) FROM transcript_turns",
            [],
            |r| r.get(0),
        )?;
        Ok(TranscriptStats {
            files: files as u64,
            turns: turns as u64,
            indexed_bytes: indexed_bytes as u64,
        })
    }

    /// W3.4 — `provenance::why`'s UNCOMMITTED-line path: every
    /// `transcript_turns` row whose JSON-encoded `file_paths`
    /// (`transcripts::parse`'s module doc) contains `abs_path` VERBATIM as a
    /// complete JSON string element, newest-first, capped at `limit`. A
    /// plain `LIKE '%"<path>"%'` scan (`%`/`_` escaped so a path containing
    /// either can't smuggle in a stray wildcard) — the quote-delimited
    /// needle means a match can only land on a COMPLETE `file_paths` entry,
    /// never a partial-path false-positive (`".../lib.rs"` never matches a
    /// stored `".../lib.rs.bak"`, since the trailing quote after `lib.rs`
    /// would have to be part of the stored string too). No FTS index backs
    /// this column (unlike `search_transcripts`'s `text` column) — a leading
    /// wildcard can't use one anyway; acceptable for a v1, pull-only,
    /// single-operator-scale scan (see `provenance::why`'s module doc for
    /// why this is the ONE place outside the transcripts lane's own routes
    /// that reads this table).
    pub fn transcript_sessions_touching_path(
        &self,
        abs_path: &str,
        limit: usize,
    ) -> Result<Vec<TranscriptPathHit>> {
        let conn = self.lock();
        let escaped = abs_path
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let needle = format!("%\"{escaped}\"%");
        let mut stmt = conn.prepare(
            "SELECT session_id, ts FROM transcript_turns
             WHERE file_paths LIKE ?1 ESCAPE '\\'
             ORDER BY ts DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![needle, limit as i64], |r| {
                Ok(TranscriptPathHit {
                    session_id: r.get(0)?,
                    ts: r.get(1)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- annotations (W4.6 migration V0006; D-server migration V0008 adds
    // anchor kinds/threads/intents) -------------------------------------
    //
    // Deliberately NO `bump_generation()` calls in this section — same
    // rationale as the commit_sessions/transcripts sections above:
    // `generation` only invalidates the files/symbols search lanes' caches,
    // which annotations have nothing to do with.
    //
    // Every SELECT below spells out the SAME 17-column order (matching
    // `annotation_row_from`'s positional `r.get(0..16)` reads — V70-A10
    // appended `set_id` LAST, at index 16, after `side`) rather than
    // pulling it into a shared string constant — mirrors this file's
    // existing convention of repeating a table's column list per query
    // (see e.g. `symbols_for_blob`/`symbols_for_repo`) rather than
    // factoring it out.

    /// Insert one annotation row. `id` is caller-minted
    /// (`annotations::new_annotation_id`) — the store never invents ids.
    pub fn insert_annotation(&self, row: &AnnotationRow) -> Result<()> {
        self.lock().execute(
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

    /// Every annotation for `(repo_id, path)` — PARENTS and their REPLIES
    /// alike (a reply shares its parent's `repo_id`/`path`, see
    /// `routes::create_annotation`'s reply branch) — oldest-first (creation
    /// order). `GET /api/annotations?repo=&path=`'s data source; the route
    /// re-resolves each row's anchor against the CURRENT working-tree
    /// content (`crate::annotations::resolve` and friends), so this is a
    /// plain, unenriched read.
    pub fn list_annotations(&self, repo_id: i64, path: &str) -> Result<Vec<AnnotationRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, repo_id, path, anchor, anchor_kind, anchor2, parent_id, intent,
                    body, author, created_at, updated_at, resolved,
                    review_id, ps_number, side, set_id
             FROM annotations WHERE repo_id = ?1 AND path = ?2
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id, path], annotation_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// V70-A10 — every annotation scoped to `set_id` (general path-less
    /// notes, `annotations::ANCHOR_KIND_SET`, AND ordinary code-anchored
    /// comments alike) — PARENTS and their REPLIES alike (a reply inherits
    /// its parent's `set_id`, `routes::assemble_reply_annotation`'s doc),
    /// oldest-first (creation order — same convention as
    /// `list_annotations`). `GET /api/annotations?set_id=`'s data source;
    /// the route re-resolves each row's anchor the same way
    /// `list_annotations`'s caller does.
    pub fn list_annotations_by_set(&self, set_id: &str) -> Result<Vec<AnnotationRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, repo_id, path, anchor, anchor_kind, anchor2, parent_id, intent,
                    body, author, created_at, updated_at, resolved,
                    review_id, ps_number, side, set_id
             FROM annotations WHERE set_id = ?1
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt
            .query_map(params![set_id], annotation_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Single annotation lookup by id — `PATCH`/`DELETE /api/annotations/
    /// {id}`'s existence check + the PATCH route's post-update re-read, and
    /// `routes::create_annotation`'s reply-parent validation
    /// (exists / not-itself-a-reply).
    pub fn get_annotation(&self, id: &str) -> Result<Option<AnnotationRow>> {
        self.lock()
            .query_row(
                "SELECT id, repo_id, path, anchor, anchor_kind, anchor2, parent_id, intent,
                        body, author, created_at, updated_at, resolved,
                        review_id, ps_number, side, set_id
                 FROM annotations WHERE id = ?1",
                params![id],
                annotation_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Update whichever of `body`/`resolved`/`intent` is `Some` (a
    /// `COALESCE` — the omitted field is left untouched), always stamping
    /// `updated_at`. NEVER touches `anchor`/`anchor_kind`/`anchor2`/
    /// `parent_id` — the v1 "PATCH never changes anchors" rule, preserved
    /// through D-server (see `routes::patch_annotation`'s doc). Returns
    /// `true` iff a row with `id` existed (the route's 404 check).
    pub fn update_annotation(
        &self,
        id: &str,
        body: Option<&str>,
        resolved: Option<bool>,
        intent: Option<&str>,
        updated_at: i64,
    ) -> Result<bool> {
        let n = self.lock().execute(
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

    /// Hard-delete one annotation AND cascade to its replies (D-server —
    /// `parent_id` has no SQL `ON DELETE CASCADE`, see the migration's doc,
    /// so this does it in code: both deletes run in ONE transaction so a
    /// crash between them can never leave an orphaned reply behind). A
    /// no-op delete on `id`'s own replies clause (a reply has none — one
    /// level of nesting only) makes this safe to call on ANY row, parent or
    /// reply alike, without a branch. Returns `true` iff `id` itself
    /// existed.
    pub fn delete_annotation(&self, id: &str) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        // V4.C1 — suggestions have no SQL FK (annotation_id is a TEXT PK
        // only). Drop the parent row's suggestion AND every reply's
        // suggestion before the annotation deletes, so a crash between
        // statements can never leave a suggestion pointing at a gone id.
        tx.execute(
            "DELETE FROM annotation_suggestions
             WHERE annotation_id = ?1
                OR annotation_id IN (SELECT id FROM annotations WHERE parent_id = ?1)",
            params![id],
        )?;
        tx.execute("DELETE FROM annotations WHERE parent_id = ?1", params![id])?;
        let n = tx.execute("DELETE FROM annotations WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// D-server — every UNRESOLVED, TOP-LEVEL (`parent_id IS NULL`)
    /// annotation for `repo_id`, optionally filtered by an exact `intent`
    /// match and/or a `path_prefix`, newest-first, each paired with its own
    /// direct reply COUNT (a correlated scalar subquery — this table is
    /// small enough per repo that this is simpler than a `GROUP BY` plus a
    /// separate zero-fill pass for parents with no replies).
    ///
    /// Replies are excluded from the listing itself: a reply has no anchor
    /// of its own to resolve independently (see `crate::annotations`'s
    /// module doc), so a flat CROSS-PATH list only makes sense over the
    /// anchored, top-level rows — a caller wanting a thread's replies
    /// already has `GET /api/annotations?repo=&path=` (`list_annotations`
    /// above), which includes them.
    ///
    /// `limit_plus_one` is `routes::list_open_annotations`'s
    /// caller-configured cap PLUS one: fetching one extra row lets the
    /// route detect truncation (`rows.len() > cap`) without a second
    /// `COUNT(*)` round trip.
    pub fn list_open_annotations(
        &self,
        repo_id: i64,
        intent: Option<&str>,
        path_prefix: Option<&str>,
        limit_plus_one: usize,
    ) -> Result<Vec<(AnnotationRow, i64)>> {
        let conn = self.lock();
        // Same `\`-escape convention as `transcript_sessions_touching_path`
        // (escape the caller's own `%`/`_` so a path containing either
        // can't smuggle in a stray SQL wildcard) — a plain prefix match,
        // `%` appended AFTER escaping the caller's text.
        let like = path_prefix.map(|p| {
            let escaped = p
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            format!("{escaped}%")
        });
        let mut stmt = conn.prepare(
            "SELECT a.id, a.repo_id, a.path, a.anchor, a.anchor_kind, a.anchor2, a.parent_id,
                    a.intent, a.body, a.author, a.created_at, a.updated_at, a.resolved,
                    a.review_id, a.ps_number, a.side, a.set_id,
                    (SELECT COUNT(*) FROM annotations r WHERE r.parent_id = a.id) AS reply_count
             FROM annotations a
             WHERE a.repo_id = ?1 AND a.resolved = 0 AND a.parent_id IS NULL
               AND (?2 IS NULL OR a.intent = ?2)
               AND (?3 IS NULL OR a.path LIKE ?3 ESCAPE '\\')
             ORDER BY a.created_at DESC, a.id DESC
             LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![repo_id, intent, like, limit_plus_one as i64], |r| {
                Ok((annotation_row_from(r)?, r.get::<_, i64>(17)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// S2-A ("One Inbox," kb-code v6.0) — every OPEN, TOP-LEVEL,
    /// WORKING-TREE (`review_id IS NULL`) annotation for `repo_id` whose
    /// `intent` is IN `intents`, newest `updated_at` first, each paired
    /// with its own direct reply count. Mirrors [`Self::list_open_
    /// annotations`]'s shape but adds two things that fn doesn't do: the
    /// `review_id IS NULL` filter (a review-scoped open thread already has
    /// its own lane, `review_inbox::compose_rows`'s `unanswered_questions`
    /// — this fn must never double-count it into the unified inbox's
    /// SEPARATE `annotations` lane) and an intent SET rather than one
    /// exact value (`unified_inbox`'s pinned `question`/`flag-for-agent`
    /// pair). `intents` empty ⇒ empty result, no query run (mirrors
    /// [`Self::list_open_annotations_on_paths`]'s empty-input short
    /// circuit). `limit_plus_one` is the caller's cap PLUS one, same
    /// truncation-detection convention as `list_open_annotations`.
    pub fn list_open_working_tree_annotations(
        &self,
        repo_id: i64,
        intents: &[&str],
        limit_plus_one: usize,
    ) -> Result<Vec<(AnnotationRow, i64)>> {
        if intents.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        // `?1` is repo_id, `?2` is limit — intent placeholders start at ?3
        // (mirrors `symbol_count_for_repo`'s `?1`-offset convention).
        let placeholders = (0..intents.len())
            .map(|i| format!("?{}", i + 3))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT a.id, a.repo_id, a.path, a.anchor, a.anchor_kind, a.anchor2, a.parent_id,
                    a.intent, a.body, a.author, a.created_at, a.updated_at, a.resolved,
                    a.review_id, a.ps_number, a.side, a.set_id,
                    (SELECT COUNT(*) FROM annotations r WHERE r.parent_id = a.id) AS reply_count
             FROM annotations a
             WHERE a.repo_id = ?1 AND a.resolved = 0 AND a.parent_id IS NULL
               AND a.review_id IS NULL
               AND a.intent IN ({placeholders})
             ORDER BY a.updated_at DESC, a.id DESC
             LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params_vec: Vec<rusqlite::types::Value> = Vec::with_capacity(2 + intents.len());
        params_vec.push(rusqlite::types::Value::Integer(repo_id));
        params_vec.push(rusqlite::types::Value::Integer(limit_plus_one as i64));
        for intent in intents {
            params_vec.push(rusqlite::types::Value::Text((*intent).to_string()));
        }
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params_vec), |r| {
                Ok((annotation_row_from(r)?, r.get::<_, i64>(17)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// V4.C1 — every review-scoped annotation for `review_id` (parents
    /// AND their replies), creation order. When `include_resolved` is
    /// false, a resolved top-level thread is omitted wholesale (the
    /// parent and every reply under it). `GET /api/reviews/{id}/comments`
    /// is the only caller; resolution is computed lazily there, never
    /// here. Never `bump_generation`.
    pub fn list_review_annotations(
        &self,
        review_id: i64,
        include_resolved: bool,
    ) -> Result<Vec<AnnotationRow>> {
        let conn = self.lock();
        // Two explicit queries rather than a bind-time boolean in SQL:
        // the unresolved-thread filter has to walk parent_id, and a
        // single statement with `(?2 OR …)` would still have to name
        // that subquery. Creation order matches `list_annotations`.
        let sql = if include_resolved {
            "SELECT id, repo_id, path, anchor, anchor_kind, anchor2, parent_id, intent,
                    body, author, created_at, updated_at, resolved,
                    review_id, ps_number, side, set_id
             FROM annotations
             WHERE review_id = ?1
             ORDER BY created_at ASC, id ASC"
        } else {
            "SELECT id, repo_id, path, anchor, anchor_kind, anchor2, parent_id, intent,
                    body, author, created_at, updated_at, resolved,
                    review_id, ps_number, side, set_id
             FROM annotations
             WHERE review_id = ?1
               AND (
                 (parent_id IS NULL AND resolved = 0)
                 OR parent_id IN (
                   SELECT id FROM annotations
                   WHERE review_id = ?1 AND parent_id IS NULL AND resolved = 0
                 )
               )
             ORDER BY created_at ASC, id ASC"
        };
        // PF-K1 — exactly two distinct SQL texts ever flow through here
        // (the `include_resolved` branch above), so `prepare_cached` keys
        // cleanly on either one.
        let mut stmt = conn.prepare_cached(sql)?;
        let rows = stmt
            .query_map(params![review_id], annotation_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// [`Self::list_review_annotations`] for a whole SET of `review_ids` —
    /// one query (dynamic `IN (…)`, plain `prepare`) instead of N round
    /// trips. A review with no annotations is simply absent from the map.
    /// `review_ids` are bound via NUMBERED placeholders (`?1..?N`) so the
    /// `include_resolved=false` branch can reference the same set TWICE
    /// (the outer `WHERE` and the open-thread subquery) while binding the
    /// values only once — SQLite reuses a numbered parameter's bound value
    /// on every occurrence of that number in the statement text.
    pub fn list_review_annotations_batch(
        &self,
        review_ids: &[i64],
        include_resolved: bool,
    ) -> Result<HashMap<i64, Vec<AnnotationRow>>> {
        let mut out: HashMap<i64, Vec<AnnotationRow>> = HashMap::new();
        if review_ids.is_empty() {
            return Ok(out);
        }
        let conn = self.lock();
        let placeholders = (0..review_ids.len())
            .map(|i| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = if include_resolved {
            format!(
                "SELECT id, repo_id, path, anchor, anchor_kind, anchor2, parent_id, intent,
                        body, author, created_at, updated_at, resolved,
                        review_id, ps_number, side, set_id
                 FROM annotations
                 WHERE review_id IN ({placeholders})
                 ORDER BY review_id ASC, created_at ASC, id ASC"
            )
        } else {
            format!(
                "SELECT id, repo_id, path, anchor, anchor_kind, anchor2, parent_id, intent,
                        body, author, created_at, updated_at, resolved,
                        review_id, ps_number, side, set_id
                 FROM annotations
                 WHERE review_id IN ({placeholders})
                   AND (
                     (parent_id IS NULL AND resolved = 0)
                     OR parent_id IN (
                       SELECT id FROM annotations
                       WHERE review_id IN ({placeholders}) AND parent_id IS NULL AND resolved = 0
                     )
                   )
                 ORDER BY review_id ASC, created_at ASC, id ASC"
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(review_ids.iter()),
                annotation_row_from,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for row in rows {
            out.entry(row.review_id.unwrap_or_default())
                .or_default()
                .push(row);
        }
        Ok(out)
    }

    /// V4.C1/C2 — lookup of a suggestion row.
    pub fn get_annotation_suggestion(
        &self,
        annotation_id: &str,
    ) -> Result<Option<AnnotationSuggestionRow>> {
        self.lock()
            .query_row(
                "SELECT annotation_id, replacement, original, base_blob_sha,
                        applied, applied_at, applied_head_sha, created_at, updated_at
                 FROM annotation_suggestions WHERE annotation_id = ?1",
                params![annotation_id],
                annotation_suggestion_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// V4.C2 — insert or replace a suggestion. Re-PUT resets `applied` /
    /// `applied_at` / `applied_head_sha` and preserves `created_at`.
    /// Never `bump_generation`.
    pub fn upsert_annotation_suggestion(
        &self,
        annotation_id: &str,
        replacement: &str,
        original: &str,
        base_blob_sha: &str,
        now: i64,
    ) -> Result<()> {
        self.lock().execute(
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

    /// V4.C2 — drop a suggestion row. Returns `true` iff a row existed.
    /// Never `bump_generation`.
    pub fn delete_annotation_suggestion(&self, annotation_id: &str) -> Result<bool> {
        let n = self.lock().execute(
            "DELETE FROM annotation_suggestions WHERE annotation_id = ?1",
            params![annotation_id],
        )?;
        Ok(n > 0)
    }

    /// V4.C2 / S1 — stamp `applied=1` on an existing suggestion.
    /// Returns `true` iff a row existed. Kept for the apply route (S1);
    /// this phase only uses it from tests. Never `bump_generation`.
    pub fn mark_annotation_suggestion_applied(
        &self,
        annotation_id: &str,
        applied_at: i64,
        applied_head_sha: &str,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE annotation_suggestions
             SET applied = 1, applied_at = ?2, applied_head_sha = ?3, updated_at = ?2
             WHERE annotation_id = ?1",
            params![annotation_id, applied_at, applied_head_sha],
        )?;
        Ok(n > 0)
    }

    /// V4.C2 — apply a prepared batch of annotation mutations in ONE
    /// transaction. Callers MUST validate every op and finish every git
    /// blob read BEFORE calling this (so this method never does I/O
    /// outside sqlite). Returns how many ops ran, the ids minted for
    /// insert ops (in op order), and whether anything actually changed
    /// (so the route can skip SSE on an all-no-op batch). Never
    /// `bump_generation`.
    pub fn apply_annotation_ops(
        &self,
        ops: &[PreparedAnnotationOp],
        now: i64,
    ) -> Result<AnnotationOpReport> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let mut report = AnnotationOpReport::default();
        for op in ops {
            match op {
                PreparedAnnotationOp::Insert { row, suggestion } => {
                    insert_annotation_on(&tx, row.as_ref())?;
                    if let Some(s) = suggestion {
                        upsert_suggestion_on(
                            &tx,
                            &s.annotation_id,
                            &s.replacement,
                            &s.original,
                            &s.base_blob_sha,
                            now,
                        )?;
                    }
                    report.created_ids.push(row.id.clone());
                    report.changed = true;
                }
                PreparedAnnotationOp::EditBody { id, body } => {
                    let Some(cur) = get_annotation_on(&tx, id)? else {
                        return Err(StoreError::NotFound(format!("annotation {id}")));
                    };
                    if cur.body != *body {
                        update_annotation_on(&tx, id, Some(body), None, None, now)?;
                        report.changed = true;
                    }
                }
                PreparedAnnotationOp::SetIntent { id, intent } => {
                    let Some(cur) = get_annotation_on(&tx, id)? else {
                        return Err(StoreError::NotFound(format!("annotation {id}")));
                    };
                    if cur.intent != *intent {
                        update_annotation_on(&tx, id, None, None, Some(intent), now)?;
                        report.changed = true;
                    }
                }
                PreparedAnnotationOp::SetResolved { id, resolved } => {
                    let Some(cur) = get_annotation_on(&tx, id)? else {
                        return Err(StoreError::NotFound(format!("annotation {id}")));
                    };
                    if cur.resolved != *resolved {
                        update_annotation_on(&tx, id, None, Some(*resolved), None, now)?;
                        report.changed = true;
                    }
                }
                PreparedAnnotationOp::Delete { id } => {
                    if delete_annotation_on(&tx, id)? {
                        report.changed = true;
                    } else {
                        return Err(StoreError::NotFound(format!("annotation {id}")));
                    }
                }
                PreparedAnnotationOp::UpsertSuggestion(s) => {
                    upsert_suggestion_on(
                        &tx,
                        &s.annotation_id,
                        &s.replacement,
                        &s.original,
                        &s.base_blob_sha,
                        now,
                    )?;
                    report.changed = true;
                }
                PreparedAnnotationOp::ClearSuggestion { annotation_id } => {
                    if delete_suggestion_on(&tx, annotation_id)? {
                        report.changed = true;
                    }
                }
            }
            report.applied += 1;
        }
        tx.commit()?;
        Ok(report)
    }

    // --- reading sets (Phase E3) -----------------------------------------
    //
    // Deliberately NO `bump_generation()` calls in this section — same
    // rationale as commit_sessions/transcripts/annotations above:
    // `generation` only invalidates the files/symbols search lanes' caches,
    // which reading sets have nothing to do with.

    /// Insert a new reading set + its initial spans in ONE transaction.
    /// `(repo_id, name)` must be unique — a collision surfaces as
    /// [`StoreError::NameConflict`] (caught via `name_conflict_or` rather
    /// than a separate pre-check SELECT: the store's single-writer `Mutex`
    /// already makes the whole call atomic, so there's nothing a
    /// check-then-insert round trip would buy beyond a second statement).
    /// Spans are assigned `ordinal`s 0..N in slice order. A thin wrapper
    /// over [`Store::create_reading_set_with_provenance`] passing four
    /// `None`s — every caller of THIS function (`create_set`,
    /// `from_session_route`) stays byte-identical (DCB-W3.C) — plus
    /// V70-A10's `kind = "set"` (the pre-existing default, a literal here
    /// rather than importing `reading_sets::SET_KIND_SET`: store.rs never
    /// depends on that module's vocab constants, the same "plain data
    /// plumbing, validation lives above" posture `insert_annotation`
    /// already takes with `anchor_kind`) and three more `None`s for
    /// `desk_json`/`ref`/`description_md`.
    pub fn create_reading_set(
        &self,
        id: &str,
        repo_id: i64,
        name: &str,
        description: Option<&str>,
        spans: &[NewReadingSetSpan],
        now: i64,
    ) -> Result<()> {
        self.create_reading_set_with_provenance(
            id,
            repo_id,
            name,
            description,
            spans,
            now,
            None,
            None,
            None,
            None,
            "set",
            None,
            None,
            None,
        )
    }

    /// DCB-W3.C — `create_reading_set` plus four nullable doc-materialization
    /// provenance columns (`V0022__reading_sets_doc_provenance.sql`),
    /// written ONLY by `reading_sets::from_doc_route`. A genuinely new
    /// method rather than a boolean flag on `create_reading_set` — every
    /// OTHER caller must stay byte-identical, so `create_reading_set` is
    /// reduced to a thin wrapper passing four `None`s instead of every call
    /// site growing four new arguments.
    ///
    /// V70-A10 ("Workspaces v0") widens this ONE already-kitchen-sink
    /// constructor further rather than minting a third tier: `kind`
    /// (`reading_sets::is_valid_set_kind`-validated at the route boundary,
    /// never here), `desk_json`/`ref`/`description_md`
    /// (`V0028__workspaces.sql`). Every existing call site
    /// (`create_reading_set` above, `reading_sets::from_doc_route`) passes
    /// `"set"`/`None`/`None`/`None` explicitly — a mechanical,
    /// behavior-preserving widening, not a new default anywhere.
    #[allow(clippy::too_many_arguments)]
    pub fn create_reading_set_with_provenance(
        &self,
        id: &str,
        repo_id: i64,
        name: &str,
        description: Option<&str>,
        spans: &[NewReadingSetSpan],
        now: i64,
        source_kb: Option<&str>,
        source_doc_id: Option<&str>,
        source_doc_path: Option<&str>,
        source_doc_hash: Option<&str>,
        kind: &str,
        desk_json: Option<&str>,
        ref_label: Option<&str>,
        description_md: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        if let Err(e) = tx.execute(
            "INSERT INTO reading_sets
                (id, repo_id, name, description, created_at, updated_at,
                 source_kb, source_doc_id, source_doc_path, source_doc_hash,
                 kind, desk_json, ref, description_md)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                id,
                repo_id,
                name,
                description,
                now,
                source_kb,
                source_doc_id,
                source_doc_path,
                source_doc_hash,
                kind,
                desk_json,
                ref_label,
                description_md,
            ],
        ) {
            return Err(name_conflict_or(e, name));
        }
        {
            // Same "repeat the insert loop rather than factor it into a
            // shared helper" convention as `replace_symbols`/
            // `replace_occurrences` above.
            let mut stmt = tx.prepare(
                "INSERT INTO reading_set_spans
                    (set_id, ordinal, path, line_start, line_end, ref, note)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (i, span) in spans.iter().enumerate() {
                stmt.execute(params![
                    id,
                    i as i64,
                    span.path,
                    span.line_start,
                    span.line_end,
                    span.git_ref,
                    span.note,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every reading set for `repo_id`, alphabetical by name, each paired
    /// with its own span count AND note count (two correlated scalar
    /// subqueries — same convention as `list_open_annotations`'s
    /// `reply_count`) — `GET /api/sets?repo=`'s data source. V70-A10 adds
    /// the optional `kind` filter: `None` (every pre-A10 caller) means
    /// `kind = 'set'` — the pre-existing default and the ONLY kind that
    /// existed before this unit — so an omitted filter is BYTE-IDENTICAL to
    /// the old unconditional listing (there was nothing else to list
    /// before). `Some("workspace")` (or any other valid kind) is an exact
    /// match. This is a filter, never a free string: `reading_sets::
    /// list_sets` validates `kind` against `is_valid_set_kind` before it
    /// ever reaches here.
    pub fn list_reading_sets(
        &self,
        repo_id: i64,
        kind: Option<&str>,
    ) -> Result<Vec<(ReadingSetRow, i64, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.repo_id, s.name, s.description, s.created_at, s.updated_at,
                    s.source_kb, s.source_doc_id, s.source_doc_path, s.source_doc_hash,
                    s.kind, s.desk_json, s.ref, s.description_md, s.workspace_id,
                    (SELECT COUNT(*) FROM reading_set_spans sp WHERE sp.set_id = s.id)
                        AS span_count,
                    (SELECT COUNT(*) FROM annotations a WHERE a.set_id = s.id)
                        AS note_count
             FROM reading_sets s
             WHERE s.repo_id = ?1 AND s.kind = ?2
             ORDER BY s.name ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id, kind.unwrap_or("set")], |r| {
                Ok((
                    reading_set_row_from(r)?,
                    r.get::<_, i64>(15)?,
                    r.get::<_, i64>(16)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Single set lookup by id — every route's (`GET`/`PATCH`/`DELETE
    /// /api/sets/{id}`, `POST /api/sets/{id}/spans`, `GET /api/pack?set=`)
    /// existence check.
    pub fn get_reading_set(&self, id: &str) -> Result<Option<ReadingSetRow>> {
        self.lock()
            .query_row(
                "SELECT id, repo_id, name, description, created_at, updated_at,
                        source_kb, source_doc_id, source_doc_path, source_doc_hash,
                        kind, desk_json, ref, description_md, workspace_id
                 FROM reading_sets WHERE id = ?1",
                params![id],
                reading_set_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Every span for `set_id`, in `ordinal` order — `GET /api/sets/{id}`'s
    /// and `GET /api/pack?set=`'s data source.
    pub fn reading_set_spans(&self, set_id: &str) -> Result<Vec<ReadingSetSpanRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT ordinal, path, line_start, line_end, ref, note
             FROM reading_set_spans WHERE set_id = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt
            .query_map(params![set_id], reading_set_span_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Update whichever of `name`/`description`/`kind`/`desk_json`/`ref`/
    /// `description_md` is `Some` (`COALESCE`, same convention as
    /// `update_annotation`), always stamping `updated_at`. A `name`
    /// collision with a DIFFERENT existing set in the same repo surfaces as
    /// [`StoreError::NameConflict`] (caught the same way `create_reading_set`
    /// does). Never touches the span list — that's `replace_reading_set_
    /// spans`'s job. Returns `true` iff `id` existed.
    ///
    /// V70-A10 widens this from two fields to six — `kind`/`desk_json`/
    /// `ref_label`/`description_md` (`V0028__workspaces.sql`) — rather than
    /// a second `update_workspace_meta` method: this is still one row's
    /// meta columns, COALESCE-updated the same way `name`/`description`
    /// always were, and `patch_set`'s route (its ONE call site) already
    /// validates each field before calling in. Like `name`/`description`,
    /// COALESCE means a `Some` sets the field and a `None` leaves it
    /// untouched — there is no "explicitly clear to NULL" here, same
    /// limitation the pre-existing two fields already had.
    #[allow(clippy::too_many_arguments)]
    pub fn update_reading_set_meta(
        &self,
        id: &str,
        name: Option<&str>,
        description: Option<&str>,
        kind: Option<&str>,
        desk_json: Option<&str>,
        ref_label: Option<&str>,
        description_md: Option<&str>,
        updated_at: i64,
    ) -> Result<bool> {
        let conn = self.lock();
        match conn.execute(
            "UPDATE reading_sets SET
                name = COALESCE(?2, name),
                description = COALESCE(?3, description),
                kind = COALESCE(?4, kind),
                desk_json = COALESCE(?5, desk_json),
                ref = COALESCE(?6, ref),
                description_md = COALESCE(?7, description_md),
                updated_at = ?8
             WHERE id = ?1",
            params![
                id,
                name,
                description,
                kind,
                desk_json,
                ref_label,
                description_md,
                updated_at
            ],
        ) {
            Ok(n) => Ok(n > 0),
            Err(e) => Err(name_conflict_or(e, name.unwrap_or_default())),
        }
    }

    /// Replace the FULL span list for `set_id` — delete then re-insert in
    /// one transaction (same "no stable per-span identity to diff against"
    /// rationale as `replace_symbols`), also stamping `updated_at`. Returns
    /// `true` iff `set_id` existed (checked via the `reading_sets` UPDATE's
    /// own row count — no separate SELECT).
    pub fn replace_reading_set_spans(
        &self,
        set_id: &str,
        spans: &[NewReadingSetSpan],
        updated_at: i64,
    ) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE reading_sets SET updated_at = ?2 WHERE id = ?1",
            params![set_id, updated_at],
        )?;
        if n == 0 {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM reading_set_spans WHERE set_id = ?1",
            params![set_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO reading_set_spans
                    (set_id, ordinal, path, line_start, line_end, ref, note)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (i, span) in spans.iter().enumerate() {
                stmt.execute(params![
                    set_id,
                    i as i64,
                    span.path,
                    span.line_start,
                    span.line_end,
                    span.git_ref,
                    span.note,
                ])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    /// Append ONE span after `set_id`'s current last ordinal
    /// (`MAX(ordinal) + 1`, `0` for an empty set) — the one span mutation
    /// that does NOT rewrite the whole list, since nothing before it
    /// shifts. Also stamps `updated_at`. Returns the new span's ordinal, or
    /// `None` if `set_id` doesn't exist.
    pub fn append_reading_set_span(
        &self,
        set_id: &str,
        span: &NewReadingSetSpan,
        updated_at: i64,
    ) -> Result<Option<i64>> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE reading_sets SET updated_at = ?2 WHERE id = ?1",
            params![set_id, updated_at],
        )?;
        if n == 0 {
            return Ok(None);
        }
        let next_ordinal: i64 = tx.query_row(
            "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM reading_set_spans WHERE set_id = ?1",
            params![set_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO reading_set_spans
                (set_id, ordinal, path, line_start, line_end, ref, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                set_id,
                next_ordinal,
                span.path,
                span.line_start,
                span.line_end,
                span.git_ref,
                span.note,
            ],
        )?;
        tx.commit()?;
        Ok(Some(next_ordinal))
    }

    /// Hard-delete a reading set AND its spans, both in ONE transaction
    /// (same "no SQL cascade, do it in code" convention as
    /// `delete_annotation`). Returns `true` iff `id` existed.
    ///
    /// V70-A10 widens the same transaction to ALSO drop every annotation
    /// scoped to this set (`annotations.set_id = id`) plus any
    /// `annotation_suggestions` row on one of those annotations — a
    /// workspace's notes have no reason to survive the workspace itself.
    /// This is a plain `WHERE set_id = ?1` sweep, not a parent/reply
    /// two-step: a reply inherits its parent's `set_id`
    /// (`routes::assemble_reply_annotation`'s `inherit_scope_field` ladder,
    /// mirroring `review_id`), so EVERY row belonging to this workspace —
    /// parent and reply alike — already carries the same `set_id`.
    pub fn delete_reading_set(&self, id: &str) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM annotation_suggestions
             WHERE annotation_id IN (SELECT id FROM annotations WHERE set_id = ?1)",
            params![id],
        )?;
        tx.execute("DELETE FROM annotations WHERE set_id = ?1", params![id])?;
        tx.execute(
            "DELETE FROM reading_set_spans WHERE set_id = ?1",
            params![id],
        )?;
        let n = tx.execute("DELETE FROM reading_sets WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    // --- bookmarks (Phase N) ---------------------------------------------
    //
    // Deliberately NO `bump_generation()` — bookmarks are operator-owned
    // places, not files/symbols search-lane inputs.

    /// Insert a bookmark. When `mnemonic` is `Some`, any existing bookmark
    /// in the same `repo` that already owns that mnemonic is DELETED first
    /// (vim-style mark reassignment — "delete-then-set"). Returns the new
    /// row's `id`.
    pub fn create_bookmark(
        &self,
        repo: &str,
        path: &str,
        line: i64,
        mnemonic: Option<&str>,
        note: Option<&str>,
        now: i64,
    ) -> Result<i64> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        if let Some(m) = mnemonic {
            tx.execute(
                "DELETE FROM bookmarks WHERE repo = ?1 AND mnemonic = ?2",
                params![repo, m],
            )?;
        }
        tx.execute(
            "INSERT INTO bookmarks (repo, path, line, mnemonic, note, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![repo, path, line, mnemonic, note, now],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Every bookmark in `repo`, mnemonic-first (non-null mnemonics ASC,
    /// then anonymous ones), then `created_at` ASC — `GET
    /// /api/bookmarks?repo=`'s data source.
    pub fn list_bookmarks(&self, repo: &str) -> Result<Vec<BookmarkRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, repo, path, line, mnemonic, note, created_at, updated_at
             FROM bookmarks
             WHERE repo = ?1
             ORDER BY (mnemonic IS NULL), mnemonic ASC, created_at ASC",
        )?;
        let rows = stmt
            .query_map(params![repo], bookmark_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Single bookmark lookup by id.
    pub fn get_bookmark(&self, id: i64) -> Result<Option<BookmarkRow>> {
        self.lock()
            .query_row(
                "SELECT id, repo, path, line, mnemonic, note, created_at, updated_at
                 FROM bookmarks WHERE id = ?1",
                params![id],
                bookmark_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Look up a bookmark by `(repo, mnemonic)` — the CLI's
    /// `bookmark rm <mnemonic>` resolution path.
    pub fn get_bookmark_by_mnemonic(
        &self,
        repo: &str,
        mnemonic: &str,
    ) -> Result<Option<BookmarkRow>> {
        self.lock()
            .query_row(
                "SELECT id, repo, path, line, mnemonic, note, created_at, updated_at
                 FROM bookmarks WHERE repo = ?1 AND mnemonic = ?2",
                params![repo, mnemonic],
                bookmark_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Update whichever of `line`/`note`/`mnemonic` is provided.
    /// `mnemonic: Some(None)` clears the mnemonic; `mnemonic: None` leaves
    /// it alone; `mnemonic: Some(Some(m))` assigns `m` (delete-then-set
    /// move semantics apply — any other bookmark in the same repo owning
    /// `m` is deleted first). Returns `true` iff `id` existed.
    pub fn update_bookmark(
        &self,
        id: i64,
        line: Option<i64>,
        note: Option<Option<&str>>,
        mnemonic: Option<Option<&str>>,
        updated_at: i64,
    ) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let repo: Option<String> = tx
            .query_row(
                "SELECT repo FROM bookmarks WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(repo) = repo else {
            return Ok(false);
        };
        if let Some(Some(m)) = mnemonic {
            tx.execute(
                "DELETE FROM bookmarks WHERE repo = ?1 AND mnemonic = ?2 AND id != ?3",
                params![repo, m, id],
            )?;
        }
        // Build the SET clause from the present fields. Always stamp
        // updated_at. Note/mnemonic use the double-option shape so an
        // explicit null clears (sets SQL NULL) rather than leaving alone.
        let n = tx.execute(
            "UPDATE bookmarks SET
                line = COALESCE(?2, line),
                note = CASE WHEN ?3 != 0 THEN ?4 ELSE note END,
                mnemonic = CASE WHEN ?5 != 0 THEN ?6 ELSE mnemonic END,
                updated_at = ?7
             WHERE id = ?1",
            params![
                id,
                line,
                if note.is_some() { 1i64 } else { 0i64 },
                note.flatten(),
                if mnemonic.is_some() { 1i64 } else { 0i64 },
                mnemonic.flatten(),
                updated_at,
            ],
        )?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// Hard-delete a bookmark by id. Returns `true` iff `id` existed.
    pub fn delete_bookmark(&self, id: i64) -> Result<bool> {
        let n = self
            .lock()
            .execute("DELETE FROM bookmarks WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    // --- todo items (Phase N) --------------------------------------------
    //
    // File-keyed derived data: replaced wholesale on every re-ingest of a
    // file (`replace_todo_items`). `ON DELETE CASCADE` from `files(id)`
    // covers the live-mirror remove path (`delete_file`).

    /// The integer `files.id` for `(repo_id, path)`, if any — Phase N's
    /// `todo_items.file_id` join key (V0012 gave `files` a free-standing
    /// INTEGER PRIMARY KEY; pre-V0012 the table was keyed only by
    /// `(repo_id, path)`).
    pub fn file_id(&self, repo_id: i64, path: &str) -> Result<Option<i64>> {
        // PF-K1 — called once per caller-group in `hierarchy::callers_at`'s
        // fan-out; `prepare_cached` (identical SQL every call) avoids a
        // full re-parse/re-plan on every group.
        let conn = self.lock();
        // `CachedStatement` carries a Drop (returns itself to the cache), so
        // it must be bound to a local that dies before `conn` — a tail-
        // expression temporary would outlive the guard (E0597).
        let mut stmt =
            conn.prepare_cached("SELECT id FROM files WHERE repo_id = ?1 AND path = ?2")?;
        stmt.query_row(params![repo_id, path], |r| r.get(0))
            .optional()
            .map_err(Into::into)
    }

    /// Replace every `todo_items` row for `file_id` in one transaction —
    /// same "delete then re-insert" discipline as `replace_symbols`. An
    /// empty `items` slice clears the file's todos (e.g. all markers
    /// removed, or a non-full-tier language that never extracts).
    pub fn replace_todo_items(&self, file_id: i64, items: &[NewTodoItem]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM todo_items WHERE file_id = ?1",
            params![file_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO todo_items (file_id, line, marker, text)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for item in items {
                stmt.execute(params![file_id, item.line, item.marker, item.text])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// List todo items for `repo_id`, optionally filtered by exact
    /// `marker` and/or `path_prefix` (SQL `path LIKE prefix || '%'`).
    /// Ordered by path, then line. Returns every matching row (the route
    /// layer applies limit/truncation + scope-glob filtering).
    pub fn list_todo_items(
        &self,
        repo_id: i64,
        marker: Option<&str>,
        path_prefix: Option<&str>,
    ) -> Result<Vec<TodoItemRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT f.path, t.line, t.marker, t.text
             FROM todo_items t
             JOIN files f ON f.id = t.file_id
             WHERE f.repo_id = ?1
               AND (?2 IS NULL OR t.marker = ?2)
               AND (?3 IS NULL OR f.path LIKE (?3 || '%'))
             ORDER BY f.path ASC, t.line ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id, marker, path_prefix], |r| {
                Ok(TodoItemRow {
                    path: r.get(0)?,
                    line: r.get(1)?,
                    marker: r.get(2)?,
                    text: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- local reviews (V3.R1, migration V0014) ---------------------------
    //
    // Deliberately NO `bump_generation()` — reviews never touch the
    // files/symbols search caches. `repo` is the configured repo NAME
    // (text), matching bookmarks/reading-sets' human-facing key rather
    // than an internal `repo_id` (reviews outlive a store re-register).

    /// Insert a new review row; returns the auto-assigned `id`.
    pub fn create_review(
        &self,
        repo: &str,
        title: Option<&str>,
        base_ref: &str,
        head_ref: &str,
        session_id: Option<&str>,
        now: i64,
    ) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO reviews
                (repo, title, base_ref, head_ref, session_id, state, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?6)",
            params![repo, title, base_ref, head_ref, session_id, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_review(&self, id: i64) -> Result<Option<ReviewRow>> {
        self.lock()
            .query_row(
                "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                        created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
                 FROM reviews WHERE id = ?1",
                params![id],
                review_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// [`Self::get_review`] for a whole SET of `ids` — one query (dynamic
    /// `IN (…)`, plain `prepare`) instead of N round trips. A vanished id
    /// (deleted between a caller's initial fetch and this lookup) is
    /// simply absent from the map, matching `get_review`'s own `None`.
    pub fn get_reviews_by_ids(&self, ids: &[i64]) -> Result<HashMap<i64, ReviewRow>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.lock();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                    created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
             FROM reviews WHERE id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), review_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().map(|r| (r.id, r)).collect())
    }

    /// List reviews for `repo`, optionally filtered to `state` (`"open"` /
    /// `"closed"`). Newest-first.
    pub fn list_reviews(&self, repo: &str, state: Option<&str>) -> Result<Vec<ReviewRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                    created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
             FROM reviews
             WHERE repo = ?1 AND (?2 IS NULL OR state = ?2)
             ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map(params![repo, state], review_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every OPEN review across every repo — the auto-capture worker's
    /// scan surface on each `repo.head_moved`.
    pub fn list_open_reviews(&self) -> Result<Vec<ReviewRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                    created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
             FROM reviews WHERE state = 'open'
             ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map([], review_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Open reviews whose `repo` equals `repo` (auto-capture filter).
    pub fn list_open_reviews_for_repo(&self, repo: &str) -> Result<Vec<ReviewRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                    created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
             FROM reviews WHERE repo = ?1 AND state = 'open'
             ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map(params![repo], review_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn update_review(
        &self,
        id: i64,
        title: Option<Option<&str>>,
        state: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        let conn = self.lock();
        // Read-then-write: only touch provided fields. `title: Some(None)`
        // clears the title; `title: None` leaves it alone.
        let Some(mut row) = conn
            .query_row(
                "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                        created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
                 FROM reviews WHERE id = ?1",
                params![id],
                review_row_from,
            )
            .optional()?
        else {
            return Ok(false);
        };
        if let Some(t) = title {
            row.title = t.map(|s| s.to_string());
        }
        if let Some(s) = state {
            row.state = s.to_string();
        }
        let n = conn.execute(
            "UPDATE reviews SET title = ?2, state = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, row.title, row.state, now],
        )?;
        Ok(n > 0)
    }

    /// V4.C2 — set (or replace) the review-pass verdict. Compares
    /// `(state, note)` only — `verdict_at` is ignored so a re-PUT of the
    /// same pair is a no-op (kb-core `ReviewFile::set_verdict` G8).
    /// Returns `Ok(None)` when `id` is missing, `Ok(Some(false))` on a
    /// no-op, `Ok(Some(true))` when the four columns were written.
    /// Never `bump_generation`.
    pub fn set_review_verdict(
        &self,
        id: i64,
        state: &str,
        note: Option<&str>,
        at: i64,
        ps: i64,
    ) -> Result<Option<bool>> {
        let conn = self.lock();
        let Some((cur_state, cur_note)): Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT verdict, verdict_note FROM reviews WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let unchanged = cur_state.as_deref() == Some(state) && cur_note.as_deref() == note;
        if unchanged {
            return Ok(Some(false));
        }
        conn.execute(
            "UPDATE reviews
             SET verdict = ?2, verdict_note = ?3, verdict_at = ?4, verdict_ps = ?5
             WHERE id = ?1",
            params![id, state, note, at, ps],
        )?;
        Ok(Some(true))
    }

    /// V4.C2 — clear all four verdict columns. Returns `Ok(None)` when
    /// `id` is missing, `Ok(Some(false))` when there was nothing to
    /// clear, `Ok(Some(true))` when a verdict was wiped. Never
    /// `bump_generation`.
    pub fn clear_review_verdict(&self, id: i64) -> Result<Option<bool>> {
        let conn = self.lock();
        let Some(cur): Option<Option<String>> = conn
            .query_row(
                "SELECT verdict FROM reviews WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        if cur.is_none() {
            return Ok(Some(false));
        }
        conn.execute(
            "UPDATE reviews
             SET verdict = NULL, verdict_note = NULL, verdict_at = NULL, verdict_ps = NULL
             WHERE id = ?1",
            params![id],
        )?;
        Ok(Some(true))
    }

    /// Delete the review row (cascades patchsets + viewed via SQL FKs)
    /// AND every review-scoped annotation + its `annotation_suggestions`
    /// row, all in ONE transaction. Annotations have no SQL FK on
    /// `review_id` (V0023 / parent_id precedent), so the cascade is
    /// code-owned. Caller is responsible for deleting the matching
    /// `refs/kbc/review/<id>/ps*` refs first. Returns `true` iff a
    /// review row was deleted. Never `bump_generation`.
    pub fn delete_review(&self, id: i64) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM annotation_suggestions
             WHERE annotation_id IN (SELECT id FROM annotations WHERE review_id = ?1)",
            params![id],
        )?;
        tx.execute("DELETE FROM annotations WHERE review_id = ?1", params![id])?;
        let n = tx.execute("DELETE FROM reviews WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn insert_patchset(
        &self,
        review_id: i64,
        ps_number: i64,
        tip_sha: &str,
        base_sha: &str,
        captured_at: i64,
    ) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO review_patchsets
                (review_id, ps_number, tip_sha, base_sha, captured_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![review_id, ps_number, tip_sha, base_sha, captured_at],
        )?;
        // Bump the parent review's updated_at so list-newest-first tracks
        // the latest capture.
        conn.execute(
            "UPDATE reviews SET updated_at = ?2 WHERE id = ?1",
            params![review_id, captured_at],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_patchsets(&self, review_id: i64) -> Result<Vec<ReviewPatchsetRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, review_id, ps_number, tip_sha, base_sha, captured_at
             FROM review_patchsets
             WHERE review_id = ?1
             ORDER BY ps_number ASC",
        )?;
        let rows = stmt
            .query_map(params![review_id], review_patchset_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_patchset(
        &self,
        review_id: i64,
        ps_number: i64,
    ) -> Result<Option<ReviewPatchsetRow>> {
        self.lock()
            .query_row(
                "SELECT id, review_id, ps_number, tip_sha, base_sha, captured_at
                 FROM review_patchsets
                 WHERE review_id = ?1 AND ps_number = ?2",
                params![review_id, ps_number],
                review_patchset_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn latest_patchset(&self, review_id: i64) -> Result<Option<ReviewPatchsetRow>> {
        // PF-K1 — hot on the (now-batched, see `latest_patchsets`) review
        // list/inbox composition paths; `prepare_cached` (identical SQL
        // every call) avoids a full re-parse/re-plan per call.
        let conn = self.lock();
        // Bound local: a `CachedStatement` tail-temporary outlives `conn`
        // (its Drop returns it to the cache) — E0597 otherwise.
        let mut stmt = conn.prepare_cached(
            "SELECT id, review_id, ps_number, tip_sha, base_sha, captured_at
             FROM review_patchsets
             WHERE review_id = ?1
             ORDER BY ps_number DESC
             LIMIT 1",
        )?;
        stmt.query_row(params![review_id], review_patchset_row_from)
            .optional()
            .map_err(Into::into)
    }

    /// [`Self::latest_patchset`] for a whole SET of `review_ids` — one
    /// query (an `IN (…)` placeholder list, dynamic per call — deliberately
    /// left as plain `prepare`, not `prepare_cached`, per this file's
    /// "dynamic placeholder counts thrash the cache" convention) instead of
    /// N round trips. A review with no captured patchset (or a vanished
    /// id) is simply ABSENT from the map, never a synthesized/default row —
    /// callers already branch on `Option`/`.get()` the same way the
    /// singular form's `Option` return does.
    pub fn latest_patchsets(&self, review_ids: &[i64]) -> Result<HashMap<i64, ReviewPatchsetRow>> {
        if review_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.lock();
        let placeholders = review_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, review_id, ps_number, tip_sha, base_sha, captured_at
             FROM review_patchsets rp
             WHERE review_id IN ({placeholders})
               AND ps_number = (
                 SELECT MAX(ps_number) FROM review_patchsets rp2
                 WHERE rp2.review_id = rp.review_id
               )"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(review_ids.iter()),
                review_patchset_row_from,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().map(|r| (r.review_id, r)).collect())
    }

    /// Next `ps_number` for a review (`MAX + 1`, or `1` when empty).
    pub fn next_ps_number(&self, review_id: i64) -> Result<i64> {
        let max: Option<i64> = self.lock().query_row(
            "SELECT MAX(ps_number) FROM review_patchsets WHERE review_id = ?1",
            params![review_id],
            |r| r.get(0),
        )?;
        Ok(max.unwrap_or(0) + 1)
    }

    /// Patchset count for a review.
    pub fn patchset_count(&self, review_id: i64) -> Result<i64> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM review_patchsets WHERE review_id = ?1",
            params![review_id],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Oldest patchset by `ps_number` (for GC of the oldest when over
    /// `max_patchsets`). `None` when the review has no patchsets.
    pub fn oldest_patchset(&self, review_id: i64) -> Result<Option<ReviewPatchsetRow>> {
        self.lock()
            .query_row(
                "SELECT id, review_id, ps_number, tip_sha, base_sha, captured_at
                 FROM review_patchsets
                 WHERE review_id = ?1
                 ORDER BY ps_number ASC
                 LIMIT 1",
                params![review_id],
                review_patchset_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn delete_patchset(&self, review_id: i64, ps_number: i64) -> Result<bool> {
        let n = self.lock().execute(
            "DELETE FROM review_patchsets WHERE review_id = ?1 AND ps_number = ?2",
            params![review_id, ps_number],
        )?;
        Ok(n > 0)
    }

    pub fn upsert_viewed(
        &self,
        review_id: i64,
        path: &str,
        blob_sha: &str,
        viewed_at: i64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO review_viewed (review_id, path, blob_sha, viewed_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(review_id, path) DO UPDATE SET
                blob_sha = excluded.blob_sha,
                viewed_at = excluded.viewed_at",
            params![review_id, path, blob_sha, viewed_at],
        )?;
        Ok(())
    }

    pub fn delete_viewed(&self, review_id: i64, path: &str) -> Result<bool> {
        let n = self.lock().execute(
            "DELETE FROM review_viewed WHERE review_id = ?1 AND path = ?2",
            params![review_id, path],
        )?;
        Ok(n > 0)
    }

    /// All viewed rows for a review, keyed by path.
    pub fn list_viewed(&self, review_id: i64) -> Result<Vec<ReviewViewedRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT review_id, path, blob_sha, viewed_at
             FROM review_viewed WHERE review_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![review_id], review_viewed_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// [`Self::list_viewed`] for a whole SET of `review_ids` — one query
    /// (dynamic `IN (…)`, plain `prepare`) instead of N round trips. A
    /// review with no viewed rows is simply absent from the map.
    pub fn list_viewed_batch(
        &self,
        review_ids: &[i64],
    ) -> Result<HashMap<i64, Vec<ReviewViewedRow>>> {
        let mut out: HashMap<i64, Vec<ReviewViewedRow>> = HashMap::new();
        if review_ids.is_empty() {
            return Ok(out);
        }
        let conn = self.lock();
        let placeholders = review_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT review_id, path, blob_sha, viewed_at
             FROM review_viewed WHERE review_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(review_ids.iter()),
                review_viewed_row_from,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for row in rows {
            out.entry(row.review_id).or_default().push(row);
        }
        Ok(out)
    }

    pub fn get_viewed(&self, review_id: i64, path: &str) -> Result<Option<ReviewViewedRow>> {
        self.lock()
            .query_row(
                "SELECT review_id, path, blob_sha, viewed_at
                 FROM review_viewed WHERE review_id = ?1 AND path = ?2",
                params![review_id, path],
                review_viewed_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Count of open (unresolved, top-level) annotations on any of the
    /// given paths in `repo_id`. Paths are matched exactly.
    pub fn count_open_annotations_on_paths(&self, repo_id: i64, paths: &[String]) -> Result<i64> {
        if paths.is_empty() {
            return Ok(0);
        }
        let conn = self.lock();
        // Build `IN (?,?,…)` dynamically — paths come from a git diff of
        // the same repo, so the set is bounded by the change set size.
        let placeholders = paths
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT COUNT(*) FROM annotations
             WHERE repo_id = ?1 AND resolved = 0 AND parent_id IS NULL
               AND path IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params_vec: Vec<rusqlite::types::Value> = Vec::with_capacity(1 + paths.len());
        params_vec.push(repo_id.into());
        for p in paths {
            params_vec.push(p.clone().into());
        }
        let n: i64 = stmt.query_row(rusqlite::params_from_iter(params_vec), |r| r.get(0))?;
        Ok(n)
    }

    /// Per-path open-annotation counts for the given paths (top-level,
    /// unresolved only). Missing paths map to 0.
    pub fn open_annotation_counts_by_path(
        &self,
        repo_id: i64,
        paths: &[String],
    ) -> Result<std::collections::HashMap<String, i64>> {
        let mut out = std::collections::HashMap::new();
        for p in paths {
            out.insert(p.clone(), 0);
        }
        if paths.is_empty() {
            return Ok(out);
        }
        let conn = self.lock();
        let placeholders = paths
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT path, COUNT(*) FROM annotations
             WHERE repo_id = ?1 AND resolved = 0 AND parent_id IS NULL
               AND path IN ({placeholders})
             GROUP BY path"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params_vec: Vec<rusqlite::types::Value> = Vec::with_capacity(1 + paths.len());
        params_vec.push(repo_id.into());
        for p in paths {
            params_vec.push(p.clone().into());
        }
        let mut rows = stmt.query(rusqlite::params_from_iter(params_vec))?;
        while let Some(r) = rows.next()? {
            let path: String = r.get(0)?;
            let n: i64 = r.get(1)?;
            out.insert(path, n);
        }
        Ok(out)
    }

    /// V71-F1 — open annotation counts for EVERY path in a repo, grouped
    /// in one query. [`Self::open_annotation_counts_by_path`] takes the
    /// path list as a dynamic `IN (…)`, which is right for a review's
    /// dozens of files and wrong for the tree's thousands (a 6,500-term
    /// `IN` per request); the tree's `annot` decoration lane wants the
    /// whole map and intersects it itself. A path with no open annotation
    /// is ABSENT from the map rather than present-with-0 — the tree reads
    /// absence as "no decoration", never as a zero badge.
    pub fn open_annotation_counts_for_repo(
        &self,
        repo_id: i64,
    ) -> Result<std::collections::HashMap<String, i64>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT path, COUNT(*) FROM annotations
             WHERE repo_id = ?1 AND resolved = 0 AND parent_id IS NULL
             GROUP BY path",
        )?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().collect())
    }

    /// Open top-level annotations whose path is in `paths` (for
    /// `GET /api/reviews/{id}/annotations`).
    pub fn list_open_annotations_on_paths(
        &self,
        repo_id: i64,
        paths: &[String],
    ) -> Result<Vec<AnnotationRow>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let placeholders = paths
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, repo_id, path, anchor, anchor_kind, anchor2, parent_id,
                    intent, body, author, created_at, updated_at, resolved,
                    review_id, ps_number, side, set_id
             FROM annotations
             WHERE repo_id = ?1 AND resolved = 0 AND parent_id IS NULL
               AND path IN ({placeholders})
             ORDER BY path ASC, created_at ASC, id ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params_vec: Vec<rusqlite::types::Value> = Vec::with_capacity(1 + paths.len());
        params_vec.push(repo_id.into());
        for p in paths {
            params_vec.push(p.clone().into());
        }
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params_vec), annotation_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- behavioral store (V3.2-B1, migration V0017) ----------------------
    //
    // Repo-addressed history counters — NOT content-addressed. No
    // `bump_generation()`: these never feed the files/symbols search
    // caches. Incremental updates are additive; only a full rebuild
    // prunes paths/pairs outside the window.

    pub fn behavioral_meta(&self, repo_id: i64) -> Result<Option<BehavioralMetaRow>> {
        self.lock()
            .query_row(
                "SELECT repo_id, last_commit_sha, updated_at
                 FROM behavioral_meta WHERE repo_id = ?1",
                params![repo_id],
                |r| {
                    Ok(BehavioralMetaRow {
                        repo_id: r.get(0)?,
                        last_commit_sha: r.get(1)?,
                        updated_at: r.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_behavioral_meta(
        &self,
        repo_id: i64,
        last_commit_sha: Option<&str>,
        updated_at: i64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO behavioral_meta (repo_id, last_commit_sha, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(repo_id) DO UPDATE SET
               last_commit_sha = excluded.last_commit_sha,
               updated_at = excluded.updated_at",
            params![repo_id, last_commit_sha, updated_at],
        )?;
        Ok(())
    }

    /// Drop every behavioral counter row for `repo_id` (full rebuild).
    pub fn clear_behavioral_stats(&self, repo_id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM path_stats WHERE repo_id = ?1",
            params![repo_id],
        )?;
        conn.execute(
            "DELETE FROM author_stats WHERE repo_id = ?1",
            params![repo_id],
        )?;
        conn.execute(
            "DELETE FROM cochange_pairs WHERE repo_id = ?1",
            params![repo_id],
        )?;
        Ok(())
    }

    /// Apply one commit's path/author/cochange deltas in a single
    /// transaction. `co_pairs` entries MUST already be `a < b` ordered.
    ///
    /// V3.2-B2: when `session_id` is `Some`, also records
    /// `author = "session:<id>"` beside the human author (both rows —
    /// human authored the commit, session produced the patch; never collapse).
    pub fn apply_behavioral_commit(
        &self,
        repo_id: i64,
        files: &[(String, i64, i64)], // (path, added, deleted)
        author: &str,
        commit_unix: i64,
        co_pairs: &[(String, String)], // (path_a, path_b) with a < b
        session_id: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for (path, added, deleted) in files {
            tx.execute(
                "INSERT INTO path_stats
                    (repo_id, path, revisions, lines_added, lines_deleted,
                     first_seen_unix, last_touch_unix)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, ?5)
                 ON CONFLICT(repo_id, path) DO UPDATE SET
                   revisions = revisions + 1,
                   lines_added = lines_added + excluded.lines_added,
                   lines_deleted = lines_deleted + excluded.lines_deleted,
                   first_seen_unix = MIN(
                       COALESCE(first_seen_unix, excluded.first_seen_unix),
                       excluded.first_seen_unix),
                   last_touch_unix = MAX(
                       COALESCE(last_touch_unix, excluded.last_touch_unix),
                       excluded.last_touch_unix)",
                params![repo_id, path, added, deleted, commit_unix],
            )?;
            // Human author row (always).
            Self::upsert_author_stat(&tx, repo_id, path, author, commit_unix)?;
            // Session dual-author row when join resolved (V3.2-B2).
            if let Some(sid) = session_id {
                if !sid.is_empty() {
                    let session_author = format!("session:{sid}");
                    Self::upsert_author_stat(&tx, repo_id, path, &session_author, commit_unix)?;
                }
            }
        }
        for (a, b) in co_pairs {
            tx.execute(
                "INSERT INTO cochange_pairs (repo_id, path_a, path_b, co_commits)
                 VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT(repo_id, path_a, path_b) DO UPDATE SET
                   co_commits = co_commits + 1",
                params![repo_id, a, b],
            )?;
        }
        tx.commit()?;
        Ok(())
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

    pub fn list_path_stats(&self, repo_id: i64) -> Result<Vec<PathStatsRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path, revisions, lines_added, lines_deleted,
                    first_seen_unix, last_touch_unix
             FROM path_stats WHERE repo_id = ?1
             ORDER BY revisions DESC, path ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                Ok(PathStatsRow {
                    path: r.get(0)?,
                    revisions: r.get(1)?,
                    lines_added: r.get(2)?,
                    lines_deleted: r.get(3)?,
                    first_seen_unix: r.get(4)?,
                    last_touch_unix: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn path_stats_for(&self, repo_id: i64, path: &str) -> Result<Option<PathStatsRow>> {
        self.lock()
            .query_row(
                "SELECT path, revisions, lines_added, lines_deleted,
                        first_seen_unix, last_touch_unix
                 FROM path_stats WHERE repo_id = ?1 AND path = ?2",
                params![repo_id, path],
                |r| {
                    Ok(PathStatsRow {
                        path: r.get(0)?,
                        revisions: r.get(1)?,
                        lines_added: r.get(2)?,
                        lines_deleted: r.get(3)?,
                        first_seen_unix: r.get(4)?,
                        last_touch_unix: r.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Partners of `path` from cochange_pairs (either side), ordered by
    /// co_commits desc then path.
    pub fn cochange_partners(&self, repo_id: i64, path: &str) -> Result<Vec<(String, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path_b AS partner, co_commits FROM cochange_pairs
             WHERE repo_id = ?1 AND path_a = ?2
             UNION ALL
             SELECT path_a AS partner, co_commits FROM cochange_pairs
             WHERE repo_id = ?1 AND path_b = ?2
             ORDER BY co_commits DESC, partner ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id, path], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn author_stats_for(&self, repo_id: i64, path: &str) -> Result<Vec<AuthorStatsRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT author, commits, first_seen_unix FROM author_stats
             WHERE repo_id = ?1 AND path = ?2
             ORDER BY commits DESC, author ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id, path], |r| {
                Ok(AuthorStatsRow {
                    author: r.get(0)?,
                    commits: r.get(1)?,
                    first_seen_unix: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- session_signals (V3.2-B2, migration V0018) -----------------------

    pub fn upsert_session_signals(
        &self,
        repo_id: i64,
        session_id: &str,
        fail_count: i64,
        error_count: i64,
        duration_secs: i64,
        captured_at: i64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO session_signals
                (repo_id, session_id, fail_count, error_count, duration_secs, captured_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(repo_id, session_id) DO UPDATE SET
               fail_count = excluded.fail_count,
               error_count = excluded.error_count,
               duration_secs = excluded.duration_secs,
               captured_at = excluded.captured_at",
            params![
                repo_id,
                session_id,
                fail_count,
                error_count,
                duration_secs,
                captured_at
            ],
        )?;
        Ok(())
    }

    pub fn session_signals_for(
        &self,
        repo_id: i64,
        session_id: &str,
    ) -> Result<Option<SessionSignalsRow>> {
        self.lock()
            .query_row(
                "SELECT session_id, fail_count, error_count, duration_secs, captured_at
                 FROM session_signals WHERE repo_id = ?1 AND session_id = ?2",
                params![repo_id, session_id],
                |r| {
                    Ok(SessionSignalsRow {
                        session_id: r.get(0)?,
                        fail_count: r.get(1)?,
                        error_count: r.get(2)?,
                        duration_secs: r.get(3)?,
                        captured_at: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// All session_signals rows for a repo (for pain-weighted hotspots).
    pub fn list_session_signals(&self, repo_id: i64) -> Result<Vec<SessionSignalsRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT session_id, fail_count, error_count, duration_secs, captured_at
             FROM session_signals WHERE repo_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                Ok(SessionSignalsRow {
                    session_id: r.get(0)?,
                    fail_count: r.get(1)?,
                    error_count: r.get(2)?,
                    duration_secs: r.get(3)?,
                    captured_at: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Paths touched by a session via dual-author rows (`session:<id>`).
    pub fn paths_for_session_author(&self, repo_id: i64, session_id: &str) -> Result<Vec<String>> {
        let author = format!("session:{session_id}");
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path FROM author_stats
             WHERE repo_id = ?1 AND author = ?2
             ORDER BY path ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id, author], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Earliest `first_seen_unix` among `session:*` authors for `path`.
    pub fn earliest_session_author(
        &self,
        repo_id: i64,
        path: &str,
    ) -> Result<Option<(String, i64)>> {
        self.lock()
            .query_row(
                "SELECT author, first_seen_unix FROM author_stats
                 WHERE repo_id = ?1 AND path = ?2
                   AND author LIKE 'session:%'
                   AND first_seen_unix IS NOT NULL
                 ORDER BY first_seen_unix ASC, author ASC
                 LIMIT 1",
                params![repo_id, path],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    /// V3.3-Q1 — does this repo have any dual-author session rows
    /// (`author_stats.author LIKE 'session:%'`)? Same source the ownership
    /// `agents[]` surface uses. Missing ⇒ agent-only recipes report
    /// `inputs_missing: ["agent_attribution"]`.
    pub fn has_session_authors(&self, repo_id: i64) -> Result<bool> {
        let n: i64 = self.lock().query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM author_stats
                 WHERE repo_id = ?1 AND author LIKE 'session:%'
             )",
            params![repo_id],
            |r| r.get(0),
        )?;
        Ok(n != 0)
    }

    /// V3.3-Q1 — any non-empty `commit_sessions.session_id` for the repo
    /// (join-ladder agent attribution without requiring behavioral dual-
    /// author rows).
    pub fn has_commit_session_ids(&self, repo_id: i64) -> Result<bool> {
        let n: i64 = self.lock().query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM commit_sessions
                 WHERE repo_id = ?1
                   AND session_id IS NOT NULL
                   AND session_id != ''
             )",
            params![repo_id],
            |r| r.get(0),
        )?;
        Ok(n != 0)
    }

    /// V3.3-Q1 — all call sites reachable from `repo_id`'s current files,
    /// paired with path / blob_hash / salt. Used by god-functions fan counts.
    pub fn call_sites_in_repo(
        &self,
        repo_id: i64,
    ) -> Result<Vec<(String, crate::hierarchy::CallSite, String, String)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT f.path, c.ordinal, c.callee_name, c.callee_qualifier, c.line, c.col,
                    c.arg_count, c.caller_ordinal, f.blob_hash, c.salt
             FROM files f
             JOIN call_sites c ON c.blob_hash = f.blob_hash
             WHERE f.repo_id = ?1 AND c.ordinal >= 0
             ORDER BY f.path, c.line, c.col",
        )?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                let arg_count: Option<i64> = r.get(6)?;
                let caller_ordinal: Option<i64> = r.get(7)?;
                Ok((
                    r.get::<_, String>(0)?,
                    crate::hierarchy::CallSite {
                        ordinal: r.get::<_, i64>(1)? as u32,
                        callee_name: r.get(2)?,
                        callee_qualifier: r.get(3)?,
                        line: r.get::<_, i64>(4)? as u32,
                        col: r.get::<_, i64>(5)? as u32,
                        arg_count: arg_count.map(|n| n as u32),
                        caller_ordinal: caller_ordinal.map(|n| n as u32),
                    },
                    r.get::<_, String>(8)?,
                    r.get::<_, String>(9)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// V3.3-Q1 — every (path, lang, symbol) for the repo's current files.
    /// Same join shape as `symbols_for_repo` plus `files.lang`.
    pub fn symbols_with_lang_for_repo(
        &self,
        repo_id: i64,
    ) -> Result<Vec<(String, String, Symbol)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT f.path, f.lang, s.ordinal, s.name, s.kind, s.line_start, s.line_end,
                    s.col_start, s.col_end, s.container, s.signature, s.doc,
                    s.param_min, s.param_max
             FROM files f
             JOIN symbols s ON s.blob_hash = f.blob_hash
             WHERE f.repo_id = ?1
             ORDER BY f.path, s.ordinal",
        )?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    symbol_from_row(r, 2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- canvas sets (V3.4-C1, migration V0019) ---------------------------
    //
    // Opaque client layout payload; server durability only. No generation
    // bump (canvas never feeds search caches). Name unique per repo.

    /// List canvas sets for a repo (summary — no payload body).
    /// Ordered by name ASC (total order).
    pub fn list_canvas_sets(&self, repo_id: i64) -> Result<Vec<CanvasSetSummaryRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, review_id, updated_unix, length(payload)
             FROM canvas_sets
             WHERE repo_id = ?1
             ORDER BY name ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                Ok(CanvasSetSummaryRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    review_id: r.get(2)?,
                    updated_unix: r.get(3)?,
                    payload_bytes: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Full canvas row by id.
    pub fn get_canvas_set(&self, id: i64) -> Result<Option<CanvasSetRow>> {
        self.lock()
            .query_row(
                "SELECT id, repo_id, name, review_id, payload, created_unix, updated_unix
                 FROM canvas_sets WHERE id = ?1",
                params![id],
                canvas_set_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Insert a canvas set. `(repo_id, name)` collision →
    /// [`StoreError::NameConflict`]. Returns the new row id.
    pub fn create_canvas_set(
        &self,
        repo_id: i64,
        name: &str,
        review_id: Option<i64>,
        payload: &str,
        now: i64,
    ) -> Result<i64> {
        let conn = self.lock();
        match conn.execute(
            "INSERT INTO canvas_sets
                (repo_id, name, review_id, payload, created_unix, updated_unix)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![repo_id, name, review_id, payload, now],
        ) {
            Ok(_) => Ok(conn.last_insert_rowid()),
            Err(e) => Err(name_conflict_or(e, name)),
        }
    }

    /// Update payload and optional name; always stamps `updated_unix`.
    /// Name collision with a different set → [`StoreError::NameConflict`].
    /// Returns `true` iff the id existed.
    pub fn update_canvas_set(
        &self,
        id: i64,
        payload: &str,
        name: Option<&str>,
        updated_unix: i64,
    ) -> Result<bool> {
        let conn = self.lock();
        match conn.execute(
            "UPDATE canvas_sets SET
                payload = ?2,
                name = COALESCE(?3, name),
                updated_unix = ?4
             WHERE id = ?1",
            params![id, payload, name, updated_unix],
        ) {
            Ok(n) => Ok(n > 0),
            Err(e) => Err(name_conflict_or(e, name.unwrap_or_default())),
        }
    }

    /// Delete by id. Returns `true` iff a row was removed.
    pub fn delete_canvas_set(&self, id: i64) -> Result<bool> {
        let n = self
            .lock()
            .execute("DELETE FROM canvas_sets WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    // --- doc-lens (DCB W1.C) ---------------------------------------------
    //
    // Deliberately NO `bump_generation()` on ANY pin write — `doc_lens_pins`
    // is invisible to `FileIndex`/`SymbolIndex`'s generation-keyed caches, and
    // bumping would throw away every repo's cached path/symbol snapshot on a
    // pin click. Same "invisible sibling table ⇒ no bump" ruling the
    // bookmarks section above records, and the same one kb's own
    // `UpsertChunks` makes on its side.

    /// Every symbol definition in `repo_id` whose name is one of `names`,
    /// path + symbol. ONE query for the whole set (a dynamically-built
    /// `IN (…)` placeholder list) rather than N round trips, so the doc-lens
    /// symbol lane costs exactly one statement per (repo, request). Backed by
    /// `idx_symbols_name` (V0020). Deliberately NOT `symbols_for_repo`: that
    /// pulls every signature/doc string in the repo to answer a question
    /// about < 100 names. Returns an empty Vec for an empty `names` WITHOUT
    /// touching sqlite. Ordered `f.path, s.line_start, s.ordinal` — the same
    /// determinism contract as [`Self::symbols_named_in_repo`].
    pub fn symbols_named_many(
        &self,
        repo_id: i64,
        names: &[String],
    ) -> Result<Vec<(String, Symbol)>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        // `?1` is repo_id, so the name placeholders start at ?2.
        let placeholders = (0..names.len())
            .map(|i| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT f.path, s.ordinal, s.name, s.kind, s.line_start, s.line_end,
                    s.col_start, s.col_end, s.container, s.signature, s.doc,
                    s.param_min, s.param_max
             FROM files f
             JOIN symbols s ON s.blob_hash = f.blob_hash
             WHERE f.repo_id = ?1 AND s.name IN ({placeholders})
             ORDER BY f.path, s.line_start, s.ordinal"
        );
        let conn = self.lock();
        let mut stmt = conn.prepare(&sql)?;
        let mut binds: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(names.len() + 1);
        binds.push(&repo_id);
        for n in names {
            binds.push(n);
        }
        let rows = stmt
            .query_map(binds.as_slice(), |r| {
                Ok((r.get::<_, String>(0)?, symbol_from_row(r, 1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_doc_lens_pin(&self, kb: &str, doc_id: &str) -> Result<Option<DocLensPin>> {
        self.lock()
            .query_row(
                "SELECT kb, doc_id, repo, repo_root, doc_hash, pinned_at
                 FROM doc_lens_pins WHERE kb = ?1 AND doc_id = ?2",
                params![kb, doc_id],
                doc_lens_pin_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// `INSERT … ON CONFLICT(kb, doc_id) DO UPDATE` — last write wins.
    pub fn put_doc_lens_pin(&self, pin: &DocLensPin) -> Result<()> {
        self.lock().execute(
            "INSERT INTO doc_lens_pins (kb, doc_id, repo, repo_root, doc_hash, pinned_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(kb, doc_id) DO UPDATE SET
                 repo = excluded.repo,
                 repo_root = excluded.repo_root,
                 doc_hash = excluded.doc_hash,
                 pinned_at = excluded.pinned_at",
            params![
                pin.kb,
                pin.doc_id,
                pin.repo,
                pin.repo_root,
                pin.doc_hash,
                pin.pinned_at
            ],
        )?;
        Ok(())
    }

    /// `true` when a row was actually removed (the caller's "there WAS a pin
    /// and it is gone now" signal — the `DELETE` itself is idempotent).
    pub fn delete_doc_lens_pin(&self, kb: &str, doc_id: &str) -> Result<bool> {
        let n = self.lock().execute(
            "DELETE FROM doc_lens_pins WHERE kb = ?1 AND doc_id = ?2",
            params![kb, doc_id],
        )?;
        Ok(n > 0)
    }

    /// All pins, `kb` then `doc_id` ordered; `Some(kb)` scopes to one corpus.
    pub fn list_doc_lens_pins(&self, kb: Option<&str>) -> Result<Vec<DocLensPin>> {
        let conn = self.lock();
        match kb {
            Some(kb) => {
                let mut stmt = conn.prepare(
                    "SELECT kb, doc_id, repo, repo_root, doc_hash, pinned_at
                     FROM doc_lens_pins WHERE kb = ?1 ORDER BY kb, doc_id",
                )?;
                let rows = stmt
                    .query_map(params![kb], doc_lens_pin_from)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(rows)
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT kb, doc_id, repo, repo_root, doc_hash, pinned_at
                     FROM doc_lens_pins ORDER BY kb, doc_id",
                )?;
                let rows = stmt
                    .query_map([], doc_lens_pin_from)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(rows)
            }
        }
    }

    /// Move a pin from `old` to `new` in one statement (kb's moves 301 chain
    /// — kb invariant #27). No-op `Ok(false)` when there was no pin.
    /// `OR REPLACE` so a pre-existing pin on the DESTINATION id (an operator
    /// pinned the moved doc under its new id before the lens ever followed
    /// the chain) loses to the row actually being re-keyed rather than
    /// aborting the migration with a PK conflict.
    pub fn rekey_doc_lens_pin(&self, kb: &str, old: &str, new: &str) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE OR REPLACE doc_lens_pins SET doc_id = ?3
             WHERE kb = ?1 AND doc_id = ?2",
            params![kb, old, new],
        )?;
        Ok(n > 0)
    }

    // --- doc_refs — the reverse "cited by" index (DCB W3.A) --------------
    //
    // Deliberately NO `bump_generation()` anywhere in this section — same
    // rationale as reading sets / doc-lens pins above: `generation` only
    // invalidates the files/symbols search lanes' in-memory caches, which
    // `doc_refs` has nothing to do with.

    /// Replace ONE document's claims in one transaction: DELETE by
    /// `(kb, doc_id)` across EVERY `repo_id` (so a doc re-pinned to a
    /// different checkout doesn't leave stale rows behind under the old one),
    /// then insert the new set. Mirrors [`Self::create_reading_set`]'s
    /// one-transaction insert-loop shape, this crate's own precedent.
    ///
    /// Returns how many claims were written.
    pub fn replace_doc_refs(&self, w: &DocRefWrite<'_>, refs: &[NewDocRef]) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM doc_refs WHERE kb = ?1 AND doc_id = ?2",
            params![w.kb, w.doc_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO doc_refs
                    (repo_id, kb, doc_id, ordinal, kind, raw_hint, resolved_path,
                     line_start, line_end, line_state, group_key, group_label,
                     doc_title, doc_path, doc_hash, head_sha, dirty, seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            )?;
            for r in refs {
                stmt.execute(params![
                    w.repo_id,
                    w.kb,
                    w.doc_id,
                    r.ordinal,
                    r.kind,
                    r.raw_hint,
                    r.resolved_path,
                    r.line_start,
                    r.line_end,
                    r.line_state,
                    r.group_key,
                    r.group_label,
                    w.doc_title,
                    w.doc_path,
                    w.doc_hash,
                    w.head_sha,
                    w.dirty,
                    w.seen_at,
                ])?;
            }
        }
        tx.commit()?;
        Ok(refs.len())
    }

    /// Drop every claim filed under `(kb, doc_id)`, across every repo — the
    /// 404 arm (`doclens::sync`) and the moves-re-key arm both need exactly
    /// this. Returns the number of rows removed.
    pub fn delete_doc_refs_for_doc(&self, kb: &str, doc_id: &str) -> Result<usize> {
        let n = self.lock().execute(
            "DELETE FROM doc_refs WHERE kb = ?1 AND doc_id = ?2",
            params![kb, doc_id],
        )?;
        Ok(n)
    }

    /// "Which documents cite path X in repo Y" — `GET /api/doc-refs`'s own
    /// lookup, served by `idx_doc_refs_resolved_path`. Deterministic order
    /// (kb, doc_id, ordinal) so a response never depends on insert order.
    pub fn doc_refs_for_path(&self, repo_id: i64, path: &str) -> Result<Vec<DocRefRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT kb, doc_id, ordinal, kind, raw_hint, resolved_path, line_start, line_end,
                    line_state, group_key, group_label, doc_title, doc_path, doc_hash,
                    head_sha, dirty, seen_at
               FROM doc_refs
              WHERE repo_id = ?1 AND resolved_path = ?2
              ORDER BY kb, doc_id, ordinal",
        )?;
        let rows = stmt
            .query_map(params![repo_id, path], |r| {
                Ok(DocRefRow {
                    kb: r.get(0)?,
                    doc_id: r.get(1)?,
                    ordinal: r.get(2)?,
                    kind: r.get(3)?,
                    raw_hint: r.get(4)?,
                    resolved_path: r.get(5)?,
                    line_start: r.get(6)?,
                    line_end: r.get(7)?,
                    line_state: r.get(8)?,
                    group_key: r.get(9)?,
                    group_label: r.get(10)?,
                    doc_title: r.get(11)?,
                    doc_path: r.get(12)?,
                    doc_hash: r.get(13)?,
                    head_sha: r.get(14)?,
                    dirty: r.get(15)?,
                    seen_at: r.get(16)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The sync SET's kb list: every corpus with at least one pin. NOT a
    /// config allowlist — a kb only enters this list by a human having pinned
    /// a checkout for one of its documents ("grows as you read"). `ORDER BY
    /// kb` so a `batch_cap` truncation always favours the same corpora rather
    /// than being iteration-order flaky.
    pub fn doc_lens_pin_kbs(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT DISTINCT kb FROM doc_lens_pins ORDER BY kb")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_doclens_sync_cursor(&self, kb: &str) -> Result<Option<DoclensSyncCursor>> {
        self.lock()
            .query_row(
                "SELECT kb, cursor, last_run_at, last_error FROM doclens_sync_cursors WHERE kb = ?1",
                params![kb],
                |r| {
                    Ok(DoclensSyncCursor {
                        kb: r.get(0)?,
                        cursor: r.get(1)?,
                        last_run_at: r.get(2)?,
                        last_error: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Upsert one kb's sync progress. `cursor = None` means "start from the
    /// beginning next time"; `last_error = None` CLEARS a previous failure
    /// (the row always reflects the most recent pass, never a stale one).
    pub fn set_doclens_sync_cursor(
        &self,
        kb: &str,
        cursor: Option<&str>,
        last_run_at: i64,
        last_error: Option<&str>,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO doclens_sync_cursors (kb, cursor, last_run_at, last_error)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(kb) DO UPDATE SET
                cursor = excluded.cursor,
                last_run_at = excluded.last_run_at,
                last_error = excluded.last_error",
            params![kb, cursor, last_run_at, last_error],
        )?;
        Ok(())
    }

    /// `POST /api/doc-lens/sync {"force": true}` — reset the watermark
    /// WITHOUT dropping the row (its `last_run_at`/`last_error` are still the
    /// operator's record of what happened last).
    pub fn clear_doclens_sync_cursor(&self, kb: &str) -> Result<()> {
        self.lock().execute(
            "UPDATE doclens_sync_cursors SET cursor = NULL WHERE kb = ?1",
            params![kb],
        )?;
        Ok(())
    }

    // ── PRR-N3: rails edges ─────────────────────────────────────────────
    // `crate::frameworks::{FrameworkEdge, EdgeKind, Trust}` + migration
    // `V0026__rails_edges.sql`. Content-addressed like `import_specs`/
    // `call_sites` (DELETE-then-INSERT, wholesale replace), but with one
    // deliberate difference: there is NO `has_rails_edges` cache-hit-skip.
    // `call_sites`/`type_relations` are a pure blob→blob CST walk, so an
    // unchanged blob's rows can never go stale; `rails_edges`' view/partial
    // resolution (`frameworks::rails::views`) depends on the LIVE sibling-
    // file set (which `_row.*.erb` variants exist on disk right now), so
    // re-extracting on every visit is correct — a cache-hit skip here would
    // silently miss a resolution that only became (un)ambiguous because a
    // SIBLING file changed, never this blob itself (mirrors
    // `import_graph::resolve_import_edges`'s same choice to always
    // re-resolve). `repo_id` rides on the row (unlike `call_sites`) for
    // exactly that reason — this table's rows are meaningfully repo-scoped
    // facts, not pure content-addressed ones.
    //
    // *R1 fix (v70-a1, recon rails-lens.md §6):* the DELETE scope is now
    // `(repo_id, src_path)`, not `(blob_hash, salt)`. Every read
    // (`rails_edges_by_src_path`/`_by_dst_path`/`_by_kind` below) queries
    // `repo_id + path/kind` with NO `blob_hash` filter, so a delete scoped
    // to only the INCOMING blob's own rows left the PREVIOUS blob's rows
    // live forever on every edit (duplicate/contradictory edges accumulate
    // across edits) and left phantom rows forever after a file's deletion
    // (`Store::delete_file` below now prunes this table too — see its own
    // doc). Reads stay path-keyed; only the replace-time delete changed.
    // Sibling-dependent staleness (a change to `_row.turbo_stream.erb`
    // flipping an UNCHANGED controller's `render_partial` trust from
    // `likely` to `candidate`) is a separate, deferred issue (recon R4) —
    // this fix does not touch it: nothing here re-visits a file that didn't
    // itself change.

    pub fn replace_rails_edges(
        &self,
        repo_id: i64,
        path: &str,
        blob_hash: &str,
        salt: &str,
        edges: &[crate::frameworks::FrameworkEdge],
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM rails_edges WHERE repo_id = ?1 AND src_path = ?2",
            params![repo_id, path],
        )?;
        {
            // `INSERT OR REPLACE`, not plain `INSERT`: the table's
            // `UNIQUE(blob_hash, salt, ordinal)` constraint (V0026) is NOT
            // scoped by `(repo_id, src_path)` — a pre-existing schema
            // choice, unchanged here — so two DIFFERENT paths that happen
            // to share byte-identical content (same `blob_hash`) and the
            // same edge count could collide on that key. The delete above
            // only clears THIS path's rows, so a plain `INSERT` could now
            // hit a row still owned by the OTHER colliding path and fail
            // the whole transaction — turning a rare content collision
            // into a hard ingest error. `OR REPLACE` keeps this idempotent
            // instead: whichever path is (re-)indexed last wins the shared
            // slot, the same "last write wins" outcome the old blob-keyed
            // bulk delete already had, just row-scoped rather than wiping
            // every row the other path ever produced.
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO rails_edges
                    (repo_id, kind, src_path, src_line, src_symbol, dst_kind, dst_path,
                     dst_symbol, trust, blob_hash, salt, ordinal, extra_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?;
            for (ordinal, e) in edges.iter().enumerate() {
                stmt.execute(params![
                    repo_id,
                    e.kind.as_str(),
                    e.src_path,
                    e.src_line.map(|n| n as i64),
                    e.src_symbol,
                    e.dst_kind,
                    e.dst_path,
                    e.dst_symbol,
                    e.trust.as_str(),
                    blob_hash,
                    salt,
                    ordinal as i64,
                    e.extra_json,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every `rails_edges` row produced BY this path (e.g. open `routes.rb`,
    /// see every `route_action`/`route_file` edge it emits; open a
    /// controller, see every render/redirect call site).
    pub fn rails_edges_by_src_path(
        &self,
        repo_id: i64,
        src_path: &str,
    ) -> Result<Vec<crate::frameworks::FrameworkEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT kind, src_path, src_line, src_symbol, dst_kind, dst_path, dst_symbol,
                    trust, extra_json
             FROM rails_edges WHERE repo_id = ?1 AND src_path = ?2 ORDER BY ordinal",
        )?;
        let rows = stmt
            .query_map(params![repo_id, src_path], rails_edge_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every `rails_edges` row that TARGETS this path (e.g. open a partial,
    /// see every render call site across the repo that renders it).
    pub fn rails_edges_by_dst_path(
        &self,
        repo_id: i64,
        dst_path: &str,
    ) -> Result<Vec<crate::frameworks::FrameworkEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT kind, src_path, src_line, src_symbol, dst_kind, dst_path, dst_symbol,
                    trust, extra_json
             FROM rails_edges WHERE repo_id = ?1 AND dst_path = ?2 ORDER BY src_path, ordinal",
        )?;
        let rows = stmt
            .query_map(params![repo_id, dst_path], rails_edge_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every `rails_edges` row of a given `kind` (the closed rails-lens/1
    /// enum's string form, `EdgeKind::as_str()`) across the repo.
    pub fn rails_edges_by_kind(
        &self,
        repo_id: i64,
        kind: &str,
    ) -> Result<Vec<crate::frameworks::FrameworkEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT kind, src_path, src_line, src_symbol, dst_kind, dst_path, dst_symbol,
                    trust, extra_json
             FROM rails_edges WHERE repo_id = ?1 AND kind = ?2 ORDER BY src_path, ordinal",
        )?;
        let rows = stmt
            .query_map(params![repo_id, kind], rails_edge_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- V71-G0 — the entity index (`entities/1`, `crate::entities`) -----

    /// Replace every `entity_defs` row for one `(repo_id, worktree, path)`
    /// in a single transaction — the same delete-then-reinsert discipline
    /// (and the same "no stable per-row identity to diff against" reason)
    /// as [`Self::replace_rails_edges`], which this method is otherwise
    /// modelled on.
    ///
    /// Deliberately NOT content-addressed, unlike `symbols`/`call_sites`:
    /// an entity's FQN depends on the PATH (via the Zeitwerk convention)
    /// and on the CHECKOUT (via that checkout's own config), so the same
    /// blob at two paths legitimately yields two different claims and a
    /// `(blob_hash, salt)` cache key would collapse them into one. There
    /// is therefore no cache-hit skip either: every visit re-derives.
    ///
    /// `zeitwerk_state` is stored per row rather than per repo because it
    /// records what the config read was worth AT INDEX TIME — the read
    /// path classes against that, not against a config that may have
    /// changed since.
    pub fn replace_entity_defs(
        &self,
        repo_id: i64,
        worktree: &str,
        path: &str,
        blob_hash: &str,
        zeitwerk_state: &str,
        defs: &[crate::entities::EntityDefClaim],
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM entity_defs WHERE repo_id = ?1 AND worktree = ?2 AND path = ?3",
            params![repo_id, worktree, path],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO entity_defs
                     (repo_id, worktree, path, ordinal, fqn, kind, nesting,
                      line_start, line_end, zeitwerk_fqn, zeitwerk_state, blob_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            for (ordinal, d) in defs.iter().enumerate() {
                stmt.execute(params![
                    repo_id,
                    worktree,
                    path,
                    ordinal as i64,
                    d.fqn,
                    d.kind,
                    d.nesting,
                    i64::from(d.line_start),
                    i64::from(d.line_end),
                    d.zeitwerk_fqn,
                    zeitwerk_state,
                    blob_hash,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every definition site addressed by `name` — matching the FQN the
    /// TREE proved, the FQN the Zeitwerk convention derived, or (only when
    /// neither matches anything) the LAST SEGMENT of either. The
    /// last-segment arm is what makes `kb-code entity Order` useful in a
    /// namespaced monolith; it never merges those hits (the caller groups
    /// by FQN and reports `ambiguous`), it only finds them.
    ///
    /// `LIKE` is escaped explicitly: `_` is a LIKE wildcard, and a Ruby
    /// constant may legitimately contain one (`Order_v2`), so an
    /// unescaped pattern would silently over-match. Callers pass
    /// `MAX_DEFS_PER_QUERY + 1` so the route can report truncation rather
    /// than silently capping.
    pub fn entity_defs_for_name(
        &self,
        repo_id: i64,
        worktree: Option<&str>,
        name: &str,
        limit: usize,
    ) -> Result<Vec<EntityDefRow>> {
        let exact = self.entity_defs_query(repo_id, worktree, name, None, limit)?;
        if !exact.is_empty() {
            return Ok(exact);
        }
        let suffix = format!("%::{}", like_escape(name));
        self.entity_defs_query(repo_id, worktree, name, Some(&suffix), limit)
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

    /// V71-F1 — every `entity_defs` CLAIM in one repo, for the tree's
    /// `namespace` projection and kbc-scope/1's `ns:` atom. Deliberately a
    /// second, whole-repo read beside [`Self::entity_defs_for_name`]
    /// rather than a widened one: that query's job is to ADDRESS a name
    /// (two `LIKE` arms, a truncation limit the route reports), and this
    /// one's is to enumerate. Rows are still CLAIMS — `entities::class_for`
    /// is the only thing that turns one into a trust class, per request
    /// (crate invariant 13), which is why the live blob hash is joined here
    /// too.
    pub fn entity_defs_for_repo(
        &self,
        repo_id: i64,
        worktree: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EntityDefRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT d.worktree, d.path, d.fqn, d.kind, d.nesting, d.line_start,
                    d.line_end, d.zeitwerk_fqn, d.zeitwerk_state, d.blob_hash, f.blob_hash
             FROM entity_defs d
             LEFT JOIN files f ON f.repo_id = d.repo_id AND f.path = d.path
             WHERE d.repo_id = ?1 AND (?2 IS NULL OR d.worktree = ?2)
             ORDER BY d.fqn ASC, d.path ASC, d.ordinal ASC
             LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![repo_id, worktree, limit as i64], |r| {
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
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- V71-G0 — kbc-seq/1, the projection layer (`crate::seq`) ---------

    /// The `reading_sets`-backed projections (set / workspace / tour /
    /// trail). `kind` filters to one projection; `workspace_id` to one
    /// workspace's bound projections. A LAYER read: it invents no row and
    /// owns no row — `reading_sets` is still the home of every one of
    /// these.
    pub fn seq_reading_sets(
        &self,
        repo_id: i64,
        kind: Option<&str>,
        workspace_id: Option<&str>,
    ) -> Result<Vec<SeqProjectionRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.name, s.kind, s.ref, s.workspace_id, s.updated_at,
                    (SELECT COUNT(*) FROM reading_set_spans sp WHERE sp.set_id = s.id)
             FROM reading_sets s
             WHERE s.repo_id = ?1
               AND (?2 IS NULL OR s.kind = ?2)
               AND (?3 IS NULL OR s.workspace_id = ?3)
             ORDER BY s.kind ASC, s.name ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id, kind, workspace_id], |r| {
                Ok(SeqProjectionRow {
                    projection: r.get::<_, String>(2)?,
                    id: r.get(0)?,
                    name: r.get(1)?,
                    size: Some(r.get::<_, i64>(6)?),
                    ref_label: r.get(3)?,
                    workspace_id: r.get(4)?,
                    source: "reading_sets",
                    updated_at: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The `canvas_sets`-backed projection (board). `size` is `None`, not
    /// `0`: the board's geometry payload is opaque to this daemon
    /// (V0019's own contract), so its element count is genuinely unknown
    /// here — an unknown count must never render as "empty".
    pub fn seq_canvas_sets(&self, repo_id: i64) -> Result<Vec<SeqProjectionRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, updated_unix FROM canvas_sets WHERE repo_id = ?1 ORDER BY name ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                Ok(SeqProjectionRow {
                    projection: crate::seq::PROJECTION_BOARD.to_string(),
                    id: r.get::<_, i64>(0)?.to_string(),
                    name: r.get(1)?,
                    size: None,
                    ref_label: None,
                    workspace_id: None,
                    source: "canvas_sets",
                    updated_at: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Bind (or, with `None`, UNBIND) one projection to a workspace —
    /// kbc-seq/1's "convert to and from it with one key" (D26). A direct
    /// assignment rather than the `COALESCE` shape
    /// [`Self::update_reading_set_meta`] uses for its six fields, because
    /// unbinding has to be expressible: with `COALESCE` there is no way to
    /// say "clear it". Returns `true` iff `id` existed. Validation that
    /// the target is a real workspace in the same repo lives at the route
    /// (`reading_sets::patch_set`), which has the repo context.
    pub fn set_reading_set_workspace(
        &self,
        id: &str,
        workspace_id: Option<&str>,
        updated_at: i64,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reading_sets SET workspace_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, workspace_id, updated_at],
        )?;
        Ok(n > 0)
    }

    // --- V70-A2 (SEC-20) — the append-only mutations audit ledger --------
    //
    // Deliberately the LAST two methods on this impl, and deliberately
    // only two: the ledger is written by exactly one middleware
    // (`security::audit::audit_mutations`) and read by exactly one route
    // (`GET /api/audit`). There is no update, no delete and no prune —
    // see `migrations/V0027__mutations_audit.sql`'s header for why
    // "append-only" is a contract rather than a trigger.

    /// Append one audited mutation. Never bumps `generation`: the ledger
    /// is not row-set state any search cache reads (the `UpsertChunks`
    /// precedent kb's invariant #2 records, applied here).
    pub fn insert_mutation(&self, m: &MutationIn) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO mutations
                 (ts_unix, route, method, admission, repo, target,
                  blob_before, blob_after, request_id, outcome)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                m.ts_unix,
                m.route,
                m.method,
                m.admission,
                m.repo,
                m.target,
                m.blob_before,
                m.blob_after,
                m.request_id,
                m.outcome,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// `GET /api/audit?since=&limit=` — newest first, `ts_unix >= since`,
    /// at most `limit` rows. The caller clamps `limit` (500 hard cap);
    /// this method takes whatever it is given so the clamp lives in ONE
    /// place (the route) rather than two that could drift.
    pub fn list_mutations(&self, since_unix: i64, limit: usize) -> Result<Vec<MutationRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, ts_unix, route, method, admission, repo, target,
                    blob_before, blob_after, request_id, outcome
             FROM mutations
             WHERE ts_unix >= ?1
             ORDER BY ts_unix DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![since_unix, limit as i64], |r| {
                Ok(MutationRow {
                    id: r.get(0)?,
                    ts_unix: r.get(1)?,
                    route: r.get(2)?,
                    method: r.get(3)?,
                    admission: r.get(4)?,
                    repo: r.get(5)?,
                    target: r.get(6)?,
                    blob_before: r.get(7)?,
                    blob_after: r.get(8)?,
                    request_id: r.get(9)?,
                    outcome: r.get(10)?,
                })
            })?
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
fn current_salt_cte() -> (String, Vec<&'static str>) {
    let salts: Vec<&'static str> = crate::lang::ALL_LANGS.iter().map(|l| l.salt).collect();
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
}

impl StaleSaltSweepCounts {
    /// `true` if every table's count is zero — the common case, so the
    /// caller can skip logging a no-op sweep (mirrors `prune_stale_pins`'s
    /// `Ok(0) => {}` boot-log convention).
    pub fn is_empty(&self) -> bool {
        self.symbols == 0 && self.highlights == 0 && self.occurrences == 0
    }

    pub fn total(&self) -> u64 {
        self.symbols + self.highlights + self.occurrences
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
) -> Result<u64> {
    let blob_slots = blobs.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "{cte}
         DELETE FROM {table}
         WHERE blob_hash IN ({blob_slots})
           AND salt NOT IN (SELECT salt FROM cur)
           AND EXISTS (
                 SELECT 1 FROM {table} t2
                 WHERE t2.blob_hash = {table}.blob_hash AND t2.salt IN (SELECT salt FROM cur)
               )"
    );
    let mut stmt = tx.prepare(&sql)?;
    // Bind order matches the SQL: the `cur` CTE's salts are written first
    // (`{cte}` opens the statement), then this page's blob hashes.
    let mut bind: Vec<Box<dyn rusqlite::ToSql>> = salts.iter().map(|s| Box::new(*s) as _).collect();
    for b in blobs {
        bind.push(Box::new(b.clone()));
    }
    let n = stmt.execute(rusqlite::params_from_iter(bind.iter()))?;
    Ok(n as u64)
}

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
                review_id, ps_number, side, set_id
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
    /// "note" | "question" | "todo" | "flag-for-agent" | "tour-stop" —
    /// vocab validated at the route boundary
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

/// One todo list row (Phase N) — path already joined from `files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItemRow {
    pub path: String,
    pub line: i64,
    pub marker: String,
    pub text: String,
}

/// One todo to write via [`Store::replace_todo_items`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTodoItem {
    pub line: i64,
    pub marker: String,
    pub text: String,
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
    })
}

const REVIEW_FINDING_COLUMNS: &str = "id, review_id, annotation_id, slug, severity, category,
    location_kind, location_path, location_lines, location_removed,
    title, rationale, recommendation, evidence_lang, evidence_source,
    origin, author,
    disposition, disposition_note, disposition_by, disposition_at,
    content_updated_at, published_state, published_at, published_url,
    superseded, superseded_at, superseded_reason, import_batch_id,
    created_at, updated_at";

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
             created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16,
                 NULL, NULL, NULL, NULL,
                 NULL, 'unpublished', NULL, NULL,
                 0, NULL, NULL, ?17, ?18, ?19)",
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
        ],
    )?;
    let finding_id = tx.last_insert_rowid();
    Ok((annotation_id, finding_id))
}

impl Store {
    // -- PR binding (V0024) -------------------------------------------------

    /// The PR-binding + artifact-hint columns on `reviews`, as read back.
    /// `Ok(None)` iff `id` does not exist; every field inside `Some` is its
    /// own independent `Option` (a review can be PR-bound with no artifact
    /// hint yet, or vice versa — `PATCH /api/reviews/{id}` sets the hint
    /// independently of `POST /api/reviews/pr` setting the binding).
    pub fn get_review_pr_binding(&self, id: i64) -> Result<Option<ReviewPrBinding>> {
        // PF-K1 — hot on the (now-batched, see `get_review_pr_bindings`)
        // review list/inbox/recurrence composition paths; `prepare_cached`
        // (identical SQL every call) avoids a full re-parse/re-plan per call.
        let conn = self.lock();
        // Bound local: a `CachedStatement` tail-temporary outlives `conn`
        // (its Drop returns it to the cache) — E0597 otherwise.
        let mut stmt = conn.prepare_cached(
            "SELECT pr_number, pr_repo_slug, pr_head_sha, pr_meta_json, pr_meta_fetched_at,
                    artifact_hint_kb, artifact_hint_id
             FROM reviews WHERE id = ?1",
        )?;
        stmt.query_row(params![id], |r| {
            Ok(ReviewPrBinding {
                pr_number: r.get(0)?,
                pr_repo_slug: r.get(1)?,
                pr_head_sha: r.get(2)?,
                pr_meta_json: r.get(3)?,
                pr_meta_fetched_at: r.get(4)?,
                artifact_hint_kb: r.get(5)?,
                artifact_hint_id: r.get(6)?,
            })
        })
        .optional()
        .map_err(Into::into)
    }

    /// [`Self::get_review_pr_binding`] for a whole SET of `ids` — one
    /// query (dynamic `IN (…)`, plain `prepare`) instead of N round trips.
    /// Shared by the review list route ([`Self::latest_patchsets`]'s
    /// sibling), the inbox composition, and the findings-recurrence route's
    /// prior-review lookup. A missing id is simply absent from the map —
    /// every caller already treats a missing/`None` binding as
    /// [`ReviewPrBinding::default`].
    pub fn get_review_pr_bindings(&self, ids: &[i64]) -> Result<HashMap<i64, ReviewPrBinding>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.lock();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, pr_number, pr_repo_slug, pr_head_sha, pr_meta_json, pr_meta_fetched_at,
                    artifact_hint_kb, artifact_hint_id
             FROM reviews WHERE id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    ReviewPrBinding {
                        pr_number: r.get(1)?,
                        pr_repo_slug: r.get(2)?,
                        pr_head_sha: r.get(3)?,
                        pr_meta_json: r.get(4)?,
                        pr_meta_fetched_at: r.get(5)?,
                        artifact_hint_kb: r.get(6)?,
                        artifact_hint_id: r.get(7)?,
                    },
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().collect())
    }

    /// Bind a review to a PR — `POST /api/reviews/pr`'s (a later phase's)
    /// write. `pr_repo_slug` is required at bind time (resolved from the
    /// git origin); `pr_head_sha`/`pr_meta_json`/`pr_meta_fetched_at` are
    /// best-effort (the git fetch that creates the binding is load-bearing,
    /// the GitHub metadata enrichment call is not — design doc §2 row 1).
    /// Returns `true` iff `id` existed. A `(repo, pr_number)` collision
    /// surfaces as `StoreError::Sqlite` (the `idx_reviews_pr_binding`
    /// UNIQUE index) — a later phase's route decides how to react.
    #[allow(clippy::too_many_arguments)]
    pub fn set_review_pr_binding(
        &self,
        id: i64,
        pr_number: i64,
        pr_repo_slug: &str,
        pr_head_sha: Option<&str>,
        pr_meta_json: Option<&str>,
        pr_meta_fetched_at: Option<i64>,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews
             SET pr_number = ?2, pr_repo_slug = ?3, pr_head_sha = ?4,
                 pr_meta_json = ?5, pr_meta_fetched_at = ?6
             WHERE id = ?1",
            params![
                id,
                pr_number,
                pr_repo_slug,
                pr_head_sha,
                pr_meta_json,
                pr_meta_fetched_at
            ],
        )?;
        Ok(n > 0)
    }

    /// Refresh just the GitHub metadata snapshot on an ALREADY-bound review
    /// (`GET /api/prs/{n}` re-fetch, a later phase) — leaves `pr_number`/
    /// `pr_repo_slug` untouched. Returns `true` iff `id` existed.
    pub fn set_review_pr_meta(
        &self,
        id: i64,
        pr_head_sha: Option<&str>,
        pr_meta_json: Option<&str>,
        pr_meta_fetched_at: i64,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews
             SET pr_head_sha = ?2, pr_meta_json = ?3, pr_meta_fetched_at = ?4
             WHERE id = ?1",
            params![id, pr_head_sha, pr_meta_json, pr_meta_fetched_at],
        )?;
        Ok(n > 0)
    }

    /// Set (or, with both `None`, clear) the artifact hint — the ONLY write
    /// path design doc §4.2 sanctions (`PATCH /api/reviews/{id}` gaining
    /// two optional fields, a later phase). Never verified here (kb-code
    /// has no business validating a kb doc id against a schema it doesn't
    /// own — verification is `GET /api/reviews/{id}/artifact`'s live,
    /// unpersisted job). Returns `true` iff `id` existed.
    pub fn set_review_artifact_hint(
        &self,
        id: i64,
        kb: Option<&str>,
        doc_id: Option<&str>,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews SET artifact_hint_kb = ?2, artifact_hint_id = ?3 WHERE id = ?1",
            params![id, kb, doc_id],
        )?;
        Ok(n > 0)
    }

    /// PRR-R2 — look up a review already bound to `(repo, pr_number)`, the
    /// `POST /api/reviews/pr` pre-check (design doc §2 row 1: "Unique
    /// (repo, pr_number) violations -> 409 pointing at the existing review
    /// id"). Reading BEFORE the insert (rather than catching
    /// `idx_reviews_pr_binding`'s UNIQUE-constraint violation the way
    /// `name_conflict_or` does for reading sets) is deliberate here: the
    /// 409 body needs the EXISTING review's full id/repo/title, which a
    /// bare constraint-violation error carries none of, and this route's
    /// mutation is loopback-only / effectively single-operator, so the
    /// pre-check-then-insert race this leaves open is the same one
    /// `create_review`'s own two-step "resolve refs, then insert" already
    /// accepts.
    pub fn get_review_by_pr_binding(
        &self,
        repo: &str,
        pr_number: i64,
    ) -> Result<Option<ReviewRow>> {
        self.lock()
            .query_row(
                "SELECT id, repo, title, base_ref, head_ref, session_id, state,
                        created_at, updated_at, verdict, verdict_note, verdict_at, verdict_ps
                 FROM reviews WHERE repo = ?1 AND pr_number = ?2",
                params![repo, pr_number],
                review_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    // -- Review report (V0024) ----------------------------------------------

    /// `Ok(None)` iff `id` does not exist; `report_json: None` inside
    /// `Some` means "no report authored yet" (`GET /api/reviews/{id}/
    /// report`'s `{report: null}` case, a later phase).
    pub fn get_review_report(&self, id: i64) -> Result<Option<ReviewReport>> {
        self.lock()
            .query_row(
                "SELECT report_json, report_updated_at FROM reviews WHERE id = ?1",
                params![id],
                |r| {
                    Ok(ReviewReport {
                        report_json: r.get(0)?,
                        report_updated_at: r.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// [`Self::get_review_report`] for a whole SET of `ids` — one query
    /// (dynamic `IN (…)`, plain `prepare`) instead of N round trips. A
    /// missing id is simply absent from the map — callers already treat a
    /// missing/`None` report the same as [`ReviewReport::default`].
    pub fn get_review_reports(&self, ids: &[i64]) -> Result<HashMap<i64, ReviewReport>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.lock();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, report_json, report_updated_at FROM reviews WHERE id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    ReviewReport {
                        report_json: r.get(1)?,
                        report_updated_at: r.get(2)?,
                    },
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().collect())
    }

    /// Wholesale-replace the report (`PUT /api/reviews/{id}/report`, a
    /// later phase — never a partial-field merge, same reasoning as
    /// `pr_meta_json`). Returns `true` iff `id` existed.
    pub fn set_review_report(&self, id: i64, report_json: &str, updated_at: i64) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews SET report_json = ?2, report_updated_at = ?3 WHERE id = ?1",
            params![id, report_json, updated_at],
        )?;
        Ok(n > 0)
    }

    // -- Verdict publish record (V0024) -------------------------------------

    /// `Ok(None)` iff `id` does not exist.
    pub fn get_review_verdict_published(
        &self,
        id: i64,
    ) -> Result<Option<(Option<i64>, Option<String>)>> {
        self.lock()
            .query_row(
                "SELECT verdict_published_at, verdict_published_url FROM reviews WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Record that the review-level verdict was published to GitHub
    /// (`POST /api/reviews/{id}/verdict/published`, a later phase) —
    /// advisory only, see design doc Risk #5. Returns `true` iff `id`
    /// existed.
    pub fn set_review_verdict_published(
        &self,
        id: i64,
        url: Option<&str>,
        published_at: i64,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE reviews SET verdict_published_at = ?2, verdict_published_url = ?3 WHERE id = ?1",
            params![id, published_at, url],
        )?;
        Ok(n > 0)
    }

    // -- Findings (V0024) -----------------------------------------------

    /// Single (non-batch) finding create — see [`insert_review_finding_on`]
    /// for the shared transactional body. Returns `(annotation_id,
    /// review_findings.id)`. A `(review_id, slug)` collision surfaces as
    /// `StoreError::Sqlite` (`idx_review_findings_review_slug`).
    pub fn insert_review_finding(&self, f: &NewReviewFinding, now: i64) -> Result<(String, i64)> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let result = insert_review_finding_on(&tx, f, now)?;
        tx.commit()?;
        Ok(result)
    }

    /// Lookup by the finding's own stable identity — `(review_id, slug)`,
    /// the same pair `idx_review_findings_review_slug` uniquely indexes.
    pub fn get_review_finding(
        &self,
        review_id: i64,
        slug: &str,
    ) -> Result<Option<ReviewFindingRow>> {
        self.lock()
            .query_row(
                &format!(
                    "SELECT {REVIEW_FINDING_COLUMNS} FROM review_findings
                     WHERE review_id = ?1 AND slug = ?2"
                ),
                params![review_id, slug],
                review_finding_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// `GET /api/reviews/{id}/findings`'s (a later phase) data source —
    /// every finding for `review_id`, oldest-first, optionally filtered to
    /// an exact `disposition` and/or including superseded (tombstoned)
    /// rows. Default (`include_superseded=false`) excludes them — the
    /// soft-forget "still readable via an explicit opt-in, invisible by
    /// default" convention (invariant #10).
    pub fn list_review_findings(
        &self,
        review_id: i64,
        disposition: Option<&str>,
        include_superseded: bool,
    ) -> Result<Vec<ReviewFindingRow>> {
        let conn = self.lock();
        // PF-K1 — the `format!`-built SQL is IDENTICAL every call (its
        // pieces are all compile-time constants), so `prepare_cached` is
        // still eligible despite the `format!` wrapper.
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {REVIEW_FINDING_COLUMNS} FROM review_findings
             WHERE review_id = ?1
               AND (?2 = 1 OR superseded = 0)
               AND (?3 IS NULL OR disposition = ?3)
             ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = stmt
            .query_map(
                params![review_id, include_superseded as i64, disposition],
                review_finding_row_from,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// [`Self::list_review_findings`] for a whole SET of `review_ids` —
    /// one query (dynamic `IN (…)`, plain `prepare`) instead of N round
    /// trips. A review with no matching findings is simply absent from the
    /// map. `review_id` numbered placeholders start at `?3` (`?1`/`?2` are
    /// `include_superseded`/`disposition`, bound once for the whole set —
    /// same two-fixed-then-N-dynamic shape `list_review_findings`'s own
    /// SQL already uses).
    pub fn list_review_findings_batch(
        &self,
        review_ids: &[i64],
        disposition: Option<&str>,
        include_superseded: bool,
    ) -> Result<HashMap<i64, Vec<ReviewFindingRow>>> {
        let mut out: HashMap<i64, Vec<ReviewFindingRow>> = HashMap::new();
        if review_ids.is_empty() {
            return Ok(out);
        }
        let conn = self.lock();
        let placeholders = (0..review_ids.len())
            .map(|i| format!("?{}", i + 3))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {REVIEW_FINDING_COLUMNS} FROM review_findings
             WHERE review_id IN ({placeholders})
               AND (?1 = 1 OR superseded = 0)
               AND (?2 IS NULL OR disposition = ?2)
             ORDER BY review_id ASC, created_at ASC, id ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut binds: Vec<rusqlite::types::Value> = Vec::with_capacity(2 + review_ids.len());
        binds.push(rusqlite::types::Value::Integer(include_superseded as i64));
        binds.push(match disposition {
            Some(d) => rusqlite::types::Value::Text(d.to_string()),
            None => rusqlite::types::Value::Null,
        });
        for id in review_ids {
            binds.push(rusqlite::types::Value::Integer(*id));
        }
        let rows = stmt
            .query_map(rusqlite::params_from_iter(binds), review_finding_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for row in rows {
            out.entry(row.review_id).or_default().push(row);
        }
        Ok(out)
    }

    /// Set (or replace) a finding's human disposition — loopback-only at
    /// the route layer (design doc Risk #3), `PUT .../findings/{slug}/
    /// disposition` (a later phase). Compares `(disposition, note)` only,
    /// same no-op convention as `set_review_verdict` — `disposition_by`
    /// alone changing (same reviewer re-clicking, or the identity changing
    /// on an otherwise-identical call) is not itself treated as a change.
    /// `Ok(None)` iff `(review_id, slug)` does not exist, `Ok(Some(false))`
    /// on a no-op, `Ok(Some(true))` when written.
    pub fn set_finding_disposition(
        &self,
        review_id: i64,
        slug: &str,
        disposition: &str,
        note: Option<&str>,
        by: &str,
        at: i64,
    ) -> Result<Option<bool>> {
        let conn = self.lock();
        let existing: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT disposition, disposition_note FROM review_findings
                 WHERE review_id = ?1 AND slug = ?2",
                params![review_id, slug],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((cur_disposition, cur_note)) = existing else {
            return Ok(None);
        };
        let unchanged =
            cur_disposition.as_deref() == Some(disposition) && cur_note.as_deref() == note;
        if unchanged {
            return Ok(Some(false));
        }
        conn.execute(
            "UPDATE review_findings
             SET disposition = ?3, disposition_note = ?4, disposition_by = ?5,
                 disposition_at = ?6, updated_at = ?6
             WHERE review_id = ?1 AND slug = ?2",
            params![review_id, slug, disposition, note, by, at],
        )?;
        Ok(Some(true))
    }

    /// Clear a finding's disposition back to undecided —
    /// `DELETE .../findings/{slug}/disposition` (a later phase). `Ok(None)`
    /// iff `(review_id, slug)` does not exist, `Ok(Some(false))` when there
    /// was nothing to clear, `Ok(Some(true))` when cleared.
    pub fn clear_finding_disposition(
        &self,
        review_id: i64,
        slug: &str,
        at: i64,
    ) -> Result<Option<bool>> {
        let conn = self.lock();
        let cur: Option<Option<String>> = conn
            .query_row(
                "SELECT disposition FROM review_findings WHERE review_id = ?1 AND slug = ?2",
                params![review_id, slug],
                |r| r.get(0),
            )
            .optional()?;
        let Some(cur) = cur else {
            return Ok(None);
        };
        if cur.is_none() {
            return Ok(Some(false));
        }
        conn.execute(
            "UPDATE review_findings
             SET disposition = NULL, disposition_note = NULL, disposition_by = NULL,
                 disposition_at = NULL, updated_at = ?3
             WHERE review_id = ?1 AND slug = ?2",
            params![review_id, slug, at],
        )?;
        Ok(Some(true))
    }

    /// Record that a finding was published to GitHub as a review comment
    /// (`POST .../findings/{slug}/published`, a later phase) — advisory
    /// only (design doc Risk #5); recorded AFTER the agent's own `gh` call
    /// succeeds, never verified. Returns `true` iff `(review_id, slug)`
    /// existed.
    pub fn set_finding_published(
        &self,
        review_id: i64,
        slug: &str,
        published_url: Option<&str>,
        published_at: i64,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE review_findings
             SET published_state = 'published', published_at = ?3, published_url = ?4,
                 updated_at = ?3
             WHERE review_id = ?1 AND slug = ?2",
            params![review_id, slug, published_at, published_url],
        )?;
        Ok(n > 0)
    }

    /// The design doc §4.3 reconciliation core, run inside ONE transaction
    /// (`POST /api/reviews/{id}/findings/import`'s data-layer body, a later
    /// phase): every `findings` entry is either a brand NEW slug (create,
    /// `origin="import"`), an EXISTING slug present again (refresh volatile
    /// fields, stamp `content_updated_at`, un-supersede — but NEVER touch
    /// disposition or the annotation's thread/anchor), or truly unchanged
    /// (skip the write entirely).
    ///
    /// PRR-R1 scope extension (operator-ratified mid-build, human-authored
    /// findings): the supersede step only ever runs under
    /// [`FindingsImportMode::Full`] (`Additive` supersedes NOTHING — a
    /// later phase's "add more findings without touching what's already
    /// there" case), and even then ONLY over EXISTING, non-superseded rows
    /// whose `origin = "import"` — a `"manual"` (human-authored) row is
    /// NEVER superseded by an agent's re-import, `Full` or `Additive`,
    /// because the agent's own findings set structurally cannot contain a
    /// human-authored slug it never generated. Every superseded row gets
    /// `superseded_reason = "not_in_reimport"` — never hard-deleted.
    ///
    /// `repo_id`/`ps_number`/`author`/`import_batch_id` apply to every
    /// newly-created finding in this call (always `origin="import"`,
    /// `review_findings.author = None` — see [`NewReviewFinding::finding_
    /// author`]'s doc); `ps_number` is the CURRENT (now-latest) patchset new
    /// findings are anchored at — an existing finding's own annotation
    /// keeps whatever `ps_number`/anchor it was first created with (design
    /// doc §4.3 point 5: "the annotation's own anchor is NOT eagerly
    /// rewritten").
    #[allow(clippy::too_many_arguments)]
    pub fn reconcile_findings_import(
        &self,
        review_id: i64,
        repo_id: i64,
        ps_number: i64,
        import_batch_id: &str,
        author: &str,
        findings: &[ImportedFinding],
        mode: FindingsImportMode,
        now: i64,
    ) -> Result<FindingsImportOutcome> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let outcome = reconcile_findings_import_on(
            &tx,
            review_id,
            repo_id,
            ps_number,
            import_batch_id,
            author,
            findings,
            mode,
            now,
        )?;
        tx.commit()?;
        Ok(outcome)
    }

    /// V70-R — `review compose` v0 (design doc D9's "one authoring
    /// transaction," scoped down to what the milestone needs): findings
    /// reconciliation (the SAME core [`Self::reconcile_findings_import`]
    /// uses, factored out as [`reconcile_findings_import_on`] so both
    /// share one implementation), the flat report
    /// (`reviews::REPORT_ALLOWED_KEYS` shape — pre-normalised by the
    /// caller; this method never re-validates the shape), and an OPTIONAL
    /// review-level verdict, all inside the SAME sqlite transaction — a
    /// real `BEGIN`/`COMMIT`, not merely "one HTTP call": unlike the
    /// three-route pipeline (`findings/import` + `PUT /report` +
    /// `PUT /verdict`, each its own `self.lock()` + commit), a failure
    /// partway through this call rolls every prior write back, so a caller
    /// never observes findings imported with no report, or a report set
    /// with no verdict, from one `compose` call. Verdict uses the SAME
    /// "no-op on an identical (state, note) pair" rule as
    /// [`Self::set_review_verdict`] (kb-core `ReviewFile::set_verdict`
    /// G8), re-implemented here against `tx` rather than calling that
    /// method directly — `self.lock()` is a `parking_lot::Mutex`, not
    /// reentrant, so a second lock attempt from within an already-locked
    /// call would deadlock, not queue.
    #[allow(clippy::too_many_arguments)]
    pub fn compose_review(
        &self,
        review_id: i64,
        repo_id: i64,
        ps_number: i64,
        import_batch_id: &str,
        author: &str,
        findings: &[ImportedFinding],
        mode: FindingsImportMode,
        report_json: &str,
        verdict: Option<(&str, Option<&str>)>,
        now: i64,
    ) -> Result<ComposeOutcome> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;

        let findings_outcome = reconcile_findings_import_on(
            &tx,
            review_id,
            repo_id,
            ps_number,
            import_batch_id,
            author,
            findings,
            mode,
            now,
        )?;

        tx.execute(
            "UPDATE reviews SET report_json = ?2, report_updated_at = ?3 WHERE id = ?1",
            params![review_id, report_json, now],
        )?;

        let mut verdict_changed = false;
        if let Some((verdict_state, note)) = verdict {
            let cur: Option<(Option<String>, Option<String>)> = tx
                .query_row(
                    "SELECT verdict, verdict_note FROM reviews WHERE id = ?1",
                    params![review_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((cur_state, cur_note)) = cur {
                let unchanged =
                    cur_state.as_deref() == Some(verdict_state) && cur_note.as_deref() == note;
                if !unchanged {
                    tx.execute(
                        "UPDATE reviews
                         SET verdict = ?2, verdict_note = ?3, verdict_at = ?4, verdict_ps = ?5
                         WHERE id = ?1",
                        params![review_id, verdict_state, note, now, ps_number],
                    )?;
                    verdict_changed = true;
                }
            }
        }

        tx.commit()?;
        Ok(ComposeOutcome {
            findings: findings_outcome,
            report_set: true,
            verdict_changed,
        })
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

    for f in findings {
        seen.insert(f.slug.clone());
        match existing.get(&f.slug) {
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
                };
                insert_review_finding_on(tx, &new_row, now)?;
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
                    || cur.evidence_source != f.evidence_source;
                if content_changed || cur.superseded {
                    tx.execute(
                        "UPDATE review_findings SET
                            severity = ?2, category = ?3, location_kind = ?4, location_path = ?5,
                            location_lines = ?6, location_removed = ?7, title = ?8, rationale = ?9,
                            recommendation = ?10, evidence_lang = ?11, evidence_source = ?12,
                            content_updated_at = ?13, superseded = 0, superseded_at = NULL,
                            superseded_reason = NULL, updated_at = ?14
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
                tx.execute(
                    "UPDATE review_findings
                     SET superseded = 1, superseded_at = ?2,
                         superseded_reason = 'not_in_reimport', updated_at = ?2
                     WHERE id = ?1",
                    params![row.id, now],
                )?;
                outcome.superseded.push(slug.clone());
            }
        }
    }

    Ok(outcome)
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

impl Store {
    /// Append one `scip_runs` row — called unconditionally on every
    /// successful (HTTP 200) `POST /api/scip/ingest`, regardless of
    /// `docs_accepted` (even an ingest that accepted zero documents — every
    /// doc stale/untracked/unsupported-lang — still honestly records "an
    /// ingest ran at this HEAD"; `ScipStatus::docs_covered` separately
    /// surfaces the zero, so `fresh = true` with `docs_covered = 0` is not
    /// misleading). Does NOT bump the store generation (mirrors
    /// `doc_lens_pins`' precedent, `pin_writes_do_not_bump_the_store_
    /// generation`): `scip_runs` feeds only `GET /api/repos`'s `ScipStatus`,
    /// never `FileIndex`/`SymbolIndex`'s generation-keyed search caches.
    pub fn record_scip_run(
        &self,
        repo_id: i64,
        head_sha: &str,
        ingested_at: i64,
        docs_accepted: i64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO scip_runs (repo_id, head_sha, ingested_at, docs_accepted)
             VALUES (?1, ?2, ?3, ?4)",
            params![repo_id, head_sha, ingested_at, docs_accepted],
        )?;
        Ok(())
    }

    /// The most recent `scip_runs` row for `repo_id`, or `None` if this repo
    /// has never had a successful SCIP ingest — `ScipStatus`'s
    /// "never-ingested" case. Ties on `ingested_at` (a same-second double
    /// ingest) break toward the row inserted LAST (`rowid DESC` as the
    /// tiebreak) rather than an arbitrary one.
    pub fn latest_scip_run(&self, repo_id: i64) -> Result<Option<ScipRunRow>> {
        self.lock()
            .query_row(
                "SELECT head_sha, ingested_at, docs_accepted FROM scip_runs
                 WHERE repo_id = ?1 ORDER BY ingested_at DESC, rowid DESC LIMIT 1",
                params![repo_id],
                |r| {
                    Ok(ScipRunRow {
                        head_sha: r.get(0)?,
                        ingested_at: r.get(1)?,
                        docs_accepted: r.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// `COUNT(DISTINCT path)` among `repo_id`'s CURRENT `files` rows whose
    /// blob carries at least one `source = 'scip'` occurrence row —
    /// `ScipStatus::docs_covered`: "how many of this repo's files actually
    /// carry exact-tier SCIP positions right now." Joins on
    /// `files.blob_hash` (ADR-2 keying — `occurrences` has no
    /// `repo_id`/`path` column of its own), so a blob shared by two paths
    /// counts once per path, matching `count_files_by_langs`'s per-path
    /// unit.
    pub fn count_scip_covered_files(&self, repo_id: i64) -> Result<u64> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(DISTINCT f.path) FROM files f
             WHERE f.repo_id = ?1 AND EXISTS (
                 SELECT 1 FROM occurrences o
                 WHERE o.blob_hash = f.blob_hash AND o.source = 'scip'
             )",
            params![repo_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// `COUNT(*)` of `repo_id`'s current `files` rows whose `lang` is one of
    /// `langs` (the repo's `[[scip.repos]] langs` — informational tags
    /// naming which languages that repo's configured SCIP indexer covers).
    /// Empty `langs` short-circuits to `0` without a query (an unconfigured
    /// repo has no SCIP langs to count against). `ScipStatus::docs_total`'s
    /// source.
    pub fn count_files_by_langs(&self, repo_id: i64, langs: &[String]) -> Result<u64> {
        if langs.is_empty() {
            return Ok(0);
        }
        let conn = self.lock();
        let placeholders = langs
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql =
            format!("SELECT COUNT(*) FROM files WHERE repo_id = ?1 AND lang IN ({placeholders})");
        let mut stmt = conn.prepare(&sql)?;
        let mut params_vec: Vec<rusqlite::types::Value> = Vec::with_capacity(1 + langs.len());
        params_vec.push(repo_id.into());
        for l in langs {
            params_vec.push(l.clone().into());
        }
        let n: i64 = stmt.query_row(rusqlite::params_from_iter(params_vec), |r| r.get(0))?;
        Ok(n as u64)
    }
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

impl Store {
    /// Every [`AnalyticsFindingRow`] (superseded INCLUDED — see the note
    /// below) joined to `reviews` for `repo` (or every repo when `None`),
    /// optionally windowed to `rf.created_at` in `[from, to]` (either bound
    /// optional). Oldest-first — `review_analytics`'s pure aggregation is
    /// order-independent, but a stable input order keeps its own test
    /// fixtures readable.
    ///
    /// `superseded` rows are DELIBERATELY still included here (unlike
    /// [`Self::list_review_findings`]'s default) — design-addendum-2 §C:
    /// "non-superseded by default, superseded reported separately." The
    /// caller (`review_analytics::compute_analytics`) does the split so
    /// the superseded count itself is a named, surfaced term rather than a
    /// silently dropped row.
    pub fn list_findings_for_analytics(
        &self,
        repo: Option<&str>,
        from: Option<i64>,
        to: Option<i64>,
    ) -> Result<Vec<AnalyticsFindingRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT rf.review_id, rf.severity, rf.category, rf.location_path,
                    rf.disposition, rf.disposition_at, rf.published_state,
                    rf.superseded, rf.created_at
             FROM review_findings rf
             JOIN reviews r ON r.id = rf.review_id
             WHERE (?1 IS NULL OR r.repo = ?1)
               AND (?2 IS NULL OR rf.created_at >= ?2)
               AND (?3 IS NULL OR rf.created_at <= ?3)
             ORDER BY rf.created_at ASC, rf.id ASC",
        )?;
        let rows = stmt
            .query_map(params![repo, from, to], analytics_finding_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// `(category, location_path)` pairs seen in `>= min_reviews` DISTINCT
    /// non-superseded findings' reviews, same `repo`/`from`/`to` filter as
    /// [`Self::list_findings_for_analytics`]. Descending by `review_count`,
    /// tied pairs broken by `category` then `location_path` ascending — a
    /// full, deterministic order (mirrors `sort_inbox_rows`'s own
    /// "same state -> byte-identical order" contract).
    pub fn recurrence_pairs(
        &self,
        repo: Option<&str>,
        from: Option<i64>,
        to: Option<i64>,
        min_reviews: i64,
    ) -> Result<Vec<RecurrenceRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT rf.category, rf.location_path,
                    COUNT(DISTINCT rf.review_id) AS review_count,
                    COUNT(*) AS finding_count,
                    GROUP_CONCAT(DISTINCT rf.review_id) AS review_ids
             FROM review_findings rf
             JOIN reviews r ON r.id = rf.review_id
             WHERE rf.superseded = 0
               AND (?1 IS NULL OR r.repo = ?1)
               AND (?2 IS NULL OR rf.created_at >= ?2)
               AND (?3 IS NULL OR rf.created_at <= ?3)
             GROUP BY rf.category, rf.location_path
             HAVING COUNT(DISTINCT rf.review_id) >= ?4
             ORDER BY review_count DESC, rf.category ASC, rf.location_path ASC",
        )?;
        let rows = stmt
            .query_map(params![repo, from, to, min_reviews], |r| {
                let ids_str: String = r.get(4)?;
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    ids_str,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (category, location_path, review_count, finding_count, ids_str) in rows {
            let mut review_ids: Vec<i64> = ids_str
                .split(',')
                .filter_map(|s| s.parse::<i64>().ok())
                .collect();
            review_ids.sort_unstable();
            out.push(RecurrenceRow {
                category,
                location_path,
                review_count,
                finding_count,
                review_ids,
            });
        }
        Ok(out)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::HighlightClass;

    fn open_temp() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        (tmp, store)
    }

    #[test]
    fn open_runs_migrations_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("index.db");
        let _s1 = Store::open(&db_path).unwrap();
        // Re-opening the same file must not fail (refinery no-ops on an
        // already-migrated schema).
        let _s2 = Store::open(&db_path).unwrap();
    }

    /// kb-sibling/1 — a volume forward-migrated by a NEWER kb-code binary
    /// must refuse to open, naming both epochs and the db path. The history
    /// row is FABRICATED at `schema_epoch() + 1000` (migrations themselves
    /// are immutable); no real migration will ever reach that version.
    // invariant:2 kb-sibling/1 schema-epoch boot refuse
    #[test]
    fn open_refuses_a_volume_whose_schema_epoch_is_ahead_of_this_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("index.db");
        let ahead = schema_epoch() + 1_000;
        {
            let store = Store::open(&db_path).expect("first open migrates normally");
            store
                .lock()
                .execute(
                    "INSERT INTO refinery_schema_history (version, name, applied_on, checksum) \
                     VALUES (?1, 'from_a_newer_binary', '', '0')",
                    params![ahead],
                )
                .unwrap();
        }
        let err = match Store::open(&db_path) {
            Ok(_) => panic!("an ahead volume must refuse to open"),
            Err(e) => e,
        };
        assert!(matches!(err, StoreError::SchemaEpoch(_)), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("refusing to boot"), "{msg}");
        assert!(msg.contains(&format!("V{ahead}")), "{msg}");
        assert!(msg.contains(&format!("V{}", schema_epoch())), "{msg}");
        assert!(msg.contains("index.db"), "{msg}");
    }

    /// The passing case at EQUAL epoch — a freshly migrated volume sits
    /// exactly at the binary epoch and re-opens normally.
    #[test]
    fn open_proceeds_when_the_volume_epoch_equals_the_binary_epoch() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        assert_eq!(
            kb_core::sibling::volume_epoch(&store.lock()).unwrap(),
            Some(schema_epoch()),
        );
        drop(store);
        Store::open(&db_path).expect("re-opening at an equal epoch must boot");
    }

    /// V0007 (B2) on a FRESH db: refinery runs the whole chain
    /// (V0001..V0007) in one shot, so `doc` is reachable and a symbol
    /// carrying `Some(doc)` round-trips normally.
    #[test]
    fn v0007_migration_applies_cleanly_on_a_fresh_db() {
        let (_tmp, store) = open_temp();
        let mut sym = sample_symbol(0, "documented");
        sym.doc = Some("a doc comment".to_string());
        store
            .replace_symbols("hashA", "rust@1", &[sym.clone()])
            .unwrap();
        assert_eq!(
            store.symbols_for_blob("hashA", "rust@1").unwrap(),
            vec![sym]
        );
    }

    /// V0007's `ALTER TABLE symbols ADD COLUMN doc TEXT` (no DEFAULT) leaves
    /// every PRE-EXISTING row's new column NULL — the same outcome a raw
    /// INSERT that never mentions `doc` produces, so this stands in for "a
    /// symbols row written before this migration existed" without needing to
    /// fake a partial-migration refinery history: either way, an old-shaped
    /// row reads back with `doc: None`.
    #[test]
    fn pre_migration_shaped_symbol_rows_read_back_with_doc_none() {
        let (_tmp, store) = open_temp();
        store
            .lock()
            .execute(
                "INSERT INTO symbols
                    (blob_hash, salt, ordinal, name, kind, line_start, line_end, col_start, col_end)
                 VALUES ('hashOld', 'rust@1', 0, 'legacy', 'fn', 1, 1, 0, 1)",
                [],
            )
            .unwrap();
        let rows = store.symbols_for_blob("hashOld", "rust@1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "legacy");
        assert_eq!(rows[0].doc, None);
        assert_eq!(rows[0].signature, None);
    }

    #[test]
    fn upsert_repo_is_idempotent_and_updates_root() {
        let (_tmp, store) = open_temp();
        let id1 = store.upsert_repo("kb", "/tmp/kb").unwrap();
        let id2 = store.upsert_repo("kb", "/tmp/kb-renamed").unwrap();
        assert_eq!(id1, id2);
        assert_eq!(store.repo_id("kb").unwrap(), Some(id1));
        assert_eq!(store.repo_id("nope").unwrap(), None);
    }

    #[test]
    fn files_upsert_and_count_round_trip() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("kb", "/tmp/kb").unwrap();
        store
            .upsert_file(repo_id, "src/lib.rs", "hash1", "rust", 100)
            .unwrap();
        store
            .upsert_file(repo_id, "src/main.rs", "hash2", "rust", 50)
            .unwrap();
        assert_eq!(store.file_count(repo_id).unwrap(), 2);

        let row = store.get_file(repo_id, "src/lib.rs").unwrap().unwrap();
        assert_eq!(row.blob_hash, "hash1");
        assert_eq!(row.size, 100);

        // Re-upserting the same path updates in place, not a new row.
        store
            .upsert_file(repo_id, "src/lib.rs", "hash1-v2", "rust", 200)
            .unwrap();
        assert_eq!(store.file_count(repo_id).unwrap(), 2);
        let row = store.get_file(repo_id, "src/lib.rs").unwrap().unwrap();
        assert_eq!(row.blob_hash, "hash1-v2");
        assert_eq!(row.size, 200);
    }

    #[test]
    fn delete_file_removes_the_row_and_leaves_derived_data_alone() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "src/lib.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .replace_symbols("hashA", "rust@1", &[sample_symbol(0, "foo")])
            .unwrap();
        assert_eq!(store.file_count(repo_id).unwrap(), 1);

        store.delete_file(repo_id, "src/lib.rs").unwrap();
        assert_eq!(store.file_count(repo_id).unwrap(), 0);
        assert!(store.get_file(repo_id, "src/lib.rs").unwrap().is_none());
        // Derived rows are blob-keyed, not path-keyed — deleting the files
        // row must never touch them.
        assert!(store.has_symbols("hashA", "rust@1").unwrap());

        // Deleting an already-gone path is a no-op, not an error.
        store.delete_file(repo_id, "src/lib.rs").unwrap();
    }

    #[test]
    fn delete_file_prunes_file_opens_rows_for_the_same_path() {
        // A4 — unlike blob-keyed symbols/highlights, `file_opens` is keyed
        // by (repo_id, path), same as `files`: an orphaned row there isn't
        // inert, it resurfaces in `recent_file_opens` (no join against
        // `files`) — see `delete_file`'s doc.
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "src/lib.rs", "hashA", "rust", 10)
            .unwrap();
        store.bump_file_open(repo_id, "src/lib.rs", 1_000).unwrap();
        assert_eq!(
            store.last_opened_map(repo_id).unwrap().get("src/lib.rs"),
            Some(&1_000)
        );
        assert_eq!(
            store.recent_file_opens(&[repo_id], 10, None).unwrap(),
            vec![(repo_id, "src/lib.rs".to_string(), 1_000)]
        );

        store.delete_file(repo_id, "src/lib.rs").unwrap();

        assert!(!store
            .last_opened_map(repo_id)
            .unwrap()
            .contains_key("src/lib.rs"));
        assert!(store
            .recent_file_opens(&[repo_id], 10, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn delete_file_only_touches_the_named_repo() {
        let (_tmp, store) = open_temp();
        let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
        let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
        store
            .upsert_file(repo_a, "same/path.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .upsert_file(repo_b, "same/path.rs", "hashB", "rust", 20)
            .unwrap();

        store.delete_file(repo_a, "same/path.rs").unwrap();
        assert!(store.get_file(repo_a, "same/path.rs").unwrap().is_none());
        assert!(store.get_file(repo_b, "same/path.rs").unwrap().is_some());
    }

    // ── PRR-N3 R1 fix: rails_edges replace/delete are path-scoped ───────

    fn sample_rails_edge(dst_path: &str, line: u32) -> crate::frameworks::FrameworkEdge {
        crate::frameworks::FrameworkEdge {
            kind: crate::frameworks::EdgeKind::RenderPartial,
            src_path: String::new(), // overwritten by the caller below
            src_line: Some(line),
            src_symbol: None,
            dst_kind: Some("partial".to_string()),
            dst_path: Some(dst_path.to_string()),
            dst_symbol: None,
            trust: crate::frameworks::Trust::Likely,
            extra_json: None,
        }
    }

    #[test]
    fn replace_rails_edges_on_a_new_blob_drops_the_previous_blobs_rows_for_the_same_path() {
        // R1 — editing a lens-relevant file must not leave the PREVIOUS
        // blob's rows live: the delete is now `(repo_id, src_path)`
        // scoped, not `(blob_hash, salt)`.
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let path = "app/controllers/x_controller.rb";

        let mut edge_a = sample_rails_edge("app/views/x/old.html.erb", 5);
        edge_a.src_path = path.to_string();
        store
            .replace_rails_edges(repo_id, path, "blobA", "rails-lens/1", &[edge_a])
            .unwrap();
        assert_eq!(
            store.rails_edges_by_src_path(repo_id, path).unwrap().len(),
            1
        );
        assert_eq!(
            store
                .rails_edges_by_dst_path(repo_id, "app/views/x/old.html.erb")
                .unwrap()
                .len(),
            1
        );

        // The SAME path, edited — a new blob with a DIFFERENT edge.
        let mut edge_b = sample_rails_edge("app/views/x/new.html.erb", 9);
        edge_b.src_path = path.to_string();
        store
            .replace_rails_edges(repo_id, path, "blobB", "rails-lens/1", &[edge_b])
            .unwrap();

        let src_rows = store.rails_edges_by_src_path(repo_id, path).unwrap();
        assert_eq!(
            src_rows.len(),
            1,
            "only blob B's row must remain: {src_rows:#?}"
        );
        assert_eq!(
            src_rows[0].dst_path.as_deref(),
            Some("app/views/x/new.html.erb")
        );
        assert_eq!(src_rows[0].src_line, Some(9));

        // The OLD blob's dst is no longer reachable via the reverse index.
        assert!(store
            .rails_edges_by_dst_path(repo_id, "app/views/x/old.html.erb")
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .rails_edges_by_dst_path(repo_id, "app/views/x/new.html.erb")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn replace_rails_edges_never_touches_a_different_paths_rows() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let p1 = "app/controllers/x_controller.rb";
        let p2 = "app/controllers/y_controller.rb";

        let mut e1 = sample_rails_edge("app/views/x/show.html.erb", 1);
        e1.src_path = p1.to_string();
        store
            .replace_rails_edges(repo_id, p1, "blobX", "rails-lens/1", &[e1])
            .unwrap();
        let mut e2 = sample_rails_edge("app/views/y/show.html.erb", 1);
        e2.src_path = p2.to_string();
        store
            .replace_rails_edges(repo_id, p2, "blobY", "rails-lens/1", &[e2])
            .unwrap();

        // Re-index p1 (a fresh edit) — p2's row must be untouched.
        let mut e1b = sample_rails_edge("app/views/x/edited.html.erb", 2);
        e1b.src_path = p1.to_string();
        store
            .replace_rails_edges(repo_id, p1, "blobX2", "rails-lens/1", &[e1b])
            .unwrap();

        assert_eq!(store.rails_edges_by_src_path(repo_id, p2).unwrap().len(), 1);
        assert_eq!(
            store.rails_edges_by_src_path(repo_id, p2).unwrap()[0]
                .dst_path
                .as_deref(),
            Some("app/views/y/show.html.erb")
        );
    }

    #[test]
    fn delete_file_prunes_rails_edges_rows_for_the_same_path() {
        // R1 — deleting a controller must not leave phantom
        // `route_action`/`render_partial` rows that `/api/usages` would
        // report as real usages of a live partial forever.
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let path = "app/controllers/x_controller.rb";
        let mut edge = sample_rails_edge("app/views/x/show.html.erb", 5);
        edge.src_path = path.to_string();
        store
            .upsert_file(repo_id, path, "blobA", "ruby", 10)
            .unwrap();
        store
            .replace_rails_edges(repo_id, path, "blobA", "rails-lens/1", &[edge])
            .unwrap();
        assert_eq!(
            store.rails_edges_by_src_path(repo_id, path).unwrap().len(),
            1
        );

        store.delete_file(repo_id, path).unwrap();

        assert!(store
            .rails_edges_by_src_path(repo_id, path)
            .unwrap()
            .is_empty());
        assert!(store
            .rails_edges_by_dst_path(repo_id, "app/views/x/show.html.erb")
            .unwrap()
            .is_empty());
    }

    // --- V71-G0 — the entity index ---------------------------------------

    fn entity_claim(fqn: &str, zeitwerk: Option<&str>) -> crate::entities::EntityDefClaim {
        crate::entities::EntityDefClaim {
            fqn: fqn.to_string(),
            kind: "class".to_string(),
            nesting: crate::entities::NESTING_LEXICAL,
            line_start: 1,
            line_end: 9,
            zeitwerk_fqn: zeitwerk.map(|s| s.to_string()),
        }
    }

    #[test]
    fn entity_defs_round_trip_and_report_whether_the_indexed_blob_is_still_live() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let path = "app/models/reseller/order.rb";
        store
            .upsert_file(repo_id, path, "blobA", "ruby", 10)
            .unwrap();
        store
            .replace_entity_defs(
                repo_id,
                "",
                path,
                "blobA",
                crate::entities::zeitwerk::STATE_READ,
                &[entity_claim("Reseller::Order", Some("Reseller::Order"))],
            )
            .unwrap();

        let rows = store
            .entity_defs_for_name(repo_id, None, "Reseller::Order", 10)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].live_blob_hash.as_deref(), Some("blobA"));
        assert_eq!(
            rows[0].zeitwerk_state,
            crate::entities::zeitwerk::STATE_READ
        );
        assert_eq!(rows[0].nesting, crate::entities::NESTING_LEXICAL);

        // The file's content moves on; the claim is still there but is now
        // demonstrably about bytes that are gone.
        store
            .upsert_file(repo_id, path, "blobB", "ruby", 11)
            .unwrap();
        let rows = store
            .entity_defs_for_name(repo_id, None, "Reseller::Order", 10)
            .unwrap();
        assert_eq!(rows[0].blob_hash, "blobA");
        assert_eq!(rows[0].live_blob_hash.as_deref(), Some("blobB"));
    }

    #[test]
    fn entity_defs_are_addressable_by_the_zeitwerk_name_as_well_as_the_nested_one() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let path = "app/models/reseller/order.rb";
        store
            .upsert_file(repo_id, path, "blobA", "ruby", 10)
            .unwrap();
        store
            .replace_entity_defs(
                repo_id,
                "",
                path,
                "blobA",
                crate::entities::zeitwerk::STATE_READ,
                // The tree proves `Order`; the convention says
                // `Reseller::Order`. Both must find the row.
                &[entity_claim("Order", Some("Reseller::Order"))],
            )
            .unwrap();
        assert_eq!(
            store
                .entity_defs_for_name(repo_id, None, "Order", 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .entity_defs_for_name(repo_id, None, "Reseller::Order", 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn the_last_segment_fallback_fires_only_when_nothing_matches_exactly() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        for (path, fqn) in [
            ("app/models/reseller/order.rb", "Reseller::Order"),
            ("app/models/billing/order.rb", "Billing::Order"),
            ("app/models/order.rb", "Order"),
        ] {
            store
                .upsert_file(repo_id, path, "blobA", "ruby", 10)
                .unwrap();
            store
                .replace_entity_defs(
                    repo_id,
                    "",
                    path,
                    "blobA",
                    crate::entities::zeitwerk::STATE_READ,
                    &[entity_claim(fqn, None)],
                )
                .unwrap();
        }
        // `Order` matches a real top-level constant EXACTLY, so the
        // fallback must not fire and drag in the two namespaced ones.
        let exact = store
            .entity_defs_for_name(repo_id, None, "Order", 10)
            .unwrap();
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].fqn, "Order");
        // A name that matches nothing exactly falls back to the last
        // segment and returns BOTH namespaced constants — the caller
        // reports them as ambiguous rather than picking one.
        store
            .replace_entity_defs(repo_id, "", "app/models/order.rb", "blobA", "read", &[])
            .unwrap();
        let fallback = store
            .entity_defs_for_name(repo_id, None, "Order", 10)
            .unwrap();
        assert_eq!(fallback.len(), 2);
    }

    #[test]
    fn a_like_wildcard_in_a_constant_name_is_escaped_not_matched() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .replace_entity_defs(
                repo_id,
                "",
                "app/models/a.rb",
                "blobA",
                "read",
                &[entity_claim("Api::OrderXv2", None)],
            )
            .unwrap();
        // `_` is a LIKE wildcard: unescaped, `%::Order_v2` would match
        // `Api::OrderXv2`.
        assert!(store
            .entity_defs_for_name(repo_id, None, "Order_v2", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn replace_entity_defs_is_scoped_to_one_worktree_and_one_path() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .replace_entity_defs(
                repo_id,
                "",
                "a.rb",
                "blobA",
                "read",
                &[entity_claim("A", None)],
            )
            .unwrap();
        store
            .replace_entity_defs(
                repo_id,
                "wt1",
                "a.rb",
                "blobB",
                "read",
                &[entity_claim("A", None)],
            )
            .unwrap();
        store
            .replace_entity_defs(
                repo_id,
                "",
                "b.rb",
                "blobC",
                "read",
                &[entity_claim("B", None)],
            )
            .unwrap();

        // Re-indexing the main worktree's `a.rb` must not disturb the
        // linked worktree's row for the same path — the whole point of
        // putting `worktree` in the key.
        store
            .replace_entity_defs(
                repo_id,
                "",
                "a.rb",
                "blobA2",
                "read",
                &[entity_claim("A", None)],
            )
            .unwrap();
        let all = store.entity_defs_for_name(repo_id, None, "A", 10).unwrap();
        assert_eq!(all.len(), 2, "both checkouts still have their row");
        let wt = store
            .entity_defs_for_name(repo_id, Some("wt1"), "A", 10)
            .unwrap();
        assert_eq!(wt.len(), 1);
        assert_eq!(wt[0].blob_hash, "blobB");
        assert_eq!(
            store
                .entity_defs_for_name(repo_id, None, "B", 10)
                .unwrap()
                .len(),
            1,
            "another path in the same worktree is untouched"
        );
    }

    #[test]
    fn delete_file_prunes_entity_defs_rows_for_the_same_path() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let path = "app/models/order.rb";
        store
            .upsert_file(repo_id, path, "blobA", "ruby", 10)
            .unwrap();
        store
            .replace_entity_defs(
                repo_id,
                "",
                path,
                "blobA",
                "read",
                &[entity_claim("Order", None)],
            )
            .unwrap();
        assert_eq!(
            store
                .entity_defs_for_name(repo_id, None, "Order", 10)
                .unwrap()
                .len(),
            1
        );

        store.delete_file(repo_id, path).unwrap();

        assert!(
            store
                .entity_defs_for_name(repo_id, None, "Order", 10)
                .unwrap()
                .is_empty(),
            "a deleted file must not keep answering ?ent= queries"
        );
    }

    #[test]
    fn delete_file_only_prunes_entity_defs_for_the_named_repo() {
        let (_tmp, store) = open_temp();
        let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
        let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
        let path = "app/models/order.rb";
        for repo_id in [repo_a, repo_b] {
            store
                .replace_entity_defs(
                    repo_id,
                    "",
                    path,
                    "blobA",
                    "read",
                    &[entity_claim("Order", None)],
                )
                .unwrap();
        }
        store.delete_file(repo_a, path).unwrap();
        assert!(store
            .entity_defs_for_name(repo_a, None, "Order", 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .entity_defs_for_name(repo_b, None, "Order", 10)
                .unwrap()
                .len(),
            1
        );
    }

    // --- V71-G0 — kbc-seq/1 ----------------------------------------------

    #[test]
    fn seq_reading_sets_lists_every_kind_and_filters_by_projection_and_workspace() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .create_reading_set("set_ws", repo_id, "ws", None, &[], 1_000)
            .unwrap();
        store
            .update_reading_set_meta(
                "set_ws",
                None,
                None,
                Some("workspace"),
                None,
                None,
                None,
                1_000,
            )
            .unwrap();
        store
            .create_reading_set(
                "set_tour",
                repo_id,
                "tour1",
                None,
                &[whole_file_span("a.rs")],
                1_000,
            )
            .unwrap();
        store
            .update_reading_set_meta(
                "set_tour",
                None,
                None,
                Some("tour"),
                None,
                None,
                None,
                1_000,
            )
            .unwrap();

        let all = store.seq_reading_sets(repo_id, None, None).unwrap();
        assert_eq!(all.len(), 2);
        let tours = store.seq_reading_sets(repo_id, Some("tour"), None).unwrap();
        assert_eq!(tours.len(), 1);
        assert_eq!(tours[0].projection, "tour");
        assert_eq!(tours[0].size, Some(1));
        assert_eq!(tours[0].source, "reading_sets");

        // Unbound: the workspace filter matches nothing yet.
        assert!(store
            .seq_reading_sets(repo_id, None, Some("set_ws"))
            .unwrap()
            .is_empty());
        assert!(store
            .set_reading_set_workspace("set_tour", Some("set_ws"), 2_000)
            .unwrap());
        let bound = store
            .seq_reading_sets(repo_id, None, Some("set_ws"))
            .unwrap();
        assert_eq!(bound.len(), 1);
        assert_eq!(bound[0].id, "set_tour");
        assert_eq!(bound[0].workspace_id.as_deref(), Some("set_ws"));

        // …and unbinding is expressible, which a COALESCE update could not
        // have said at all.
        store
            .set_reading_set_workspace("set_tour", None, 3_000)
            .unwrap();
        assert!(store
            .seq_reading_sets(repo_id, None, Some("set_ws"))
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .get_reading_set("set_tour")
                .unwrap()
                .unwrap()
                .workspace_id,
            None
        );
    }

    #[test]
    fn set_reading_set_workspace_reports_a_missing_id_as_false() {
        let (_tmp, store) = open_temp();
        assert!(!store
            .set_reading_set_workspace("set_nope", Some("set_ws"), 1)
            .unwrap());
    }

    #[test]
    fn like_escape_neutralises_every_sql_wildcard() {
        assert_eq!(like_escape("Order_v2"), "Order\\_v2");
        assert_eq!(like_escape("100%"), "100\\%");
        assert_eq!(like_escape("a\\b"), "a\\\\b");
        assert_eq!(like_escape("Plain"), "Plain");
    }

    #[test]
    fn delete_file_only_prunes_rails_edges_for_the_named_repo() {
        let (_tmp, store) = open_temp();
        let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
        let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
        let path = "app/controllers/x_controller.rb";
        let mut edge_a = sample_rails_edge("app/views/x/show.html.erb", 1);
        edge_a.src_path = path.to_string();
        let mut edge_b = sample_rails_edge("app/views/x/show.html.erb", 1);
        edge_b.src_path = path.to_string();
        store
            .replace_rails_edges(repo_a, path, "blobA", "rails-lens/1", &[edge_a])
            .unwrap();
        store
            .replace_rails_edges(repo_b, path, "blobB", "rails-lens/1", &[edge_b])
            .unwrap();

        store.delete_file(repo_a, path).unwrap();

        assert!(store
            .rails_edges_by_src_path(repo_a, path)
            .unwrap()
            .is_empty());
        assert_eq!(
            store.rails_edges_by_src_path(repo_b, path).unwrap().len(),
            1
        );
    }

    #[test]
    fn symbols_for_repo_joins_files_and_symbols_scoped_per_repo() {
        let (_tmp, store) = open_temp();
        let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
        let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();

        store
            .upsert_file(repo_a, "lib.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .upsert_file(repo_a, "copy.rs", "hashA", "rust", 10)
            .unwrap(); // same content, second path
        store
            .replace_symbols("hashA", "rust@1", &[sample_symbol(0, "alpha")])
            .unwrap();

        store
            .upsert_file(repo_b, "other.rs", "hashB", "rust", 5)
            .unwrap();
        store
            .replace_symbols("hashB", "rust@1", &[sample_symbol(0, "beta")])
            .unwrap();

        let rows = store.symbols_for_repo(repo_a).unwrap();
        let mut got: Vec<(String, String)> = rows
            .iter()
            .map(|(path, sym)| (path.clone(), sym.name.clone()))
            .collect();
        got.sort();
        // "alpha" appears twice: once per path pointing at the shared blob
        // (unlike symbol_count_for_repo, a listing needs a path per hit).
        assert_eq!(
            got,
            vec![
                ("copy.rs".to_string(), "alpha".to_string()),
                ("lib.rs".to_string(), "alpha".to_string()),
            ]
        );
        assert!(store
            .symbols_for_repo(repo_b)
            .unwrap()
            .iter()
            .all(|(_, s)| s.name == "beta"));
    }

    #[test]
    fn symbols_for_repo_hides_a_stale_salt_row_once_a_current_salt_sibling_exists() {
        // V70-A3X — the actual production bug: a blob re-derived under a
        // NEW salt (grammar/query bump) used to leave the OLD salt's rows
        // visible ALONGSIDE the new ones in this un-salted join, doubling
        // every symbol. `lang::RUST.salt` is the one genuinely "current"
        // salt `current_salt_cte` knows about; a fake old salt stands in
        // for a pre-bump derivation.
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "lib.rs", "hashA", "rust", 10)
            .unwrap();

        // A fresh derivation lands under the REAL current salt.
        store
            .replace_symbols(
                "hashA",
                crate::lang::RUST.salt,
                &[sample_symbol(0, "new_name")],
            )
            .unwrap();

        // Simulate the OLD grammar's rows STILL sitting in the table (raw
        // INSERT — bypassing `replace_symbols`'s own write-side purge,
        // as if written by a pre-fix binary and never swept) — this
        // isolates the READ-side filter: `symbols_for_repo` must hide it
        // even though nothing purged it at write time.
        store
            .lock()
            .execute(
                "INSERT INTO symbols (blob_hash, salt, ordinal, name, kind, line_start, \
                 line_end, col_start, col_end) VALUES (?1, 'rust@stale-fake', 0, 'old_name', \
                 'fn', 1, 1, 0, 1)",
                params!["hashA"],
            )
            .unwrap();

        let rows = store.symbols_for_repo(repo_id).unwrap();
        let names: Vec<&str> = rows.iter().map(|(_, s)| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["new_name"],
            "the stale-salt row must be hidden once a current-salt sibling exists: {names:?}"
        );
    }

    #[test]
    fn symbols_for_repo_still_shows_fixture_only_salts_with_no_current_sibling() {
        // V70-A3X fallback: a repo whose ONLY rows are under a non-current
        // (ad hoc test / not-yet-recognised) salt must still show them —
        // this is what keeps the REST of this crate's "rust@1"-style
        // fixtures byte-identical (see `current_salt_cte`'s doc).
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "lib.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .replace_symbols(
                "hashA",
                "rust@ad-hoc-fixture-salt",
                &[sample_symbol(0, "x")],
            )
            .unwrap();

        let rows = store.symbols_for_repo(repo_id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.name, "x");
    }

    #[test]
    fn replace_symbols_purges_only_this_blobs_stale_same_language_rows() {
        // A degenerate/empty file can share ONE blob_hash across DIFFERENT
        // languages (different extensions detecting to different
        // `LangInfo`s over identical bytes) — the purge must be scoped to
        // the SAME language as the incoming salt, never touching a
        // sibling language's CURRENT rows for that same blob_hash.
        let (_tmp, store) = open_temp();
        store
            .replace_symbols(
                "sharedBlob",
                crate::lang::PYTHON.salt,
                &[sample_symbol(0, "py_fn")],
            )
            .unwrap();
        // An OLD rust salt for the SAME blob (simulating a pre-bump rust
        // derivation that happens to share this content).
        store
            .replace_symbols(
                "sharedBlob",
                "rust@old-fake",
                &[sample_symbol(0, "old_rust_fn")],
            )
            .unwrap();

        // Re-derive rust under its CURRENT salt — must purge the old rust
        // row but leave python's untouched.
        store
            .replace_symbols(
                "sharedBlob",
                crate::lang::RUST.salt,
                &[sample_symbol(0, "new_rust_fn")],
            )
            .unwrap();

        let rust_rows = store
            .symbols_for_blob("sharedBlob", "rust@old-fake")
            .unwrap();
        assert!(rust_rows.is_empty(), "old rust salt must be purged");
        let new_rust = store
            .symbols_for_blob("sharedBlob", crate::lang::RUST.salt)
            .unwrap();
        assert_eq!(new_rust.len(), 1);
        assert_eq!(new_rust[0].name, "new_rust_fn");
        let py_rows = store
            .symbols_for_blob("sharedBlob", crate::lang::PYTHON.salt)
            .unwrap();
        assert_eq!(
            py_rows.len(),
            1,
            "a DIFFERENT language's rows for the same blob_hash must survive"
        );
        assert_eq!(py_rows[0].name, "py_fn");
    }

    #[test]
    fn sweep_stale_salt_derived_prunes_only_genuinely_superseded_rows() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .replace_symbols("hashA", crate::lang::RUST.salt, &[sample_symbol(0, "cur")])
            .unwrap();
        // "hashA" ALSO carries a genuinely stale row — direct INSERT
        // (bypassing `replace_symbols`'s own write-side purge), simulating
        // leftover data from BEFORE this fix shipped, which is exactly what
        // the boot-time sweep exists to catch (the write-side purge alone
        // can't clean up rows a pre-fix binary already wrote).
        store
            .lock()
            .execute(
                "INSERT INTO symbols (blob_hash, salt, ordinal, name, kind, line_start, \
                 line_end, col_start, col_end) VALUES ('hashA', 'rust@stale-fake', 0, 'old', \
                 'fn', 1, 1, 0, 1)",
                [],
            )
            .unwrap();

        // Untouched repo: a fixture-only blob with NO current-salt sibling
        // at all — the sweep must leave it alone entirely.
        let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
        store
            .upsert_file(repo_b, "b.rs", "hashB", "rust", 5)
            .unwrap();
        store
            .replace_symbols("hashB", "rust@fixture-only", &[sample_symbol(0, "fixture")])
            .unwrap();

        let counts = store.sweep_stale_salt_derived().unwrap();
        assert_eq!(counts.symbols, 1, "exactly the one genuinely-stale row");
        assert_eq!(counts.highlights, 0);
        assert_eq!(counts.occurrences, 0);

        assert!(store
            .symbols_for_blob("hashA", "rust@stale-fake")
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .symbols_for_blob("hashB", "rust@fixture-only")
                .unwrap()
                .len(),
            1,
            "a fixture-only blob with no current sibling must survive the sweep"
        );
    }

    /// V72-B0 — the property the boot fix rests on: one page touches ONLY
    /// its own slice of `files.blob_hash`, the cursor advances, the walk
    /// terminates, and the union over pages equals the un-paged result.
    /// A page that silently swept the whole table would put the hours-long
    /// transaction straight back onto the store's write mutex.
    #[test]
    fn sweep_stale_salt_page_is_bounded_resumable_and_totals_to_a_full_sweep() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        // Six blobs, each with one current-salt row and one genuinely stale
        // sibling — deliberately more than the page size used below.
        let hashes: Vec<String> = (0..6).map(|i| format!("hash{i}")).collect();
        for (i, h) in hashes.iter().enumerate() {
            store
                .upsert_file(repo_id, &format!("f{i}.rs"), h, "rust", 10)
                .unwrap();
            store
                .replace_symbols(h, crate::lang::RUST.salt, &[sample_symbol(0, "cur")])
                .unwrap();
            store
                .lock()
                .execute(
                    "INSERT INTO symbols (blob_hash, salt, ordinal, name, kind, line_start, \
                     line_end, col_start, col_end) VALUES (?1, 'rust@stale-fake', 0, 'old', \
                     'fn', 1, 1, 0, 1)",
                    params![h],
                )
                .unwrap();
        }

        // Page size 2 over 6 blobs: three full pages, then a short/empty one.
        let mut cursor: Option<String> = None;
        let mut swept = 0u64;
        let mut seen_cursors: Vec<String> = Vec::new();
        let mut pages = 0;
        loop {
            let (counts, next) = store.sweep_stale_salt_page(cursor.as_deref(), 2).unwrap();
            pages += 1;
            assert!(
                counts.symbols <= 2,
                "a page of 2 blobs can never delete more than 2 stale symbol rows, got {}",
                counts.symbols
            );
            swept += counts.symbols;
            match next {
                Some(c) => {
                    if let Some(prev) = seen_cursors.last() {
                        assert!(&c > prev, "the cursor must advance strictly: {prev} -> {c}");
                    }
                    seen_cursors.push(c.clone());
                    cursor = Some(c);
                }
                None => break,
            }
            assert!(pages < 20, "the paged sweep must terminate");
        }
        assert_eq!(swept, 6, "every blob's one stale row, exactly once");
        assert!(pages >= 3, "6 blobs at 2 per page must take >= 3 pages");

        for h in &hashes {
            assert!(
                store
                    .symbols_for_blob(h, "rust@stale-fake")
                    .unwrap()
                    .is_empty(),
                "{h}'s stale row must be gone"
            );
            assert_eq!(
                store
                    .symbols_for_blob(h, crate::lang::RUST.salt)
                    .unwrap()
                    .len(),
                1,
                "{h}'s current-salt row must survive"
            );
        }

        // Idempotent: a second full walk finds nothing left to do.
        assert_eq!(store.sweep_stale_salt_derived().unwrap().total(), 0);
    }

    fn sample_symbol(ordinal: u32, name: &str) -> Symbol {
        Symbol {
            ordinal,
            name: name.to_string(),
            kind: "fn".to_string(),
            line_start: 1,
            line_end: 3,
            col_start: 0,
            col_end: 1,
            container: None,
            signature: None,
            doc: None,
            param_min: None,
            param_max: None,
        }
    }

    #[test]
    fn symbols_cache_hit_and_replace_round_trip() {
        let (_tmp, store) = open_temp();
        assert!(!store.has_symbols("blobA", "rust@1").unwrap());

        let syms = vec![sample_symbol(0, "foo"), sample_symbol(1, "bar")];
        store.replace_symbols("blobA", "rust@1", &syms).unwrap();
        assert!(store.has_symbols("blobA", "rust@1").unwrap());

        let got = store.symbols_for_blob("blobA", "rust@1").unwrap();
        assert_eq!(got, syms);

        // Different salt is a separate cache slot entirely.
        assert!(!store.has_symbols("blobA", "rust@2").unwrap());

        // Replacing overwrites, not appends.
        let syms2 = vec![sample_symbol(0, "baz")];
        store.replace_symbols("blobA", "rust@1", &syms2).unwrap();
        assert_eq!(store.symbols_for_blob("blobA", "rust@1").unwrap(), syms2);
    }

    #[test]
    fn highlights_round_trip_json_blob() {
        let (_tmp, store) = open_temp();
        assert!(!store.has_highlights("blobA", "rust@1").unwrap());
        assert_eq!(store.highlights_for_blob("blobA", "rust@1").unwrap(), None);

        let spans = vec![
            Span {
                byte_start: 0,
                byte_len: 3,
                class: HighlightClass::Keyword,
            },
            Span {
                byte_start: 4,
                byte_len: 2,
                class: HighlightClass::Variable,
            },
        ];
        store.put_highlights("blobA", "rust@1", &spans).unwrap();
        assert!(store.has_highlights("blobA", "rust@1").unwrap());
        assert_eq!(
            store.highlights_for_blob("blobA", "rust@1").unwrap(),
            Some(spans)
        );
    }

    // --- occurrences (B2) ---------------------------------------------------

    fn occ(ordinal: u32, name: &str, role: &str, line: u32) -> crate::occurrences::Occurrence {
        crate::occurrences::Occurrence {
            ordinal,
            name: name.to_string(),
            role: role.to_string(),
            line,
            col_start: 0,
            col_end: 1,
            source: crate::occurrences::SOURCE_TS.to_string(),
            local_def_ordinal: None,
        }
    }

    #[test]
    fn occurrences_insert_has_and_lookup_round_trip() {
        let (_tmp, store) = open_temp();
        assert!(!store.has_occurrences("blobA", "rust@1").unwrap());
        assert_eq!(
            store.occurrences_for_blob("blobA", "rust@1").unwrap(),
            vec![]
        );

        let occs = vec![
            occ(0, "foo", "def", 1),
            occ(1, "foo", "ref", 2),
            occ(2, "bar", "def", 3),
        ];
        store.replace_occurrences("blobA", "rust@1", &occs).unwrap();
        assert!(store.has_occurrences("blobA", "rust@1").unwrap());
        assert_eq!(store.occurrences_for_blob("blobA", "rust@1").unwrap(), occs);

        // Different salt is a separate cache slot, same convention as symbols.
        assert!(!store.has_occurrences("blobA", "rust@2").unwrap());

        // Replacing overwrites, not appends.
        let occs2 = vec![occ(0, "baz", "ref", 1)];
        store
            .replace_occurrences("blobA", "rust@1", &occs2)
            .unwrap();
        assert_eq!(
            store.occurrences_for_blob("blobA", "rust@1").unwrap(),
            occs2
        );
    }

    #[test]
    fn def_occurrences_by_name_filters_to_the_def_role_only() {
        let (_tmp, store) = open_temp();
        store
            .replace_occurrences(
                "blobA",
                "rust@1",
                &[
                    occ(0, "run", "def", 1),
                    occ(1, "run", "ref", 5),
                    occ(2, "run", "import", 8),
                ],
            )
            .unwrap();
        let defs = store
            .def_occurrences_by_name("blobA", "rust@1", "run")
            .unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].role, "def");
        assert_eq!(defs[0].line, 1);

        assert!(store
            .def_occurrences_by_name("blobA", "rust@1", "zzz-nope")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn occurrence_at_finds_the_covering_span_only() {
        let (_tmp, store) = open_temp();
        store
            .replace_occurrences(
                "blobA",
                "rust@1",
                &[crate::occurrences::Occurrence {
                    ordinal: 0,
                    name: "widget".to_string(),
                    role: "def".to_string(),
                    line: 4,
                    col_start: 3,
                    col_end: 9,
                    source: crate::occurrences::SOURCE_TS.to_string(),
                    local_def_ordinal: None,
                }],
            )
            .unwrap();

        // Inside the span.
        let hit = store.occurrence_at("blobA", "rust@1", 4, 5).unwrap();
        assert_eq!(hit.map(|o| o.name), Some("widget".to_string()));

        // Exactly at col_start (inclusive).
        assert!(store
            .occurrence_at("blobA", "rust@1", 4, 3)
            .unwrap()
            .is_some());
        // Exactly at col_end (exclusive) — must miss.
        assert!(store
            .occurrence_at("blobA", "rust@1", 4, 9)
            .unwrap()
            .is_none());
        // Wrong line — must miss.
        assert!(store
            .occurrence_at("blobA", "rust@1", 5, 5)
            .unwrap()
            .is_none());
    }

    // --- occurrences (scip-sourced, S1) -------------------------------------

    #[test]
    fn replace_scip_occurrences_is_isolated_from_the_ts_source_and_continues_the_ordinal_space() {
        let (_tmp, store) = open_temp();
        store
            .replace_occurrences(
                "blobA",
                "rust@1",
                &[occ(0, "widget", "def", 1), occ(1, "widget", "ref", 2)],
            )
            .unwrap();

        store
            .replace_scip_occurrences(
                "blobA",
                "rust@1",
                &[crate::store::ScipOccurrenceIn {
                    name: "widget".to_string(),
                    role: "def".to_string(),
                    line: 1,
                    col_start: 3,
                    col_end: 9,
                }],
            )
            .unwrap();

        let all = store.occurrences_for_blob("blobA", "rust@1").unwrap();
        assert_eq!(all.len(), 3, "got: {all:#?}");
        // The scip row's ordinal continues AFTER the two ts ordinals (0, 1)
        // — no primary-key collision.
        let scip_row = all.iter().find(|o| o.source == "scip").expect("a scip row");
        assert_eq!(scip_row.ordinal, 2);

        // A re-derive of the TS pass (`replace_occurrences`) must NOT touch
        // the scip row.
        store
            .replace_occurrences("blobA", "rust@1", &[occ(0, "widget", "def", 1)])
            .unwrap();
        let after = store.occurrences_for_blob("blobA", "rust@1").unwrap();
        assert_eq!(after.len(), 2, "got: {after:#?}"); // 1 ts + 1 scip
        assert!(after
            .iter()
            .any(|o| o.source == "scip" && o.name == "widget"));

        // Re-ingesting scip occurrences REPLACES the prior scip set, still
        // never touching the ts row.
        store
            .replace_scip_occurrences(
                "blobA",
                "rust@1",
                &[crate::store::ScipOccurrenceIn {
                    name: "renamed".to_string(),
                    role: "def".to_string(),
                    line: 1,
                    col_start: 3,
                    col_end: 10,
                }],
            )
            .unwrap();
        let final_rows = store.occurrences_for_blob("blobA", "rust@1").unwrap();
        assert_eq!(final_rows.len(), 2, "got: {final_rows:#?}");
        assert!(final_rows
            .iter()
            .any(|o| o.source == "scip" && o.name == "renamed"));
        assert!(!final_rows
            .iter()
            .any(|o| o.name == "widget" && o.source == "scip"));
    }

    #[test]
    fn def_occurrences_by_name_is_scoped_to_ts_scip_def_occurrences_by_name_to_scip() {
        let (_tmp, store) = open_temp();
        store
            .replace_occurrences("blobA", "rust@1", &[occ(0, "widget", "def", 1)])
            .unwrap();
        store
            .replace_scip_occurrences(
                "blobA",
                "rust@1",
                &[crate::store::ScipOccurrenceIn {
                    name: "widget".to_string(),
                    role: "def".to_string(),
                    line: 5,
                    col_start: 0,
                    col_end: 6,
                }],
            )
            .unwrap();

        let ts_defs = store
            .def_occurrences_by_name("blobA", "rust@1", "widget")
            .unwrap();
        assert_eq!(ts_defs.len(), 1);
        assert_eq!(ts_defs[0].line, 1);

        let scip_defs = store
            .scip_def_occurrences_by_name("blobA", "rust@1", "widget")
            .unwrap();
        assert_eq!(scip_defs.len(), 1);
        assert_eq!(scip_defs[0].line, 5);
    }

    #[test]
    fn occurrence_at_prefers_the_scip_row_when_both_sources_cover_the_same_span() {
        let (_tmp, store) = open_temp();
        store
            .replace_occurrences(
                "blobA",
                "rust@1",
                &[crate::occurrences::Occurrence {
                    ordinal: 0,
                    name: "widget".to_string(),
                    role: "ref".to_string(),
                    line: 4,
                    col_start: 3,
                    col_end: 9,
                    source: crate::occurrences::SOURCE_TS.to_string(),
                    local_def_ordinal: None,
                }],
            )
            .unwrap();
        store
            .replace_scip_occurrences(
                "blobA",
                "rust@1",
                &[crate::store::ScipOccurrenceIn {
                    name: "widget".to_string(),
                    role: "def".to_string(),
                    line: 4,
                    col_start: 3,
                    col_end: 9,
                }],
            )
            .unwrap();

        let hit = store
            .occurrence_at("blobA", "rust@1", 4, 5)
            .unwrap()
            .expect("a hit");
        assert_eq!(hit.source, "scip", "the scip row must win the tie-break");
        assert_eq!(hit.role, "def");
    }

    /// Migration `V0010__occurrences_source.sql`'s own contract: `ALTER
    /// TABLE occurrences ADD COLUMN source TEXT NOT NULL DEFAULT 'ts'`
    /// backfills every PRE-EXISTING row (inserted before the column
    /// existed) to `'ts'` for free, no separate UPDATE. Exercised here by
    /// inserting a row with a raw SQL statement that OMITS the `source`
    /// column entirely (the exact shape SQLite's own `ALTER TABLE ADD
    /// COLUMN ... DEFAULT` backfill produces for a row that predates the
    /// column) — reading it back through the normal `Store` API must see
    /// `source == "ts"`.
    #[test]
    fn v0010_defaults_pre_existing_rows_without_a_source_column_to_ts() {
        let (_tmp, store) = open_temp();
        store
            .lock()
            .execute(
                "INSERT INTO occurrences (blob_hash, salt, ordinal, name, role, line, col_start, col_end)
                 VALUES ('blobA', 'rust@1', 0, 'legacy', 'def', 1, 0, 6)",
                [],
            )
            .unwrap();
        let rows = store.occurrences_for_blob("blobA", "rust@1").unwrap();
        assert_eq!(rows.len(), 1, "got: {rows:#?}");
        assert_eq!(rows[0].name, "legacy");
        assert_eq!(rows[0].source, "ts");
    }

    // --- W2.1: generation counter + list_files + file_opens ---------------

    #[test]
    fn generation_starts_at_zero_and_bumps_on_every_mutation() {
        let (_tmp, store) = open_temp();
        assert_eq!(store.generation(), 0);
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        // upsert_repo itself does not bump — only files/symbols mutations do
        // (the search-lane path/symbol caches only ever key on repo_id
        // contents, never the repos table itself).
        assert_eq!(store.generation(), 0);

        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();
        let g1 = store.generation();
        assert!(g1 > 0);

        store.delete_file(repo_id, "a.rs").unwrap();
        let g2 = store.generation();
        assert!(g2 > g1);

        // V70-A3X: a file OPEN is read-only w.r.t. the files/symbols
        // candidate SET, so it must NOT bump the main `generation` any
        // more — see `bump_file_open`'s doc. It bumps its OWN
        // `opens_generation` counter instead (the next test).
        store.bump_file_open(repo_id, "a.rs", 1_000).unwrap();
        assert_eq!(
            store.generation(),
            g2,
            "a file open must not invalidate the files/symbols path caches"
        );
    }

    #[test]
    fn bump_file_open_bumps_only_opens_generation_not_the_main_generation() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();
        let gen_before = store.generation();
        let opens_gen_before = store.opens_generation();

        store.bump_file_open(repo_id, "a.rs", 1_000).unwrap();

        assert_eq!(store.generation(), gen_before, "main generation untouched");
        assert!(
            store.opens_generation() > opens_gen_before,
            "opens_generation must advance on a file open"
        );

        // A second open advances it again (monotonic, not a one-shot flag).
        let opens_gen_mid = store.opens_generation();
        store.bump_file_open(repo_id, "a.rs", 2_000).unwrap();
        assert!(store.opens_generation() > opens_gen_mid);
        assert_eq!(store.generation(), gen_before);
    }

    #[test]
    fn list_files_returns_every_row_path_ordered() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "z.rs", "hashZ", "rust", 1)
            .unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 2)
            .unwrap();
        let rows = store.list_files(repo_id).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            vec!["a.rs", "z.rs"]
        );
    }

    #[test]
    fn bump_file_open_and_last_opened_map_track_the_latest_open() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store.bump_file_open(repo_id, "a.rs", 1_000).unwrap();
        store.bump_file_open(repo_id, "a.rs", 5_000).unwrap();
        store.bump_file_open(repo_id, "b.rs", 2_000).unwrap();

        let map = store.last_opened_map(repo_id).unwrap();
        assert_eq!(map.get("a.rs"), Some(&5_000));
        assert_eq!(map.get("b.rs"), Some(&2_000));
        assert_eq!(map.get("c.rs"), None);
    }

    #[test]
    fn recent_file_opens_orders_newest_first_across_repos_and_respects_limit() {
        let (_tmp, store) = open_temp();
        let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
        let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
        store.bump_file_open(repo_a, "old.rs", 1_000).unwrap();
        store.bump_file_open(repo_b, "newer.rs", 3_000).unwrap();
        store.bump_file_open(repo_a, "old.rs", 2_000).unwrap(); // re-open, newer ts
        store.bump_file_open(repo_a, "newest.rs", 9_000).unwrap();

        let rows = store
            .recent_file_opens(&[repo_a, repo_b], 10, None)
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (repo_a, "newest.rs".to_string(), 9_000),
                (repo_b, "newer.rs".to_string(), 3_000),
                (repo_a, "old.rs".to_string(), 2_000),
            ]
        );

        let limited = store.recent_file_opens(&[repo_a, repo_b], 1, None).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].1, "newest.rs");

        assert!(store.recent_file_opens(&[], 10, None).unwrap().is_empty());
    }

    #[test]
    fn recent_file_opens_path_filter_applies_in_sql_before_limit() {
        // V70-A3X: an unfiltered `limit=1` picks the single newest open
        // ("newest.rs") — a path filter that excludes it must fall through
        // to the next-newest MATCHING row, not just re-check the already
        // narrowed top-1 page (proving the filter runs in the SQL query,
        // not as a post-filter over an already-`LIMIT`-ed result).
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store.bump_file_open(repo_id, "keep/old.rs", 1_000).unwrap();
        store.bump_file_open(repo_id, "newest.rs", 9_000).unwrap();

        let unfiltered = store.recent_file_opens(&[repo_id], 1, None).unwrap();
        assert_eq!(unfiltered, vec![(repo_id, "newest.rs".to_string(), 9_000)]);

        let filtered = store
            .recent_file_opens(&[repo_id], 1, Some("keep/"))
            .unwrap();
        assert_eq!(filtered, vec![(repo_id, "keep/old.rs".to_string(), 1_000)]);

        // Case-insensitive substring, same grammar as `search::grammar`'s
        // `path:` filter.
        let cased = store
            .recent_file_opens(&[repo_id], 10, Some("KEEP/"))
            .unwrap();
        assert_eq!(cased, vec![(repo_id, "keep/old.rs".to_string(), 1_000)]);
    }

    #[test]
    fn ref_and_def_occurrences_by_name_in_repo_hide_stale_salt_duplicates() {
        // V70-A3X — same production bug as symbols, one layer over
        // (`usages.rs`'s "find usages" / Code Vision counts): a stale-salt
        // occurrence row must not double a genuine current-salt hit once
        // both exist for the same blob.
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .replace_occurrences(
                "hashA",
                crate::lang::RUST.salt,
                &[occ(0, "widget", "ref", 2)],
            )
            .unwrap();
        // A genuinely stale sibling row for the SAME blob (raw INSERT under
        // a DIFFERENT salt — no PK collision, since salt is part of the key
        // — bypassing `replace_occurrences`'s write-side purge entirely):
        // simulates data left behind from BEFORE this fix shipped, which
        // the READ-side fallback-aware filter must still hide.
        store
            .lock()
            .execute(
                "INSERT INTO occurrences (blob_hash, salt, ordinal, name, role, line, \
                 col_start, col_end, source) VALUES ('hashA', 'rust@stale-fake', 0, 'widget', \
                 'ref', 1, 0, 1, 'ts')",
                [],
            )
            .unwrap();

        let refs = store
            .ref_occurrences_by_name_in_repo(repo_id, "widget")
            .unwrap();
        assert_eq!(refs.len(), 1, "got {refs:?}");
        assert_eq!(refs[0].1.line, 2, "must be the CURRENT-salt row's data");

        let by_names = store
            .occurrences_by_names_in_repo(repo_id, &["widget".to_string()])
            .unwrap();
        assert_eq!(by_names.len(), 1, "got {by_names:?}");

        // Fallback: a repo whose ONLY occurrence rows are under a
        // non-current salt still shows them (no current sibling exists).
        let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
        store
            .upsert_file(repo_b, "b.rs", "hashB", "rust", 5)
            .unwrap();
        store
            .replace_occurrences("hashB", "rust@fixture-only", &[occ(0, "gadget", "def", 1)])
            .unwrap();
        let defs = store
            .def_occurrences_by_name_in_repo(repo_b, "gadget")
            .unwrap();
        assert_eq!(
            defs.len(),
            1,
            "fixture-only salt must still surface: {defs:?}"
        );
    }

    #[test]
    fn put_highlights_purges_only_this_blobs_stale_same_language_rows() {
        let (_tmp, store) = open_temp();
        let old_spans = vec![];
        store
            .put_highlights("sharedBlob", "rust@old-fake", &old_spans)
            .unwrap();
        store
            .put_highlights("sharedBlob", crate::lang::PYTHON.salt, &old_spans)
            .unwrap();

        store
            .put_highlights("sharedBlob", crate::lang::RUST.salt, &old_spans)
            .unwrap();

        assert!(store
            .highlights_for_blob("sharedBlob", "rust@old-fake")
            .unwrap()
            .is_none());
        assert!(store
            .highlights_for_blob("sharedBlob", crate::lang::RUST.salt)
            .unwrap()
            .is_some());
        assert!(
            store
                .highlights_for_blob("sharedBlob", crate::lang::PYTHON.salt)
                .unwrap()
                .is_some(),
            "a DIFFERENT language's highlights for the same blob_hash must survive"
        );
    }

    // --- W2.3: chunk_status (semantic lane bookkeeping) --------------------

    #[test]
    fn has_chunks_and_mark_chunked_round_trip() {
        let (_tmp, store) = open_temp();
        assert!(!store.has_chunks("blobA", "rust@1").unwrap());

        store.mark_chunked("blobA", "rust@1", 3).unwrap();
        assert!(store.has_chunks("blobA", "rust@1").unwrap());

        // Different salt is a separate cache slot, same as symbols/highlights.
        assert!(!store.has_chunks("blobA", "rust@2").unwrap());

        // Upsert: re-marking updates chunk_count in place, not a new row
        // (verified indirectly — has_chunks still true, no error on repeat).
        store.mark_chunked("blobA", "rust@1", 5).unwrap();
        assert!(store.has_chunks("blobA", "rust@1").unwrap());
    }

    #[test]
    fn clear_chunk_status_for_blob_removes_every_salt() {
        let (_tmp, store) = open_temp();
        store.mark_chunked("blobA", "rust@1", 2).unwrap();
        store.mark_chunked("blobA", "rust@2", 1).unwrap();
        store.mark_chunked("blobB", "rust@1", 4).unwrap();

        store.clear_chunk_status_for_blob("blobA").unwrap();
        assert!(!store.has_chunks("blobA", "rust@1").unwrap());
        assert!(!store.has_chunks("blobA", "rust@2").unwrap());
        // A different blob's status is untouched.
        assert!(store.has_chunks("blobB", "rust@1").unwrap());

        // Clearing an already-clear blob is a no-op, not an error.
        store.clear_chunk_status_for_blob("blobA").unwrap();
    }

    #[test]
    fn orphaned_chunk_blobs_finds_only_blobs_with_no_owning_file_anywhere() {
        let (_tmp, store) = open_temp();
        let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
        let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();

        store
            .upsert_file(repo_a, "a.rs", "hashLive", "rust", 10)
            .unwrap();
        store.mark_chunked("hashLive", "rust@1", 2).unwrap();
        // Orphaned: chunked once, but no files row (anywhere) references it.
        store.mark_chunked("hashGone", "rust@1", 1).unwrap();

        let orphans = store.orphaned_chunk_blobs().unwrap();
        assert_eq!(
            orphans,
            vec![("hashGone".to_string(), "rust@1".to_string())]
        );

        // A blob referenced from a DIFFERENT repo must not be reported —
        // blob_hash sharing is global (ADR-2), so the ref-count is too.
        store
            .upsert_file(repo_b, "b.rs", "hashLive", "rust", 10)
            .unwrap();
        store.delete_file(repo_a, "a.rs").unwrap();
        let orphans2 = store.orphaned_chunk_blobs().unwrap();
        assert_eq!(
            orphans2,
            vec![("hashGone".to_string(), "rust@1".to_string())],
            "hashLive still lives in repo b — must not be reported as orphaned"
        );

        // Once EVERY referencing file is gone, it becomes an orphan too.
        store.delete_file(repo_b, "b.rs").unwrap();
        let mut orphans3 = store.orphaned_chunk_blobs().unwrap();
        orphans3.sort();
        assert_eq!(
            orphans3,
            vec![
                ("hashGone".to_string(), "rust@1".to_string()),
                ("hashLive".to_string(), "rust@1".to_string()),
            ]
        );
    }

    // --- transcripts (W2.5) -------------------------------------------------

    fn sample_turn(
        uuid: &str,
        session_id: &str,
        kind: &'static str,
        ts: i64,
        text: &str,
    ) -> IndexedTurn {
        IndexedTurn {
            turn: crate::transcripts::parse::ParsedTurn {
                session_id: session_id.to_string(),
                uuid: uuid.to_string(),
                parent_uuid: None,
                ts,
                kind,
                tool_name: None,
                file_paths: Vec::new(),
                is_sidechain: false,
                text: text.to_string(),
            },
            byte_offset: 0,
            byte_len: text.len() as i64,
        }
    }

    #[test]
    fn transcript_file_state_round_trips_and_updates_in_place() {
        let (_tmp, store) = open_temp();
        assert!(store.get_transcript_file("proj/a.jsonl").unwrap().is_none());

        let id1 = store
            .upsert_transcript_file_state("proj", "proj/a.jsonl", 111, 0, 1_000)
            .unwrap();
        let row = store.get_transcript_file("proj/a.jsonl").unwrap().unwrap();
        assert_eq!(row.id, id1);
        assert_eq!(row.inode, 111);
        assert_eq!(row.byte_offset, 0);

        // Same src_file → same row, updated in place (not a new row).
        let id2 = store
            .upsert_transcript_file_state("proj", "proj/a.jsonl", 111, 500, 2_000)
            .unwrap();
        assert_eq!(id1, id2);
        let row2 = store.get_transcript_file("proj/a.jsonl").unwrap().unwrap();
        assert_eq!(row2.byte_offset, 500);
        assert_eq!(row2.mtime, 2_000);
    }

    #[test]
    fn insert_transcript_turns_makes_them_searchable_newest_first() {
        let (_tmp, store) = open_temp();
        let file_id = store
            .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
            .unwrap();
        let turns = vec![
            sample_turn("u1", "s1", "user", 1_000, "an unusual error string alpha"),
            sample_turn(
                "u2",
                "s1",
                "assistant",
                2_000,
                "an unusual error string beta",
            ),
        ];
        store.insert_transcript_turns(file_id, &turns).unwrap();

        let hits = store.search_transcripts("unusual", 10, None, None).unwrap();
        assert_eq!(hits.len(), 2);
        // Newest first: ts=2_000 (uuid u2) before ts=1_000 (uuid u1).
        assert_eq!(hits[0].uuid, "u2");
        assert_eq!(hits[1].uuid, "u1");
        assert_eq!(hits[0].project_dir, "proj");
        assert_eq!(hits[0].src_file, "proj/a.jsonl");
    }

    #[test]
    fn search_transcripts_session_and_kind_filters_narrow_results() {
        let (_tmp, store) = open_temp();
        let file_id = store
            .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
            .unwrap();
        let turns = vec![
            sample_turn("u1", "s1", "user", 1_000, "widget lookup"),
            sample_turn("u2", "s2", "user", 2_000, "widget lookup"),
            sample_turn("u3", "s1", "assistant", 3_000, "widget lookup"),
        ];
        store.insert_transcript_turns(file_id, &turns).unwrap();

        let by_session = store
            .search_transcripts("widget", 10, Some("s1"), None)
            .unwrap();
        assert_eq!(by_session.len(), 2);
        assert!(by_session.iter().all(|h| h.session_id == "s1"));

        let by_kind = store
            .search_transcripts("widget", 10, None, Some("assistant"))
            .unwrap();
        assert_eq!(by_kind.len(), 1);
        assert_eq!(by_kind[0].uuid, "u3");

        let both = store
            .search_transcripts("widget", 10, Some("s1"), Some("user"))
            .unwrap();
        assert_eq!(both.len(), 1);
        assert_eq!(both[0].uuid, "u1");
    }

    /// A `tool_use` turn variant of [`sample_turn`], carrying a `tool_name`
    /// and `file_paths` — `sessiondiff`'s own data source, which
    /// `search_transcripts`' tests above never need.
    fn sample_tool_use_turn(
        uuid: &str,
        session_id: &str,
        ts: i64,
        tool_name: &str,
        file_paths: &[&str],
    ) -> IndexedTurn {
        IndexedTurn {
            turn: crate::transcripts::parse::ParsedTurn {
                session_id: session_id.to_string(),
                uuid: uuid.to_string(),
                parent_uuid: None,
                ts,
                kind: "tool_use",
                tool_name: Some(tool_name.to_string()),
                file_paths: file_paths.iter().map(|s| s.to_string()).collect(),
                is_sidechain: false,
                text: format!("{tool_name} edit"),
            },
            byte_offset: 0,
            byte_len: 10,
        }
    }

    #[test]
    fn transcript_turns_for_session_returns_every_turn_oldest_first_with_file_paths() {
        let (_tmp, store) = open_temp();
        let file_id = store
            .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
            .unwrap();
        let turns = vec![
            sample_turn("u1", "s1", "user", 3_000, "third"),
            sample_tool_use_turn("u2", "s1", 1_000, "Edit", &["/repo/a.rs"]),
            sample_turn("u3", "s2", "user", 500, "other session"),
            sample_turn("u4", "s1", "assistant", 2_000, "second"),
        ];
        store.insert_transcript_turns(file_id, &turns).unwrap();

        let rows = store.transcript_turns_for_session("s1").unwrap();
        assert_eq!(rows.len(), 3, "only s1's turns, s2's u3 excluded");
        // Oldest first (ts ASC) — the session's own narrative order, the
        // OPPOSITE of search_transcripts' newest-first convention.
        assert_eq!(rows[0].uuid, "u2");
        assert_eq!(rows[0].tool_name.as_deref(), Some("Edit"));
        assert_eq!(rows[0].file_paths, vec!["/repo/a.rs".to_string()]);
        assert_eq!(rows[1].uuid, "u4");
        assert_eq!(rows[2].uuid, "u1");

        assert!(store
            .transcript_turns_for_session("unknown-session")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn delete_transcript_turns_for_file_removes_fts_rows_too() {
        let (_tmp, store) = open_temp();
        let file_id = store
            .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
            .unwrap();
        store
            .insert_transcript_turns(file_id, &[sample_turn("u1", "s1", "user", 1_000, "gizmo")])
            .unwrap();
        assert_eq!(
            store
                .search_transcripts("gizmo", 10, None, None)
                .unwrap()
                .len(),
            1
        );

        store.delete_transcript_turns_for_file(file_id).unwrap();
        assert!(store
            .search_transcripts("gizmo", 10, None, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn transcript_stats_counts_files_turns_and_bytes() {
        let (_tmp, store) = open_temp();
        assert_eq!(
            store.transcript_stats().unwrap(),
            TranscriptStats::default()
        );

        let file_a = store
            .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
            .unwrap();
        let file_b = store
            .upsert_transcript_file_state("proj", "proj/b.jsonl", 2, 0, 0)
            .unwrap();
        store
            .insert_transcript_turns(file_a, &[sample_turn("u1", "s1", "user", 1_000, "12345")])
            .unwrap();
        store
            .insert_transcript_turns(
                file_b,
                &[
                    sample_turn("u2", "s1", "user", 2_000, "1234567890"),
                    sample_turn("u3", "s1", "assistant", 3_000, "12"),
                ],
            )
            .unwrap();

        let stats = store.transcript_stats().unwrap();
        assert_eq!(stats.files, 2);
        assert_eq!(stats.turns, 3);
        assert_eq!(stats.indexed_bytes, 5 + 10 + 2);
    }

    fn sample_turn_with_paths(
        uuid: &str,
        session_id: &str,
        ts: i64,
        file_paths: Vec<String>,
    ) -> IndexedTurn {
        IndexedTurn {
            turn: crate::transcripts::parse::ParsedTurn {
                session_id: session_id.to_string(),
                uuid: uuid.to_string(),
                parent_uuid: None,
                ts,
                kind: crate::transcripts::parse::KIND_TOOL_USE,
                tool_name: Some("Edit".to_string()),
                file_paths,
                is_sidechain: false,
                text: "Edit ...".to_string(),
            },
            byte_offset: 0,
            byte_len: 8,
        }
    }

    #[test]
    fn transcript_sessions_touching_path_finds_exact_matches_newest_first() {
        let (_tmp, store) = open_temp();
        let file_id = store
            .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
            .unwrap();
        store
            .insert_transcript_turns(
                file_id,
                &[
                    sample_turn_with_paths(
                        "u1",
                        "s-older",
                        1_000,
                        vec!["/repo/src/lib.rs".to_string()],
                    ),
                    sample_turn_with_paths(
                        "u2",
                        "s-newer",
                        2_000,
                        vec![
                            "/repo/src/lib.rs".to_string(),
                            "/repo/README.md".to_string(),
                        ],
                    ),
                    // A DIFFERENT, longer path that merely ENDS in the same
                    // basename must never match — proves the quote-delimited
                    // needle isn't a bare substring scan.
                    sample_turn_with_paths(
                        "u3",
                        "s-unrelated",
                        3_000,
                        vec!["/repo/src/other_lib.rs".to_string()],
                    ),
                ],
            )
            .unwrap();

        let hits = store
            .transcript_sessions_touching_path("/repo/src/lib.rs", 10)
            .unwrap();
        let sessions: Vec<&str> = hits.iter().map(|h| h.session_id.as_str()).collect();
        assert_eq!(
            sessions,
            vec!["s-newer", "s-older"],
            "newest-first, and the unrelated longer path must not match: {hits:?}"
        );
    }

    #[test]
    fn transcript_sessions_touching_path_is_bounded_by_limit() {
        let (_tmp, store) = open_temp();
        let file_id = store
            .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
            .unwrap();
        let turns: Vec<IndexedTurn> = (0..5)
            .map(|i| {
                sample_turn_with_paths(
                    &format!("u{i}"),
                    &format!("s{i}"),
                    1_000 + i,
                    vec!["/repo/hot_file.rs".to_string()],
                )
            })
            .collect();
        store.insert_transcript_turns(file_id, &turns).unwrap();

        let hits = store
            .transcript_sessions_touching_path("/repo/hot_file.rs", 2)
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].session_id, "s4", "newest first");
    }

    #[test]
    fn transcript_sessions_touching_path_no_match_is_an_honest_empty_vec() {
        let (_tmp, store) = open_temp();
        let file_id = store
            .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
            .unwrap();
        store
            .insert_transcript_turns(
                file_id,
                &[sample_turn_with_paths(
                    "u1",
                    "s1",
                    1_000,
                    vec!["/repo/other.rs".to_string()],
                )],
            )
            .unwrap();

        let hits = store
            .transcript_sessions_touching_path("/repo/nothing_here.rs", 10)
            .unwrap();
        assert!(hits.is_empty());
    }

    // --- commit_sessions (W3.2 join ladder cache) --------------------------

    fn sample_commit_session(confidence: &str, via: &str) -> CommitSessionRow {
        CommitSessionRow {
            confidence: confidence.to_string(),
            via: via.to_string(),
            session_id: Some("sess-1".to_string()),
            kb: Some("memory".to_string()),
            display_name: Some("fixed the gizmo race".to_string()),
            started_at: Some(1_700_000_000),
            resolved_at: 1_700_000_100,
        }
    }

    #[test]
    fn commit_session_round_trips_and_misses_cleanly() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        assert_eq!(store.get_commit_session(repo_id, "deadbeef").unwrap(), None);

        let row = sample_commit_session("exact", "by-commit");
        store
            .upsert_commit_session(repo_id, "deadbeef", &row)
            .unwrap();
        let got = store.get_commit_session(repo_id, "deadbeef").unwrap();
        assert_eq!(got, Some(row));

        // A different sha in the same repo is a separate cache slot.
        assert_eq!(store.get_commit_session(repo_id, "other").unwrap(), None);
    }

    #[test]
    fn commit_session_upsert_replaces_every_column_in_place() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_commit_session(repo_id, "sha1", &sample_commit_session("none", "no-match"))
            .unwrap();
        assert_eq!(
            store
                .get_commit_session(repo_id, "sha1")
                .unwrap()
                .unwrap()
                .confidence,
            "none"
        );

        // A later re-resolution upgrades none -> fuzzy, replacing the row
        // wholesale (not merging fields).
        let upgraded = CommitSessionRow {
            confidence: "fuzzy".to_string(),
            via: "time-window".to_string(),
            session_id: Some("sess-2".to_string()),
            kb: Some("memory".to_string()),
            display_name: None,
            started_at: Some(1_700_050_000),
            resolved_at: 1_700_060_000,
        };
        store
            .upsert_commit_session(repo_id, "sha1", &upgraded)
            .unwrap();
        assert_eq!(
            store.get_commit_session(repo_id, "sha1").unwrap(),
            Some(upgraded)
        );
    }

    #[test]
    fn commit_session_is_scoped_per_repo() {
        let (_tmp, store) = open_temp();
        let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
        let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
        store
            .upsert_commit_session(repo_a, "sha1", &sample_commit_session("exact", "by-commit"))
            .unwrap();
        assert!(store.get_commit_session(repo_a, "sha1").unwrap().is_some());
        assert_eq!(store.get_commit_session(repo_b, "sha1").unwrap(), None);
    }

    #[test]
    fn commit_session_none_row_carries_no_enrichment() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let none_row = CommitSessionRow {
            confidence: "none".to_string(),
            via: "kb-unreachable".to_string(),
            session_id: None,
            kb: None,
            display_name: None,
            started_at: None,
            resolved_at: 1_700_000_000,
        };
        store
            .upsert_commit_session(repo_id, "sha1", &none_row)
            .unwrap();
        let got = store.get_commit_session(repo_id, "sha1").unwrap().unwrap();
        assert_eq!(got.session_id, None);
        assert_eq!(got.kb, None);
        assert_eq!(got.display_name, None);
        assert_eq!(got.started_at, None);
    }

    // --- annotations (W4.6) -------------------------------------------------

    fn sample_annotation(id: &str, repo_id: i64, path: &str) -> AnnotationRow {
        AnnotationRow {
            id: id.to_string(),
            repo_id,
            path: path.to_string(),
            anchor: Some(
                r#"{"kind":"selection","css_path":"","offset":1,"snippet":"fn a() {}"}"#
                    .to_string(),
            ),
            anchor_kind: "line".to_string(),
            anchor2: None,
            parent_id: None,
            intent: "note".to_string(),
            body: "why is this here?".to_string(),
            author: "you".to_string(),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
            resolved: false,
            review_id: None,
            ps_number: None,
            side: None,
            set_id: None,
        }
    }

    #[test]
    fn annotation_insert_list_get_round_trip() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let row = sample_annotation("ann_1", repo_id, "src/lib.rs");
        store.insert_annotation(&row).unwrap();

        assert_eq!(store.get_annotation("ann_1").unwrap(), Some(row.clone()));
        assert_eq!(store.get_annotation("nope").unwrap(), None);

        let listed = store.list_annotations(repo_id, "src/lib.rs").unwrap();
        assert_eq!(listed, vec![row]);
        assert!(store
            .list_annotations(repo_id, "src/other.rs")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn annotation_list_is_scoped_per_path_and_ordered_by_creation() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let mut first = sample_annotation("ann_1", repo_id, "a.rs");
        first.created_at = 100;
        let mut second = sample_annotation("ann_2", repo_id, "a.rs");
        second.created_at = 200;
        let other_path = sample_annotation("ann_3", repo_id, "b.rs");
        // Insert out of chronological order to prove the ORDER BY, not
        // insertion order, decides.
        store.insert_annotation(&second).unwrap();
        store.insert_annotation(&first).unwrap();
        store.insert_annotation(&other_path).unwrap();

        let listed = store.list_annotations(repo_id, "a.rs").unwrap();
        assert_eq!(
            listed.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec!["ann_1", "ann_2"]
        );
    }

    #[test]
    fn annotation_update_patches_only_given_fields_and_reports_existence() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let row = sample_annotation("ann_1", repo_id, "a.rs");
        store.insert_annotation(&row).unwrap();

        // Body only.
        assert!(store
            .update_annotation("ann_1", Some("edited body"), None, None, 1_700_000_100)
            .unwrap());
        let got = store.get_annotation("ann_1").unwrap().unwrap();
        assert_eq!(got.body, "edited body");
        assert!(!got.resolved);
        assert_eq!(got.intent, "note");
        assert_eq!(got.updated_at, 1_700_000_100);

        // Resolved only — body from the previous update must survive.
        assert!(store
            .update_annotation("ann_1", None, Some(true), None, 1_700_000_200)
            .unwrap());
        let got = store.get_annotation("ann_1").unwrap().unwrap();
        assert_eq!(got.body, "edited body");
        assert!(got.resolved);
        assert_eq!(got.updated_at, 1_700_000_200);

        // Intent only — body/resolved from previous updates must survive.
        assert!(store
            .update_annotation("ann_1", None, None, Some("todo"), 1_700_000_250)
            .unwrap());
        let got = store.get_annotation("ann_1").unwrap().unwrap();
        assert_eq!(got.body, "edited body");
        assert!(got.resolved);
        assert_eq!(got.intent, "todo");
        assert_eq!(got.updated_at, 1_700_000_250);

        // Missing id.
        assert!(!store
            .update_annotation("nope", Some("x"), None, None, 1_700_000_300)
            .unwrap());
    }

    #[test]
    fn annotation_update_never_touches_anchor_columns() {
        // The v1 rule ("PATCH never changes anchors"), preserved through
        // D-server — `update_annotation`'s SQL simply has no `anchor*`
        // column in its SET list at all, but pin it with a real round trip
        // anyway (a future careless edit to that SQL would show up here).
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let row = sample_annotation("ann_1", repo_id, "a.rs");
        store.insert_annotation(&row).unwrap();

        store
            .update_annotation(
                "ann_1",
                Some("edited"),
                Some(true),
                Some("flag-for-agent"),
                1_700_000_100,
            )
            .unwrap();
        let got = store.get_annotation("ann_1").unwrap().unwrap();
        assert_eq!(got.anchor, row.anchor);
        assert_eq!(got.anchor_kind, row.anchor_kind);
        assert_eq!(got.anchor2, row.anchor2);
        assert_eq!(got.parent_id, row.parent_id);
    }

    #[test]
    fn annotation_delete_removes_the_row_and_reports_existence() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let row = sample_annotation("ann_1", repo_id, "a.rs");
        store.insert_annotation(&row).unwrap();

        assert!(store.delete_annotation("ann_1").unwrap());
        assert_eq!(store.get_annotation("ann_1").unwrap(), None);
        // Already gone — reports false, not an error.
        assert!(!store.delete_annotation("ann_1").unwrap());
    }

    // --- D-server: anchor kinds / threads / intents -------------------------

    fn sample_reply(id: &str, parent: &AnnotationRow, body: &str) -> AnnotationRow {
        AnnotationRow {
            id: id.to_string(),
            repo_id: parent.repo_id,
            path: parent.path.clone(),
            anchor: None,
            anchor_kind: "line".to_string(),
            anchor2: None,
            parent_id: Some(parent.id.clone()),
            intent: "note".to_string(),
            body: body.to_string(),
            author: "you".to_string(),
            created_at: parent.created_at + 1,
            updated_at: parent.created_at + 1,
            resolved: false,
            review_id: parent.review_id,
            ps_number: parent.ps_number,
            side: parent.side.clone(),
            set_id: parent.set_id.clone(),
        }
    }

    #[test]
    fn annotation_delete_of_a_parent_cascades_to_its_replies() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let parent = sample_annotation("ann_parent", repo_id, "a.rs");
        store.insert_annotation(&parent).unwrap();
        let reply1 = sample_reply("ann_reply1", &parent, "first reply");
        let reply2 = sample_reply("ann_reply2", &parent, "second reply");
        store.insert_annotation(&reply1).unwrap();
        store.insert_annotation(&reply2).unwrap();
        assert_eq!(store.list_annotations(repo_id, "a.rs").unwrap().len(), 3);

        assert!(store.delete_annotation("ann_parent").unwrap());

        assert_eq!(store.get_annotation("ann_parent").unwrap(), None);
        assert_eq!(store.get_annotation("ann_reply1").unwrap(), None);
        assert_eq!(store.get_annotation("ann_reply2").unwrap(), None);
        assert!(store.list_annotations(repo_id, "a.rs").unwrap().is_empty());
    }

    #[test]
    fn annotation_delete_of_a_reply_only_removes_that_one_row() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let parent = sample_annotation("ann_parent", repo_id, "a.rs");
        store.insert_annotation(&parent).unwrap();
        let reply = sample_reply("ann_reply", &parent, "a reply");
        store.insert_annotation(&reply).unwrap();

        assert!(store.delete_annotation("ann_reply").unwrap());
        assert!(store.get_annotation("ann_parent").unwrap().is_some());
        assert_eq!(store.get_annotation("ann_reply").unwrap(), None);
    }

    #[test]
    fn list_open_annotations_excludes_resolved_and_replies() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();

        let mut open_one = sample_annotation("ann_open", repo_id, "a.rs");
        open_one.created_at = 100;
        let mut resolved_one = sample_annotation("ann_resolved", repo_id, "a.rs");
        resolved_one.created_at = 200;
        resolved_one.resolved = true;
        store.insert_annotation(&open_one).unwrap();
        store.insert_annotation(&resolved_one).unwrap();
        let reply = sample_reply("ann_reply", &open_one, "a reply");
        store.insert_annotation(&reply).unwrap();

        let open = store
            .list_open_annotations(repo_id, None, None, 500)
            .unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].0.id, "ann_open");
        assert_eq!(open[0].1, 1, "one direct reply");
    }

    #[test]
    fn list_open_annotations_filters_by_intent_and_path_prefix() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();

        let mut a = sample_annotation("ann_a", repo_id, "src/lib.rs");
        a.intent = "todo".to_string();
        let mut b = sample_annotation("ann_b", repo_id, "src/main.rs");
        b.intent = "question".to_string();
        let mut c = sample_annotation("ann_c", repo_id, "docs/readme.md");
        c.intent = "todo".to_string();
        store.insert_annotation(&a).unwrap();
        store.insert_annotation(&b).unwrap();
        store.insert_annotation(&c).unwrap();

        let todos = store
            .list_open_annotations(repo_id, Some("todo"), None, 500)
            .unwrap();
        assert_eq!(
            todos.iter().map(|(r, _)| r.id.as_str()).collect::<Vec<_>>(),
            vec!["ann_c", "ann_a"],
            "newest first"
        );

        let under_src = store
            .list_open_annotations(repo_id, None, Some("src/"), 500)
            .unwrap();
        assert_eq!(
            under_src
                .iter()
                .map(|(r, _)| r.id.as_str())
                .collect::<std::collections::HashSet<_>>(),
            std::collections::HashSet::from(["ann_a", "ann_b"])
        );

        let both = store
            .list_open_annotations(repo_id, Some("todo"), Some("src/"), 500)
            .unwrap();
        assert_eq!(both.len(), 1);
        assert_eq!(both[0].0.id, "ann_a");
    }

    #[test]
    fn list_open_annotations_respects_the_limit_for_truncation_detection() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        for i in 0..5 {
            let mut row = sample_annotation(&format!("ann_{i}"), repo_id, "a.rs");
            row.created_at = 1_000 + i;
            store.insert_annotation(&row).unwrap();
        }
        // Ask for 3 (a caller-configured cap of 2 plus one, per this
        // method's own doc) — exactly 3 must come back so the route can
        // tell `rows.len() > 2` and report `truncated: true`.
        let rows = store.list_open_annotations(repo_id, None, None, 3).unwrap();
        assert_eq!(rows.len(), 3);
    }

    // --- S2-A: unified-inbox annotations lane --------------------------

    #[test]
    fn list_open_working_tree_annotations_filters_review_scoped_and_intent() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();

        // A working-tree "question" — must be included.
        let mut question = sample_annotation("ann_q", repo_id, "a.rs");
        question.intent = "question".to_string();
        question.created_at = 100;
        question.updated_at = 100;
        // A working-tree "flag-for-agent" — must be included.
        let mut flag = sample_annotation("ann_flag", repo_id, "b.rs");
        flag.intent = "flag-for-agent".to_string();
        flag.created_at = 200;
        flag.updated_at = 200;
        // A working-tree "note" — wrong intent, must be excluded.
        let mut note = sample_annotation("ann_note", repo_id, "c.rs");
        note.intent = "note".to_string();
        note.created_at = 300;
        note.updated_at = 300;
        // A REVIEW-scoped "question" — must be excluded (review_inbox's
        // own lane already counts it; this lane must never double it).
        let mut review_scoped = sample_annotation("ann_review", repo_id, "d.rs");
        review_scoped.intent = "question".to_string();
        review_scoped.review_id = Some(1);
        review_scoped.created_at = 400;
        review_scoped.updated_at = 400;
        // A RESOLVED working-tree "question" — must be excluded.
        let mut resolved = sample_annotation("ann_resolved", repo_id, "e.rs");
        resolved.intent = "question".to_string();
        resolved.resolved = true;
        resolved.created_at = 500;
        resolved.updated_at = 500;
        for row in [&question, &flag, &note, &review_scoped, &resolved] {
            store.insert_annotation(row).unwrap();
        }
        // A reply on `question` — must never appear as its own row, but
        // must count toward `question`'s reply_count.
        let reply = sample_reply("ann_q_reply", &question, "a reply");
        store.insert_annotation(&reply).unwrap();

        let rows = store
            .list_open_working_tree_annotations(repo_id, &["question", "flag-for-agent"], 500)
            .unwrap();
        assert_eq!(
            rows.iter().map(|(r, _)| r.id.as_str()).collect::<Vec<_>>(),
            vec!["ann_flag", "ann_q"],
            "newest updated_at first; note/review-scoped/resolved excluded"
        );
        let (q_row, q_replies) = rows.iter().find(|(r, _)| r.id == "ann_q").unwrap();
        assert_eq!(q_row.intent, "question");
        assert_eq!(*q_replies, 1, "one direct reply counted");
    }

    #[test]
    fn list_open_working_tree_annotations_empty_intents_short_circuits() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let row = sample_annotation("ann_1", repo_id, "a.rs");
        store.insert_annotation(&row).unwrap();

        let rows = store
            .list_open_working_tree_annotations(repo_id, &[], 500)
            .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn list_open_working_tree_annotations_respects_the_limit_for_truncation_detection() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        for i in 0..5 {
            let mut row = sample_annotation(&format!("ann_{i}"), repo_id, "a.rs");
            row.intent = "question".to_string();
            row.created_at = 1_000 + i;
            row.updated_at = 1_000 + i;
            store.insert_annotation(&row).unwrap();
        }
        let rows = store
            .list_open_working_tree_annotations(repo_id, &["question"], 3)
            .unwrap();
        assert_eq!(rows.len(), 3);
    }

    /// The migration's byte-for-byte pin: a row shaped exactly like a
    /// pre-D-server (V0006/W4.6) INSERT — omitting every new column — reads
    /// back with `anchor_kind: "line"`, `intent: "note"`,
    /// `anchor2`/`parent_id` both `None` (the rebuilt table's own column
    /// DEFAULTs), and resolves identically to how it did before this
    /// migration existed. Inserting directly (rather than faking a
    /// partial-migration refinery history) is this crate's own established
    /// precedent — see `pre_migration_shaped_symbol_rows_read_back_with_doc_none`
    /// above for the identical technique applied to V0007.
    #[test]
    fn legacy_v1_shaped_annotation_rows_read_back_as_line_note_and_resolve_identically() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let anchor = crate::annotations::anchor_for_line(2, "fn b() {}");
        let anchor_json = serde_json::to_string(&anchor).unwrap();
        store
            .lock()
            .execute(
                "INSERT INTO annotations
                    (id, repo_id, path, anchor, body, author, created_at, updated_at, resolved)
                 VALUES ('ann_legacy', ?1, 'src/lib.rs', ?2, 'a v1 comment', 'you', 1000, 1000, 0)",
                params![repo_id, anchor_json],
            )
            .unwrap();

        let row = store.get_annotation("ann_legacy").unwrap().unwrap();
        assert_eq!(row.anchor_kind, "line");
        assert_eq!(row.intent, "note");
        assert_eq!(row.anchor2, None);
        assert_eq!(row.parent_id, None);
        assert_eq!(row.review_id, None);
        assert_eq!(row.ps_number, None);
        assert_eq!(row.side, None);
        assert_eq!(row.anchor.as_deref(), Some(anchor_json.as_str()));

        // Byte-for-byte resolution pin: `crate::annotations::resolve` is
        // completely unchanged by D-server for a `line`-kind anchor.
        let content = "fn a() {}\nfn b() {}\nfn c() {}";
        let resolved = crate::annotations::resolve(content, &anchor);
        assert_eq!(resolved.line, 2);
        assert!(!resolved.stale);

        // Still findable via the ordinary per-path list, alongside a
        // freshly-created D-server row in the SAME file.
        let listed = store.list_annotations(repo_id, "src/lib.rs").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "ann_legacy");
    }

    /// V0023 on a fresh db: refinery runs the whole chain, so the new
    /// annotation columns and `annotation_suggestions` are reachable.
    #[test]
    fn v0023_migration_applies_cleanly_on_a_fresh_db() {
        let (_tmp, store) = open_temp();
        let names: Vec<String> = store
            .lock()
            .prepare("PRAGMA table_info(annotations)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        for col in ["review_id", "ps_number", "side"] {
            assert!(
                names.iter().any(|n| n == col),
                "annotations missing {col}: {names:?}"
            );
        }
        let review_cols: Vec<String> = store
            .lock()
            .prepare("PRAGMA table_info(reviews)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        for col in ["verdict", "verdict_note", "verdict_at", "verdict_ps"] {
            assert!(
                review_cols.iter().any(|n| n == col),
                "reviews missing {col}: {review_cols:?}"
            );
        }
        let n: i64 = store
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'annotation_suggestions'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    fn insert_suggestion(store: &Store, annotation_id: &str) {
        store
            .lock()
            .execute(
                "INSERT INTO annotation_suggestions
                    (annotation_id, replacement, original, base_blob_sha,
                     applied, created_at, updated_at)
                 VALUES (?1, 'new', 'old', 'deadbeef', 0, 1, 1)",
                params![annotation_id],
            )
            .unwrap();
    }

    #[test]
    fn delete_review_cascades_annotations_and_suggestions_in_one_tx() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let review_id = store
            .create_review("r", Some("feat"), "main", "feature", None, 1_000)
            .unwrap();
        store
            .insert_patchset(review_id, 1, "aaa", "bbb", 1_000)
            .unwrap();
        let mut parent = sample_annotation("ann_rev", repo_id, "a.rs");
        parent.review_id = Some(review_id);
        parent.ps_number = Some(1);
        parent.side = Some("new".into());
        store.insert_annotation(&parent).unwrap();
        let reply = sample_reply("ann_rev_reply", &parent, "ok");
        store.insert_annotation(&reply).unwrap();
        insert_suggestion(&store, "ann_rev");
        insert_suggestion(&store, "ann_rev_reply");

        // An unrelated plain annotation must survive.
        store
            .insert_annotation(&sample_annotation("ann_plain", repo_id, "b.rs"))
            .unwrap();

        assert!(store.delete_review(review_id).unwrap());
        assert!(store.get_review(review_id).unwrap().is_none());
        assert!(store.list_patchsets(review_id).unwrap().is_empty());
        assert!(store.get_annotation("ann_rev").unwrap().is_none());
        assert!(store.get_annotation("ann_rev_reply").unwrap().is_none());
        assert!(store
            .get_annotation_suggestion("ann_rev")
            .unwrap()
            .is_none());
        assert!(store
            .get_annotation_suggestion("ann_rev_reply")
            .unwrap()
            .is_none());
        assert!(store.get_annotation("ann_plain").unwrap().is_some());
    }

    #[test]
    fn delete_annotation_cascades_reply_suggestions() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let parent = sample_annotation("ann_parent", repo_id, "a.rs");
        store.insert_annotation(&parent).unwrap();
        let reply = sample_reply("ann_reply", &parent, "a reply");
        store.insert_annotation(&reply).unwrap();
        insert_suggestion(&store, "ann_parent");
        insert_suggestion(&store, "ann_reply");

        assert!(store.delete_annotation("ann_parent").unwrap());
        assert!(store.get_annotation("ann_parent").unwrap().is_none());
        assert!(store.get_annotation("ann_reply").unwrap().is_none());
        assert!(store
            .get_annotation_suggestion("ann_parent")
            .unwrap()
            .is_none());
        assert!(store
            .get_annotation_suggestion("ann_reply")
            .unwrap()
            .is_none());
    }

    #[test]
    fn set_review_verdict_is_a_noop_when_state_and_note_match() {
        let (_tmp, store) = open_temp();
        let id = store
            .create_review("r", None, "main", "feature", None, 1_000)
            .unwrap();
        assert_eq!(
            store
                .set_review_verdict(id, "approve", None, 2_000, 1)
                .unwrap(),
            Some(true)
        );
        let first = store.get_review(id).unwrap().unwrap();
        assert_eq!(first.verdict.as_deref(), Some("approve"));
        assert_eq!(first.verdict_at, Some(2_000));
        assert_eq!(first.verdict_ps, Some(1));

        assert_eq!(
            store
                .set_review_verdict(id, "approve", None, 3_000, 2)
                .unwrap(),
            Some(false)
        );
        let again = store.get_review(id).unwrap().unwrap();
        assert_eq!(again.verdict_at, Some(2_000), "no-op must not restamp at");
        assert_eq!(again.verdict_ps, Some(1), "no-op must not restamp ps");

        assert_eq!(
            store
                .set_review_verdict(id, "approve", Some("lgtm"), 3_000, 1)
                .unwrap(),
            Some(true)
        );
        assert_eq!(store.clear_review_verdict(id).unwrap(), Some(true));
        assert_eq!(store.clear_review_verdict(id).unwrap(), Some(false));
        assert_eq!(store.clear_review_verdict(99).unwrap(), None);
        assert_eq!(
            store.set_review_verdict(99, "comment", None, 1, 1).unwrap(),
            None
        );
    }

    #[test]
    fn upsert_annotation_suggestion_replaces_and_resets_applied() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .insert_annotation(&sample_annotation("ann_s", repo_id, "a.rs"))
            .unwrap();
        store
            .upsert_annotation_suggestion("ann_s", "new", "old", "deadbeef", 10)
            .unwrap();
        assert!(store
            .mark_annotation_suggestion_applied("ann_s", 20, "headsha")
            .unwrap());
        let marked = store.get_annotation_suggestion("ann_s").unwrap().unwrap();
        assert!(marked.applied);
        assert_eq!(marked.applied_at, Some(20));
        assert_eq!(marked.created_at, 10);

        store
            .upsert_annotation_suggestion("ann_s", "newer", "old", "cafe", 30)
            .unwrap();
        let reset = store.get_annotation_suggestion("ann_s").unwrap().unwrap();
        assert!(!reset.applied);
        assert_eq!(reset.applied_at, None);
        assert_eq!(reset.applied_head_sha, None);
        assert_eq!(reset.replacement, "newer");
        assert_eq!(reset.created_at, 10, "created_at survives re-PUT");
        assert_eq!(reset.updated_at, 30);
        assert!(store.delete_annotation_suggestion("ann_s").unwrap());
        assert!(!store.delete_annotation_suggestion("ann_s").unwrap());
    }

    #[test]
    fn apply_annotation_ops_is_atomic_and_reports_noops() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let parent = sample_annotation("ann_p", repo_id, "a.rs");
        store.insert_annotation(&parent).unwrap();

        let report = store
            .apply_annotation_ops(
                &[PreparedAnnotationOp::SetResolved {
                    id: "ann_p".into(),
                    resolved: true,
                }],
                11,
            )
            .unwrap();
        assert!(report.changed);
        assert_eq!(report.applied, 1);

        let noop = store
            .apply_annotation_ops(
                &[PreparedAnnotationOp::SetResolved {
                    id: "ann_p".into(),
                    resolved: true,
                }],
                12,
            )
            .unwrap();
        assert!(!noop.changed);
        assert_eq!(noop.applied, 1);

        let before = store.list_annotations(repo_id, "a.rs").unwrap().len();
        let err = store
            .apply_annotation_ops(
                &[
                    PreparedAnnotationOp::Insert {
                        row: Box::new(sample_annotation("ann_new", repo_id, "a.rs")),
                        suggestion: None,
                    },
                    PreparedAnnotationOp::Delete {
                        id: "does-not-exist".into(),
                    },
                ],
                13,
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::NotFound(_)));
        assert_eq!(
            store.list_annotations(repo_id, "a.rs").unwrap().len(),
            before,
            "failing op must roll back the insert"
        );
        assert!(store.get_annotation("ann_new").unwrap().is_none());
    }

    #[test]
    fn list_review_annotations_orders_and_filters_resolved_threads() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let review_id = store
            .create_review("r", None, "main", "feature", None, 1_000)
            .unwrap();
        let mut open = sample_annotation("ann_open", repo_id, "a.rs");
        open.review_id = Some(review_id);
        open.ps_number = Some(1);
        open.side = Some("new".into());
        open.created_at = 100;
        let mut resolved = sample_annotation("ann_done", repo_id, "a.rs");
        resolved.review_id = Some(review_id);
        resolved.ps_number = Some(1);
        resolved.side = Some("new".into());
        resolved.resolved = true;
        resolved.created_at = 200;
        store.insert_annotation(&open).unwrap();
        store.insert_annotation(&resolved).unwrap();
        let reply = sample_reply("ann_open_reply", &open, "ack");
        store.insert_annotation(&reply).unwrap();

        let open_only = store.list_review_annotations(review_id, false).unwrap();
        assert_eq!(
            open_only.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["ann_open", "ann_open_reply"]
        );

        let all = store.list_review_annotations(review_id, true).unwrap();
        assert_eq!(
            all.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["ann_open", "ann_open_reply", "ann_done"]
        );
    }

    /// Regression (V72-C2): the batch SELECT once omitted `set_id` — a
    /// 16- vs. 17-column mismatch against `annotation_row_from`'s
    /// positional `r.get(0..16)` reads (the module doc's own "every SELECT
    /// spells out the SAME 17-column order" convention, broken by this one
    /// query). That doesn't just drop the field: `r.get(16)` on a
    /// 16-column row is a hard `rusqlite::Error::InvalidColumnIndex`, so
    /// ANY non-empty result set errored out `review_inbox::compose_rows`
    /// (and therefore both `GET /api/reviews/inbox` and `GET /api/inbox`)
    /// end to end — caught only at the HTTP layer
    /// (`review_inbox_timeline`/`unified_inbox` e2e tests), never at this
    /// store layer, since no prior unit test called this fn with a
    /// non-empty result. Pins both branches (`include_resolved` true/false)
    /// against the SAME fixture `list_review_annotations_orders_and_
    /// filters_resolved_threads` uses, plus a `set_id` round trip the
    /// singular query already covered.
    #[test]
    fn list_review_annotations_batch_matches_the_singular_query_incl_set_id() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let review_id = store
            .create_review("r", None, "main", "feature", None, 1_000)
            .unwrap();
        let mut open = sample_annotation("ann_open", repo_id, "a.rs");
        open.review_id = Some(review_id);
        open.ps_number = Some(1);
        open.side = Some("new".into());
        open.set_id = Some("set_abc123456789".into());
        open.created_at = 100;
        let mut resolved = sample_annotation("ann_done", repo_id, "a.rs");
        resolved.review_id = Some(review_id);
        resolved.ps_number = Some(1);
        resolved.side = Some("new".into());
        resolved.resolved = true;
        resolved.created_at = 200;
        store.insert_annotation(&open).unwrap();
        store.insert_annotation(&resolved).unwrap();
        let reply = sample_reply("ann_open_reply", &open, "ack");
        store.insert_annotation(&reply).unwrap();

        let open_only = store
            .list_review_annotations_batch(&[review_id], false)
            .unwrap();
        assert_eq!(
            open_only
                .get(&review_id)
                .unwrap()
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ann_open", "ann_open_reply"]
        );

        let all = store
            .list_review_annotations_batch(&[review_id], true)
            .unwrap();
        let all_rows = all.get(&review_id).unwrap();
        assert_eq!(
            all_rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["ann_open", "ann_open_reply", "ann_done"]
        );
        let open_row = all_rows.iter().find(|r| r.id == "ann_open").unwrap();
        assert_eq!(open_row.set_id.as_deref(), Some("set_abc123456789"));
    }

    // --- reading sets (Phase E3) --------------------------------------------

    fn whole_file_span(path: &str) -> NewReadingSetSpan {
        NewReadingSetSpan {
            path: path.to_string(),
            ..Default::default()
        }
    }

    fn ranged_span(path: &str, start: i64, end: i64, note: &str) -> NewReadingSetSpan {
        NewReadingSetSpan {
            path: path.to_string(),
            line_start: Some(start),
            line_end: Some(end),
            git_ref: Some("deadbeef".to_string()),
            note: Some(note.to_string()),
        }
    }

    #[test]
    fn create_list_get_reading_set_round_trip() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let spans = vec![
            whole_file_span("src/lib.rs"),
            ranged_span("src/main.rs", 10, 20, "the entrypoint"),
        ];
        store
            .create_reading_set(
                "set_1",
                repo_id,
                "the ingest path",
                Some("how a request flows in"),
                &spans,
                1_000,
            )
            .unwrap();

        let listed = store.list_reading_sets(repo_id, None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0.name, "the ingest path");
        assert_eq!(listed[0].1, 2, "span_count");
        assert_eq!(
            listed[0].2, 0,
            "note_count — no annotations scoped to this set"
        );

        let row = store.get_reading_set("set_1").unwrap().unwrap();
        assert_eq!(row.name, "the ingest path");
        assert_eq!(row.description.as_deref(), Some("how a request flows in"));
        assert_eq!(row.created_at, 1_000);
        assert_eq!(row.updated_at, 1_000);
        // V70-A10 — a plain `create_reading_set` row defaults to kind "set"
        // with every workspace-only column `None`.
        assert_eq!(row.kind, "set");
        assert_eq!(row.desk_json, None);
        assert_eq!(row.ref_label, None);
        assert_eq!(row.description_md, None);

        let got_spans = store.reading_set_spans("set_1").unwrap();
        assert_eq!(got_spans.len(), 2);
        assert_eq!(got_spans[0].ordinal, 0);
        assert_eq!(got_spans[0].path, "src/lib.rs");
        assert_eq!(got_spans[0].line_start, None);
        assert_eq!(got_spans[1].ordinal, 1);
        assert_eq!(got_spans[1].path, "src/main.rs");
        assert_eq!(got_spans[1].line_start, Some(10));
        assert_eq!(got_spans[1].line_end, Some(20));
        assert_eq!(got_spans[1].git_ref.as_deref(), Some("deadbeef"));
        assert_eq!(got_spans[1].note.as_deref(), Some("the entrypoint"));

        assert!(store.get_reading_set("set_nope").unwrap().is_none());

        // DCB-W3.C — a set created via the plain `create_reading_set`
        // wrapper carries all four provenance columns as `None` (the
        // wrapper passes four `None`s through to
        // `create_reading_set_with_provenance`) — proves the delegation in
        // §3.2 didn't change this existing call site's behavior.
        assert_eq!(row.source_kb, None);
        assert_eq!(row.source_doc_id, None);
        assert_eq!(row.source_doc_path, None);
        assert_eq!(row.source_doc_hash, None);
    }

    /// DCB-W3.C — the four `source_*` columns round-trip through
    /// `create_reading_set_with_provenance` → `get_reading_set` →
    /// `list_reading_sets`, and a `None` stays `None` (never coerced to an
    /// empty string).
    #[test]
    fn create_reading_set_with_provenance_round_trips_the_four_columns() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .create_reading_set_with_provenance(
                "set_from_doc",
                repo_id,
                "materialized",
                None,
                &[whole_file_span("src/lib.rs")],
                1_000,
                Some("platform"),
                Some("9f8b7182d433"),
                Some("docs/checkout.html"),
                Some("deadbeef"),
                "set",
                None,
                None,
                None,
            )
            .unwrap();

        let row = store.get_reading_set("set_from_doc").unwrap().unwrap();
        assert_eq!(row.source_kb.as_deref(), Some("platform"));
        assert_eq!(row.source_doc_id.as_deref(), Some("9f8b7182d433"));
        assert_eq!(row.source_doc_path.as_deref(), Some("docs/checkout.html"));
        assert_eq!(row.source_doc_hash.as_deref(), Some("deadbeef"));

        let listed = store.list_reading_sets(repo_id, None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0.source_kb.as_deref(), Some("platform"));
        assert_eq!(listed[0].0.source_doc_hash.as_deref(), Some("deadbeef"));

        // A sibling set with no provenance at all (`create_reading_set`,
        // same repo) stays `None` — the two rows don't cross-contaminate.
        store
            .create_reading_set("set_plain", repo_id, "plain", None, &[], 1_000)
            .unwrap();
        let plain = store.get_reading_set("set_plain").unwrap().unwrap();
        assert_eq!(plain.source_kb, None);
    }

    #[test]
    fn create_reading_set_rejects_a_duplicate_name_in_the_same_repo() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .create_reading_set("set_1", repo_id, "dup", None, &[], 1_000)
            .unwrap();
        let err = store
            .create_reading_set("set_2", repo_id, "dup", None, &[], 1_000)
            .unwrap_err();
        assert!(matches!(err, StoreError::NameConflict(n) if n == "dup"));

        // A different repo may reuse the same name freely — the UNIQUE
        // constraint is scoped to (repo_id, name), not name alone.
        let other_repo = store.upsert_repo("r2", "/tmp/r2").unwrap();
        store
            .create_reading_set("set_3", other_repo, "dup", None, &[], 1_000)
            .unwrap();
    }

    #[test]
    fn update_reading_set_meta_coalesces_and_rejects_a_colliding_rename() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .create_reading_set("set_1", repo_id, "one", Some("d1"), &[], 1_000)
            .unwrap();
        store
            .create_reading_set("set_2", repo_id, "two", None, &[], 1_000)
            .unwrap();

        // Description-only update leaves the name untouched.
        let existed = store
            .update_reading_set_meta(
                "set_1",
                None,
                Some("new desc"),
                None,
                None,
                None,
                None,
                2_000,
            )
            .unwrap();
        assert!(existed);
        let row = store.get_reading_set("set_1").unwrap().unwrap();
        assert_eq!(row.name, "one");
        assert_eq!(row.description.as_deref(), Some("new desc"));
        assert_eq!(row.updated_at, 2_000);

        // Renaming to an unknown id is a no-op `false`, not an error.
        assert!(!store
            .update_reading_set_meta("set_nope", Some("x"), None, None, None, None, None, 2_000)
            .unwrap());

        // Renaming "two" to "one" collides with set_1 in the same repo.
        let err = store
            .update_reading_set_meta("set_2", Some("one"), None, None, None, None, None, 2_000)
            .unwrap_err();
        assert!(matches!(err, StoreError::NameConflict(n) if n == "one"));

        // V70-A10 — kind/desk_json/ref/description_md are COALESCE-updated
        // exactly like name/description, and independently of them.
        let existed = store
            .update_reading_set_meta(
                "set_1",
                None,
                None,
                Some("workspace"),
                Some("{\"v\":1}"),
                Some("feature/x"),
                Some("# why"),
                3_000,
            )
            .unwrap();
        assert!(existed);
        let row = store.get_reading_set("set_1").unwrap().unwrap();
        assert_eq!(row.kind, "workspace");
        assert_eq!(row.desk_json.as_deref(), Some("{\"v\":1}"));
        assert_eq!(row.ref_label.as_deref(), Some("feature/x"));
        assert_eq!(row.description_md.as_deref(), Some("# why"));
        // The pre-existing fields untouched by this second update.
        assert_eq!(row.description.as_deref(), Some("new desc"));
    }

    #[test]
    fn replace_reading_set_spans_rewrites_ordinals_contiguously_in_one_tx() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .create_reading_set(
                "set_1",
                repo_id,
                "s",
                None,
                &[whole_file_span("a.rs"), whole_file_span("b.rs")],
                1_000,
            )
            .unwrap();

        let replaced = store
            .replace_reading_set_spans(
                "set_1",
                &[whole_file_span("c.rs"), ranged_span("d.rs", 1, 2, "n")],
                2_000,
            )
            .unwrap();
        assert!(replaced);

        let spans = store.reading_set_spans("set_1").unwrap();
        assert_eq!(spans.len(), 2, "old spans fully replaced, not appended to");
        assert_eq!(
            spans.iter().map(|s| s.ordinal).collect::<Vec<_>>(),
            vec![0, 1],
            "ordinals rewritten contiguously from 0"
        );
        assert_eq!(spans[0].path, "c.rs");
        assert_eq!(spans[1].path, "d.rs");
        assert_eq!(
            store.get_reading_set("set_1").unwrap().unwrap().updated_at,
            2_000
        );

        // Unknown set id: false, no partial write.
        assert!(!store
            .replace_reading_set_spans("set_nope", &[whole_file_span("x.rs")], 3_000)
            .unwrap());
    }

    #[test]
    fn append_reading_set_span_assigns_the_next_ordinal() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .create_reading_set("set_1", repo_id, "s", None, &[], 1_000)
            .unwrap();

        // First append into an EMPTY set gets ordinal 0.
        let ord = store
            .append_reading_set_span("set_1", &whole_file_span("a.rs"), 2_000)
            .unwrap();
        assert_eq!(ord, Some(0));

        let ord = store
            .append_reading_set_span("set_1", &ranged_span("b.rs", 5, 9, "n"), 3_000)
            .unwrap();
        assert_eq!(ord, Some(1));

        let spans = store.reading_set_spans("set_1").unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].path, "a.rs");
        assert_eq!(spans[1].path, "b.rs");
        assert_eq!(
            store.get_reading_set("set_1").unwrap().unwrap().updated_at,
            3_000
        );

        assert_eq!(
            store
                .append_reading_set_span("set_nope", &whole_file_span("x.rs"), 4_000)
                .unwrap(),
            None
        );
    }

    #[test]
    fn delete_reading_set_cascades_to_spans_in_one_tx() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .create_reading_set(
                "set_1",
                repo_id,
                "s",
                None,
                &[whole_file_span("a.rs")],
                1_000,
            )
            .unwrap();

        assert!(store.delete_reading_set("set_1").unwrap());
        assert!(store.get_reading_set("set_1").unwrap().is_none());
        assert!(store.reading_set_spans("set_1").unwrap().is_empty());

        // Deleting an already-gone id is a clean `false`, not an error.
        assert!(!store.delete_reading_set("set_1").unwrap());
    }

    /// V70-A10 — a workspace ('kind: "workspace"') round-trips its
    /// `desk_json`/`ref`/`description_md` sidecar, `list_reading_sets`'s
    /// `kind` filter defaults to `'set'` (so a plain `None` filter never
    /// sees a workspace row), an explicit `Some("workspace")` finds it, and
    /// `note_count` reflects `annotations.set_id` scoped to it.
    #[test]
    fn workspace_kind_sidecar_and_note_count_round_trip() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .create_reading_set_with_provenance(
                "set_ws1",
                repo_id,
                "feature x",
                None,
                &[whole_file_span("a.rs")],
                1_000,
                None,
                None,
                None,
                None,
                "workspace",
                Some("{\"v\":1,\"preset\":\"read\"}"),
                Some("feature/x"),
                Some("# why this exists"),
            )
            .unwrap();
        // A plain 'set' sibling, same repo.
        store
            .create_reading_set("set_plain", repo_id, "plain", None, &[], 1_000)
            .unwrap();

        let row = store.get_reading_set("set_ws1").unwrap().unwrap();
        assert_eq!(row.kind, "workspace");
        assert_eq!(
            row.desk_json.as_deref(),
            Some("{\"v\":1,\"preset\":\"read\"}")
        );
        assert_eq!(row.ref_label.as_deref(), Some("feature/x"));
        assert_eq!(row.description_md.as_deref(), Some("# why this exists"));

        // Default (no `kind` filter) sees only the plain 'set' row — the
        // pre-A10 listing's behavior is byte-identical.
        let default_listed = store.list_reading_sets(repo_id, None).unwrap();
        assert_eq!(default_listed.len(), 1);
        assert_eq!(default_listed[0].0.id, "set_plain");

        // Explicit `kind = "workspace"` finds ONLY the workspace.
        let ws_listed = store.list_reading_sets(repo_id, Some("workspace")).unwrap();
        assert_eq!(ws_listed.len(), 1);
        assert_eq!(ws_listed[0].0.id, "set_ws1");
        assert_eq!(ws_listed[0].2, 0, "note_count starts at zero");

        // Two annotations scoped to the workspace (a general note + a
        // code-anchored one) bump note_count; a plain annotation on the
        // SAME repo/path with no `set_id` does not.
        let mut general = sample_annotation("ann_general", repo_id, "");
        general.anchor = Some(String::new());
        general.anchor_kind = "set".to_string();
        general.set_id = Some("set_ws1".to_string());
        store.insert_annotation(&general).unwrap();

        let mut anchored = sample_annotation("ann_anchored", repo_id, "a.rs");
        anchored.set_id = Some("set_ws1".to_string());
        store.insert_annotation(&anchored).unwrap();

        let unrelated = sample_annotation("ann_unrelated", repo_id, "a.rs");
        store.insert_annotation(&unrelated).unwrap();

        let ws_listed = store.list_reading_sets(repo_id, Some("workspace")).unwrap();
        assert_eq!(ws_listed[0].2, 2, "note_count counts both workspace notes");

        let by_set = store.list_annotations_by_set("set_ws1").unwrap();
        assert_eq!(by_set.len(), 2);
        assert!(by_set
            .iter()
            .all(|a| a.set_id.as_deref() == Some("set_ws1")));
        assert!(by_set.iter().any(|a| a.id == "ann_general"));
        assert!(by_set.iter().any(|a| a.id == "ann_anchored"));

        // Deleting the workspace cascades to its notes (and NOT the
        // unrelated plain annotation on the same path).
        assert!(store.delete_reading_set("set_ws1").unwrap());
        assert!(store.list_annotations_by_set("set_ws1").unwrap().is_empty());
        assert!(store.get_annotation("ann_general").unwrap().is_none());
        assert!(store.get_annotation("ann_anchored").unwrap().is_none());
        assert!(store.get_annotation("ann_unrelated").unwrap().is_some());
    }

    // --- doc-lens (DCB W1.C) ---------------------------------------------

    fn pin(kb: &str, doc: &str, repo: &str) -> DocLensPin {
        DocLensPin {
            kb: kb.to_string(),
            doc_id: doc.to_string(),
            repo: repo.to_string(),
            repo_root: format!("/tmp/{repo}"),
            doc_hash: Some("h1".to_string()),
            pinned_at: 1_754_500_000,
        }
    }

    #[test]
    fn symbols_named_many_returns_only_the_requested_names_in_order() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "b/two.rb", "hashB", "ruby", 10)
            .unwrap();
        store
            .upsert_file(repo_id, "a/one.rb", "hashA", "ruby", 10)
            .unwrap();
        store
            .replace_symbols(
                "hashA",
                "ruby@1",
                &[sample_symbol(0, "wanted"), sample_symbol(1, "ignored")],
            )
            .unwrap();
        store
            .replace_symbols("hashB", "ruby@1", &[sample_symbol(0, "wanted")])
            .unwrap();

        let got = store
            .symbols_named_many(repo_id, &["wanted".to_string(), "absent".to_string()])
            .unwrap();
        assert_eq!(got.len(), 2, "only the requested names");
        assert!(got.iter().all(|(_, s)| s.name == "wanted"));
        // Ordered `f.path, s.line_start, s.ordinal` — the same determinism
        // contract `symbols_named_in_repo` carries.
        assert_eq!(got[0].0, "a/one.rb");
        assert_eq!(got[1].0, "b/two.rb");

        // A repo that has none of them answers empty, not everything.
        let other = store.upsert_repo("r2", "/tmp/r2").unwrap();
        assert!(store
            .symbols_named_many(other, &["wanted".to_string()])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn symbols_named_many_with_an_empty_name_list_touches_no_sql() {
        let (_tmp, store) = open_temp();
        // A repo_id that does not exist: an implementation that still built
        // and ran a `WHERE name IN ()` statement would error rather than
        // return the documented empty Vec.
        assert!(store.symbols_named_many(-1, &[]).unwrap().is_empty());
    }

    #[test]
    fn doc_lens_pin_round_trip_upsert_get_list_delete() {
        let (_tmp, store) = open_temp();
        assert!(store.get_doc_lens_pin("platform", "d1").unwrap().is_none());

        store
            .put_doc_lens_pin(&pin("platform", "d1", "alpha"))
            .unwrap();
        let got = store.get_doc_lens_pin("platform", "d1").unwrap().unwrap();
        assert_eq!(got.repo, "alpha");
        assert_eq!(got.repo_root, "/tmp/alpha");
        assert_eq!(got.doc_hash.as_deref(), Some("h1"));

        // Last write wins on the (kb, doc_id) PK.
        store
            .put_doc_lens_pin(&pin("platform", "d1", "beta"))
            .unwrap();
        assert_eq!(
            store
                .get_doc_lens_pin("platform", "d1")
                .unwrap()
                .unwrap()
                .repo,
            "beta"
        );

        store
            .put_doc_lens_pin(&pin("platform", "d0", "alpha"))
            .unwrap();
        store
            .put_doc_lens_pin(&pin("research", "d9", "alpha"))
            .unwrap();
        let all = store.list_doc_lens_pins(None).unwrap();
        assert_eq!(
            all.iter()
                .map(|p| (p.kb.as_str(), p.doc_id.as_str()))
                .collect::<Vec<_>>(),
            vec![("platform", "d0"), ("platform", "d1"), ("research", "d9")]
        );
        assert_eq!(store.list_doc_lens_pins(Some("research")).unwrap().len(), 1);

        assert!(store.delete_doc_lens_pin("platform", "d1").unwrap());
        // Idempotent: the second delete removed nothing, but is not an error.
        assert!(!store.delete_doc_lens_pin("platform", "d1").unwrap());
    }

    #[test]
    fn rekey_doc_lens_pin_moves_a_pin_to_a_new_doc_id() {
        let (_tmp, store) = open_temp();
        store
            .put_doc_lens_pin(&pin("platform", "old", "alpha"))
            .unwrap();
        assert!(store.rekey_doc_lens_pin("platform", "old", "new").unwrap());
        assert!(store.get_doc_lens_pin("platform", "old").unwrap().is_none());
        assert_eq!(
            store
                .get_doc_lens_pin("platform", "new")
                .unwrap()
                .unwrap()
                .repo,
            "alpha"
        );
        // No pin to move ⇒ a clean `false`, never an error.
        assert!(!store
            .rekey_doc_lens_pin("platform", "nope", "new2")
            .unwrap());

        // A pre-existing pin on the DESTINATION id loses to the row actually
        // being re-keyed rather than aborting the migration on the PK.
        store
            .put_doc_lens_pin(&pin("platform", "src", "beta"))
            .unwrap();
        assert!(store.rekey_doc_lens_pin("platform", "src", "new").unwrap());
        assert_eq!(
            store
                .get_doc_lens_pin("platform", "new")
                .unwrap()
                .unwrap()
                .repo,
            "beta"
        );
    }

    #[test]
    fn pin_writes_do_not_bump_the_store_generation() {
        // `doc_lens_pins` is invisible to `FileIndex`/`SymbolIndex`'s
        // generation-keyed caches; bumping would throw away every repo's
        // cached path/symbol snapshot on a pin click.
        let (_tmp, store) = open_temp();
        let before = store.generation();
        store
            .put_doc_lens_pin(&pin("platform", "d1", "alpha"))
            .unwrap();
        store.rekey_doc_lens_pin("platform", "d1", "d2").unwrap();
        store.delete_doc_lens_pin("platform", "d2").unwrap();
        assert_eq!(store.generation(), before);

        // Control: a files write DOES bump, so this test can't pass vacuously.
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store.upsert_file(repo_id, "a.rb", "h", "ruby", 1).unwrap();
        assert!(store.generation() > before);
    }

    // ── PRR-R1: PR binding + findings tests ────────────────────────────

    /// V0024's byte-for-byte pin (mirrors `legacy_v1_shaped_annotation_
    /// rows_read_back_as_line_note_and_resolve_identically` above): a
    /// review created via the EXISTING `create_review` — which never
    /// mentions any V0024 column — reads back with every new PR-binding /
    /// report / verdict-publish field `None`, exactly as it did before this
    /// migration existed.
    #[test]
    fn legacy_review_row_reads_back_with_pr_binding_report_and_verdict_publish_columns_null() {
        let (_tmp, store) = open_temp();
        let id = store
            .create_review("r", Some("feat"), "main", "feature", None, 1_000)
            .unwrap();

        let binding = store.get_review_pr_binding(id).unwrap().unwrap();
        assert_eq!(binding, ReviewPrBinding::default());

        let report = store.get_review_report(id).unwrap().unwrap();
        assert_eq!(report, ReviewReport::default());

        let (published_at, published_url) =
            store.get_review_verdict_published(id).unwrap().unwrap();
        assert_eq!(published_at, None);
        assert_eq!(published_url, None);

        // A missing review id is a clean `None`, not an error, at every one
        // of these getters.
        assert!(store.get_review_pr_binding(999).unwrap().is_none());
        assert!(store.get_review_report(999).unwrap().is_none());
        assert!(store.get_review_verdict_published(999).unwrap().is_none());
    }

    #[test]
    fn set_review_pr_binding_round_trips_and_is_independent_of_the_artifact_hint() {
        let (_tmp, store) = open_temp();
        let id = store
            .create_review("r", None, "main", "feature", None, 1_000)
            .unwrap();

        assert!(store
            .set_review_pr_binding(
                id,
                42,
                "acme/widgets",
                Some("deadbeef"),
                Some(r#"{"title":"x"}"#),
                Some(1_100)
            )
            .unwrap());
        let binding = store.get_review_pr_binding(id).unwrap().unwrap();
        assert_eq!(binding.pr_number, Some(42));
        assert_eq!(binding.pr_repo_slug.as_deref(), Some("acme/widgets"));
        assert_eq!(binding.pr_head_sha.as_deref(), Some("deadbeef"));
        assert_eq!(binding.pr_meta_json.as_deref(), Some(r#"{"title":"x"}"#));
        assert_eq!(binding.pr_meta_fetched_at, Some(1_100));
        assert_eq!(
            binding.artifact_hint_kb, None,
            "binding must not touch the hint"
        );

        // Best-effort GitHub enrichment failing at bind time — pr_meta_*
        // stay None, the git-fetch-backed binding itself still lands.
        let id2 = store
            .create_review("r", None, "main", "feature2", None, 1_000)
            .unwrap();
        store
            .set_review_pr_binding(id2, 43, "acme/widgets2", None, None, None)
            .unwrap();
        let binding2 = store.get_review_pr_binding(id2).unwrap().unwrap();
        assert_eq!(binding2.pr_number, Some(43));
        assert_eq!(binding2.pr_head_sha, None);

        // Refreshing metadata later (a re-fetch) leaves pr_number/slug alone.
        store
            .set_review_pr_meta(id2, Some("cafef00d"), Some(r#"{"title":"y"}"#), 1_200)
            .unwrap();
        let refreshed = store.get_review_pr_binding(id2).unwrap().unwrap();
        assert_eq!(refreshed.pr_number, Some(43));
        assert_eq!(refreshed.pr_repo_slug.as_deref(), Some("acme/widgets2"));
        assert_eq!(refreshed.pr_head_sha.as_deref(), Some("cafef00d"));

        // The artifact hint sets/clears independently of the PR binding.
        assert!(store
            .set_review_artifact_hint(id, Some("platform"), Some("doc123"))
            .unwrap());
        let with_hint = store.get_review_pr_binding(id).unwrap().unwrap();
        assert_eq!(with_hint.artifact_hint_kb.as_deref(), Some("platform"));
        assert_eq!(
            with_hint.pr_number,
            Some(42),
            "hint must not touch the binding"
        );
        assert!(store.set_review_artifact_hint(id, None, None).unwrap());
        assert_eq!(
            store
                .get_review_pr_binding(id)
                .unwrap()
                .unwrap()
                .artifact_hint_kb,
            None
        );

        assert!(!store
            .set_review_pr_binding(999, 1, "a/b", None, None, None)
            .unwrap());
        assert!(!store
            .set_review_artifact_hint(999, Some("k"), Some("d"))
            .unwrap());
    }

    #[test]
    fn set_review_report_replaces_wholesale_and_stamps_updated_at() {
        let (_tmp, store) = open_temp();
        let id = store
            .create_review("r", None, "main", "feature", None, 1_000)
            .unwrap();
        assert!(store
            .set_review_report(id, r#"{"summary":"a"}"#, 1_050)
            .unwrap());
        let report = store.get_review_report(id).unwrap().unwrap();
        assert_eq!(report.report_json.as_deref(), Some(r#"{"summary":"a"}"#));
        assert_eq!(report.report_updated_at, Some(1_050));

        // A later PUT replaces wholesale, not a merge.
        assert!(store
            .set_review_report(id, r#"{"summary":"b"}"#, 1_060)
            .unwrap());
        let report2 = store.get_review_report(id).unwrap().unwrap();
        assert_eq!(report2.report_json.as_deref(), Some(r#"{"summary":"b"}"#));
        assert_eq!(report2.report_updated_at, Some(1_060));

        assert!(!store.set_review_report(999, "{}", 1_000).unwrap());
    }

    #[test]
    fn set_review_verdict_published_round_trips() {
        let (_tmp, store) = open_temp();
        let id = store
            .create_review("r", None, "main", "feature", None, 1_000)
            .unwrap();
        assert!(store
            .set_review_verdict_published(
                id,
                Some("https://github.com/a/b/pull/1#pullrequestreview-1"),
                1_070
            )
            .unwrap());
        let (at, url) = store.get_review_verdict_published(id).unwrap().unwrap();
        assert_eq!(at, Some(1_070));
        assert_eq!(
            url.as_deref(),
            Some("https://github.com/a/b/pull/1#pullrequestreview-1")
        );
    }

    // -- location-kind -> anchor derivation (design doc §1.4) ---------------

    #[test]
    fn derive_finding_anchor_single_builds_one_line_selection() {
        let derived =
            derive_finding_anchor("single", "app/models/order.rb", Some(&[14]), false, |n| {
                assert_eq!(n, 14);
                "  def total".to_string()
            })
            .unwrap();
        assert_eq!(derived.anchor_kind, crate::annotations::ANCHOR_KIND_LINE);
        assert_eq!(derived.anchor2, None);
        assert_eq!(derived.side.as_deref(), Some("new"));
        let anchor: kb_core::review::Anchor = serde_json::from_str(&derived.anchor).unwrap();
        match anchor {
            kb_core::review::Anchor::Selection {
                offset, snippet, ..
            } => {
                assert_eq!(offset, 14);
                assert_eq!(snippet, "def total");
            }
            other => panic!("expected Selection, got {other:?}"),
        }
    }

    #[test]
    fn derive_finding_anchor_range_builds_two_selections_start_and_end() {
        let derived = derive_finding_anchor(
            "range",
            "app/models/order.rb",
            Some(&[13, 30]),
            false,
            |n| format!("line {n}"),
        )
        .unwrap();
        assert_eq!(derived.anchor_kind, crate::annotations::ANCHOR_KIND_RANGE);
        let start: kb_core::review::Anchor = serde_json::from_str(&derived.anchor).unwrap();
        let end: kb_core::review::Anchor =
            serde_json::from_str(derived.anchor2.as_deref().unwrap()).unwrap();
        match (start, end) {
            (
                kb_core::review::Anchor::Selection {
                    offset: s,
                    snippet: ss,
                    ..
                },
                kb_core::review::Anchor::Selection {
                    offset: e,
                    snippet: es,
                    ..
                },
            ) => {
                assert_eq!(s, 13);
                assert_eq!(ss, "line 13");
                assert_eq!(e, 30);
                assert_eq!(es, "line 30");
            }
            other => panic!("expected two Selections, got {other:?}"),
        }
    }

    #[test]
    fn derive_finding_anchor_multi_anchors_only_the_first_line() {
        let mut calls = Vec::new();
        let derived = derive_finding_anchor(
            "multi",
            "app/models/order.rb",
            Some(&[13, 30, 33, 36]),
            false,
            |n| {
                calls.push(n);
                format!("line {n}")
            },
        )
        .unwrap();
        // Documented approximation: only the FIRST line is ever resolved
        // for text — the ladder never even calls `line_text` for 30/33/36.
        assert_eq!(calls, vec![13]);
        assert_eq!(derived.anchor_kind, crate::annotations::ANCHOR_KIND_LINE);
        assert_eq!(derived.anchor2, None);
        let anchor: kb_core::review::Anchor = serde_json::from_str(&derived.anchor).unwrap();
        match anchor {
            kb_core::review::Anchor::Selection { offset, .. } => assert_eq!(offset, 13),
            other => panic!("expected Selection, got {other:?}"),
        }
    }

    #[test]
    fn derive_finding_anchor_whole_file_stores_the_bare_path_not_json() {
        let derived = derive_finding_anchor(
            "whole_file",
            "config/routes.rb",
            None,
            false,
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(
            derived.anchor_kind,
            crate::annotations::ANCHOR_KIND_WHOLE_FILE
        );
        assert_eq!(derived.anchor, "config/routes.rb");
        assert_eq!(derived.anchor2, None);
        // Not JSON — a bare path, per the migration's own doc.
        assert!(serde_json::from_str::<kb_core::review::Anchor>(&derived.anchor).is_err());
    }

    #[test]
    fn derive_finding_anchor_removed_forces_side_old_regardless_of_kind() {
        let single =
            derive_finding_anchor("single", "a.rb", Some(&[1]), true, |_| "x".to_string()).unwrap();
        assert_eq!(single.side.as_deref(), Some("old"));

        let whole_file =
            derive_finding_anchor("whole_file", "a.rb", None, true, |_| unreachable!()).unwrap();
        assert_eq!(whole_file.side.as_deref(), Some("old"));

        // Un-removed stays "new" — always an explicit string, never bare
        // `None` (matches `resolve_review_create_scope`'s convention).
        let not_removed =
            derive_finding_anchor("single", "a.rb", Some(&[1]), false, |_| "x".to_string())
                .unwrap();
        assert_eq!(not_removed.side.as_deref(), Some("new"));
    }

    #[test]
    fn derive_finding_anchor_rejects_malformed_locations() {
        assert!(derive_finding_anchor("single", "a.rb", None, false, |_| String::new()).is_err());
        assert!(
            derive_finding_anchor("single", "a.rb", Some(&[1, 2]), false, |_| String::new())
                .is_err()
        );
        assert!(
            derive_finding_anchor("range", "a.rb", Some(&[1]), false, |_| String::new()).is_err()
        );
        assert!(
            derive_finding_anchor("range", "a.rb", Some(&[1, 2, 3]), false, |_| String::new())
                .is_err()
        );
        assert!(
            derive_finding_anchor("multi", "a.rb", Some(&[]), false, |_| String::new()).is_err()
        );
        assert!(derive_finding_anchor("multi", "a.rb", None, false, |_| String::new()).is_err());
        assert!(
            derive_finding_anchor("bogus-kind", "a.rb", Some(&[1]), false, |_| String::new())
                .is_err()
        );
    }

    // -- vocab validators (severity / disposition / location_kind) ----------

    #[test]
    fn severity_vocab_accepts_exactly_the_three_severities() {
        for s in ["blocker", "concern", "ok"] {
            assert!(is_valid_severity(s), "{s} should be valid");
        }
        for s in ["Blocker", "info", "", "nit", "praise"] {
            assert!(!is_valid_severity(s), "{s} should be invalid");
        }
    }

    #[test]
    fn disposition_vocab_accepts_exactly_the_four_dispositions() {
        for d in ["agree", "dispute", "waive", "fix-later"] {
            assert!(is_valid_disposition(d), "{d} should be valid");
        }
        for d in ["Agree", "fixed", "", "wontfix"] {
            assert!(!is_valid_disposition(d), "{d} should be invalid");
        }
    }

    #[test]
    fn location_kind_vocab_accepts_exactly_the_four_kinds() {
        for k in ["single", "range", "multi", "whole_file"] {
            assert!(is_valid_location_kind(k), "{k} should be valid");
        }
        for k in ["Single", "whole-file", "", "line"] {
            assert!(!is_valid_location_kind(k), "{k} should be invalid");
        }
    }

    /// PRR-R1 scope extension.
    #[test]
    fn finding_origin_vocab_accepts_exactly_the_two_origins() {
        for o in ["import", "manual"] {
            assert!(is_valid_finding_origin(o), "{o} should be valid");
        }
        for o in ["Import", "human", "", "agent"] {
            assert!(!is_valid_finding_origin(o), "{o} should be invalid");
        }
    }

    // -- findings CRUD + the §4.3 reconciliation matrix ----------------------

    fn sample_imported_finding(slug: &str) -> ImportedFinding {
        ImportedFinding {
            slug: slug.to_string(),
            severity: SEVERITY_CONCERN.to_string(),
            category: "Concurrency".to_string(),
            location_kind: LOCATION_KIND_SINGLE.to_string(),
            location_path: "app/models/order.rb".to_string(),
            location_lines: Some(location_lines_json(&[88])),
            location_removed: false,
            title: "Duplicate order rows possible".to_string(),
            rationale: "Verified against db/schema.rb:41".to_string(),
            recommendation: Some("Add a unique index.".to_string()),
            evidence_lang: Some("ruby".to_string()),
            evidence_source: Some("def checkout!\nend".to_string()),
            anchor_kind: crate::annotations::ANCHOR_KIND_LINE.to_string(),
            anchor: serde_json::to_string(&crate::annotations::anchor_for_line(
                88,
                "def checkout!",
            ))
            .unwrap(),
            anchor2: None,
            side: Some("new".to_string()),
        }
    }

    fn setup_review_for_findings(store: &Store) -> (i64, i64) {
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let review_id = store
            .create_review("r", Some("feat"), "main", "feature", None, 1_000)
            .unwrap();
        (repo_id, review_id)
    }

    #[test]
    fn insert_review_finding_creates_a_1to1_annotation_with_intent_finding() {
        let (_tmp, store) = open_temp();
        let (repo_id, review_id) = setup_review_for_findings(&store);
        let new = NewReviewFinding {
            review_id,
            repo_id,
            ps_number: 1,
            slug: "f-dedup-race".to_string(),
            severity: SEVERITY_CONCERN.to_string(),
            category: "Concurrency".to_string(),
            location_kind: LOCATION_KIND_SINGLE.to_string(),
            location_path: "app/models/order.rb".to_string(),
            location_lines: Some(location_lines_json(&[88])),
            location_removed: false,
            title: "Duplicate order rows possible".to_string(),
            rationale: "Verified against db/schema.rb:41".to_string(),
            recommendation: None,
            evidence_lang: None,
            evidence_source: None,
            anchor_kind: crate::annotations::ANCHOR_KIND_LINE.to_string(),
            anchor: serde_json::to_string(&crate::annotations::anchor_for_line(
                88,
                "def checkout!",
            ))
            .unwrap(),
            anchor2: None,
            side: Some("new".to_string()),
            author: "claude".to_string(),
            import_batch_id: "batch-1".to_string(),
            origin: FINDING_ORIGIN_IMPORT.to_string(),
            finding_author: None,
        };
        let (annotation_id, finding_id) = store.insert_review_finding(&new, 1_000).unwrap();
        assert!(finding_id > 0);

        let ann = store.get_annotation(&annotation_id).unwrap().unwrap();
        assert_eq!(ann.intent, crate::annotations::INTENT_FINDING);
        assert_eq!(ann.review_id, Some(review_id));
        assert_eq!(ann.ps_number, Some(1));
        assert_eq!(ann.side.as_deref(), Some("new"));
        assert_eq!(ann.body, "Duplicate order rows possible");
        assert!(!ann.resolved);

        let finding = store
            .get_review_finding(review_id, "f-dedup-race")
            .unwrap()
            .unwrap();
        assert_eq!(finding.annotation_id, annotation_id);
        assert_eq!(finding.severity, "concern");
        assert_eq!(finding.disposition, None);
        assert!(!finding.superseded);
        assert_eq!(finding.published_state, "unpublished");
        assert_eq!(finding.origin, "import");
        assert_eq!(
            finding.author, None,
            "NULL is acceptable for origin=import in v1"
        );
        assert_eq!(
            finding.content_updated_at, None,
            "unset until a re-import refresh"
        );
    }

    #[test]
    fn reconcile_findings_import_new_slug_creates() {
        let (_tmp, store) = open_temp();
        let (repo_id, review_id) = setup_review_for_findings(&store);
        let findings = vec![
            sample_imported_finding("f-a"),
            sample_imported_finding("f-b"),
        ];
        let outcome = store
            .reconcile_findings_import(
                review_id,
                repo_id,
                1,
                "batch-1",
                "claude",
                &findings,
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        assert_eq!(outcome.created, vec!["f-a".to_string(), "f-b".to_string()]);
        assert!(outcome.updated.is_empty());
        assert!(outcome.superseded.is_empty());
        assert!(outcome.unchanged.is_empty());

        let listed = store.list_review_findings(review_id, None, false).unwrap();
        assert_eq!(listed.len(), 2);
    }

    #[test]
    fn reconcile_findings_import_existing_slug_present_again_refreshes_but_preserves_disposition_and_thread(
    ) {
        let (_tmp, store) = open_temp();
        let (repo_id, review_id) = setup_review_for_findings(&store);
        let first = vec![sample_imported_finding("f-a")];
        store
            .reconcile_findings_import(
                review_id,
                repo_id,
                1,
                "batch-1",
                "claude",
                &first,
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        let before = store.get_review_finding(review_id, "f-a").unwrap().unwrap();

        // A human dispositions it, and asks a question in its thread.
        store
            .set_finding_disposition(review_id, "f-a", "agree", Some("yep"), "you", 1_010)
            .unwrap();
        let reply_id = crate::annotations::new_annotation_id();
        store
            .insert_annotation(&AnnotationRow {
                id: reply_id.clone(),
                repo_id,
                path: before.location_path.clone(),
                anchor: None,
                anchor_kind: "line".to_string(),
                anchor2: None,
                parent_id: Some(before.annotation_id.clone()),
                intent: "note".to_string(),
                body: "why?".to_string(),
                author: "you".to_string(),
                created_at: 1_020,
                updated_at: 1_020,
                resolved: false,
                review_id: Some(review_id),
                ps_number: Some(1),
                side: Some("new".to_string()),
                set_id: None,
            })
            .unwrap();

        // Re-review: same slug, DIFFERENT severity/title/rationale.
        let mut refreshed_input = sample_imported_finding("f-a");
        refreshed_input.severity = SEVERITY_BLOCKER.to_string();
        refreshed_input.title = "Actually a blocker now".to_string();
        refreshed_input.rationale = "New evidence found".to_string();
        let second = vec![refreshed_input];
        let outcome = store
            .reconcile_findings_import(
                review_id,
                repo_id,
                2,
                "batch-2",
                "claude",
                &second,
                FindingsImportMode::Full,
                2_000,
            )
            .unwrap();
        assert_eq!(outcome.updated, vec!["f-a".to_string()]);
        assert!(outcome.created.is_empty());
        assert!(outcome.superseded.is_empty());
        assert!(outcome.unchanged.is_empty());

        let after = store.get_review_finding(review_id, "f-a").unwrap().unwrap();
        assert_eq!(
            after.annotation_id, before.annotation_id,
            "same finding, same annotation row"
        );
        assert_eq!(after.severity, "blocker");
        assert_eq!(after.title, "Actually a blocker now");
        assert_eq!(after.rationale, "New evidence found");
        assert_eq!(after.content_updated_at, Some(2_000));

        // Disposition survives untouched.
        assert_eq!(after.disposition.as_deref(), Some("agree"));
        assert_eq!(after.disposition_note.as_deref(), Some("yep"));
        assert_eq!(after.disposition_by.as_deref(), Some("you"));
        assert_eq!(after.disposition_at, Some(1_010));

        // The thread reply survives, and the annotation's anchor/ps_number
        // (its creation-time position) is NOT eagerly rewritten to ps 2.
        let thread = store
            .list_annotations(repo_id, &before.location_path)
            .unwrap();
        assert!(thread.iter().any(|a| a.id == reply_id), "reply survives");
        let ann_after = store.get_annotation(&after.annotation_id).unwrap().unwrap();
        assert_eq!(
            ann_after.ps_number,
            Some(1),
            "anchor stays pinned to its FIRST-import ps"
        );
        assert_eq!(
            ann_after.body, "Actually a blocker now",
            "display body mirrors the refreshed title"
        );
    }

    #[test]
    fn reconcile_findings_import_existing_slug_absent_soft_supersedes() {
        let (_tmp, store) = open_temp();
        let (repo_id, review_id) = setup_review_for_findings(&store);
        let first = vec![
            sample_imported_finding("f-a"),
            sample_imported_finding("f-b"),
        ];
        store
            .reconcile_findings_import(
                review_id,
                repo_id,
                1,
                "batch-1",
                "claude",
                &first,
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();

        // Re-review only reproduces f-a — f-b is gone from the new diff.
        let second = vec![sample_imported_finding("f-a")];
        let outcome = store
            .reconcile_findings_import(
                review_id,
                repo_id,
                2,
                "batch-2",
                "claude",
                &second,
                FindingsImportMode::Full,
                2_000,
            )
            .unwrap();
        assert_eq!(outcome.superseded, vec!["f-b".to_string()]);
        // f-a is byte-identical to its first import, so it lands in
        // "unchanged", not "updated".
        assert_eq!(outcome.unchanged, vec!["f-a".to_string()]);

        // Never hard-deleted: invisible by default, still readable via
        // include_superseded=true.
        let default_list = store.list_review_findings(review_id, None, false).unwrap();
        assert_eq!(default_list.len(), 1);
        assert_eq!(default_list[0].slug, "f-a");

        let all_list = store.list_review_findings(review_id, None, true).unwrap();
        assert_eq!(all_list.len(), 2);
        let f_b = all_list.iter().find(|f| f.slug == "f-b").unwrap();
        assert!(f_b.superseded);
        assert_eq!(f_b.superseded_reason.as_deref(), Some("not_in_reimport"));
        assert_eq!(f_b.superseded_at, Some(2_000));

        let direct = store.get_review_finding(review_id, "f-b").unwrap().unwrap();
        assert!(
            direct.superseded,
            "still directly gettable by slug — a tombstone, not an erasure"
        );
    }

    fn insert_manual_finding(
        store: &Store,
        repo_id: i64,
        review_id: i64,
        slug: &str,
    ) -> NewReviewFinding {
        let new = NewReviewFinding {
            review_id,
            repo_id,
            ps_number: 1,
            slug: slug.to_string(),
            severity: SEVERITY_OK.to_string(),
            category: "Style".to_string(),
            location_kind: LOCATION_KIND_SINGLE.to_string(),
            location_path: "app/models/order.rb".to_string(),
            location_lines: Some(location_lines_json(&[5])),
            location_removed: false,
            title: "A human noticed this too".to_string(),
            rationale: "Spotted while reading the diff.".to_string(),
            recommendation: None,
            evidence_lang: None,
            evidence_source: None,
            anchor_kind: crate::annotations::ANCHOR_KIND_LINE.to_string(),
            anchor: serde_json::to_string(&crate::annotations::anchor_for_line(5, "x")).unwrap(),
            anchor2: None,
            side: Some("new".to_string()),
            author: "you".to_string(),
            import_batch_id: "manual".to_string(),
            origin: FINDING_ORIGIN_MANUAL.to_string(),
            finding_author: Some("you".to_string()),
        };
        store.insert_review_finding(&new, 900).unwrap();
        new
    }

    /// PRR-R1 scope extension (operator-ratified mid-build) — a
    /// human-authored ("manual") finding is NEVER superseded by an agent's
    /// re-import: the agent's own findings set structurally cannot contain
    /// a slug it never generated, and that absence must not tombstone it.
    /// Disposition, thread, and every field survive a FULL re-import
    /// untouched.
    #[test]
    fn reconcile_findings_import_never_supersedes_a_manual_finding() {
        let (_tmp, store) = open_temp();
        let (repo_id, review_id) = setup_review_for_findings(&store);
        insert_manual_finding(&store, repo_id, review_id, "f-manual");
        store
            .set_finding_disposition(
                review_id,
                "f-manual",
                "agree",
                Some("good catch"),
                "you",
                910,
            )
            .unwrap();
        let before = store
            .get_review_finding(review_id, "f-manual")
            .unwrap()
            .unwrap();
        assert_eq!(before.origin, "manual");
        assert!(!before.superseded);

        // A full agent re-import that never mentions "f-manual" at all.
        let imported = vec![sample_imported_finding("f-a")];
        let outcome = store
            .reconcile_findings_import(
                review_id,
                repo_id,
                1,
                "batch-1",
                "claude",
                &imported,
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        assert!(
            !outcome.superseded.contains(&"f-manual".to_string()),
            "a manual finding must never appear in the superseded bucket"
        );
        assert_eq!(outcome.created, vec!["f-a".to_string()]);

        let after = store
            .get_review_finding(review_id, "f-manual")
            .unwrap()
            .unwrap();
        assert_eq!(after, before, "byte-identical — untouched by the re-import");

        // A second full re-import, still never mentioning it — still safe.
        store
            .reconcile_findings_import(
                review_id,
                repo_id,
                2,
                "batch-2",
                "claude",
                &imported,
                FindingsImportMode::Full,
                2_000,
            )
            .unwrap();
        assert!(
            !store
                .get_review_finding(review_id, "f-manual")
                .unwrap()
                .unwrap()
                .superseded
        );
    }

    /// PRR-R3 — the OWED defense-in-depth fix: an import batch whose slug
    /// COLLIDES with an existing manual finding must never refresh it
    /// (content, disposition, thread — none of it), in ANY mode. The route
    /// boundary rejects such a batch wholesale before ever reaching this
    /// function, but this test pins the data-layer guard directly, calling
    /// `reconcile_findings_import` the way a hypothetical future caller
    /// that skipped route-level validation still would.
    #[test]
    fn reconcile_findings_import_refresh_never_overwrites_a_manual_finding_even_on_slug_collision()
    {
        let (_tmp, store) = open_temp();
        let (repo_id, review_id) = setup_review_for_findings(&store);
        insert_manual_finding(&store, repo_id, review_id, "f-shared-slug");
        let before = store
            .get_review_finding(review_id, "f-shared-slug")
            .unwrap()
            .unwrap();
        assert_eq!(before.origin, "manual");

        // An import batch that reuses the SAME slug with entirely
        // different content — as if the generator agent happened to mint
        // an identical-looking slug independently.
        let mut colliding = sample_imported_finding("f-shared-slug");
        colliding.title = "A totally different agent-authored title".to_string();
        colliding.severity = SEVERITY_BLOCKER.to_string();
        let outcome = store
            .reconcile_findings_import(
                review_id,
                repo_id,
                1,
                "batch-1",
                "claude",
                &[colliding],
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();

        // Never counted as created or updated — the write never happened.
        assert!(outcome.created.is_empty());
        assert!(outcome.updated.is_empty());
        assert_eq!(outcome.unchanged, vec!["f-shared-slug".to_string()]);

        let after = store
            .get_review_finding(review_id, "f-shared-slug")
            .unwrap()
            .unwrap();
        assert_eq!(
            after, before,
            "byte-identical — the manual row must be completely untouched \
             by a colliding import, not just its disposition/origin"
        );
    }

    /// PRR-R1 scope extension — `FindingsImportMode::Additive` never
    /// supersedes anything, even an `origin="import"` slug that would have
    /// been superseded under `Full`.
    #[test]
    fn reconcile_findings_import_additive_mode_never_supersedes() {
        let (_tmp, store) = open_temp();
        let (repo_id, review_id) = setup_review_for_findings(&store);
        let first = vec![
            sample_imported_finding("f-a"),
            sample_imported_finding("f-b"),
        ];
        store
            .reconcile_findings_import(
                review_id,
                repo_id,
                1,
                "batch-1",
                "claude",
                &first,
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();

        // An additive batch mentioning only a brand-new slug — f-a/f-b are
        // absent, but MUST survive because mode=Additive.
        let additive = vec![sample_imported_finding("f-c")];
        let outcome = store
            .reconcile_findings_import(
                review_id,
                repo_id,
                2,
                "batch-2",
                "claude",
                &additive,
                FindingsImportMode::Additive,
                2_000,
            )
            .unwrap();
        assert_eq!(outcome.created, vec!["f-c".to_string()]);
        assert!(
            outcome.superseded.is_empty(),
            "additive mode supersedes nothing"
        );

        let all = store.list_review_findings(review_id, None, false).unwrap();
        let slugs: std::collections::BTreeSet<_> = all.iter().map(|f| f.slug.as_str()).collect();
        assert_eq!(
            slugs,
            std::collections::BTreeSet::from(["f-a", "f-b", "f-c"]),
            "f-a/f-b stay visible — additive mode never tombstones an absent slug"
        );
        assert!(
            !store
                .get_review_finding(review_id, "f-a")
                .unwrap()
                .unwrap()
                .superseded
        );
        assert!(
            !store
                .get_review_finding(review_id, "f-b")
                .unwrap()
                .unwrap()
                .superseded
        );
    }

    #[test]
    fn reconcile_findings_import_unsupersedes_a_slug_that_reappears() {
        let (_tmp, store) = open_temp();
        let (repo_id, review_id) = setup_review_for_findings(&store);
        let one = vec![sample_imported_finding("f-a")];
        store
            .reconcile_findings_import(
                review_id,
                repo_id,
                1,
                "batch-1",
                "claude",
                &one,
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        // Drop it (superseded)...
        store
            .reconcile_findings_import(
                review_id,
                repo_id,
                2,
                "batch-2",
                "claude",
                &[],
                FindingsImportMode::Full,
                2_000,
            )
            .unwrap();
        assert!(
            store
                .get_review_finding(review_id, "f-a")
                .unwrap()
                .unwrap()
                .superseded
        );

        // ...then it comes back in a later re-review (same content).
        let outcome = store
            .reconcile_findings_import(
                review_id,
                repo_id,
                3,
                "batch-3",
                "claude",
                &one,
                FindingsImportMode::Full,
                3_000,
            )
            .unwrap();
        assert_eq!(
            outcome.updated,
            vec!["f-a".to_string()],
            "un-superseding is a real write"
        );
        let back = store.get_review_finding(review_id, "f-a").unwrap().unwrap();
        assert!(!back.superseded);
        assert_eq!(back.superseded_at, None);
        assert_eq!(back.superseded_reason, None);
    }

    #[test]
    fn reconcile_findings_import_a_second_identical_import_reports_unchanged_and_writes_nothing() {
        let (_tmp, store) = open_temp();
        let (repo_id, review_id) = setup_review_for_findings(&store);
        let findings = vec![sample_imported_finding("f-a")];
        store
            .reconcile_findings_import(
                review_id,
                repo_id,
                1,
                "batch-1",
                "claude",
                &findings,
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        let before = store.get_review_finding(review_id, "f-a").unwrap().unwrap();

        let outcome = store
            .reconcile_findings_import(
                review_id,
                repo_id,
                1,
                "batch-2",
                "claude",
                &findings,
                FindingsImportMode::Full,
                2_000,
            )
            .unwrap();
        assert_eq!(outcome.unchanged, vec!["f-a".to_string()]);
        assert!(outcome.updated.is_empty());

        let after = store.get_review_finding(review_id, "f-a").unwrap().unwrap();
        assert_eq!(
            after, before,
            "a true no-op import touches nothing, not even updated_at"
        );
    }

    #[test]
    fn list_review_findings_filters_by_disposition_and_include_superseded() {
        let (_tmp, store) = open_temp();
        let (repo_id, review_id) = setup_review_for_findings(&store);
        let findings = vec![
            sample_imported_finding("f-a"),
            sample_imported_finding("f-b"),
            sample_imported_finding("f-c"),
        ];
        store
            .reconcile_findings_import(
                review_id,
                repo_id,
                1,
                "batch-1",
                "claude",
                &findings,
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        store
            .set_finding_disposition(review_id, "f-a", "agree", None, "you", 1_010)
            .unwrap();
        store
            .set_finding_disposition(review_id, "f-b", "waive", None, "you", 1_010)
            .unwrap();

        let agreed = store
            .list_review_findings(review_id, Some("agree"), false)
            .unwrap();
        assert_eq!(agreed.len(), 1);
        assert_eq!(agreed[0].slug, "f-a");

        let all = store.list_review_findings(review_id, None, false).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn set_and_clear_finding_disposition_report_missing_vs_noop_vs_changed() {
        let (_tmp, store) = open_temp();
        let (repo_id, review_id) = setup_review_for_findings(&store);
        let findings = vec![sample_imported_finding("f-a")];
        store
            .reconcile_findings_import(
                review_id,
                repo_id,
                1,
                "batch-1",
                "claude",
                &findings,
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();

        // Missing slug -> None.
        assert_eq!(
            store
                .set_finding_disposition(review_id, "nope", "agree", None, "you", 1_000)
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .clear_finding_disposition(review_id, "nope", 1_000)
                .unwrap(),
            None
        );

        // First set -> changed.
        assert_eq!(
            store
                .set_finding_disposition(review_id, "f-a", "waive", Some("later"), "you", 1_010)
                .unwrap(),
            Some(true)
        );
        // Identical re-set -> no-op.
        assert_eq!(
            store
                .set_finding_disposition(review_id, "f-a", "waive", Some("later"), "you", 1_020)
                .unwrap(),
            Some(false)
        );
        // Different note -> changed.
        assert_eq!(
            store
                .set_finding_disposition(
                    review_id,
                    "f-a",
                    "waive",
                    Some("actually now"),
                    "you",
                    1_030
                )
                .unwrap(),
            Some(true)
        );

        // Clear -> changed, then no-op.
        assert_eq!(
            store
                .clear_finding_disposition(review_id, "f-a", 1_040)
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            store
                .clear_finding_disposition(review_id, "f-a", 1_050)
                .unwrap(),
            Some(false)
        );
        let cleared = store.get_review_finding(review_id, "f-a").unwrap().unwrap();
        assert_eq!(cleared.disposition, None);
        assert_eq!(cleared.disposition_note, None);
        assert_eq!(cleared.disposition_by, None);
        assert_eq!(cleared.disposition_at, None);
    }

    #[test]
    fn set_finding_published_records_state_url_and_timestamp() {
        let (_tmp, store) = open_temp();
        let (repo_id, review_id) = setup_review_for_findings(&store);
        let findings = vec![sample_imported_finding("f-a")];
        store
            .reconcile_findings_import(
                review_id,
                repo_id,
                1,
                "batch-1",
                "claude",
                &findings,
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        assert!(store
            .set_finding_published(
                review_id,
                "f-a",
                Some("https://github.com/a/b/pull/1#discussion_r1"),
                1_500
            )
            .unwrap());
        let published = store.get_review_finding(review_id, "f-a").unwrap().unwrap();
        assert_eq!(published.published_state, "published");
        assert_eq!(published.published_at, Some(1_500));
        assert_eq!(
            published.published_url.as_deref(),
            Some("https://github.com/a/b/pull/1#discussion_r1")
        );
        assert!(!store
            .set_finding_published(review_id, "nope", None, 1_500)
            .unwrap());
    }

    /// V0024 on a fresh db: refinery runs the whole chain, so the new
    /// `reviews` columns and `review_findings` are reachable, and a raw
    /// insert using the NEW `intent="finding"` / `anchor_kind="whole_file"`
    /// vocab values succeeds at the SQL layer with no CHECK constraint in
    /// the way (vocab is route-validated only — see the migration's doc).
    #[test]
    fn v0024_migration_applies_cleanly_and_accepts_the_new_vocab_values_unconstrained() {
        let (_tmp, store) = open_temp();
        let names: Vec<String> = store
            .lock()
            .prepare("PRAGMA table_info(reviews)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        for col in [
            "pr_number",
            "pr_repo_slug",
            "pr_head_sha",
            "pr_meta_json",
            "pr_meta_fetched_at",
            "artifact_hint_kb",
            "artifact_hint_id",
            "report_json",
            "report_updated_at",
            "verdict_published_at",
            "verdict_published_url",
        ] {
            assert!(names.contains(&col.to_string()), "missing column {col}");
        }

        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let review_id = store
            .create_review("r", None, "main", "feature", None, 1_000)
            .unwrap();
        let new = NewReviewFinding {
            review_id,
            repo_id,
            ps_number: 1,
            slug: "f-whole".to_string(),
            severity: SEVERITY_OK.to_string(),
            category: "Style".to_string(),
            location_kind: LOCATION_KIND_WHOLE_FILE.to_string(),
            location_path: "config/routes.rb".to_string(),
            location_lines: None,
            location_removed: false,
            title: "Consider splitting this file".to_string(),
            rationale: "It has grown large.".to_string(),
            recommendation: None,
            evidence_lang: None,
            evidence_source: None,
            anchor_kind: crate::annotations::ANCHOR_KIND_WHOLE_FILE.to_string(),
            anchor: "config/routes.rb".to_string(),
            anchor2: None,
            side: Some("new".to_string()),
            author: "claude".to_string(),
            import_batch_id: "batch-1".to_string(),
            // Also exercises the "manual" origin + a real author string —
            // the SQL layer imposes no CHECK on either.
            origin: FINDING_ORIGIN_MANUAL.to_string(),
            finding_author: Some("carol".to_string()),
        };
        let (annotation_id, _finding_id) = store.insert_review_finding(&new, 1_000).unwrap();
        let ann = store.get_annotation(&annotation_id).unwrap().unwrap();
        assert_eq!(ann.intent, "finding");
        assert_eq!(ann.anchor_kind, "whole_file");
        assert_eq!(ann.anchor.as_deref(), Some("config/routes.rb"));

        let finding = store
            .get_review_finding(review_id, "f-whole")
            .unwrap()
            .unwrap();
        assert_eq!(finding.origin, "manual");
        assert_eq!(finding.author.as_deref(), Some("carol"));
    }

    /// `PATCH TABLE review_findings` cascades on review delete (SQL
    /// `ON DELETE CASCADE` per the migration), unlike `annotations.review_id`
    /// (no SQL FK, cascade is code-owned — V0023's own precedent). This pins
    /// that the two mechanisms both actually clean up, even though they get
    /// there differently.
    #[test]
    fn review_findings_cascade_deletes_with_their_review_via_sql_fk() {
        let (_tmp, store) = open_temp();
        let (repo_id, review_id) = setup_review_for_findings(&store);
        let findings = vec![sample_imported_finding("f-a")];
        store
            .reconcile_findings_import(
                review_id,
                repo_id,
                1,
                "batch-1",
                "claude",
                &findings,
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        assert_eq!(
            store
                .list_review_findings(review_id, None, true)
                .unwrap()
                .len(),
            1
        );

        store
            .lock()
            .execute("DELETE FROM reviews WHERE id = ?1", params![review_id])
            .unwrap();
        assert_eq!(
            store
                .list_review_findings(review_id, None, true)
                .unwrap()
                .len(),
            0
        );
    }

    // ── PRR-N12: scip runs ──────────────────────────────────────────────

    #[test]
    fn latest_scip_run_is_none_when_never_ingested() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        assert!(store.latest_scip_run(repo_id).unwrap().is_none());
    }

    #[test]
    fn record_scip_run_appends_and_latest_scip_run_reads_the_newest() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();

        store.record_scip_run(repo_id, "sha-one", 100, 3).unwrap();
        let latest = store.latest_scip_run(repo_id).unwrap().unwrap();
        assert_eq!(latest.head_sha, "sha-one");
        assert_eq!(latest.ingested_at, 100);
        assert_eq!(latest.docs_accepted, 3);

        // A second, later run supersedes — `latest_scip_run` never returns
        // the older row once a newer one lands (multiple `scip run`
        // invocations against the same repo are all kept, but only the
        // newest drives `ScipStatus`).
        store.record_scip_run(repo_id, "sha-two", 200, 5).unwrap();
        let latest = store.latest_scip_run(repo_id).unwrap().unwrap();
        assert_eq!(latest.head_sha, "sha-two");
        assert_eq!(latest.ingested_at, 200);
        assert_eq!(latest.docs_accepted, 5);
    }

    #[test]
    fn scip_run_writes_do_not_bump_the_store_generation() {
        // `scip_runs` is invisible to `FileIndex`/`SymbolIndex`'s
        // generation-keyed caches — same posture as `doc_lens_pins`'
        // `pin_writes_do_not_bump_the_store_generation`.
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let before = store.generation();
        store.record_scip_run(repo_id, "sha", 1, 0).unwrap();
        assert_eq!(store.generation(), before);

        // Control: a files write DOES bump, so this test can't pass
        // vacuously.
        store.upsert_file(repo_id, "a.rs", "h", "rust", 1).unwrap();
        assert!(store.generation() > before);
    }

    #[test]
    fn count_files_by_langs_counts_only_the_named_langs_and_short_circuits_on_empty() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store.upsert_file(repo_id, "a.rs", "h1", "rust", 1).unwrap();
        store.upsert_file(repo_id, "b.rs", "h2", "rust", 1).unwrap();
        store.upsert_file(repo_id, "c.rb", "h3", "ruby", 1).unwrap();
        store
            .upsert_file(repo_id, "d.txt", "h4", "unknown", 1)
            .unwrap();

        assert_eq!(
            store
                .count_files_by_langs(repo_id, &["rust".to_string()])
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .count_files_by_langs(repo_id, &["rust".to_string(), "ruby".to_string()])
                .unwrap(),
            3
        );
        assert_eq!(store.count_files_by_langs(repo_id, &[]).unwrap(), 0);
        assert_eq!(
            store
                .count_files_by_langs(repo_id, &["python".to_string()])
                .unwrap(),
            0
        );
    }

    #[test]
    fn count_scip_covered_files_counts_distinct_paths_with_an_scip_source_occurrence() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hash-a", "rust", 1)
            .unwrap();
        store
            .upsert_file(repo_id, "b.rs", "hash-b", "rust", 1)
            .unwrap();
        let lang = crate::lang::for_id("rust").unwrap();

        // Nothing ingested yet.
        assert_eq!(store.count_scip_covered_files(repo_id).unwrap(), 0);

        store
            .replace_scip_occurrences(
                "hash-a",
                lang.salt,
                &[ScipOccurrenceIn {
                    name: "widget".to_string(),
                    role: "def".to_string(),
                    line: 1,
                    col_start: 0,
                    col_end: 6,
                }],
            )
            .unwrap();
        assert_eq!(store.count_scip_covered_files(repo_id).unwrap(), 1);

        // A plain tree-sitter (`source = 'ts'`) occurrence on `b.rs` does
        // NOT count — only `source = 'scip'` rows do.
        store
            .replace_occurrences(
                "hash-b",
                lang.salt,
                &[crate::occurrences::Occurrence {
                    ordinal: 0,
                    name: "other".to_string(),
                    role: "def".to_string(),
                    line: 1,
                    col_start: 0,
                    col_end: 5,
                    source: crate::occurrences::SOURCE_TS.to_string(),
                    local_def_ordinal: None,
                }],
            )
            .unwrap();
        assert_eq!(store.count_scip_covered_files(repo_id).unwrap(), 1);
    }

    // --- PRR-R9: review analytics ------------------------------------------

    fn analytics_finding(
        slug: &str,
        severity: &str,
        category: &str,
        path: &str,
    ) -> ImportedFinding {
        let mut f = sample_imported_finding(slug);
        f.severity = severity.to_string();
        f.category = category.to_string();
        f.location_path = path.to_string();
        f
    }

    #[test]
    fn list_findings_for_analytics_filters_by_repo_and_created_at_window_and_includes_superseded() {
        let (_tmp, store) = open_temp();
        let repo_id_a = store.upsert_repo("a", "/tmp/a").unwrap();
        let review_a = store
            .create_review("a", None, "main", "feature", None, 1_000)
            .unwrap();
        let repo_id_b = store.upsert_repo("b", "/tmp/b").unwrap();
        let review_b = store
            .create_review("b", None, "main", "feature", None, 1_000)
            .unwrap();

        store
            .reconcile_findings_import(
                review_a,
                repo_id_a,
                1,
                "batch-a",
                "claude",
                &[analytics_finding(
                    "f-a1",
                    SEVERITY_BLOCKER,
                    "Security",
                    "x.rb",
                )],
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        store
            .reconcile_findings_import(
                review_a,
                repo_id_a,
                1,
                "batch-a2",
                "claude",
                &[analytics_finding("f-a2", SEVERITY_OK, "Style", "y.rb")],
                FindingsImportMode::Additive,
                5_000,
            )
            .unwrap();
        store
            .reconcile_findings_import(
                review_b,
                repo_id_b,
                1,
                "batch-b",
                "claude",
                &[analytics_finding(
                    "f-b1",
                    SEVERITY_CONCERN,
                    "Security",
                    "x.rb",
                )],
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        // Supersede f-a1 by re-importing batch-a in `Full` mode without it.
        store
            .reconcile_findings_import(
                review_a,
                repo_id_a,
                1,
                "batch-a3",
                "claude",
                &[],
                FindingsImportMode::Full,
                9_000,
            )
            .unwrap();

        // repo filter.
        let repo_a = store
            .list_findings_for_analytics(Some("a"), None, None)
            .unwrap();
        assert_eq!(repo_a.len(), 2, "both a-findings, superseded included");

        // no repo filter -> every repo.
        let all = store.list_findings_for_analytics(None, None, None).unwrap();
        assert_eq!(all.len(), 3);

        // created_at window excludes f-a1 (created_at=1_000).
        let windowed = store
            .list_findings_for_analytics(Some("a"), Some(2_000), None)
            .unwrap();
        assert_eq!(windowed.len(), 1);
        assert_eq!(windowed[0].category, "Style");

        // superseded is surfaced, not dropped.
        let a1 = repo_a
            .iter()
            .find(|r| r.category == "Security")
            .expect("f-a1 present");
        assert!(a1.superseded, "reimport without f-a1 must supersede it");
    }

    #[test]
    fn recurrence_pairs_requires_at_least_min_reviews_distinct_reviews() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let review1 = store
            .create_review("r", None, "main", "f1", None, 1_000)
            .unwrap();
        let review2 = store
            .create_review("r", None, "main", "f2", None, 1_000)
            .unwrap();
        let review3 = store
            .create_review("r", None, "main", "f3", None, 1_000)
            .unwrap();

        // (Security, x.rb) recurs across review1 + review2.
        store
            .reconcile_findings_import(
                review1,
                repo_id,
                1,
                "b1",
                "claude",
                &[analytics_finding(
                    "f1",
                    SEVERITY_BLOCKER,
                    "Security",
                    "x.rb",
                )],
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        store
            .reconcile_findings_import(
                review2,
                repo_id,
                1,
                "b2",
                "claude",
                &[analytics_finding(
                    "f2",
                    SEVERITY_BLOCKER,
                    "Security",
                    "x.rb",
                )],
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        // (Style, y.rb) appears only once -> below threshold.
        store
            .reconcile_findings_import(
                review3,
                repo_id,
                1,
                "b3",
                "claude",
                &[analytics_finding("f3", SEVERITY_OK, "Style", "y.rb")],
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();

        let pairs = store
            .recurrence_pairs(Some("r"), None, None, RECURRENCE_MIN_REVIEWS)
            .unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].category, "Security");
        assert_eq!(pairs[0].location_path, "x.rb");
        assert_eq!(pairs[0].review_count, 2);
        assert_eq!(pairs[0].review_ids, {
            let mut v = vec![review1, review2];
            v.sort_unstable();
            v
        });
    }

    #[test]
    fn recurrence_pairs_excludes_superseded_findings_from_the_count() {
        let (_tmp, store) = open_temp();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        let review1 = store
            .create_review("r", None, "main", "f1", None, 1_000)
            .unwrap();
        let review2 = store
            .create_review("r", None, "main", "f2", None, 1_000)
            .unwrap();
        store
            .reconcile_findings_import(
                review1,
                repo_id,
                1,
                "b1",
                "claude",
                &[analytics_finding(
                    "f1",
                    SEVERITY_BLOCKER,
                    "Security",
                    "x.rb",
                )],
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        store
            .reconcile_findings_import(
                review2,
                repo_id,
                1,
                "b2",
                "claude",
                &[analytics_finding(
                    "f2",
                    SEVERITY_BLOCKER,
                    "Security",
                    "x.rb",
                )],
                FindingsImportMode::Full,
                1_000,
            )
            .unwrap();
        // Supersede review2's finding — re-import Full with an empty batch.
        store
            .reconcile_findings_import(
                review2,
                repo_id,
                1,
                "b3",
                "claude",
                &[],
                FindingsImportMode::Full,
                2_000,
            )
            .unwrap();

        let pairs = store
            .recurrence_pairs(Some("r"), None, None, RECURRENCE_MIN_REVIEWS)
            .unwrap();
        assert!(
            pairs.is_empty(),
            "only one non-superseded review left -> below threshold"
        );
    }
}
