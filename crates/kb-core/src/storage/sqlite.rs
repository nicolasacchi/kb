//! Per-kb SQLite tracking — sources, errors, index_runs, edges. Lance owns
//! the indexed Doc columns; this side-channel holds operational state that
//! doesn't fit columnar storage. Refinery handles forward migrations from
//! `crates/kb-core/migrations/`.
//!
//! Connection settings: WAL journal mode, synchronous NORMAL (durable-enough
//! with WAL), ~16 MiB page cache, a modest mmap window, busy_timeout 5 s,
//! foreign_keys ON (per topic 01 §Decisions).

use crate::config::RetentionSection;
use crate::ids::{ErrorId, RunId, SourceSlug};
use crate::lists::{ImportMode, NewListEntry, Patch, PositionSpec, ResolutionUpdate};
use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

mod embedded {
    refinery::embed_migrations!("./migrations");
}

/// This binary's kb schema epoch — the highest version among the migrations
/// EMBEDDED in it (`crates/kb-core/migrations/`). One binary, one epoch:
/// every per-kb `index.db` this process opens is migrated by this same set.
/// Surfaced on `GET /api/identity` as `schema_epoch` (kb-sibling/1) and
/// compared against each volume's own epoch by [`Db::open`].
pub fn schema_epoch() -> u32 {
    crate::sibling::binary_epoch(&embedded::migrations::runner())
}

/// Per-kb SQLite handle. Owns one rusqlite `Connection`.
pub struct Db {
    conn: Connection,
}

/// Invariant #11 — the ONE newest-capture collapse, as a SQL predicate.
///
/// A long Claude Code session is captured at EVERY Stop, so one `session_id`
/// accrues many `sessions` rows (one per capture, each with its own
/// `artifact_id`), and every capture re-records the same child rows in
/// `session_files`/`session_decisions`/`session_commits`/`session_research`.
/// The newest capture is the SUPERSET, so any read that lists sessions, joins
/// child rows, or aggregates (`COUNT(*)`, `SUM`, `GROUP BY`) must scope to it
/// — otherwise a 5-Stop session counts 5×. `sessions_folders` shipped without
/// this scope and double-counted the folder facet for months; the milestone
/// verification greps for bare `GROUP BY cwd|session_id` in sessions queries,
/// and this fn is the only sanctioned answer.
///
/// `artifact_col` is the column being constrained (`artifact_id` on `sessions`
/// itself, `artifact_id_session` — optionally alias-qualified — on a child
/// table). `session_id_expr` is the SQL expression naming the session whose
/// newest capture we want: a correlated column (`sessions.session_id`,
/// `c.session_id`) or a bind placeholder (`?1`).
///
/// PF-R1 (V0040): this used to be a correlated `ORDER BY started_at DESC,
/// artifact_id ASC LIMIT 1` re-sort of the WHOLE capture group, evaluated on
/// EVERY read. `sessions.is_newest` now materializes that same tie-break at
/// WRITE time ([`recompute_is_newest`]), so the subquery here is a plain
/// indexed equality lookup (`idx_sessions_newest_by_session`, a UNIQUE
/// partial index over `session_id WHERE is_newest = 1`) — same shape, same
/// two-arg contract, cheaper. `LIMIT 1` stays as defense in depth: a scalar
/// subquery returning more than one row is a SQLite runtime error, and the
/// UNIQUE index only guarantees that CAN'T happen if maintenance is correct.
///
/// The subquery aliases `sessions` as `s2`, so no caller may use that alias in
/// its outer query. A handful of call sites compare the `sessions` table
/// against itself (`artifact_col = "artifact_id"`, `session_id_expr =
/// "sessions.session_id"`, no other table in scope) — those read
/// `is_newest = 1` directly instead of calling this fn, since the subquery
/// then degenerates to "is this row itself the flagged one".
fn newest_capture_pred(artifact_col: &str, session_id_expr: &str) -> String {
    format!(
        "{artifact_col} = (SELECT artifact_id FROM sessions s2 \
         WHERE s2.session_id = {session_id_expr} AND s2.is_newest = 1 LIMIT 1)"
    )
}

/// PF-R1 (V0040) — re-derive the materialized `is_newest` flag for one
/// `session_id`'s capture group, inside the CALLER's transaction. Clears any
/// currently-flagged row for the group, then re-applies the flag to the
/// group's newest capture by the EXACT tie-break `newest_capture_pred` used
/// before materialization (`started_at DESC, artifact_id ASC`) — a full
/// recompute rather than a diff against the candidate row, so it stays
/// correct regardless of write order (out-of-order captures, a reindex that
/// repairs `session_id`, a delete of any row in the group). A group left
/// with zero rows is a no-op: the second UPDATE's subquery returns no match.
///
/// Called by every write that can change which capture is newest for a
/// session_id: [`Db::sessions_upsert`] (new or reindexed capture) and the
/// `sessions` step of [`Db::cascade_delete_doc`] / [`Db::sessions_delete`]
/// (removing a capture, possibly the currently-flagged one).
/// [`Db::cascade_relocate_doc`] needs no call here — it only ever rekeys
/// `artifact_id`, never `session_id`, so the flag rides the row unchanged.
fn recompute_is_newest(tx: &rusqlite::Transaction<'_>, session_id: &str) -> Result<()> {
    tx.execute(
        "UPDATE sessions SET is_newest = 0 WHERE session_id = ?1 AND is_newest = 1",
        params![session_id],
    )?;
    tx.execute(
        "UPDATE sessions SET is_newest = 1 WHERE artifact_id = (
            SELECT artifact_id FROM sessions
            WHERE session_id = ?1
            ORDER BY started_at DESC, artifact_id ASC LIMIT 1
        )",
        params![session_id],
    )?;
    Ok(())
}

/// Saturating `i64` → `u32` for counter columns read from sqlite (negative
/// values collapse to 0; values above `u32::MAX` clamp rather than wrap).
fn i64_as_u32_sat(v: i64) -> u32 {
    u32::try_from(v.max(0)).unwrap_or(u32::MAX)
}

/// Saturating `i64` → `u64` for large counters (`token_total`, etc.).
fn i64_as_u64_sat(v: i64) -> u64 {
    u64::try_from(v.max(0)).unwrap_or(u64::MAX)
}

impl Db {
    /// Open (or create) the database at `path` and run all pending migrations.
    /// Idempotent across processes thanks to refinery's `refinery_schema_history`
    /// bookkeeping table.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)
            .map_err(|e| crate::Error::Storage(format!("sqlite open: {e}")))?;

        // GC-B6 — turn on incremental auto-vacuum for FREE on a brand-new
        // (zero-schema) database file. This MUST run before any other pragma
        // or write below: `auto_vacuum` only takes effect on a database that
        // has never had a page written to it, and setting `journal_mode`
        // (next) already writes page 1 — any later than this and SQLite
        // silently ignores the pragma. An existing database opened here
        // (schema already present, e.g. every kb sqlite file created before
        // this change) is deliberately left alone; `Db::retention_prune`
        // handles that guarded one-time upgrade instead, only when the
        // retention feature is actually enabled.
        let is_new_db: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
            .map_err(|e| crate::Error::Storage(format!("check new db: {e}")))?;
        if is_new_db == 0 {
            conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")
                .map_err(|e| crate::Error::Storage(format!("set auto_vacuum: {e}")))?;
        }

        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| crate::Error::Storage(format!("set WAL: {e}")))?;
        // synchronous=NORMAL is the standard durable-enough pairing with WAL:
        // it elides the per-commit WAL fsync (paid on every bulk-import /
        // reconcile / session / edge / history write) and risks only the last
        // txn on OS crash / power loss — never corruption.
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| crate::Error::Storage(format!("set synchronous: {e}")))?;
        // Larger page cache (negative = KiB, so ~16 MiB) and a modest mmap
        // window cut read cache-misses on the operational side-channel. Both
        // are best-effort tuning; open still succeeds if the platform ignores
        // mmap.
        conn.pragma_update(None, "cache_size", -16000)
            .map_err(|e| crate::Error::Storage(format!("set cache_size: {e}")))?;
        conn.pragma_update(None, "mmap_size", 268_435_456_i64)
            .map_err(|e| crate::Error::Storage(format!("set mmap_size: {e}")))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| crate::Error::Storage(format!("busy_timeout: {e}")))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| crate::Error::Storage(format!("set FK: {e}")))?;
        // PF-R1 — the prepare_cached sweep put ~74 distinct statement texts
        // in play (plus bounded-variant shapes); rusqlite's default LRU
        // capacity (16) would evict most of them per request cycle. A miss
        // is only ever a re-prepare (correctness-identical), so this is
        // sizing, not semantics.
        conn.set_prepared_statement_cache_capacity(128);

        // kb-sibling/1 — HARD schema-epoch guard, BEFORE the migration run
        // so a refused boot never writes to a volume it can't understand.
        // Refinery only ever migrates FORWARD, so an older binary opening a
        // newer volume silently no-ops here and then fails at request time
        // on the columns it doesn't know about (the 13.5 h kbc outage). Each
        // kb has its own `index.db`, so this fires per kb as it opens; the
        // error propagates out of `StorageActor::spawn` → `bring_up_kb` →
        // `serve_with_paths`, refusing the daemon's boot.
        crate::sibling::refuse_if_volume_ahead(&conn, path, schema_epoch())
            .map_err(|e| crate::Error::Storage(e.to_string()))?;

        embedded::migrations::runner()
            .run(&mut conn)
            .map_err(|e| crate::Error::Storage(format!("migration: {e}")))?;

        Ok(Self { conn })
    }

    // --- Sources -----------------------------------------------------------

    /// Idempotent UPSERT — first insert, subsequent calls update `path` if
    /// it changed (rare but possible if the user re-aliases a slug).
    pub fn upsert_source(
        &mut self,
        slug: &SourceSlug,
        path: &Path,
        added_at_unix: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sources (slug, path, added_at, paused) VALUES (?1, ?2, ?3, 0)
             ON CONFLICT(slug) DO UPDATE SET path = excluded.path",
            params![slug.as_str(), path.to_string_lossy(), added_at_unix],
        )?;
        Ok(())
    }

    pub fn list_sources(&self) -> Result<Vec<SourceRow>> {
        // GC-F3 (GC-B1 follow-up) — slug tiebreak so added_at ties (bulk
        // config adoption in one second) don't ride rowid/insertion order.
        let mut stmt = self.conn.prepare_cached(
            "SELECT slug, path, added_at, paused FROM sources ORDER BY added_at, slug",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SourceRow {
                    slug: SourceSlug::from_path(Path::new("placeholder")),
                    raw_slug: row.get::<_, String>(0)?,
                    path: PathBuf::from(row.get::<_, String>(1)?),
                    added_at_unix: row.get(2)?,
                    paused: row.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_source_paused(&mut self, slug: &SourceSlug, paused: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE sources SET paused = ?1 WHERE slug = ?2",
            params![if paused { 1 } else { 0 }, slug.as_str()],
        )?;
        Ok(())
    }

    // --- Excluded files (X2) -------------------------------------------------
    //
    // Durable per-file exclusion intent, keyed by source-relative path (the
    // `paths::doc_rel_path` form). Callers normalise the path BEFORE storing
    // (`exclusions::normalize_rel`) so lookups are byte-exact.

    /// Record an exclusion. Idempotent: returns `false` if the path was
    /// already excluded (the original `excluded_at`/`note` win).
    pub fn add_exclusion(
        &mut self,
        path: &str,
        excluded_at_unix: i64,
        note: Option<&str>,
    ) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT INTO excluded_files (path, excluded_at, note) VALUES (?1, ?2, ?3)
             ON CONFLICT(path) DO NOTHING",
            params![path, excluded_at_unix, note],
        )?;
        Ok(n > 0)
    }

    /// Remove an exclusion. Returns `false` if the path wasn't excluded.
    pub fn remove_exclusion(&mut self, path: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM excluded_files WHERE path = ?1", params![path])?;
        Ok(n > 0)
    }

    /// Every excluded path, newest exclusion first (path tiebreak for
    /// same-second bulk excludes).
    pub fn list_exclusions(&self) -> Result<Vec<ExclusionRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT path, excluded_at, note FROM excluded_files
             ORDER BY excluded_at DESC, path",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ExclusionRow {
                    path: row.get(0)?,
                    excluded_at_unix: row.get(1)?,
                    note: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // --- Runs --------------------------------------------------------------

    pub fn start_run(&mut self, source_slug: &SourceSlug, started_at_unix: i64) -> Result<RunId> {
        let id = RunId::new();
        self.conn.execute(
            "INSERT INTO index_runs (id, source_slug, started_at) VALUES (?1, ?2, ?3)",
            params![id.as_str(), source_slug.as_str(), started_at_unix],
        )?;
        Ok(id)
    }

    pub fn finish_run(
        &mut self,
        run_id: &RunId,
        ok_count: u32,
        err_count: u32,
        finished_at_unix: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE index_runs SET ok_count = ?1, err_count = ?2, finished_at = ?3
             WHERE id = ?4",
            params![ok_count, err_count, finished_at_unix, run_id.as_str()],
        )?;
        Ok(())
    }

    pub fn last_run_for_source(&self, slug: &SourceSlug) -> Result<Option<RunRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, source_slug, started_at, finished_at, ok_count, err_count
                 FROM index_runs WHERE source_slug = ?1
                 ORDER BY started_at DESC LIMIT 1",
                params![slug.as_str()],
                |row| {
                    Ok(RunRow {
                        id: row.get(0)?,
                        source_slug: row.get(1)?,
                        started_at_unix: row.get(2)?,
                        finished_at_unix: row.get(3)?,
                        ok_count: row.get(4)?,
                        err_count: row.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    // --- Errors ------------------------------------------------------------

    pub fn record_error(
        &mut self,
        kind: &str,
        source_slug: &SourceSlug,
        path: &Path,
        message: &str,
        content_hash: Option<&str>,
        created_at_unix: i64,
    ) -> Result<ErrorId> {
        let id = ErrorId::new();
        // Bump retry_count if this (path, content_hash) tuple already has an
        // open error; otherwise insert a new row.
        let bumped = self.conn.execute(
            "UPDATE errors SET retry_count = retry_count + 1, message = ?1, created_at = ?2
             WHERE path = ?3 AND COALESCE(content_hash, '') = COALESCE(?4, '')
                   AND dismissed = 0",
            params![
                message,
                created_at_unix,
                path.to_string_lossy(),
                content_hash
            ],
        )?;
        if bumped == 0 {
            self.conn.execute(
                "INSERT INTO errors (id, kind, source_slug, path, message, content_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id.as_str(),
                    kind,
                    source_slug.as_str(),
                    path.to_string_lossy(),
                    message,
                    content_hash,
                    created_at_unix,
                ],
            )?;
        }
        Ok(id)
    }

    /// Clear all open errors for `path` whose `content_hash` differs from the
    /// new one — i.e. the file changed, the parse error may no longer apply.
    /// Returns the number of rows cleared.
    pub fn clear_errors_for_path_hash(
        &mut self,
        path: &Path,
        new_content_hash: &str,
    ) -> Result<usize> {
        let rows = self.conn.execute(
            "UPDATE errors SET dismissed = 1
             WHERE path = ?1
               AND COALESCE(content_hash, '') != ?2
               AND dismissed = 0",
            params![path.to_string_lossy(), new_content_hash],
        )?;
        Ok(rows)
    }

    /// Clear every open error for `path`, regardless of content_hash. Used by
    /// the un-quarantine route to reset the retry counter so the indexer
    /// gives the artifact a fresh N attempts. Returns rows cleared.
    pub fn clear_errors_for_path(&mut self, path: &Path) -> Result<usize> {
        let rows = self.conn.execute(
            "UPDATE errors SET dismissed = 1
             WHERE path = ?1 AND dismissed = 0",
            params![path.to_string_lossy()],
        )?;
        Ok(rows)
    }

    pub fn dismiss_error(&mut self, id: &ErrorId) -> Result<()> {
        self.conn.execute(
            "UPDATE errors SET dismissed = 1 WHERE id = ?1",
            params![id.as_str()],
        )?;
        Ok(())
    }

    pub fn list_open_errors(&self) -> Result<Vec<ErrorRow>> {
        // GC-F3 (GC-B1 follow-up) — id tiebreak (asc, matching lance's
        // rank_sort convention: primary desc, id asc) so a burst of errors
        // in one second lists deterministically, not in insertion order.
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, kind, source_slug, path, message, content_hash, retry_count, created_at
             FROM errors WHERE dismissed = 0 ORDER BY created_at DESC, id ASC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ErrorRow {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    source_slug: row.get(2)?,
                    path: PathBuf::from(row.get::<_, String>(3)?),
                    message: row.get(4)?,
                    content_hash: row.get(5)?,
                    retry_count: row.get(6)?,
                    created_at_unix: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Count consecutive failures for (path, content_hash) — used by the
    /// indexer to decide when to quarantine (>= 3 per discussion topic 10).
    pub fn retry_count_for_path_hash(&self, path: &Path, content_hash: &str) -> Result<u32> {
        let count: Option<i64> = self
            .conn
            .query_row(
                "SELECT retry_count FROM errors
                 WHERE path = ?1 AND COALESCE(content_hash, '') = ?2 AND dismissed = 0
                 LIMIT 1",
                params![path.to_string_lossy(), content_hash],
                |row| row.get(0),
            )
            .optional()?;
        Ok(count.unwrap_or(0) as u32)
    }

    // --- Edges (v0.3 F1) -------------------------------------------------

    /// Replace the outbound edges for `from_id` with the supplied list.
    /// Atomic — wraps DELETE + INSERTs in a single transaction so a
    /// reindex never leaves the table in a half-rewritten state.
    /// `to_kinds` is a list of `(to_id, kind)`; deduplication of identical
    /// rows is handled by the table's primary key.
    ///
    /// Returns `true` iff the set actually changed — G8: lets the storage
    /// actor skip a redundant gallery-cache generation bump when a reindex's
    /// links are byte-identical (the common case for a content edit that
    /// touches no link). The DB dedups on the `(src, dst, kind)` constraint
    /// (hence `INSERT OR IGNORE`), so comparing as sets matches that.
    pub fn record_edges(&mut self, from_id: &str, to_kinds: &[(String, String)]) -> Result<bool> {
        let existing: std::collections::HashSet<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare_cached("SELECT dst_artifact, kind FROM edges WHERE src_artifact = ?1")?;
            let rows = stmt.query_map(params![from_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let incoming: std::collections::HashSet<(String, String)> =
            to_kinds.iter().cloned().collect();
        if existing == incoming {
            // No change — leave the table (and the gallery generation) alone.
            return Ok(false);
        }

        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM edges WHERE src_artifact = ?1",
            params![from_id],
        )?;
        if !to_kinds.is_empty() {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO edges (src_artifact, dst_artifact, kind) \
                 VALUES (?1, ?2, ?3)",
            )?;
            for (to_id, kind) in to_kinds {
                stmt.execute(params![from_id, to_id, kind])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    /// Outbound BFS from `start_id` to depth `max_depth` (clamped to
    /// `[1, 3]`). Returns one `EdgeRow` per traversed edge in BFS order;
    /// duplicate edges (same src+dst+kind) are emitted only once.
    /// Self-loops (`from == to`) are dropped.
    pub fn edges_from(&self, start_id: &str, max_depth: u32) -> Result<Vec<EdgeRow>> {
        let depth = max_depth.clamp(1, 3);
        let mut frontier: Vec<String> = vec![start_id.to_string()];
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        visited.insert(start_id.to_string());
        let mut out: Vec<EdgeRow> = Vec::new();
        let mut current_depth = 0u32;
        // Prepare once — the same statement is rebound per frontier node.
        let mut stmt = self.conn.prepare_cached(
            "SELECT src_artifact, dst_artifact, kind FROM edges WHERE src_artifact = ?1",
        )?;
        while !frontier.is_empty() && current_depth < depth {
            current_depth += 1;
            let mut next_frontier: Vec<String> = Vec::new();
            for src in &frontier {
                let rows = stmt
                    .query_map(params![src], |row| {
                        Ok(EdgeRow {
                            from_id: row.get::<_, String>(0)?,
                            to_id: row.get::<_, String>(1)?,
                            kind: row.get::<_, String>(2)?,
                            depth: current_depth,
                        })
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for edge in rows {
                    if edge.to_id == edge.from_id {
                        continue;
                    }
                    if visited.insert(edge.to_id.clone()) {
                        next_frontier.push(edge.to_id.clone());
                    }
                    out.push(edge);
                }
            }
            frontier = next_frontier;
        }
        Ok(out)
    }

    /// Bulk fetch every `kind = 'link'` edge in the kb. Each kb has its
    /// own sqlite, so all rows are intra-kb — no kb filter needed. Used
    /// by the SPA atlas view to draw lines between linked artifacts.
    /// Returns `(src, dst)` pairs in arbitrary order.
    pub fn link_pairs(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT src_artifact, dst_artifact FROM edges WHERE kind = 'link'")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Inbound edges to `dst_id` — every artifact that links *here*. One
    /// [`EdgeRow`] per incoming edge (`from_id` = the linker, `depth = 1`),
    /// self-edges excluded. Backs the "Linked from" / "Referenced in notes"
    /// panels in the SPA and the `kb notes links` / `kb backlinks` CLI verbs.
    /// The reverse of [`Self::edges_from`]'s first hop; depth-1 only (a
    /// transitive "what eventually reaches here" isn't a backlink).
    pub fn backlinks_of(&self, dst_id: &str) -> Result<Vec<EdgeRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT src_artifact, kind FROM edges \
             WHERE dst_artifact = ?1 AND src_artifact != ?1 \
             ORDER BY src_artifact",
        )?;
        let rows = stmt
            .query_map(params![dst_id], |row| {
                Ok(EdgeRow {
                    from_id: row.get::<_, String>(0)?,
                    to_id: dst_id.to_string(),
                    kind: row.get::<_, String>(1)?,
                    depth: 1,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// v0.6 B2 — backlink + outlink counts for every artifact that has
    /// at least one edge. Two GROUP-BY queries; cost is O(edges) so on
    /// a personal kb (~tens of thousands of edges) this is microsecond
    /// territory. Used by the gallery list-docs route to fill the
    /// `backlinks`/`outlinks` numbers shown on the hybrid card's
    /// glyph-strip footer.
    ///
    /// Returns a map `artifact_id → (outbound, inbound)`. Artifacts
    /// with zero edges are absent from the map; the caller treats
    /// missing entries as `(0, 0)`.
    pub fn edge_counts(&self) -> Result<std::collections::HashMap<String, (u32, u32)>> {
        let mut out: std::collections::HashMap<String, (u32, u32)> =
            std::collections::HashMap::new();
        // One statement instead of two separate GROUP-BY passes: tally
        // outbound + inbound counts per artifact in a single grouped scan
        // over a UNION ALL. The resulting `(out, in)` map is identical to
        // the two-pass form (ids seen only as src get in=0 and vice versa).
        let mut q = self.conn.prepare_cached(
            "SELECT id, SUM(out_c) AS out_count, SUM(in_c) AS in_count FROM (
                 SELECT src_artifact AS id, 1 AS out_c, 0 AS in_c FROM edges
                 UNION ALL
                 SELECT dst_artifact AS id, 0 AS out_c, 1 AS in_c FROM edges
             ) GROUP BY id",
        )?;
        for row in q.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })? {
            let (id, out_c, in_c) = row?;
            out.insert(id, (out_c as u32, in_c as u32));
        }
        Ok(out)
    }

    // --- Code refs (DCB W1.A) ---------------------------------------------
    //
    // The doc→code half of invariant #2. Two tables (V0036): `code_refs_docs`
    // is the per-document header written on EVERY extraction (so a zero-ref
    // doc is distinguishable from an unscanned one and the cursor feed has
    // something monotonic to page over), `code_refs` the ordered rows.
    //
    // Unlike `record_edges`, a code-ref write NEVER bumps the storage-actor
    // index generation — see the `RecordCodeRefs` arm in `actor.rs`.

    /// Replace this artifact's code-ref extraction with `header` + `refs`.
    /// Atomic: DELETE both tables + INSERT both, one transaction — the
    /// [`Self::record_edges`] discipline, so a reindex never leaves a
    /// half-rewritten set.
    ///
    /// Returns `true` iff the extraction actually CHANGED. The comparison
    /// deliberately EXCLUDES `extracted_at` (R4 — a wall-clock candidate
    /// differs on literally EVERY call, touched-doc or not, so including it
    /// would make every no-op reindex pass look "changed") and compares
    /// `doc_hash`, `code_rev`, the counters, `truncated`, and the ORDERED ref
    /// vector. On `false` NOTHING is written — so the stored `extracted_at`
    /// means "wall clock at the write that most recently changed this doc's
    /// extraction", which is monotonic per artifact by construction and is
    /// what makes the W1.B cursor feed both cheap (a corpus-wide `kb reindex`
    /// does not re-emit every doc onto it) and forward-safe for a keyset
    /// consumer.
    pub fn record_code_refs(
        &mut self,
        header: &CodeRefHeaderRow,
        refs: &[CodeRefRow],
    ) -> Result<bool> {
        if let Some(existing) = self.code_refs_of(&header.artifact_id)? {
            let same_header = existing.header.doc_hash == header.doc_hash
                && existing.header.code_rev == header.code_rev
                && existing.header.ref_count == header.ref_count
                && existing.header.group_count == header.group_count
                && existing.header.ungrouped_count == header.ungrouped_count
                && existing.header.truncated == header.truncated;
            if same_header && existing.refs == refs {
                // No change — leave the row (and its `extracted_at`) alone.
                return Ok(false);
            }
        }

        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM code_refs WHERE artifact_id = ?1",
            params![header.artifact_id],
        )?;
        tx.execute(
            "DELETE FROM code_refs_docs WHERE artifact_id = ?1",
            params![header.artifact_id],
        )?;
        tx.execute(
            "INSERT INTO code_refs_docs \
                 (artifact_id, doc_hash, extracted_at, code_rev, ref_count, group_count, \
                  ungrouped_count, truncated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                header.artifact_id,
                header.doc_hash,
                header.extracted_at,
                header.code_rev,
                header.ref_count,
                header.group_count,
                header.ungrouped_count,
                i64::from(header.truncated),
            ],
        )?;
        if !refs.is_empty() {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO code_refs \
                     (artifact_id, ordinal, kind, raw_text, path_hint, line_start, line_end, \
                      line_spans, symbol_container, symbol_member, context, context_tokens, \
                      group_key, group_label, group_anchor, declared) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            )?;
            for r in refs {
                stmt.execute(params![
                    header.artifact_id,
                    r.ordinal,
                    r.kind,
                    r.raw_text,
                    r.path_hint,
                    r.line_start,
                    r.line_end,
                    r.line_spans,
                    r.symbol_container,
                    r.symbol_member,
                    r.context,
                    r.context_tokens,
                    r.group_key,
                    r.group_label,
                    r.group_anchor,
                    i64::from(r.declared),
                ])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    /// One artifact's extraction, refs ordered by `ordinal`. `None` when the
    /// doc has never been scanned — deliberately distinct from a scan that
    /// found nothing, which is `Some` with an empty `refs`.
    pub fn code_refs_of(&self, artifact_id: &str) -> Result<Option<CodeRefDoc>> {
        let header: Option<CodeRefHeaderRow> = self
            .conn
            .query_row(
                "SELECT artifact_id, doc_hash, extracted_at, code_rev, ref_count, group_count, \
                        ungrouped_count, truncated \
                 FROM code_refs_docs WHERE artifact_id = ?1",
                params![artifact_id],
                Self::map_code_ref_header,
            )
            .optional()?;
        let Some(header) = header else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare_cached(
            "SELECT artifact_id, ordinal, kind, raw_text, path_hint, line_start, line_end, \
                    line_spans, symbol_container, symbol_member, context, context_tokens, \
                    group_key, group_label, group_anchor, declared \
             FROM code_refs WHERE artifact_id = ?1 ORDER BY ordinal",
        )?;
        let refs = stmt
            .query_map(params![artifact_id], Self::map_code_ref_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(_, r)| r)
            .collect();
        Ok(Some(CodeRefDoc { header, refs }))
    }

    /// Keyset page over the corpus, ASCENDING `(extracted_at, artifact_id)`.
    /// `after` is the previous page's last `(extracted_at, artifact_id)`.
    /// `with_refs = false` returns headers only (`refs` empty) for a cheap
    /// what-changed walk.
    ///
    /// The `WHERE` is spelled out rather than using sqlite's row-value form
    /// so `idx_code_refs_docs_cursor` stays usable; ref bodies are fetched
    /// with ONE `artifact_id IN (…)` statement and bucketed in Rust — never
    /// N+1.
    pub fn code_refs_feed(
        &self,
        after: Option<(i64, &str)>,
        limit: u32,
        with_refs: bool,
    ) -> Result<Vec<CodeRefDoc>> {
        let limit = limit.max(1) as i64;
        let headers: Vec<CodeRefHeaderRow> = match after {
            Some((ts, id)) => {
                let mut stmt = self.conn.prepare_cached(
                    "SELECT artifact_id, doc_hash, extracted_at, code_rev, ref_count, \
                            group_count, ungrouped_count, truncated \
                     FROM code_refs_docs \
                     WHERE extracted_at > ?1 OR (extracted_at = ?1 AND artifact_id > ?2) \
                     ORDER BY extracted_at ASC, artifact_id ASC LIMIT ?3",
                )?;
                let rows = stmt
                    .query_map(params![ts, id, limit], Self::map_code_ref_header)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            }
            None => {
                let mut stmt = self.conn.prepare_cached(
                    "SELECT artifact_id, doc_hash, extracted_at, code_rev, ref_count, \
                            group_count, ungrouped_count, truncated \
                     FROM code_refs_docs \
                     ORDER BY extracted_at ASC, artifact_id ASC LIMIT ?1",
                )?;
                let rows = stmt
                    .query_map(params![limit], Self::map_code_ref_header)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            }
        };
        let mut docs: Vec<CodeRefDoc> = headers
            .into_iter()
            .map(|header| CodeRefDoc {
                header,
                refs: Vec::new(),
            })
            .collect();

        if with_refs && !docs.is_empty() {
            let ids: Vec<String> = docs.iter().map(|d| d.header.artifact_id.clone()).collect();
            let placeholders = std::iter::repeat_n("?", ids.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT artifact_id, ordinal, kind, raw_text, path_hint, line_start, line_end, \
                        line_spans, symbol_container, symbol_member, context, context_tokens, \
                        group_key, group_label, group_anchor, declared \
                 FROM code_refs WHERE artifact_id IN ({placeholders}) ORDER BY artifact_id, ordinal"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let mut by_id: std::collections::HashMap<String, Vec<CodeRefRow>> =
                std::collections::HashMap::new();
            for row in stmt.query_map(
                rusqlite::params_from_iter(ids.iter()),
                Self::map_code_ref_row,
            )? {
                let (id, r) = row?;
                by_id.entry(id).or_default().push(r);
            }
            for d in &mut docs {
                if let Some(rows) = by_id.remove(&d.header.artifact_id) {
                    d.refs = rows;
                }
            }
        }
        Ok(docs)
    }

    /// CT-B3 — reverse lookup: every doc with a `code_refs` row whose
    /// `path_hint` EXACTLY matches `path`. Backs `?by_target=` on the feed
    /// route (`kb refs --by-target`) so "every doc citing path P" resolves
    /// to an artifact-id set in one query rather than a corpus-wide cursor
    /// walk. Unlike [`Self::code_refs_feed`] this is a complete resolution,
    /// not a page — the caller returns every match with no `next_cursor`.
    /// Same no-N+1 discipline as the feed: one `IN (artifact_id)` query for
    /// headers, one for ref bodies (skipped entirely when `with_refs` is
    /// false).
    pub fn code_refs_by_target(&self, path: &str, with_refs: bool) -> Result<Vec<CodeRefDoc>> {
        let mut id_stmt = self
            .conn
            .prepare_cached("SELECT DISTINCT artifact_id FROM code_refs WHERE path_hint = ?1")?;
        let ids: Vec<String> = id_stmt
            .query_map(params![path], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let header_sql = format!(
            "SELECT artifact_id, doc_hash, extracted_at, code_rev, ref_count, \
                    group_count, ungrouped_count, truncated \
             FROM code_refs_docs WHERE artifact_id IN ({placeholders}) \
             ORDER BY extracted_at ASC, artifact_id ASC"
        );
        let mut stmt = self.conn.prepare(&header_sql)?;
        let headers: Vec<CodeRefHeaderRow> = stmt
            .query_map(
                rusqlite::params_from_iter(ids.iter()),
                Self::map_code_ref_header,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut docs: Vec<CodeRefDoc> = headers
            .into_iter()
            .map(|header| CodeRefDoc {
                header,
                refs: Vec::new(),
            })
            .collect();

        if with_refs && !docs.is_empty() {
            let refs_sql = format!(
                "SELECT artifact_id, ordinal, kind, raw_text, path_hint, line_start, line_end, \
                        line_spans, symbol_container, symbol_member, context, context_tokens, \
                        group_key, group_label, group_anchor, declared \
                 FROM code_refs WHERE artifact_id IN ({placeholders}) ORDER BY artifact_id, ordinal"
            );
            let mut stmt = self.conn.prepare(&refs_sql)?;
            let mut by_id: std::collections::HashMap<String, Vec<CodeRefRow>> =
                std::collections::HashMap::new();
            for row in stmt.query_map(
                rusqlite::params_from_iter(ids.iter()),
                Self::map_code_ref_row,
            )? {
                let (id, r) = row?;
                by_id.entry(id).or_default().push(r);
            }
            for d in &mut docs {
                if let Some(rows) = by_id.remove(&d.header.artifact_id) {
                    d.refs = rows;
                }
            }
        }
        Ok(docs)
    }

    // --- CT-F5 corpus-health SLOs ------------------------------------------
    //
    // Three cheap aggregate reads (one per sqlite-backed indicator) plus the
    // append-only snapshot log. Every one of these is a COUNT over a table
    // that already exists — the SLO surface adds no scan the corpus wasn't
    // already paying for elsewhere, and it never writes anything a reader
    // could mistake for truth about a document.

    /// CT-F5 — `(total_hints, path_shaped_hints)` over this kb's `code_refs`.
    ///
    /// The numerator is [`crate::slo::CODEREF_PATH_SHAPED_KINDS`] — the
    /// closed set of PATH-shaped kinds, single-sourced there and bound into
    /// the `IN (…)` below so the documented definition and the predicate that
    /// implements it cannot drift. The denominator is every extracted row:
    /// symbol/issue/external hints are real extractions and belong in the
    /// total, just never in the numerator. See
    /// `kb_core::slo::SloKey::CoderefResolutionPct` for the ONE definition
    /// this implements.
    ///
    /// NEVER a kb-code call (invariant #2): kb has no checkout, so this is a
    /// structural shape count, not a resolution result.
    pub fn code_ref_shape_counts(&self) -> Result<(u64, u64)> {
        let kinds = crate::slo::CODEREF_PATH_SHAPED_KINDS;
        let placeholders = vec!["?"; kinds.len()].join(",");
        let sql = format!(
            "SELECT COUNT(*), \
                    COALESCE(SUM(CASE WHEN kind IN ({placeholders}) THEN 1 ELSE 0 END), 0) \
             FROM code_refs"
        );
        let (total, shaped): (i64, i64) =
            self.conn
                .query_row(&sql, rusqlite::params_from_iter(kinds.iter()), |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
        Ok((i64_as_u64_sat(total), i64_as_u64_sat(shaped)))
    }

    /// CT-F5 — the newest `sessions.started_at` in this kb, or `None` when the
    /// table is empty. Backs the capture-freshness indicator. `started_at`
    /// (not `ended_at`, not an artifact mtime) is the session's own clock and
    /// the column every newest-capture ordering already keys on.
    pub fn sessions_newest_started_at(&self) -> Result<Option<i64>> {
        let v: Option<i64> =
            self.conn
                .query_row("SELECT MAX(started_at) FROM sessions", [], |r| r.get(0))?;
        Ok(v)
    }

    /// CT-F5 — which of `session_ids` have a `sessions` row here. Presence
    /// only: the orphan indicator needs a set-membership answer, not a row, so
    /// this is a one-column projection rather than a
    /// [`Self::sessions_get_many`] call that would map 31 columns per hit.
    /// Multi-capture (invariant #11) is irrelevant to presence — one row or
    /// fifty, the id is present either way — so there is no newest-capture
    /// tie-break here.
    pub fn session_ids_present(&self, session_ids: &[String]) -> Result<Vec<String>> {
        if session_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; session_ids.len()].join(",");
        let sql = format!(
            "SELECT DISTINCT session_id FROM sessions WHERE session_id IN ({placeholders})"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(session_ids.iter()), |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// CT-F5 — record one capture's CT-A3 recall parse census (V0039).
    ///
    /// A plain UPDATE, deliberately NOT folded into [`Self::sessions_upsert`]'s
    /// column list: the census is derived by a DIFFERENT hook
    /// (`memory-recall-ledger`, which runs immediately after
    /// `session-capture` in `default_hooks()`'s sequential order), and keeping
    /// it out of the upsert's `ON CONFLICT DO UPDATE SET` means a later
    /// re-capture of the same artifact can never clobber it back to NULL.
    ///
    /// Returns the number of rows updated — `0` when no `sessions` row exists
    /// for this artifact (its capture hook failed). That is a silent no-op by
    /// design: the census stays NULL, and the indicator reports `unknown`
    /// rather than inventing a zero.
    pub fn sessions_set_recall_census(
        &mut self,
        artifact_id: &str,
        marker_parsed: u32,
        fallback_parsed: u32,
        failed: u32,
    ) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE sessions SET recall_marker_parsed = ?2, recall_fallback_parsed = ?3, \
                                 recall_failed = ?4 \
             WHERE artifact_id = ?1",
            params![
                artifact_id,
                i64::from(marker_parsed),
                i64::from(fallback_parsed),
                i64::from(failed),
            ],
        )?;
        Ok(n)
    }

    /// CT-F5 — `(marker_parsed, fallback_parsed, failed, censused_captures)`
    /// summed over this kb's censused captures.
    ///
    /// Scoped to the NEWEST capture per session via [`newest_capture_pred`]
    /// (invariant #11): a long session is captured at every Stop, so summing
    /// every row would weight one session's injections once per capture and
    /// tilt the corpus-wide rate toward whatever the chattiest session did.
    ///
    /// Rows whose census is NULL (captures predating V0039 — never
    /// backfilled) are excluded from BOTH the sums and the capture count, so a
    /// pre-census corpus answers `(0,0,0,0)` and the indicator reads
    /// `unknown`, not a fabricated 0%.
    pub fn sessions_recall_census_totals(&self) -> Result<(u64, u64, u64, u64)> {
        // PF-R1 (V0040) — self-referential (this row against its OWN
        // session_id group), so it reads the materialized flag directly
        // instead of going through `newest_capture_pred`'s subquery.
        let sql = "SELECT COALESCE(SUM(recall_marker_parsed), 0), \
                    COALESCE(SUM(recall_fallback_parsed), 0), \
                    COALESCE(SUM(recall_failed), 0), \
                    COUNT(*) \
             FROM sessions \
             WHERE recall_marker_parsed IS NOT NULL AND is_newest = 1";
        let (m, f, x, n): (i64, i64, i64, i64) = self.conn.query_row(sql, [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        Ok((
            i64_as_u64_sat(m),
            i64_as_u64_sat(f),
            i64_as_u64_sat(x),
            i64_as_u64_sat(n),
        ))
    }

    /// CT-F5 — append one `kb slo snapshot` run to the append-only log: one
    /// row per indicator, all sharing `taken_at_unix` (that shared instant IS
    /// the run's identity — see the V0039 header for why there is no run id).
    ///
    /// APPEND-ONLY, literally: this is the only write path to `slo_snapshots`,
    /// and it only ever INSERTs. There is deliberately NO dedup-on-unchanged
    /// (the `atlas_snapshots` `coord_hash` skip would be wrong here — a
    /// flat-lining indicator is itself the signal, so every run must land) and
    /// no prune. One transaction so a run is all-or-nothing: a half-written
    /// run would read as a corpus that briefly lost three indicators.
    pub fn slo_snapshot_append(
        &mut self,
        taken_at_unix: i64,
        indicators: &[crate::slo::SloIndicator],
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO slo_snapshots (taken_at_unix, indicator, value, target, status) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for i in indicators {
                stmt.execute(params![
                    taken_at_unix,
                    i.key,
                    i.value,
                    i.target,
                    i.status.as_str(),
                ])?;
            }
        }
        tx.commit()?;
        Ok(indicators.len())
    }

    /// CT-F5 — the newest `limit` snapshot rows, newest-first. Matches
    /// `idx_slo_snapshots_taken`'s `(taken_at_unix DESC, id DESC)` so runs
    /// inside the same second page deterministically.
    pub fn slo_snapshots_list(&self, limit: u32) -> Result<Vec<SloSnapshotRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, taken_at_unix, indicator, value, target, status \
             FROM slo_snapshots ORDER BY taken_at_unix DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(SloSnapshotRow {
                    id: r.get(0)?,
                    taken_at_unix: r.get(1)?,
                    indicator: r.get(2)?,
                    value: r.get(3)?,
                    target: r.get(4)?,
                    status: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn map_code_ref_header(row: &rusqlite::Row<'_>) -> rusqlite::Result<CodeRefHeaderRow> {
        Ok(CodeRefHeaderRow {
            artifact_id: row.get(0)?,
            doc_hash: row.get(1)?,
            extracted_at: row.get(2)?,
            code_rev: row.get(3)?,
            ref_count: row.get::<_, i64>(4)? as u32,
            group_count: row.get::<_, i64>(5)? as u32,
            ungrouped_count: row.get::<_, i64>(6)? as u32,
            truncated: row.get::<_, i64>(7)? != 0,
        })
    }

    fn map_code_ref_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, CodeRefRow)> {
        Ok((
            row.get::<_, String>(0)?,
            CodeRefRow {
                ordinal: row.get::<_, i64>(1)? as u32,
                kind: row.get(2)?,
                raw_text: row.get(3)?,
                path_hint: row.get(4)?,
                line_start: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                line_end: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                line_spans: row.get(7)?,
                symbol_container: row.get(8)?,
                symbol_member: row.get(9)?,
                context: row.get(10)?,
                context_tokens: row.get(11)?,
                group_key: row.get(12)?,
                group_label: row.get(13)?,
                group_anchor: row.get(14)?,
                declared: row.get::<_, i64>(15)? != 0,
            },
        ))
    }

    // --- Atlas labels (W1.B) ---------------------------------------------
    //
    // Deterministic c-TF-IDF cluster labels (`kb_core::atlas_labels`),
    // recomputed whole-kb on every atlas recompute/recluster. One row per
    // (cluster, rank); `set_atlas_labels` replaces the ENTIRE table in one
    // transaction — same replace-the-set shape as `record_edges`, but
    // whole-kb rather than per-`from_id` since a recompute reassigns every
    // cluster at once. Writing this table must NOT bump the storage-actor
    // index generation (invariant #15) — it never touches the lance
    // row-set, exactly like `update_atlas`.

    /// Replace every `atlas_labels` row with `labels`. Idempotent: an empty
    /// `labels` slice just clears the table (e.g. an atlas recompute that
    /// found zero clusterable docs).
    pub fn set_atlas_labels(&mut self, labels: &[AtlasLabelRow]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM atlas_labels", [])?;
        if !labels.is_empty() {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO atlas_labels (cluster, rank, term, tf, ft, score, computed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for l in labels {
                stmt.execute(params![
                    l.cluster,
                    l.rank,
                    l.term,
                    l.tf,
                    l.ft,
                    l.score,
                    l.computed_at
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every stored label, ordered `(cluster, rank)` — the same order the
    /// `PRIMARY KEY` enforces, made explicit so callers don't depend on
    /// SQLite's incidental physical order.
    pub fn atlas_labels(&self) -> Result<Vec<AtlasLabelRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT cluster, rank, term, tf, ft, score, computed_at \
             FROM atlas_labels ORDER BY cluster, rank",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(AtlasLabelRow {
                    cluster: row.get(0)?,
                    rank: row.get(1)?,
                    term: row.get(2)?,
                    tf: row.get(3)?,
                    ft: row.get(4)?,
                    score: row.get(5)?,
                    computed_at: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- Atlas frames / time-lapse (W3 T-a, V0028) ------------------------
    //
    // One `atlas_snapshots` row per atlas recompute/recluster whose
    // coordinates actually changed, plus its `atlas_snapshot_points`. See
    // `migrations/V0028__atlas_snapshots.sql` for why this is sqlite and not
    // lance columns (one column set = one layout), and for the two honesty
    // caveats every consumer needs: frames start EMPTY and cluster ids
    // RENUMBER between frames (`kmeans_lloyd` seeds by array position and
    // reseeds empty clusters randomly), so colours must be remapped per
    // frame.
    //
    // W3 T-d amendment: `provenance = 'reconstructed'` frames
    // (`atlas::backfill_reconstructed_frames`) can seed an empty timeline —
    // today's embeddings laid out over each past cut point's doc subset.
    // The migration header's "a historical backfill is IMPOSSIBLE" still
    // holds for TRUE history (nothing retains a past layout or embedding);
    // a reconstruction is a different, weaker claim and is labelled as one
    // on every surface (see [`FrameProvenance`]).
    //
    // Writing a frame must NOT bump the storage-actor index generation
    // (invariant #15) — same rule as `update_atlas`/`set_atlas_labels`.

    /// Insert one atlas frame + its points in ONE transaction, then prune to
    /// the newest [`DEFAULT_ATLAS_FRAMES_KEEP`] frames.
    ///
    /// Deduped on `coord_hash` ([`atlas_frame_coord_hash`]): if the computed
    /// hash equals the NEWEST stored frame's hash, nothing is written and
    /// `Ok(None)` is returned — a recompute that changed nothing is not a
    /// frame, and the time-lapse would otherwise fill with identical stills
    /// on every idle reindex. Otherwise the new row id is returned.
    ///
    /// `point_count`/`cluster_count` are derived from `points`, never
    /// caller-supplied, so they can't drift from the stored rows.
    pub fn atlas_frame_insert(
        &mut self,
        frame: &NewAtlasFrame,
        points: &[AtlasFramePoint],
    ) -> Result<Option<i64>> {
        let coord_hash = atlas_frame_coord_hash(points);
        let newest: Option<String> = self
            .conn
            .prepare_cached(
                "SELECT coord_hash FROM atlas_snapshots \
                 ORDER BY created_at_unix DESC, id DESC LIMIT 1",
            )?
            .query_row([], |r| r.get::<_, String>(0))
            .optional()?;
        if newest.as_deref() == Some(coord_hash.as_str()) {
            return Ok(None);
        }

        let cluster_count = points
            .iter()
            .map(|p| p.cluster)
            .collect::<std::collections::BTreeSet<_>>()
            .len() as i64;

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO atlas_snapshots \
                (created_at_unix, point_count, cluster_count, layout, coord_hash, provenance) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                frame.created_at_unix,
                points.len() as i64,
                cluster_count,
                frame.layout,
                coord_hash,
                frame.provenance.as_str(),
            ],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO atlas_snapshot_points \
                    (snapshot_id, artifact_id, x, y, cluster) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for p in points {
                stmt.execute(params![
                    id,
                    p.artifact_id,
                    p.x as f64,
                    p.y as f64,
                    p.cluster
                ])?;
            }
        }
        // Prune INSIDE the same transaction: a frame write either lands
        // fully-pruned or not at all, so the retention bound can't be left
        // violated by a crash between the two statements.
        prune_atlas_frames(&tx, DEFAULT_ATLAS_FRAMES_KEEP as i64)?;
        tx.commit()?;
        Ok(Some(id))
    }

    /// Frame metadata, newest first, capped at `limit` (no points — the
    /// timeline list stays cheap).
    pub fn atlas_frames(&self, limit: u32) -> Result<Vec<AtlasFrameRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, created_at_unix, point_count, cluster_count, layout, coord_hash, provenance \
             FROM atlas_snapshots ORDER BY created_at_unix DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(AtlasFrameRow {
                    id: row.get(0)?,
                    created_at_unix: row.get(1)?,
                    point_count: row.get(2)?,
                    cluster_count: row.get(3)?,
                    layout: row.get(4)?,
                    coord_hash: row.get(5)?,
                    provenance: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every point of one frame, ordered by `artifact_id` — the SAME order
    /// [`atlas_frame_coord_hash`] hashes, so a caller can re-derive the hash
    /// from a read-back frame and verify it.
    pub fn atlas_frame_points(&self, snapshot_id: i64) -> Result<Vec<AtlasFramePoint>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT artifact_id, x, y, cluster FROM atlas_snapshot_points \
             WHERE snapshot_id = ?1 ORDER BY artifact_id",
        )?;
        let rows = stmt
            .query_map(params![snapshot_id], |row| {
                Ok(AtlasFramePoint {
                    artifact_id: row.get(0)?,
                    x: row.get::<_, f64>(1)? as f32,
                    y: row.get::<_, f64>(2)? as f32,
                    cluster: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Drop all but the newest `keep` frames. Returns frames deleted (their
    /// points go with them via `ON DELETE CASCADE` — `foreign_keys` is ON in
    /// [`Db::open`]). Exposed for an explicit operator prune; the insert
    /// path already prunes itself.
    pub fn atlas_frames_prune(&mut self, keep: u32) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let n = prune_atlas_frames(&tx, keep as i64)?;
        tx.commit()?;
        Ok(n)
    }

    // --- History (v0.6+ H1) ---------------------------------------------

    /// Begin or resume a visit for `artifact_id`. The 30-minute gap rule:
    /// if the most recent open for this artifact was bumped within the
    /// last 30 minutes, treat this as the same visit — bump `updated_at`
    /// on the existing row and return the prior scroll. Otherwise
    /// INSERT a new row with scroll_y = 0.
    ///
    /// `is_new_visit` in the result lets the HTTP layer emit the
    /// `history.recorded` SSE only on actual inserts; bumps are silent
    /// so the SPA gallery doesn't refresh on every tab reload.
    ///
    /// `source` (GC-B5) is only stamped on a brand-new row — a bump of an
    /// existing visit within the 30-min gap leaves the original opener's
    /// `source` untouched (append-only, invariant #8: a resume doesn't
    /// retroactively relabel who started the visit).
    pub fn history_record_open(
        &mut self,
        artifact_id: &str,
        now_unix: i64,
        source: Option<&str>,
        user: &str,
    ) -> Result<OpenResult> {
        const VISIT_GAP_SECS: i64 = 30 * 60;
        // Visit-gap dedup is per (artifact_id, user) — two users within
        // 30 min get SEPARATE rows (v0.34 X1).
        let existing: Option<(i64, i64, i64)> = self
            .conn
            .query_row(
                "SELECT id, scroll_y, updated_at FROM history
                 WHERE kind = 'open' AND artifact_id = ?1 AND \"user\" = ?2
                 ORDER BY started_at DESC LIMIT 1",
                params![artifact_id, user],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((id, scroll_y, updated_at)) = existing {
            if now_unix - updated_at < VISIT_GAP_SECS {
                self.conn.execute(
                    "UPDATE history SET updated_at = ?1 WHERE id = ?2",
                    params![now_unix, id],
                )?;
                return Ok(OpenResult {
                    id,
                    scroll_y,
                    is_new_visit: false,
                });
            }
        }
        self.conn.execute(
            "INSERT INTO history (kind, artifact_id, scroll_y, scroll_max, started_at, updated_at, source, \"user\")
             VALUES ('open', ?1, 0, 0, ?2, ?2, ?3, ?4)",
            params![artifact_id, now_unix, source, user],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(OpenResult {
            id,
            scroll_y: 0,
            is_new_visit: true,
        })
    }

    /// UPDATE scroll position on an existing open visit. Returns the
    /// number of rows updated — 0 means the visit_id doesn't exist or
    /// isn't an open-kind row (caller should 404).
    pub fn history_update_scroll(
        &mut self,
        visit_id: i64,
        scroll_y: i64,
        scroll_max: i64,
        now_unix: i64,
    ) -> Result<usize> {
        // scroll_y_max is the per-visit high-water mark — bumped via
        // SQLite's max() builtin so the SPA's "fully read" chip stays
        // sticky once the user reaches the bottom even if they later
        // scroll back up. scroll_y itself still tracks the current
        // position for resume.
        let rows = self.conn.execute(
            "UPDATE history SET scroll_y = ?1, scroll_max = ?2,
                                scroll_y_max = max(scroll_y_max, ?1),
                                updated_at = ?3
             WHERE id = ?4 AND kind = 'open'",
            params![scroll_y, scroll_max, now_unix, visit_id],
        )?;
        Ok(rows)
    }

    /// Record a search query. If the most recent `search` row holds the
    /// identical query within the last 5 seconds, bump its `updated_at`
    /// and return its id (deduplicates accidental double-fires from
    /// SPA Enter + click-result). Otherwise INSERT.
    pub fn history_record_search(&mut self, query: &str, now_unix: i64, user: &str) -> Result<i64> {
        const DEDUP_WINDOW_SECS: i64 = 5;
        // Dedup window is per (query, user) (v0.34 X1).
        let existing: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT id, updated_at FROM history
                 WHERE kind = 'search' AND query = ?1 AND \"user\" = ?2
                 ORDER BY started_at DESC LIMIT 1",
                params![query, user],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((id, updated_at)) = existing {
            if now_unix - updated_at < DEDUP_WINDOW_SECS {
                self.conn.execute(
                    "UPDATE history SET updated_at = ?1 WHERE id = ?2",
                    params![now_unix, id],
                )?;
                return Ok(id);
            }
        }
        self.conn.execute(
            "INSERT INTO history (kind, query, started_at, updated_at, \"user\")
             VALUES ('search', ?1, ?2, ?2, ?3)",
            params![query, now_unix, user],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Record a comment-author event. Always inserts a new row (the
    /// route handler decides whether the upstream save was a "new
    /// comment" vs an edit/resolve and only calls this for the former).
    pub fn history_record_comment(
        &mut self,
        artifact_id: &str,
        comment_id: &str,
        now_unix: i64,
        user: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO history (kind, artifact_id, comment_id, started_at, updated_at, \"user\")
             VALUES ('comment', ?1, ?2, ?3, ?3, ?4)",
            params![artifact_id, comment_id, now_unix, user],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Newest-first list of history rows. `limit` clamps the page size;
    /// `before_unix` (exclusive cursor on `started_at`) and `kind_filter`
    /// are optional. Pass `kind_filter = None` for the full timeline.
    pub fn history_list(
        &self,
        limit: u32,
        before_unix: Option<i64>,
        kind_filter: Option<&str>,
        user: Option<&str>,
    ) -> Result<Vec<HistoryRow>> {
        let mut sql = String::from(
            "SELECT id, kind, artifact_id, query, comment_id, scroll_y, scroll_max,
                    scroll_y_max, started_at, updated_at, source, \"user\"
             FROM history WHERE 1=1",
        );
        if before_unix.is_some() {
            sql.push_str(" AND started_at < ?");
        }
        if kind_filter.is_some() {
            sql.push_str(" AND kind = ?");
        }
        if user.is_some() {
            sql.push_str(" AND \"user\" = ?");
        }
        // Tie-break same-second inserts by rowid DESC so the list is
        // deterministically newest-first even when several writes land
        // within the same unix-second boundary (the route is a hot path
        // for the SPA gallery; tests rely on stable ordering).
        sql.push_str(" ORDER BY started_at DESC, id DESC LIMIT ?");
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let limit_i64 = limit as i64;
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::new();
        if let Some(b) = before_unix.as_ref() {
            params_vec.push(b);
        }
        if let Some(k) = kind_filter.as_ref() {
            params_vec.push(k);
        }
        if let Some(u) = user.as_ref() {
            params_vec.push(u);
        }
        params_vec.push(&limit_i64);
        let rows = stmt
            .query_map(params_vec.as_slice(), |row| {
                Ok(HistoryRow {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    artifact_id: row.get(2)?,
                    query: row.get(3)?,
                    comment_id: row.get(4)?,
                    scroll_y: row.get(5)?,
                    scroll_max: row.get(6)?,
                    scroll_y_max: row.get(7)?,
                    started_at_unix: row.get(8)?,
                    updated_at_unix: row.get(9)?,
                    source: row.get(10)?,
                    user: row.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // --- Reading progress (RP-track, V0014) ----------------------------

    /// UPSERT the per-section reading rows for one visit. The client sends
    /// CUMULATIVE per-visit dwell_ms/enters (seeded on a 30-min resume from
    /// `reading_state_for_visit`), so every numeric field is max()-merged —
    /// a re-sent beacon, or a remount that restored prior state, can never
    /// double-count or regress. Returns the number of section rows written.
    /// The FK on visit_id means a missing visit errors; the route gates with
    /// `reading_set_active` first.
    pub fn reading_upsert_sections(
        &mut self,
        visit_id: i64,
        artifact_id: &str,
        sections: &[SectionDwell],
        now_unix: i64,
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO reading_sections
                   (visit_id, artifact_id, section_id, section_idx, section_text, level,
                    words, content_px, dwell_ms, enters, first_at, last_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
                 ON CONFLICT(visit_id, section_id) DO UPDATE SET
                   section_idx  = excluded.section_idx,
                   section_text = excluded.section_text,
                   level        = excluded.level,
                   words        = max(words, excluded.words),
                   content_px   = max(content_px, excluded.content_px),
                   dwell_ms     = max(dwell_ms, excluded.dwell_ms),
                   enters       = max(enters, excluded.enters),
                   last_at      = excluded.last_at",
            )?;
            for s in sections {
                stmt.execute(params![
                    visit_id,
                    artifact_id,
                    s.section_id,
                    s.section_idx,
                    s.section_text,
                    s.level,
                    s.words,
                    s.content_px,
                    s.dwell_ms,
                    s.enters,
                    now_unix,
                ])?;
            }
        }
        tx.commit()?;
        Ok(sections.len())
    }

    /// Set the per-visit active-reading roll-up. `active_ms` is max()-merged
    /// (the client value is cumulative-across-the-visit via seed-on-open, so
    /// max is correct); `last_section` (the stop-point) is updated when
    /// provided (COALESCE keeps a known stop-point when a beacon carries no
    /// active section). Returns rows-affected — 0 means the visit is unknown
    /// or not an open row. This is the visit-existence gate the reading
    /// endpoint calls BEFORE upserting sections.
    pub fn reading_set_active(
        &mut self,
        visit_id: i64,
        active_ms: i64,
        last_section: Option<&str>,
        now_unix: i64,
    ) -> Result<usize> {
        let rows = self.conn.execute(
            "UPDATE history SET active_ms = max(active_ms, ?1),
                                last_section = COALESCE(?2, last_section),
                                updated_at = ?3
             WHERE id = ?4 AND kind = 'open'",
            params![active_ms, last_section, now_unix, visit_id],
        )?;
        Ok(rows)
    }

    /// Resume baseline for a visit (seed-on-open, F6): its prior `active_ms`,
    /// stop-point, and per-section cumulative dwell/enters, so the iframe
    /// runtime restores its counters on a 30-min resume and its cumulative
    /// beacons never regress. Empty/zero for a brand-new visit.
    pub fn reading_state_for_visit(&self, visit_id: i64) -> Result<ReadingResume> {
        let head: Option<(i64, Option<String>)> = self
            .conn
            .query_row(
                "SELECT active_ms, last_section FROM history WHERE id = ?1 AND kind = 'open'",
                params![visit_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (active_ms, last_section) = head.unwrap_or((0, None));
        let mut stmt = self.conn.prepare_cached(
            "SELECT section_id, dwell_ms, enters FROM reading_sections WHERE visit_id = ?1",
        )?;
        let sections = stmt
            .query_map(params![visit_id], |row| {
                Ok(ReadingSeedSection {
                    section_id: row.get(0)?,
                    dwell_ms: row.get(1)?,
                    enters: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ReadingResume {
            active_ms,
            last_section,
            sections,
        })
    }

    /// All inputs for one artifact's reading summary: every section row
    /// (across visits) + the per-visit roll-ups. Fed straight into
    /// `crate::reading::summarize`.
    #[allow(clippy::type_complexity)]
    /// Section rows + visit roll-ups for an artifact's reading summary.
    /// When `user` is `Some`, only that user's open visits (and their
    /// section rows) are included — v0.34 Y1 per-user reading summary.
    /// `None` = all users (legacy / team-narration paths).
    pub fn reading_inputs_for_artifact(
        &self,
        artifact_id: &str,
        user: Option<&str>,
    ) -> Result<(
        Vec<crate::reading::ReadingSectionRow>,
        Vec<crate::reading::VisitRollup>,
    )> {
        let (ssql, vsql) = if user.is_some() {
            (
                "SELECT rs.visit_id, rs.section_id, rs.section_idx, rs.section_text, rs.level,
                        rs.words, rs.content_px, rs.dwell_ms, rs.enters, rs.first_at, rs.last_at
                 FROM reading_sections rs
                 INNER JOIN history h ON h.id = rs.visit_id
                 WHERE rs.artifact_id = ?1 AND h.kind = 'open' AND h.\"user\" = ?2
                 ORDER BY rs.section_idx, rs.visit_id",
                "SELECT id, started_at, updated_at, scroll_y_max, scroll_max, active_ms, last_section
                 FROM history WHERE kind = 'open' AND artifact_id = ?1 AND \"user\" = ?2
                 ORDER BY started_at",
            )
        } else {
            (
                "SELECT visit_id, section_id, section_idx, section_text, level, words, content_px,
                        dwell_ms, enters, first_at, last_at
                 FROM reading_sections WHERE artifact_id = ?1 ORDER BY section_idx, visit_id",
                "SELECT id, started_at, updated_at, scroll_y_max, scroll_max, active_ms, last_section
                 FROM history WHERE kind = 'open' AND artifact_id = ?1 ORDER BY started_at",
            )
        };
        let mut sstmt = self.conn.prepare_cached(ssql)?;
        let sections = if let Some(u) = user {
            sstmt
                .query_map(params![artifact_id, u], |row| {
                    Ok(crate::reading::ReadingSectionRow {
                        visit_id: row.get(0)?,
                        section_id: row.get(1)?,
                        section_idx: row.get(2)?,
                        section_text: row.get(3)?,
                        level: row.get(4)?,
                        words: row.get(5)?,
                        content_px: row.get(6)?,
                        dwell_ms: row.get(7)?,
                        enters: row.get(8)?,
                        first_at: row.get(9)?,
                        last_at: row.get(10)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            sstmt
                .query_map(params![artifact_id], |row| {
                    Ok(crate::reading::ReadingSectionRow {
                        visit_id: row.get(0)?,
                        section_id: row.get(1)?,
                        section_idx: row.get(2)?,
                        section_text: row.get(3)?,
                        level: row.get(4)?,
                        words: row.get(5)?,
                        content_px: row.get(6)?,
                        dwell_ms: row.get(7)?,
                        enters: row.get(8)?,
                        first_at: row.get(9)?,
                        last_at: row.get(10)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut vstmt = self.conn.prepare_cached(vsql)?;
        let visits = if let Some(u) = user {
            vstmt
                .query_map(params![artifact_id, u], |row| {
                    Ok(crate::reading::VisitRollup {
                        visit_id: row.get(0)?,
                        started_at: row.get(1)?,
                        updated_at: row.get(2)?,
                        scroll_y_max: row.get(3)?,
                        scroll_max: row.get(4)?,
                        active_ms: row.get(5)?,
                        last_section: row.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            vstmt
                .query_map(params![artifact_id], |row| {
                    Ok(crate::reading::VisitRollup {
                        visit_id: row.get(0)?,
                        started_at: row.get(1)?,
                        updated_at: row.get(2)?,
                        scroll_y_max: row.get(3)?,
                        scroll_max: row.get(4)?,
                        active_ms: row.get(5)?,
                        last_section: row.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok((sections, visits))
    }

    /// Batched variant of [`Self::reading_inputs_for_artifact`]: the
    /// section rows + visit roll-ups for a SET of artifact ids in two
    /// grouped `IN (...)` queries (instead of two per id), keyed by
    /// artifact id. An id with no reading rows is simply absent — callers
    /// derive from empty (invariant #25: read-state stays DERIVED; only the
    /// fetch pattern batches). The per-artifact orderings match the
    /// single-id query (`section_idx, visit_id` for sections; `started_at`
    /// for visits) because the batched sort leads with `artifact_id`.
    #[allow(clippy::type_complexity)]
    /// Batched variant of [`Self::reading_inputs_for_artifact`]. When
    /// `user` is `Some`, only that user's visits/sections are returned
    /// (v0.34 Y1 — list enrichment is per-requester).
    pub fn reading_inputs_for_artifacts(
        &self,
        artifact_ids: &[String],
        user: Option<&str>,
    ) -> Result<
        std::collections::HashMap<
            String,
            (
                Vec<crate::reading::ReadingSectionRow>,
                Vec<crate::reading::VisitRollup>,
            ),
        >,
    > {
        let mut map: std::collections::HashMap<
            String,
            (
                Vec<crate::reading::ReadingSectionRow>,
                Vec<crate::reading::VisitRollup>,
            ),
        > = std::collections::HashMap::new();
        if artifact_ids.is_empty() {
            return Ok(map);
        }
        let placeholders = vec!["?"; artifact_ids.len()].join(",");
        let ssql = if user.is_some() {
            format!(
                "SELECT rs.artifact_id, rs.visit_id, rs.section_id, rs.section_idx, rs.section_text,
                        rs.level, rs.words, rs.content_px, rs.dwell_ms, rs.enters, rs.first_at, rs.last_at
                 FROM reading_sections rs
                 INNER JOIN history h ON h.id = rs.visit_id
                 WHERE rs.artifact_id IN ({placeholders}) AND h.kind = 'open' AND h.\"user\" = ?
                 ORDER BY rs.artifact_id, rs.section_idx, rs.visit_id"
            )
        } else {
            format!(
                "SELECT artifact_id, visit_id, section_id, section_idx, section_text, level, words,
                        content_px, dwell_ms, enters, first_at, last_at
                 FROM reading_sections WHERE artifact_id IN ({placeholders})
                 ORDER BY artifact_id, section_idx, visit_id"
            )
        };
        let mut sstmt = self.conn.prepare(&ssql)?;
        let srows = {
            let mut binds: Vec<&dyn rusqlite::ToSql> = artifact_ids
                .iter()
                .map(|s| s as &dyn rusqlite::ToSql)
                .collect();
            if let Some(u) = user.as_ref() {
                binds.push(u);
            }
            sstmt.query_map(binds.as_slice(), |row| {
                let aid: String = row.get(0)?;
                Ok((
                    aid,
                    crate::reading::ReadingSectionRow {
                        visit_id: row.get(1)?,
                        section_id: row.get(2)?,
                        section_idx: row.get(3)?,
                        section_text: row.get(4)?,
                        level: row.get(5)?,
                        words: row.get(6)?,
                        content_px: row.get(7)?,
                        dwell_ms: row.get(8)?,
                        enters: row.get(9)?,
                        first_at: row.get(10)?,
                        last_at: row.get(11)?,
                    },
                ))
            })?
        };
        for r in srows {
            let (aid, sec) = r?;
            map.entry(aid).or_default().0.push(sec);
        }
        let vsql = if user.is_some() {
            format!(
                "SELECT artifact_id, id, started_at, updated_at, scroll_y_max, scroll_max,
                        active_ms, last_section
                 FROM history WHERE kind = 'open' AND artifact_id IN ({placeholders})
                   AND \"user\" = ?
                 ORDER BY artifact_id, started_at"
            )
        } else {
            format!(
                "SELECT artifact_id, id, started_at, updated_at, scroll_y_max, scroll_max,
                        active_ms, last_section
                 FROM history WHERE kind = 'open' AND artifact_id IN ({placeholders})
                 ORDER BY artifact_id, started_at"
            )
        };
        let mut vstmt = self.conn.prepare(&vsql)?;
        let vrows = {
            let mut binds: Vec<&dyn rusqlite::ToSql> = artifact_ids
                .iter()
                .map(|s| s as &dyn rusqlite::ToSql)
                .collect();
            if let Some(u) = user.as_ref() {
                binds.push(u);
            }
            vstmt.query_map(binds.as_slice(), |row| {
                let aid: String = row.get(0)?;
                Ok((
                    aid,
                    crate::reading::VisitRollup {
                        visit_id: row.get(1)?,
                        started_at: row.get(2)?,
                        updated_at: row.get(3)?,
                        scroll_y_max: row.get(4)?,
                        scroll_max: row.get(5)?,
                        active_ms: row.get(6)?,
                        last_section: row.get(7)?,
                    },
                ))
            })?
        };
        for r in vrows {
            let (aid, v) = r?;
            map.entry(aid).or_default().1.push(v);
        }
        Ok(map)
    }

    /// v0.34 Y1 — distinct non-empty `history.user` values observed in
    /// this kb (for `GET /api/users` observed set). Sorted ascending.
    pub fn history_distinct_users(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT \"user\" FROM history
             WHERE \"user\" IS NOT NULL AND \"user\" != ''
             ORDER BY \"user\" ASC",
        )?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Cheap latest-visit reading state for recall enrichment:
    /// `(completion_pct, last_section, last_read_at_unix)` from the
    /// most-recent visit, or `None` if the artifact was never opened.
    /// One indexed row — safe on the recall hot path.
    pub fn reading_latest_for_artifact(
        &self,
        artifact_id: &str,
        user: &str,
    ) -> Result<Option<(u8, Option<String>, i64)>> {
        // Per-user: recall enrichment is the requester's reading (v0.34 X1).
        let row: Option<(i64, i64, Option<String>, i64)> = self
            .conn
            .query_row(
                "SELECT scroll_y_max, scroll_max, last_section, updated_at FROM history
                 WHERE kind = 'open' AND artifact_id = ?1 AND \"user\" = ?2
                 ORDER BY started_at DESC, id DESC LIMIT 1",
                params![artifact_id, user],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        Ok(row.map(|(ymax, smax, last_section, updated_at)| {
            let pct = if smax > 0 {
                (ymax as f64 / smax as f64 * 100.0)
                    .round()
                    .clamp(0.0, 100.0) as u8
            } else {
                0
            };
            (pct, last_section, updated_at)
        }))
    }

    /// Batched variant of [`Self::reading_latest_for_artifact`]: the
    /// newest-open-visit `(completion_pct, last_section, last_read_at_unix)`
    /// for a SET of artifact ids, in one `ROW_NUMBER()` window pass, keyed by
    /// artifact id. Used by recall enrichment so a page of hits is one query
    /// per kb instead of one round-trip per hit. An id that was never opened
    /// is simply absent from the map (callers leave the fields unset), matching
    /// the single-id `None`. Empty input → empty map (no query). Ids are bound
    /// parameters; the recall limit keeps the set well under SQLite's cap.
    #[allow(clippy::type_complexity)]
    pub fn reading_latest_for_ids(
        &self,
        artifact_ids: &[String],
        user: &str,
    ) -> Result<std::collections::HashMap<String, (u8, Option<String>, i64)>> {
        // Per-user batch (v0.34 X1) — same rationale as single-id form.
        let mut map: std::collections::HashMap<String, (u8, Option<String>, i64)> =
            std::collections::HashMap::new();
        if artifact_ids.is_empty() {
            return Ok(map);
        }
        let placeholders = vec!["?"; artifact_ids.len()].join(",");
        let sql = format!(
            "SELECT artifact_id, scroll_y_max, scroll_max, last_section, updated_at FROM (
                 SELECT artifact_id, scroll_y_max, scroll_max, last_section, updated_at,
                        ROW_NUMBER() OVER (
                            PARTITION BY artifact_id
                            ORDER BY started_at DESC, id DESC) AS rn
                 FROM history
                 WHERE kind = 'open' AND \"user\" = ? AND artifact_id IN ({placeholders})
             ) WHERE rn = 1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut binds: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(1 + artifact_ids.len());
        binds.push(&user);
        for id in artifact_ids {
            binds.push(id);
        }
        let rows = stmt.query_map(binds.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        for r in rows {
            let (id, ymax, smax, last_section, updated_at) = r?;
            let pct = if smax > 0 {
                (ymax as f64 / smax as f64 * 100.0)
                    .round()
                    .clamp(0.0, 100.0) as u8
            } else {
                0
            };
            map.insert(id, (pct, last_section, updated_at));
        }
        Ok(map)
    }

    /// Batched read-state rollup over the entire `history` table — one
    /// entry per artifact that has an open visit OR a list `read_override`.
    /// Backs the search read-state facet and the `opened` / `progress`
    /// sorts (Q-track). A `ROW_NUMBER()` window picks each artifact's
    /// newest open visit in one indexed pass (the partial index
    /// `idx_history_artifact_open` covers the filter + ordering), reusing
    /// the scroll math of [`Self::reading_latest_for_artifact`]; list
    /// overrides are then overlaid (`read` beats `unread`, override wins
    /// absolutely — matching [`crate::lists::derive_read_state`]). Pure
    /// reads: never mutates, so it can't bump the index generation
    /// (invariants #8 / #15 / #19).
    pub fn reading_rollup(
        &self,
        user: &str,
    ) -> Result<std::collections::HashMap<String, crate::reading::ReadRollup>> {
        self.reading_rollup_inner(None, user)
    }

    /// Q-track — [`Self::reading_rollup`] scoped to a candidate id set. The
    /// search page only consumes rollup entries for the surviving hits (≤ the
    /// filtered pool), so constraining both the window scan and the
    /// `list_entries` override overlay to those ids keeps the cost
    /// proportional to the page instead of the whole append-only
    /// (unboundedly-growing, invariant #8) `history` table. The output is
    /// byte-identical to filtering the full rollup down to `ids`: an id absent
    /// here reads as `Unread` at the call site, exactly as it would if omitted
    /// from the full map. An empty slice short-circuits to an empty map. Ids
    /// are bound parameters (like [`Self::reading_inputs_for_artifacts`]); the
    /// caller keeps the set within SQLite's variable cap (the search filtered
    /// pool does).
    pub fn reading_rollup_for_ids(
        &self,
        ids: &[String],
        user: &str,
    ) -> Result<std::collections::HashMap<String, crate::reading::ReadRollup>> {
        self.reading_rollup_inner(Some(ids), user)
    }

    /// Shared body for [`Self::reading_rollup`] (`ids = None`, whole table) and
    /// [`Self::reading_rollup_for_ids`] (`ids = Some(&[…])`, scoped). When an
    /// id set is given, both the window scan and the override overlay are
    /// constrained to it via a bound-parameter `IN (…)`; an empty set returns
    /// an empty map without querying.
    fn reading_rollup_inner(
        &self,
        ids: Option<&[String]>,
        user: &str,
    ) -> Result<std::collections::HashMap<String, crate::reading::ReadRollup>> {
        use crate::lists::ReadState;
        use crate::reading::{ReadRollup, FULLY_READ_PCT};
        let mut map: std::collections::HashMap<String, ReadRollup> =
            std::collections::HashMap::new();

        // Optional artifact_id IN (…) fragment. `None` → whole table.
        let scoped = match ids {
            Some(ids) => {
                if ids.is_empty() {
                    return Ok(map);
                }
                ids
            }
            None => &[][..],
        };
        let hist_in = if ids.is_some() {
            format!(
                " AND artifact_id IN ({})",
                vec!["?"; scoped.len()].join(",")
            )
        } else {
            String::new()
        };
        let ov_in = if ids.is_some() {
            format!(
                " AND le.artifact_id IN ({})",
                vec!["?"; scoped.len()].join(",")
            )
        } else {
            String::new()
        };

        // user first, then optional id set — WHERE user = ? BEFORE the
        // ROW_NUMBER PARTITION so two users never share a rollup row.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT artifact_id, scroll_y_max, scroll_max, started_at FROM (
                 SELECT artifact_id, scroll_y_max, scroll_max, started_at,
                        ROW_NUMBER() OVER (
                            PARTITION BY artifact_id
                            ORDER BY started_at DESC, id DESC) AS rn
                 FROM history
                 WHERE kind = 'open' AND artifact_id IS NOT NULL
                   AND \"user\" = ?{hist_in}
             ) WHERE rn = 1"
        ))?;
        let mut binds: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(1 + scoped.len());
        binds.push(&user);
        for id in scoped {
            binds.push(id);
        }
        let rows = stmt.query_map(binds.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        for r in rows {
            let (id, ymax, smax, started_at) = r?;
            let pct = if smax > 0 {
                (ymax as f64 / smax as f64 * 100.0)
                    .round()
                    .clamp(0.0, 100.0) as u8
            } else {
                0
            };
            let state = if pct >= FULLY_READ_PCT {
                ReadState::Read
            } else {
                ReadState::InProgress
            };
            map.insert(
                id,
                ReadRollup {
                    last_opened_unix: Some(started_at),
                    completion_pct: pct,
                    state,
                },
            );
        }

        // Per-user overrides from list_entry_user_state (NOT the frozen
        // list_entries.read_override column — load-bearing for ?read=).
        let mut ostmt = self.conn.prepare(&format!(
            "SELECT le.artifact_id, us.read_override
             FROM list_entry_user_state us
             JOIN list_entries le ON le.id = us.entry_id
             WHERE us.\"user\" = ? AND us.read_override IS NOT NULL{ov_in}"
        ))?;
        let mut obinds: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(1 + scoped.len());
        obinds.push(&user);
        for id in scoped {
            obinds.push(id);
        }
        let orows = ostmt.query_map(obinds.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut overrides: std::collections::HashMap<String, ReadState> =
            std::collections::HashMap::new();
        for r in orows {
            let (id, ov) = r?;
            let st = match ov.as_str() {
                "read" => ReadState::Read,
                "unread" => ReadState::Unread,
                _ => continue,
            };
            overrides
                .entry(id)
                .and_modify(|cur| {
                    if st == ReadState::Read {
                        *cur = ReadState::Read;
                    }
                })
                .or_insert(st);
        }
        for (id, st) in overrides {
            map.entry(id)
                .and_modify(|r| r.state = st)
                .or_insert(ReadRollup {
                    last_opened_unix: None,
                    completion_pct: if st == ReadState::Read { 100 } else { 0 },
                    state: st,
                });
        }

        Ok(map)
    }

    /// Open-visits whose `started_at` falls in `[from_unix, to_unix]` —
    /// "what the human read during a session window" (session readings,
    /// the read-counterpart to transcript `touches`). Newest-first, capped.
    ///
    /// All-users by design (v0.34 X1): sessions readings narrate the whole
    /// team's reading of a window, not one requester's.
    pub fn history_opens_in_window(
        &self,
        from_unix: i64,
        to_unix: i64,
        limit: u32,
    ) -> Result<Vec<HistoryRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, kind, artifact_id, query, comment_id, scroll_y, scroll_max,
                    scroll_y_max, started_at, updated_at, source, \"user\"
             FROM history WHERE kind = 'open' AND started_at >= ?1 AND started_at <= ?2
             ORDER BY started_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![from_unix, to_unix, limit as i64], |row| {
                Ok(HistoryRow {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    artifact_id: row.get(2)?,
                    query: row.get(3)?,
                    comment_id: row.get(4)?,
                    scroll_y: row.get(5)?,
                    scroll_max: row.get(6)?,
                    scroll_y_max: row.get(7)?,
                    started_at_unix: row.get(8)?,
                    updated_at_unix: row.get(9)?,
                    source: row.get(10)?,
                    user: row.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// R8 — comment-creation events whose `started_at` falls in
    /// `[from_unix, to_unix]`: "what comments were RAISED during this session's
    /// window". Backed by the `kind='comment'` rows `history_record_comment`
    /// writes on every new comment (so `artifact_id` + `comment_id` are
    /// populated). Newest-first, capped. kb-comments/1 has no resolved_at, so
    /// only *raised* is computable — never *resolved*.
    pub fn history_comments_in_window(
        &self,
        from_unix: i64,
        to_unix: i64,
        limit: u32,
    ) -> Result<Vec<HistoryRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, kind, artifact_id, query, comment_id, scroll_y, scroll_max,
                    scroll_y_max, started_at, updated_at, source, \"user\"
             FROM history WHERE kind = 'comment' AND started_at >= ?1 AND started_at <= ?2
             ORDER BY started_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![from_unix, to_unix, limit as i64], |row| {
                Ok(HistoryRow {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    artifact_id: row.get(2)?,
                    query: row.get(3)?,
                    comment_id: row.get(4)?,
                    scroll_y: row.get(5)?,
                    scroll_max: row.get(6)?,
                    scroll_y_max: row.get(7)?,
                    started_at_unix: row.get(8)?,
                    updated_at_unix: row.get(9)?,
                    source: row.get(10)?,
                    user: row.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// W2.10 — per-day, per-kind event counts in `[from_unix, to_unix]`, for
    /// the gallery's activity-calendar density grid. One `GROUP BY` over the
    /// existing `idx_history_started` index (bounded by the caller's window,
    /// same shape as `history_opens_in_window`/`history_comments_in_window`
    /// above). Days are UTC calendar days — sqlite's `unixepoch` modifier has
    /// no timezone concept, matching the `echoes` route's UTC convention; the
    /// route/SPA label the result as UTC rather than reinterpreting it
    /// locally. Ordered by day then kind so the route's pivot into
    /// `{day, opens, searches, comments}` is deterministic.
    pub fn history_counts_by_day(&self, from_unix: i64, to_unix: i64) -> Result<Vec<DayKindCount>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT strftime('%Y-%m-%d', started_at, 'unixepoch') AS day, kind, COUNT(*)
             FROM history WHERE started_at >= ?1 AND started_at <= ?2
             GROUP BY day, kind
             ORDER BY day ASC, kind ASC",
        )?;
        let rows = stmt
            .query_map(params![from_unix, to_unix], |row| {
                Ok(DayKindCount {
                    day: row.get(0)?,
                    kind: row.get(1)?,
                    count: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// S5 admin — wipe the history table (and its cascaded reading rows).
    /// Returns the number of rows deleted. This is the operator's
    /// unconditional "start over" button — it wipes ALL rows regardless of
    /// age. `kb.toml`-driven *age-based* retention (R3, `retention_prune`)
    /// is the complementary path that drops only rows OLDER than a window.
    /// The explicit reading_sections delete (vs relying on ON DELETE
    /// CASCADE) keeps the returned count correct and is robust if FK
    /// enforcement is ever opened without the pragma.
    pub fn history_purge(&mut self) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let r = tx.execute("DELETE FROM reading_sections", [])?;
        let h = tx.execute("DELETE FROM history", [])?;
        tx.commit()?;
        Ok(r + h)
    }

    /// R3 (v0.24) — opt-in age-based retention prune. Deletes rows OLDER
    /// than the configured windows, in ONE transaction, and returns the
    /// total rows removed. Each window is `Option<seconds>`; `None` prunes
    /// nothing for that table, so an unset window is a no-op.
    ///
    /// Two independent deletes:
    ///   1. `reading_sections.last_at < now - reading_max_age` — prunes
    ///      section rows more aggressively than their parent visit (only
    ///      when a reading window is set); the parent history row survives.
    ///   2. `history.started_at < now - history_max_age` — prunes old
    ///      visits; its `ON DELETE CASCADE` removes any surviving child
    ///      sections. The child rows are deleted EXPLICITLY first (same as
    ///      `history_purge`) so the returned count includes them (cascade
    ///      rows aren't counted by `execute()`).
    ///
    /// This is invariant #8's documented retention exception: `history` is
    /// append-only EXCEPT this coarse delete of OLD rows — a delete, never
    /// an edit, and it never touches a live/recent row. It deliberately does
    /// NOT manage the R2-cascade tables (sessions/edges/…); those are pruned
    /// per-artifact on delete, not by age (and `edges` has no timestamp to
    /// prune on).
    pub fn retention_prune(
        &mut self,
        now_unix: i64,
        history_max_age_secs: Option<i64>,
        reading_max_age_secs: Option<i64>,
    ) -> Result<usize> {
        let enabled = history_max_age_secs.is_some() || reading_max_age_secs.is_some();
        // Guarded one-time upgrade FIRST — must run outside any transaction
        // (VACUUM errors inside one) and before the delete, so an existing
        // pre-retention database is on incremental auto-vacuum by the time
        // we ask it to reclaim space below.
        if enabled {
            self.ensure_incremental_auto_vacuum()?;
        }
        let tx = self.conn.transaction()?;
        let mut deleted = 0usize;
        // (1) Independent reading_sections window — keep the parent visit.
        if let Some(age) = reading_max_age_secs {
            let cutoff = now_unix.saturating_sub(age);
            deleted += tx.execute(
                "DELETE FROM reading_sections WHERE last_at < ?1",
                params![cutoff],
            )?;
        }
        // (2) History window — explicit child delete FIRST (correct count),
        // then the parent rows (whose cascade would drop any stragglers).
        if let Some(age) = history_max_age_secs {
            let cutoff = now_unix.saturating_sub(age);
            deleted += tx.execute(
                "DELETE FROM reading_sections
                   WHERE visit_id IN (SELECT id FROM history WHERE started_at < ?1)",
                params![cutoff],
            )?;
            deleted += tx.execute("DELETE FROM history WHERE started_at < ?1", params![cutoff])?;
        }
        tx.commit()?;
        // Reclaim SOME of the freed pages right away, bounded so a huge
        // one-time backlog can't stall this prune tick indefinitely — see
        // `RetentionSection::INCREMENTAL_VACUUM_PAGES`. A no-op (per SQLite)
        // when auto_vacuum isn't INCREMENTAL/FULL, e.g. this db was skipped
        // by the size-gated upgrade above.
        if enabled {
            // NOTE: this must be `pragma()` (which drains every result row),
            // NOT `pragma_update()` (which is `execute_batch` — ONE
            // `sqlite3_step()`). `incremental_vacuum` frees exactly one page
            // per step, so `pragma_update` would silently reclaim only a
            // single page no matter how large `N` is.
            self.conn.pragma(
                None,
                "incremental_vacuum",
                RetentionSection::INCREMENTAL_VACUUM_PAGES,
                |_row| Ok(()),
            )?;
            // `incremental_vacuum` only shrinks the LOGICAL page count; under
            // WAL (every kb db — see `Db::open`) the OS-visible main file
            // doesn't actually truncate until a checkpoint runs, so without
            // this the freed pages sit invisible on disk until SQLite's own
            // opportunistic auto-checkpoint gets around to it. TRUNCATE mode
            // is best-effort (falls back to a partial checkpoint if a reader
            // holds an old snapshot open) — never errors the prune run.
            let _ = self
                .conn
                .pragma(None, "wal_checkpoint", "TRUNCATE", |_row| Ok(()));
        }
        Ok(deleted)
    }

    /// GC-B6 — one-time upgrade of an EXISTING database to incremental
    /// auto-vacuum, run at the top of every enabled `retention_prune` call.
    /// SQLite only honours an `auto_vacuum` change on an EMPTY database
    /// without a full `VACUUM` rewrite (`Db::open` already does the free
    /// no-VACUUM version for brand-new files) — so a database that already
    /// had a schema before retention was ever configured needs an explicit,
    /// one-time `VACUUM` to pick it up.
    ///
    /// A blind migration would run this on EVERY kb at EVERY startup
    /// (refinery migrations can't run `VACUUM` inside their transaction
    /// anyway); instead this only fires from the prune path, only when
    /// retention is actually enabled, only once (idempotent — a DB already
    /// in INCREMENTAL/FULL mode short-circuits on the first pragma read),
    /// and only once the file has grown past
    /// `RetentionSection::AUTO_VACUUM_UPGRADE_MIN_BYTES` — a brand-new or
    /// lightly used kb never pays for a `VACUUM` it doesn't need yet.
    fn ensure_incremental_auto_vacuum(&mut self) -> Result<()> {
        let mode: i64 = self
            .conn
            .pragma_query_value(None, "auto_vacuum", |row| row.get(0))?;
        if mode != 0 {
            return Ok(()); // already FULL(1) or INCREMENTAL(2) — nothing to do.
        }
        let page_count: i64 = self
            .conn
            .pragma_query_value(None, "page_count", |row| row.get(0))?;
        let page_size: i64 = self
            .conn
            .pragma_query_value(None, "page_size", |row| row.get(0))?;
        let size_bytes = page_count.saturating_mul(page_size);
        if size_bytes < RetentionSection::AUTO_VACUUM_UPGRADE_MIN_BYTES {
            return Ok(());
        }
        tracing::info!(
            size_bytes,
            "retention: one-time VACUUM to upgrade sqlite db to incremental auto_vacuum"
        );
        self.conn
            .pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }

    /// S5 admin — wipe every transient table for a kb drop. Lance is
    /// emptied separately via `Storage::delete_all_rows()`; this is
    /// the sqlite side. Intentionally LEAVES `shares` intact because
    /// those rows track external deployments (Cloudflare/GitHub) whose
    /// teardown requires the `kb share revoke` flow; orphaning the
    /// rows would lose track of live URLs. `.review/*.json` files are
    /// also untouched (they live on disk and re-attach on reindex).
    /// Returns the count of rows removed across tables.
    pub fn purge_kb_data(&mut self) -> Result<usize> {
        let tx = self.conn.transaction()?;
        // reading_sections before history so the explicit count is right
        // (ON DELETE CASCADE rows aren't counted by execute()).
        let r = tx.execute("DELETE FROM reading_sections", [])?;
        let h = tx.execute("DELETE FROM history", [])?;
        let e = tx.execute("DELETE FROM errors", [])?;
        let ed = tx.execute("DELETE FROM edges", [])?;
        tx.commit()?;
        Ok(r + h + e + ed)
    }

    // --- Shares (kb share registry, V0004) ------------------------------

    /// Insert — or, on `--update`, upsert — a share row keyed by `name`.
    /// On conflict the mutable fields + `updated_at` are rewritten while
    /// `created_at` is preserved, so re-deploying to the same project
    /// keeps the original creation time.
    pub fn shares_insert(&mut self, row: &ShareRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO shares (name, target, host, deployed_url, gate, cf_account_id,
                                 pages_project, cf_deployment_id, access_app_id,
                                 access_policy_id, github_repo, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(name) DO UPDATE SET
                 target = excluded.target,
                 host = excluded.host,
                 deployed_url = excluded.deployed_url,
                 gate = excluded.gate,
                 cf_account_id = excluded.cf_account_id,
                 pages_project = excluded.pages_project,
                 cf_deployment_id = excluded.cf_deployment_id,
                 access_app_id = excluded.access_app_id,
                 access_policy_id = excluded.access_policy_id,
                 github_repo = excluded.github_repo,
                 updated_at = excluded.updated_at",
            params![
                row.name,
                row.target,
                row.host,
                row.deployed_url,
                row.gate,
                row.cf_account_id,
                row.pages_project,
                row.cf_deployment_id,
                row.access_app_id,
                row.access_policy_id,
                row.github_repo,
                row.created_at_unix,
                row.updated_at_unix,
            ],
        )?;
        Ok(())
    }

    /// All shares, newest-first.
    pub fn shares_list(&self) -> Result<Vec<ShareRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT name, target, host, deployed_url, gate, cf_account_id, pages_project,
                    cf_deployment_id, access_app_id, access_policy_id, github_repo,
                    created_at, updated_at
             FROM shares ORDER BY created_at DESC, name ASC",
        )?;
        let rows = stmt
            .query_map([], Self::row_to_share)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Look up a single share by `name` (the primary key).
    pub fn shares_get(&self, name: &str) -> Result<Option<ShareRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT name, target, host, deployed_url, gate, cf_account_id, pages_project,
                        cf_deployment_id, access_app_id, access_policy_id, github_repo,
                        created_at, updated_at
                 FROM shares WHERE name = ?1",
                params![name],
                Self::row_to_share,
            )
            .optional()?;
        Ok(row)
    }

    /// Most-recent share for a given `target`. Drives `--update`, which
    /// re-deploys to the recorded project rather than creating a new one.
    pub fn shares_get_by_target(&self, target: &str) -> Result<Option<ShareRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT name, target, host, deployed_url, gate, cf_account_id, pages_project,
                        cf_deployment_id, access_app_id, access_policy_id, github_repo,
                        created_at, updated_at
                 FROM shares WHERE target = ?1
                 ORDER BY created_at DESC LIMIT 1",
                params![target],
                Self::row_to_share,
            )
            .optional()?;
        Ok(row)
    }

    /// Delete a share row by `name` (revoke). Returns rows affected
    /// (0 = no such share).
    pub fn shares_delete(&mut self, name: &str) -> Result<usize> {
        let rows = self
            .conn
            .execute("DELETE FROM shares WHERE name = ?1", params![name])?;
        Ok(rows)
    }

    fn row_to_share(row: &rusqlite::Row) -> rusqlite::Result<ShareRow> {
        Ok(ShareRow {
            name: row.get(0)?,
            target: row.get(1)?,
            host: row.get(2)?,
            deployed_url: row.get(3)?,
            gate: row.get(4)?,
            cf_account_id: row.get(5)?,
            pages_project: row.get(6)?,
            cf_deployment_id: row.get(7)?,
            access_app_id: row.get(8)?,
            access_policy_id: row.get(9)?,
            github_repo: row.get(10)?,
            created_at_unix: row.get(11)?,
            updated_at_unix: row.get(12)?,
        })
    }

    // --- Corkboard (anchor bookmarks, V0005) ----------------------------
    //
    // Naming: external surface ("anchors") in the SPA + HTTP routes; the
    // internal table + functions use `corkboard` to avoid colliding with
    // the unrelated stale-comment-anchor sidecar (kb_core::anchors).

    // --- doc_first_seen (v0.33 X2) ---------------------------------------

    /// Record the first time kb indexed `artifact_id`. INSERT OR IGNORE —
    /// reindex never rewrites the timestamp. Returns true when a row was
    /// inserted.
    pub fn first_seen_insert_ignore(&mut self, artifact_id: &str, ts: i64) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO doc_first_seen (artifact_id, first_indexed_unix)
             VALUES (?1, ?2)",
            params![artifact_id, ts],
        )?;
        Ok(n > 0)
    }

    /// Batched lookup of first-indexed timestamps for a page of ids.
    /// Missing ids are simply absent from the map (caller treats as None).
    pub fn first_seen_for_ids(
        &self,
        ids: &[String],
    ) -> Result<std::collections::HashMap<String, i64>> {
        let mut map = std::collections::HashMap::new();
        if ids.is_empty() {
            return Ok(map);
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT artifact_id, first_indexed_unix FROM doc_first_seen \
             WHERE artifact_id IN ({placeholders})"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (id, ts) = row?;
            map.insert(id, ts);
        }
        Ok(map)
    }

    /// v0.33 X2 — pure coalesce used by the bring-up seed: prefer fs btime
    /// (`created_unix`), else mtime, else last indexed_at. Mirrors the
    /// pre-X2 "created" sort's effective anchor so upgrading does not
    /// reshuffle existing corpora.
    pub fn first_seen_coalesce_ts(
        created_unix: Option<i64>,
        mtime_unix: Option<i64>,
        indexed_at_unix: Option<i64>,
    ) -> Option<i64> {
        created_unix.or(mtime_unix).or(indexed_at_unix)
    }

    /// Bring-up seed: bulk INSERT OR IGNORE of `(id, ts)` pairs in one
    /// transaction. Used when `doc_first_seen` is empty and the corpus
    /// already has lance rows (preserves current created-sort order via
    /// coalesce(created, mtime, indexed_at)).
    pub fn first_seen_seed(&mut self, rows: &[(String, i64)]) -> Result<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut n = 0usize;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO doc_first_seen (artifact_id, first_indexed_unix)
                 VALUES (?1, ?2)",
            )?;
            for (id, ts) in rows {
                n += stmt.execute(params![id, ts])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// True when `doc_first_seen` has zero rows (bring-up seed gate).
    pub fn first_seen_is_empty(&self) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM doc_first_seen", [], |r| r.get(0))?;
        Ok(n == 0)
    }

    // --- v0.34 X1 — identity backfill + per-user list overrides ----------

    /// Idempotent, config-aware startup pass (V0034). Migrations cannot
    /// know the operator name, so they leave `history.user = ''` and keep
    /// legacy `list_entries.read_override` values in place. At boot
    /// (phase Y) the daemon calls this once config is in hand:
    ///
    /// 1. If `identity_backfill_done` marker row exists → return `Ok(0)`.
    /// 2. `UPDATE history SET user = operator WHERE user = ''`
    /// 3. Copy non-null legacy overrides into `list_entry_user_state`
    ///    for the operator (`INSERT OR IGNORE` — never overwrites a
    ///    fresher per-user row).
    /// 4. INSERT the marker row.
    ///
    /// Returns rows touched (history updates + override inserts). A second
    /// run touches 0 **via the marker** — not by re-deriving from the
    /// frozen legacy column (which would resurrect overrides a user has
    /// since CLEARED). Does NOT bump the index generation (sqlite
    /// side-channel only; invariants #8 / #15).
    pub fn identity_backfill(&mut self, operator: &str, now_unix: i64) -> Result<u64> {
        let tx = self.conn.transaction()?;
        let done: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM identity_backfill_done WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if done.is_some() {
            return Ok(0);
        }
        let hist = tx.execute(
            "UPDATE history SET \"user\" = ?1 WHERE \"user\" = ''",
            params![operator],
        )? as u64;
        let ov = tx.execute(
            "INSERT OR IGNORE INTO list_entry_user_state
                (entry_id, \"user\", read_override, updated_at)
             SELECT id, ?1, read_override, updated_at
             FROM list_entries
             WHERE read_override IS NOT NULL",
            params![operator],
        )? as u64;
        tx.execute(
            "INSERT INTO identity_backfill_done (id, done_at) VALUES (1, ?1)",
            params![now_unix],
        )?;
        tx.commit()?;
        Ok(hist + ov)
    }

    /// Set (or clear) a per-user list-entry read override in
    /// `list_entry_user_state`. `override_ = None` deletes the row.
    /// Does NOT touch the frozen `list_entries.read_override` column.
    /// Never bumps the index generation (invariant #25).
    pub fn list_entry_set_user_override(
        &mut self,
        entry_id: &str,
        user: &str,
        override_: Option<&str>,
        now_unix: i64,
    ) -> Result<()> {
        let exists: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM list_entries WHERE id = ?1",
                params![entry_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(crate::Error::NotFound(format!("list entry {entry_id}")));
        }
        match override_ {
            None => {
                self.conn.execute(
                    "DELETE FROM list_entry_user_state
                     WHERE entry_id = ?1 AND \"user\" = ?2",
                    params![entry_id, user],
                )?;
            }
            Some(ov) => {
                self.conn.execute(
                    "INSERT INTO list_entry_user_state
                        (entry_id, \"user\", read_override, updated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(entry_id, \"user\") DO UPDATE SET
                        read_override = excluded.read_override,
                        updated_at = excluded.updated_at",
                    params![entry_id, user, ov, now_unix],
                )?;
            }
        }
        Ok(())
    }

    /// Per-user read overrides for every entry in `list_id`, keyed by
    /// entry id. Missing keys mean "no override for this user". Pure
    /// SELECT — safe on the read lane.
    pub fn list_entry_user_overrides_for_list(
        &self,
        list_id: &str,
        user: &str,
    ) -> Result<std::collections::HashMap<String, String>> {
        let mut map = std::collections::HashMap::new();
        let mut stmt = self.conn.prepare_cached(
            "SELECT us.entry_id, us.read_override
             FROM list_entry_user_state us
             JOIN list_entries le ON le.id = us.entry_id
             WHERE le.list_id = ?1 AND us.\"user\" = ?2
               AND us.read_override IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![list_id, user], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for r in rows {
            let (id, ov) = r?;
            map.insert(id, ov);
        }
        Ok(map)
    }

    /// Pin an artifact to this kb's corkboard. Idempotent — returns
    /// `true` if a fresh row was inserted, `false` if the artifact was
    /// already on the corkboard (in which case `created_at` is left
    /// untouched, so the original pin time wins).
    pub fn corkboard_add(&mut self, artifact_id: &str, now_unix: i64) -> Result<bool> {
        let inserted = self.conn.execute(
            "INSERT INTO corkboard (artifact_id, created_at) VALUES (?1, ?2)
             ON CONFLICT(artifact_id) DO NOTHING",
            params![artifact_id, now_unix],
        )?;
        Ok(inserted > 0)
    }

    /// Remove an artifact from this kb's corkboard. Returns `true` if a
    /// row was deleted, `false` if it wasn't on the corkboard to begin
    /// with (idempotent).
    pub fn corkboard_remove(&mut self, artifact_id: &str) -> Result<bool> {
        let deleted = self.conn.execute(
            "DELETE FROM corkboard WHERE artifact_id = ?1",
            params![artifact_id],
        )?;
        Ok(deleted > 0)
    }

    /// True iff the artifact is on the corkboard. Used by `list_docs`'s
    /// `anchored` projection extension (K3+).
    pub fn corkboard_contains(&self, artifact_id: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM corkboard WHERE artifact_id = ?1",
            params![artifact_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// All corkboard entries, newest-first.
    pub fn corkboard_list(&self) -> Result<Vec<CorkboardRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT artifact_id, created_at FROM corkboard ORDER BY created_at DESC, artifact_id ASC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(CorkboardRow {
                    artifact_id: r.get(0)?,
                    created_at_unix: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Row count — cheap; used by the Header anchor-pill badge to skip
    /// a full list fetch.
    pub fn corkboard_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM corkboard", [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    // --- Pinned memories (V0006) ---------------------------------------
    //
    // Same shape as corkboard — pinned memories survive the recall
    // route's DecayPolicy floor at every level. SPA writes pin/unpin via
    // /api/kb/{kb}/memories/{id}/pin.

    pub fn pinned_memory_add(&mut self, artifact_id: &str, now_unix: i64) -> Result<bool> {
        let inserted = self.conn.execute(
            "INSERT INTO pinned_memories (artifact_id, pinned_at) VALUES (?1, ?2)
             ON CONFLICT(artifact_id) DO NOTHING",
            params![artifact_id, now_unix],
        )?;
        Ok(inserted > 0)
    }

    pub fn pinned_memory_remove(&mut self, artifact_id: &str) -> Result<bool> {
        let deleted = self.conn.execute(
            "DELETE FROM pinned_memories WHERE artifact_id = ?1",
            params![artifact_id],
        )?;
        Ok(deleted > 0)
    }

    /// All pinned ids — cheap fetch the recall route uses to decorate
    /// the kb's hits. Returns a HashSet for O(1) per-hit lookup.
    pub fn pinned_memories_set(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT artifact_id FROM pinned_memories")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
        Ok(rows)
    }

    // --- Memory links (V0010 / V0011) -----------------------------------
    //
    // Many-to-many edges between memories (in this memory-scoped kb) and
    // the normal kbs that should recall them. Sentinel `linked_kb = '*'`
    // means "visible everywhere" (global). See V0010 + V0011 SQL
    // headers and the L-track plan for the full mental model.

    /// All `linked_kb` rows for a memory. Includes the `*` sentinel if
    /// the memory is global. Returns an empty Vec when the memory has
    /// no links at all (which only happens between seeding pass and
    /// first link, or for a memory whose user-driven UI ops cleared
    /// every link).
    pub fn memory_links_for(&self, artifact_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT linked_kb FROM memory_links WHERE artifact_id = ?1 ORDER BY linked_kb ASC",
        )?;
        let rows = stmt
            .query_map(params![artifact_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Add a single link. INSERT OR IGNORE — returns `true` if the row
    /// was new, `false` if it already existed. `linked_kb` is the
    /// target kb name (or the `*` sentinel for global).
    pub fn memory_link_add(
        &mut self,
        artifact_id: &str,
        linked_kb: &str,
        now_unix: i64,
    ) -> Result<bool> {
        let inserted = self.conn.execute(
            "INSERT INTO memory_links (artifact_id, linked_kb, created_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(artifact_id, linked_kb) DO NOTHING",
            params![artifact_id, linked_kb, now_unix],
        )?;
        Ok(inserted > 0)
    }

    /// Remove a single link. Returns `true` if a row was deleted.
    pub fn memory_link_remove(&mut self, artifact_id: &str, linked_kb: &str) -> Result<bool> {
        let deleted = self.conn.execute(
            "DELETE FROM memory_links WHERE artifact_id = ?1 AND linked_kb = ?2",
            params![artifact_id, linked_kb],
        )?;
        Ok(deleted > 0)
    }

    /// Atomic replace of the memory's link set. Drops every existing
    /// row for this `artifact_id` then inserts the new set. When
    /// `global` is true, the `*` sentinel is added regardless of
    /// `linked_kbs`. Empty `linked_kbs` + `global = false` means the
    /// memory becomes unlinked (invisible to recall).
    pub fn memory_links_replace(
        &mut self,
        artifact_id: &str,
        linked_kbs: &[String],
        global: bool,
        now_unix: i64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM memory_links WHERE artifact_id = ?1",
            params![artifact_id],
        )?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO memory_links (artifact_id, linked_kb, created_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(artifact_id, linked_kb) DO NOTHING",
            )?;
            if global {
                stmt.execute(params![artifact_id, "*", now_unix])?;
            }
            for kb in linked_kbs {
                stmt.execute(params![artifact_id, kb, now_unix])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Drop every link row for this memory. Paired with the lance
    /// delete in `process_delete` so a removed memory file leaves no
    /// dangling edges. Also clears the seeded tombstone so a
    /// recreated artifact (same id) re-seeds on its next index.
    pub fn memory_links_remove_all(&mut self, artifact_id: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM memory_links WHERE artifact_id = ?1",
            params![artifact_id],
        )?;
        tx.execute(
            "DELETE FROM memory_links_seeded WHERE artifact_id = ?1",
            params![artifact_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Bulk fetch — every (artifact_id → {linked_kb}) edge in this kb.
    /// Called once per recall fan-out (one extra sqlite scan per
    /// memory corpus) so the recall route can do the membership
    /// filter inline without per-hit roundtrips.
    pub fn memory_links_all(
        &self,
    ) -> Result<std::collections::HashMap<String, std::collections::HashSet<String>>> {
        use std::collections::{HashMap, HashSet};
        let mut stmt = self
            .conn
            .prepare_cached("SELECT artifact_id, linked_kb FROM memory_links")?;
        let mut out: HashMap<String, HashSet<String>> = HashMap::new();
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, kb) = row?;
            out.entry(id).or_default().insert(kb);
        }
        Ok(out)
    }

    /// V0011 — has this memory ever been seeded? `true` means the
    /// indexer must NOT re-import metas; the existing rows in
    /// `memory_links` are authoritative.
    pub fn memory_links_seeded_has(&self, artifact_id: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memory_links_seeded WHERE artifact_id = ?1",
            params![artifact_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// V0011 — mark this memory as seeded. Called by the indexer
    /// after the first-time seed pass and by the L4 backfill.
    /// Idempotent.
    pub fn memory_links_seeded_mark(&mut self, artifact_id: &str, now_unix: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO memory_links_seeded (artifact_id, seeded_at) VALUES (?1, ?2)
             ON CONFLICT(artifact_id) DO NOTHING",
            params![artifact_id, now_unix],
        )?;
        Ok(())
    }

    // --- Reading lists (V0015, RL-track) ---------------------------------
    //
    // Multiple named, ordered lists per kb; entries target a whole
    // artifact or an anchored section. Cross-kb listing via the HTTP
    // fan-out (bookmarks/sessions precedent). `position` is dense 0-based
    // and renumbered inside one transaction on every structural mutation —
    // race-free because all writes serialise through the storage actor.
    // User-curated state: the indexer delete pass and `purge_kb_data`
    // leave these tables alone (tombstones render at read time).

    fn row_to_list(row: &rusqlite::Row) -> rusqlite::Result<ListRow> {
        Ok(ListRow {
            id: row.get(0)?,
            title: row.get(1)?,
            description: row.get(2)?,
            pinned: row.get::<_, i64>(3)? != 0,
            archived: row.get::<_, i64>(4)? != 0,
            created_at_unix: row.get(5)?,
            updated_at_unix: row.get(6)?,
        })
    }

    fn row_to_list_entry(row: &rusqlite::Row) -> rusqlite::Result<ListEntryRow> {
        Ok(ListEntryRow {
            id: row.get(0)?,
            list_id: row.get(1)?,
            kb: row.get(2)?,
            artifact_id: row.get(3)?,
            anchor_json: row.get(4)?,
            note: row.get(5)?,
            position: row.get(6)?,
            read_override: row.get(7)?,
            words: row.get(8)?,
            anchor_stale: row.get::<_, i64>(9)? != 0,
            created_at_unix: row.get(10)?,
            updated_at_unix: row.get(11)?,
        })
    }

    const LIST_COLS: &'static str =
        "id, title, description, pinned, archived, created_at, updated_at";
    const LIST_ENTRY_COLS: &'static str = "id, list_id, kb, artifact_id, anchor, note, position, \
         read_override, words, anchor_stale, created_at, updated_at";

    /// Ordered entry ids of one list — the renumber input. Works on a
    /// `Transaction` too (it derefs to `Connection`).
    fn list_entry_ids_ordered(conn: &rusqlite::Connection, list_id: &str) -> Result<Vec<String>> {
        let mut stmt = conn.prepare_cached(
            "SELECT id FROM list_entries WHERE list_id = ?1 ORDER BY position, id",
        )?;
        let ids = stmt
            .query_map(params![list_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// Resolve a [`PositionSpec`] to an insert index into `existing`
    /// (which must NOT contain the entry being placed). `Before`/`After`
    /// referencing an unknown sibling is a `NotFound`.
    fn list_resolve_index(existing: &[String], pos: &PositionSpec) -> Result<usize> {
        Ok(match pos {
            PositionSpec::First => 0,
            PositionSpec::Last => existing.len(),
            PositionSpec::At(n) => (*n as usize).min(existing.len()),
            PositionSpec::Before(id) => existing
                .iter()
                .position(|e| e == id)
                .ok_or_else(|| crate::Error::NotFound(format!("list entry {id}")))?,
            PositionSpec::After(id) => {
                existing
                    .iter()
                    .position(|e| e == id)
                    .ok_or_else(|| crate::Error::NotFound(format!("list entry {id}")))?
                    + 1
            }
        })
    }

    /// Rewrite positions dense `0..n` to match `order`, and bump the
    /// parent list's `updated_at` (structural mutations are user actions).
    fn list_renumber_and_touch(
        conn: &rusqlite::Connection,
        list_id: &str,
        order: &[&str],
        now_unix: i64,
    ) -> Result<()> {
        let mut stmt =
            conn.prepare_cached("UPDATE list_entries SET position = ?1 WHERE id = ?2")?;
        for (i, eid) in order.iter().enumerate() {
            stmt.execute(params![i as i64, eid])?;
        }
        conn.execute(
            "UPDATE lists SET updated_at = ?1 WHERE id = ?2",
            params![now_unix, list_id],
        )?;
        Ok(())
    }

    /// Create a list. `Conflict` when another list already uses the title
    /// (case-insensitive — the column is COLLATE NOCASE) so the CLI can
    /// resolve lists by title unambiguously.
    pub fn list_create(
        &mut self,
        id: &str,
        title: &str,
        description: Option<&str>,
        pinned: bool,
        now_unix: i64,
    ) -> Result<ListRow> {
        let clash: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM lists WHERE title = ?1",
                params![title],
                |r| r.get(0),
            )
            .optional()?;
        if clash.is_some() {
            return Err(crate::Error::Conflict(format!(
                "a list titled {title:?} already exists"
            )));
        }
        self.conn.execute(
            "INSERT INTO lists (id, title, description, pinned, archived, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?5)",
            params![id, title, description, if pinned { 1 } else { 0 }, now_unix],
        )?;
        Ok(self
            .list_get(id)?
            .expect("list_create just inserted this id"))
    }

    pub fn list_get(&self, id: &str) -> Result<Option<ListRow>> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT {} FROM lists WHERE id = ?1", Self::LIST_COLS),
                params![id],
                Self::row_to_list,
            )
            .optional()?;
        Ok(row)
    }

    /// All lists: pinned first, then newest-touched, id tiebreak.
    pub fn lists_all(&self) -> Result<Vec<ListRow>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {} FROM lists ORDER BY pinned DESC, updated_at DESC, id ASC",
            Self::LIST_COLS
        ))?;
        let rows = stmt
            .query_map([], Self::row_to_list)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Patch list header fields. `Ok(None)` when the list doesn't exist;
    /// `Conflict` when a title change collides with another list.
    pub fn list_update(
        &mut self,
        id: &str,
        title: Option<&str>,
        description: &Patch<String>,
        pinned: Option<bool>,
        archived: Option<bool>,
        now_unix: i64,
    ) -> Result<Option<ListRow>> {
        let Some(cur) = self.list_get(id)? else {
            return Ok(None);
        };
        if let Some(t) = title {
            let clash: Option<String> = self
                .conn
                .query_row(
                    "SELECT id FROM lists WHERE title = ?1 AND id != ?2",
                    params![t, id],
                    |r| r.get(0),
                )
                .optional()?;
            if clash.is_some() {
                return Err(crate::Error::Conflict(format!(
                    "a list titled {t:?} already exists"
                )));
            }
        }
        let new_title = title.unwrap_or(&cur.title);
        let new_desc = description.apply(cur.description);
        let new_pinned = pinned.unwrap_or(cur.pinned);
        let new_archived = archived.unwrap_or(cur.archived);
        self.conn.execute(
            "UPDATE lists SET title = ?1, description = ?2, pinned = ?3, archived = ?4,
                              updated_at = ?5
             WHERE id = ?6",
            params![
                new_title,
                new_desc,
                if new_pinned { 1 } else { 0 },
                if new_archived { 1 } else { 0 },
                now_unix,
                id
            ],
        )?;
        self.list_get(id)
    }

    /// Delete a list; entries cascade (FK ON). `true` if a row was deleted.
    pub fn list_delete(&mut self, id: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM lists WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// Insert one entry at `pos`. `NotFound` for an unknown list or an
    /// unknown `Before`/`After` sibling; `Conflict` when the exact target
    /// `(artifact_id, anchor)` is already in the list.
    ///
    /// v0.34 X1 — the legacy `list_entries.read_override` column is frozen
    /// (always written NULL). When `new.read_override` is set, the value
    /// lands in `list_entry_user_state` for `user` inside the same
    /// transaction.
    pub fn list_entry_add(
        &mut self,
        new: &NewListEntry,
        pos: &PositionSpec,
        user: &str,
        now_unix: i64,
    ) -> Result<ListEntryRow> {
        let tx = self.conn.transaction()?;
        let exists: Option<String> = tx
            .query_row(
                "SELECT id FROM lists WHERE id = ?1",
                params![new.list_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(crate::Error::NotFound(format!("list {}", new.list_id)));
        }
        let dup: Option<String> = tx
            .query_row(
                "SELECT id FROM list_entries
                 WHERE list_id = ?1 AND artifact_id = ?2
                   AND ifnull(anchor, '') = ifnull(?3, '')",
                params![new.list_id, new.artifact_id, new.anchor_json],
                |r| r.get(0),
            )
            .optional()?;
        if dup.is_some() {
            return Err(crate::Error::Conflict(
                "this target is already in the list (same artifact + anchor)".into(),
            ));
        }
        let ids = Self::list_entry_ids_ordered(&tx, &new.list_id)?;
        let idx = Self::list_resolve_index(&ids, pos)?;
        // Legacy column frozen: always NULL. Per-user override (if any)
        // goes into list_entry_user_state below.
        tx.execute(
            "INSERT INTO list_entries
               (id, list_id, kb, artifact_id, anchor, note, position,
                read_override, words, anchor_stale, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, 0, ?9, ?9)",
            params![
                new.id,
                new.list_id,
                new.kb,
                new.artifact_id,
                new.anchor_json,
                new.note,
                idx as i64, // provisional; renumber below makes it exact
                new.words,
                now_unix
            ],
        )?;
        if let Some(ref ov) = new.read_override {
            tx.execute(
                "INSERT INTO list_entry_user_state
                    (entry_id, \"user\", read_override, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(entry_id, \"user\") DO UPDATE SET
                    read_override = excluded.read_override,
                    updated_at = excluded.updated_at",
                params![new.id, user, ov, now_unix],
            )?;
        }
        let mut order: Vec<&str> = ids.iter().map(String::as_str).collect();
        order.insert(idx, &new.id);
        Self::list_renumber_and_touch(&tx, &new.list_id, &order, now_unix)?;
        tx.commit()?;
        Ok(self
            .list_entry_get(&new.id)?
            .expect("list_entry_add just inserted this id"))
    }

    pub fn list_entry_get(&self, entry_id: &str) -> Result<Option<ListEntryRow>> {
        let row = self
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM list_entries WHERE id = ?1",
                    Self::LIST_ENTRY_COLS
                ),
                params![entry_id],
                Self::row_to_list_entry,
            )
            .optional()?;
        Ok(row)
    }

    /// One list's entries in display order.
    pub fn list_entries_for_list(&self, list_id: &str) -> Result<Vec<ListEntryRow>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {} FROM list_entries WHERE list_id = ?1 ORDER BY position, id",
            Self::LIST_ENTRY_COLS
        ))?;
        let rows = stmt
            .query_map(params![list_id], Self::row_to_list_entry)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every entry in the kb — the cross-kb index roll-up input.
    pub fn list_entries_all(&self) -> Result<Vec<ListEntryRow>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {} FROM list_entries ORDER BY list_id, position, id",
            Self::LIST_ENTRY_COLS
        ))?;
        let rows = stmt
            .query_map([], Self::row_to_list_entry)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Entries targeting one artifact, across all lists — the
    /// ListAnchorHook's per-reindex lookup (indexed, ~free when unused).
    pub fn list_entries_for_artifact(&self, artifact_id: &str) -> Result<Vec<ListEntryRow>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {} FROM list_entries WHERE artifact_id = ?1 ORDER BY list_id, position",
            Self::LIST_ENTRY_COLS
        ))?;
        let rows = stmt
            .query_map(params![artifact_id], Self::row_to_list_entry)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Patch one entry's content fields (note / anchor / read_override).
    /// Non-structural: positions and the parent list's `updated_at` stay.
    /// Setting or clearing the anchor resets `anchor_stale` and rewrites
    /// `words` (`Set` carries the fresh per-section estimate).
    ///
    /// v0.34 X1 — `read_override` routes to `list_entry_user_state` for
    /// `user` (never writes the frozen `list_entries.read_override`
    /// column). `Patch::Keep` leaves the per-user row alone.
    pub fn list_entry_update(
        &mut self,
        entry_id: &str,
        note: &Patch<String>,
        anchor: &Patch<(String, Option<i64>)>,
        read_override: &Patch<String>,
        user: &str,
        now_unix: i64,
    ) -> Result<Option<ListEntryRow>> {
        let Some(cur) = self.list_entry_get(entry_id)? else {
            return Ok(None);
        };
        let new_note = note.apply(cur.note.clone());
        let (new_anchor, new_words, new_stale) = match anchor {
            Patch::Keep => (cur.anchor_json.clone(), cur.words, cur.anchor_stale),
            Patch::Clear => (None, None, false),
            Patch::Set((json, words)) => (Some(json.clone()), *words, false),
        };
        if !anchor.is_keep() {
            // Re-anchoring can collide with an existing entry's target.
            let dup: Option<String> = self
                .conn
                .query_row(
                    "SELECT id FROM list_entries
                     WHERE list_id = ?1 AND artifact_id = ?2
                       AND ifnull(anchor, '') = ifnull(?3, '') AND id != ?4",
                    params![cur.list_id, cur.artifact_id, new_anchor, entry_id],
                    |r| r.get(0),
                )
                .optional()?;
            if dup.is_some() {
                return Err(crate::Error::Conflict(
                    "another entry in this list already has that target".into(),
                ));
            }
        }
        // Note/anchor/updated_at only — never touch frozen read_override.
        self.conn.execute(
            "UPDATE list_entries
             SET note = ?1, anchor = ?2, words = ?3, anchor_stale = ?4,
                 updated_at = ?5
             WHERE id = ?6",
            params![
                new_note,
                new_anchor,
                new_words,
                if new_stale { 1 } else { 0 },
                now_unix,
                entry_id
            ],
        )?;
        // Per-user override lives in list_entry_user_state.
        match read_override {
            Patch::Keep => {}
            Patch::Clear => {
                self.list_entry_set_user_override(entry_id, user, None, now_unix)?;
            }
            Patch::Set(ov) => {
                self.list_entry_set_user_override(entry_id, user, Some(ov.as_str()), now_unix)?;
            }
        }
        self.list_entry_get(entry_id)
    }

    /// Move one entry within its list. `Ok(None)` when the entry isn't in
    /// `list_id`; `NotFound` for an unknown `Before`/`After` sibling.
    pub fn list_entry_move(
        &mut self,
        list_id: &str,
        entry_id: &str,
        pos: &PositionSpec,
        now_unix: i64,
    ) -> Result<Option<ListEntryRow>> {
        let tx = self.conn.transaction()?;
        let ids = Self::list_entry_ids_ordered(&tx, list_id)?;
        if !ids.iter().any(|i| i == entry_id) {
            return Ok(None);
        }
        let reduced: Vec<String> = ids.into_iter().filter(|i| i != entry_id).collect();
        let idx = Self::list_resolve_index(&reduced, pos)?;
        let mut order: Vec<&str> = reduced.iter().map(String::as_str).collect();
        order.insert(idx, entry_id);
        Self::list_renumber_and_touch(&tx, list_id, &order, now_unix)?;
        tx.execute(
            "UPDATE list_entries SET updated_at = ?1 WHERE id = ?2",
            params![now_unix, entry_id],
        )?;
        tx.commit()?;
        self.list_entry_get(entry_id)
    }

    /// Remove one entry, renumbering the remainder. Returns the removed
    /// row (`None` when it wasn't in `list_id` — idempotent for routes).
    pub fn list_entry_remove(
        &mut self,
        list_id: &str,
        entry_id: &str,
        now_unix: i64,
    ) -> Result<Option<ListEntryRow>> {
        let tx = self.conn.transaction()?;
        let row = tx
            .query_row(
                &format!(
                    "SELECT {} FROM list_entries WHERE id = ?1 AND list_id = ?2",
                    Self::LIST_ENTRY_COLS
                ),
                params![entry_id, list_id],
                Self::row_to_list_entry,
            )
            .optional()?;
        let Some(row) = row else {
            return Ok(None);
        };
        tx.execute("DELETE FROM list_entries WHERE id = ?1", params![entry_id])?;
        let remaining = Self::list_entry_ids_ordered(&tx, list_id)?;
        let order: Vec<&str> = remaining.iter().map(String::as_str).collect();
        Self::list_renumber_and_touch(&tx, list_id, &order, now_unix)?;
        tx.commit()?;
        Ok(Some(row))
    }

    /// v0.33 X3 — delete every entry in `entry_ids` that still belongs to
    /// `list_id`, renumber survivors, bump list `updated_at`. ONE
    /// transaction. Returns the removed rows (order undefined). Empty
    /// `entry_ids` is a no-op (Ok(vec![])). Unknown ids are skipped.
    pub fn list_entries_remove_many(
        &mut self,
        list_id: &str,
        entry_ids: &[String],
        now_unix: i64,
    ) -> Result<Vec<ListEntryRow>> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        let tx = self.conn.transaction()?;
        let mut removed = Vec::new();
        for eid in entry_ids {
            let row = tx
                .query_row(
                    &format!(
                        "SELECT {} FROM list_entries WHERE id = ?1 AND list_id = ?2",
                        Self::LIST_ENTRY_COLS
                    ),
                    params![eid, list_id],
                    Self::row_to_list_entry,
                )
                .optional()?;
            if let Some(row) = row {
                tx.execute("DELETE FROM list_entries WHERE id = ?1", params![eid])?;
                removed.push(row);
            }
        }
        if !removed.is_empty() {
            let remaining = Self::list_entry_ids_ordered(&tx, list_id)?;
            let order: Vec<&str> = remaining.iter().map(String::as_str).collect();
            Self::list_renumber_and_touch(&tx, list_id, &order, now_unix)?;
        }
        tx.commit()?;
        Ok(removed)
    }

    /// Machine write from the indexer's ListAnchorHook: refreshed
    /// resolution state + word estimates after a reindex. Deliberately
    /// does NOT touch `updated_at` — a reindex must not churn the SPA's
    /// recency ordering. `words: None` keeps the prior estimate.
    pub fn list_entries_sync_resolution(&mut self, updates: &[ResolutionUpdate]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut n = 0usize;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE list_entries
                 SET anchor_stale = ?1, words = COALESCE(?2, words)
                 WHERE id = ?3",
            )?;
            for u in updates {
                n += stmt.execute(params![
                    if u.anchor_stale { 1 } else { 0 },
                    u.words,
                    u.entry_id
                ])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Bulk-load entries from an import. `Replace` wipes the list first —
    /// the true round-trip mode — preserving `created_at` for incoming
    /// entries whose id matches a pre-import row (the `kb-entry` comment
    /// round-trip). `Append` keeps existing entries and skips incoming
    /// duplicates. One transaction; returns the number inserted.
    ///
    /// v0.34 X1 — the legacy `list_entries.read_override` column is frozen
    /// (always written NULL). Read markers on incoming entries land in
    /// `list_entry_user_state` for `user` inside the same transaction
    /// (invariant #25: one tx + one `list.updated`).
    pub fn list_import_entries(
        &mut self,
        list_id: &str,
        mode: ImportMode,
        entries: &[NewListEntry],
        user: &str,
        now_unix: i64,
    ) -> Result<usize> {
        use std::collections::{HashMap, HashSet};

        let tx = self.conn.transaction()?;
        let exists: Option<String> = tx
            .query_row(
                "SELECT id FROM lists WHERE id = ?1",
                params![list_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(crate::Error::NotFound(format!("list {list_id}")));
        }

        // Pre-import created_at by id (Replace keeps them across the wipe).
        let mut created_by_id: HashMap<String, i64> = HashMap::new();
        {
            let mut stmt =
                tx.prepare_cached("SELECT id, created_at FROM list_entries WHERE list_id = ?1")?;
            for r in stmt.query_map(params![list_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })? {
                let (id, at) = r?;
                created_by_id.insert(id, at);
            }
        }

        // Existing targets + next position (Append dedupe baseline).
        let mut seen_targets: HashSet<(String, String)> = HashSet::new();
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut next_pos: i64 = 0;
        match mode {
            ImportMode::Replace => {
                tx.execute(
                    "DELETE FROM list_entries WHERE list_id = ?1",
                    params![list_id],
                )?;
            }
            ImportMode::Append => {
                let mut stmt = tx.prepare_cached(
                    "SELECT id, artifact_id, ifnull(anchor, '') FROM list_entries
                     WHERE list_id = ?1",
                )?;
                for r in stmt.query_map(params![list_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })? {
                    let (id, art, anc) = r?;
                    seen_ids.insert(id);
                    seen_targets.insert((art, anc));
                    next_pos += 1;
                }
            }
        }

        let mut inserted = 0usize;
        // Collect (entry_id, override) pairs to upsert after the insert
        // loop (stmt borrows tx; user-state upserts run on the same tx
        // after the prepared statements drop).
        let mut pending_overrides: Vec<(String, String)> = Vec::new();
        {
            let mut exists_elsewhere =
                tx.prepare_cached("SELECT 1 FROM list_entries WHERE id = ?1 AND list_id != ?2")?;
            let mut stmt = tx.prepare_cached(
                "INSERT INTO list_entries
                   (id, list_id, kb, artifact_id, anchor, note, position,
                    read_override, words, anchor_stale, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, 0, ?9, ?10)",
            )?;
            for e in entries {
                let target = (
                    e.artifact_id.clone(),
                    e.anchor_json.clone().unwrap_or_default(),
                );
                // Defensive dedupe within the batch and (Append) against
                // existing rows — a malformed import must not 500 on the
                // unique index. Duplicate TARGETS are skipped; id problems
                // are repaired by reminting instead (below).
                if seen_targets.contains(&target) {
                    continue;
                }
                // The id is a TABLE-wide PK: a round-tripped id belonging
                // to THIS list (Replace just wiped it) is preserved so
                // `created_at` survives; an id living in another list —
                // a cross-list import — gets a fresh id rather than a
                // constraint failure. Same for an id repeated in-batch.
                let mut id = e.id.clone();
                let clashes_in_batch = seen_ids.contains(&id);
                let clashes_elsewhere = exists_elsewhere
                    .query_row(params![id, list_id], |_| Ok(()))
                    .optional()?
                    .is_some();
                if clashes_in_batch || clashes_elsewhere {
                    id = crate::lists::new_entry_id();
                }
                // created_at preservation keys on the ORIGINAL id — only
                // pre-import rows of this list populate the map.
                let created = created_by_id.get(&e.id).copied().unwrap_or(now_unix);
                // Legacy column frozen: always NULL. Per-user override
                // (if any) is staged for list_entry_user_state below.
                stmt.execute(params![
                    id,
                    list_id,
                    e.kb,
                    e.artifact_id,
                    e.anchor_json,
                    e.note,
                    next_pos,
                    e.words,
                    created,
                    now_unix
                ])?;
                if let Some(ref ov) = e.read_override {
                    pending_overrides.push((id.clone(), ov.clone()));
                }
                seen_targets.insert(target);
                seen_ids.insert(id);
                next_pos += 1;
                inserted += 1;
            }
        }
        for (entry_id, ov) in &pending_overrides {
            tx.execute(
                "INSERT INTO list_entry_user_state
                    (entry_id, \"user\", read_override, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(entry_id, \"user\") DO UPDATE SET
                    read_override = excluded.read_override,
                    updated_at = excluded.updated_at",
                params![entry_id, user, ov, now_unix],
            )?;
        }
        tx.execute(
            "UPDATE lists SET updated_at = ?1 WHERE id = ?2",
            params![now_unix, list_id],
        )?;
        tx.commit()?;
        Ok(inserted)
    }

    // --- Sessions enrichment (V0008) ------------------------------------
    //
    // One row per memory-session artifact in this kb. Populated by the
    // indexer (`kb_core::indexer`) when a doc lands with
    // `kb_category = "memory-session"`. Read by the HTTP /api/sessions
    // route via the storage actor.
    //
    // EVERY aggregate/list read here MUST scope to the newest capture per
    // session id via [`newest_capture_pred`] (invariant #11) — see that fn's
    // doc for why a bare `GROUP BY cwd` / `COUNT(*)` is a live double-count.

    /// Upsert a session enrichment row. Replaces on conflict — the
    /// indexer reruns on every reindex, so the latest parse wins.
    ///
    /// PF-R1 (V0040) — also maintains the materialized `is_newest` flag in
    /// the SAME transaction: this row's (post-upsert) session_id group is
    /// always re-derived, since a first-ever capture, an out-of-order
    /// capture, or a reindex that only touches other columns all fall out of
    /// the same recompute. A reindex can also REPAIR an existing
    /// artifact_id's `session_id` (the truncated-meta bug's fix path,
    /// invariant #11) — when that happens the OLD group loses a member and
    /// must be re-derived too, or it could be left with no flagged row at
    /// all (if this was its only capture) or a stale one (if not).
    pub fn sessions_upsert(&mut self, row: &SessionRow) -> Result<()> {
        let tx = self.conn.transaction()?;
        let prior_session_id: Option<String> = tx
            .query_row(
                "SELECT session_id FROM sessions WHERE artifact_id = ?1",
                params![row.artifact_id],
                |r| r.get(0),
            )
            .optional()?;
        // `is_newest` is deliberately absent from the ON CONFLICT SET list
        // below: it is a materialized derived flag, re-derived explicitly
        // after this statement via `recompute_is_newest`, never blindly
        // overwritten by the `excluded` value (which is always the literal
        // 0 placeholder a re-run through this same INSERT would otherwise
        // clobber it with).
        tx.execute(
            "INSERT INTO sessions
                (artifact_id, session_id, started_at, ended_at,
                 message_count, first_user_prompt, source_relative,
                 title, cwd, git_branch, files_read_count, files_edited_count,
                 token_total, tool_calls, model, error_count,
                 subagent_count, subagent_tokens, subagent_tool_calls,
                 subagent_files_edited, subagent_launched_unstatted,
                 project_key, repo_root, harness, cc_version,
                 last_assistant_text, all_cwds, commit_count, user_turns,
                 active_secs, substance, is_newest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                     ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21,
                     ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, 0)
             ON CONFLICT(artifact_id) DO UPDATE SET
                session_id                  = excluded.session_id,
                started_at                  = excluded.started_at,
                ended_at                    = excluded.ended_at,
                message_count               = excluded.message_count,
                first_user_prompt           = excluded.first_user_prompt,
                source_relative             = excluded.source_relative,
                title                       = excluded.title,
                cwd                         = excluded.cwd,
                git_branch                  = excluded.git_branch,
                files_read_count            = excluded.files_read_count,
                files_edited_count          = excluded.files_edited_count,
                token_total                 = excluded.token_total,
                tool_calls                  = excluded.tool_calls,
                model                       = excluded.model,
                error_count                 = excluded.error_count,
                subagent_count              = excluded.subagent_count,
                subagent_tokens             = excluded.subagent_tokens,
                subagent_tool_calls         = excluded.subagent_tool_calls,
                subagent_files_edited       = excluded.subagent_files_edited,
                subagent_launched_unstatted = excluded.subagent_launched_unstatted,
                project_key                 = excluded.project_key,
                repo_root                   = excluded.repo_root,
                harness                     = excluded.harness,
                cc_version                  = excluded.cc_version,
                last_assistant_text         = excluded.last_assistant_text,
                all_cwds                    = excluded.all_cwds,
                commit_count                = excluded.commit_count,
                user_turns                  = excluded.user_turns,
                active_secs                 = excluded.active_secs,
                substance                   = excluded.substance",
            params![
                row.artifact_id,
                row.session_id,
                row.started_at,
                row.ended_at,
                row.message_count,
                row.first_user_prompt,
                row.source_relative,
                row.title,
                row.cwd,
                row.git_branch,
                row.files_read_count,
                row.files_edited_count,
                row.token_total as i64,
                row.tool_calls,
                row.model,
                row.error_count,
                row.subagent_count,
                row.subagent_tokens as i64,
                row.subagent_tool_calls,
                row.subagent_files_edited,
                row.subagent_launched_unstatted,
                row.project_key,
                row.repo_root,
                row.harness,
                row.cc_version,
                row.last_assistant_text,
                row.all_cwds,
                row.commit_count,
                row.user_turns,
                row.active_secs,
                row.substance,
            ],
        )?;
        recompute_is_newest(&tx, &row.session_id)?;
        if let Some(prior) = prior_session_id {
            if prior != row.session_id {
                recompute_is_newest(&tx, &prior)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Remove a session row by its artifact id. Returns the number of
    /// `sessions` rows deleted (0 or 1). Also drops the session's
    /// `session_files` edges (V0017) — they share the V0008 lifecycle and
    /// vanish with their parent. Called by the indexer when the underlying
    /// memory-session file is unlinked. All six DELETEs run in one
    /// transaction so a mid-cascade failure never leaves orphan child rows.
    pub fn sessions_delete(&mut self, artifact_id: &str) -> Result<usize> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM session_files WHERE artifact_id_session = ?1",
            params![artifact_id],
        )?;
        tx.execute(
            "DELETE FROM session_decisions WHERE artifact_id_session = ?1",
            params![artifact_id],
        )?;
        tx.execute(
            "DELETE FROM session_commits WHERE artifact_id_session = ?1",
            params![artifact_id],
        )?;
        tx.execute(
            "DELETE FROM session_research WHERE artifact_id_session = ?1",
            params![artifact_id],
        )?;
        // MI-W1.1 (revised) — `memory_recalls` is keyed by THIS capture's
        // own `artifact_id` (see `memory_recalls_replace`'s doc comment),
        // exactly like `session_files`/`session_decisions`/etc above: this
        // cascade drops precisely the rows this one capture wrote, whether
        // it's the live capture or an already-superseded stale one — a
        // different capture (different `artifact_id`, same `session_id`)
        // is untouched, matching every other child table's cascade here.
        tx.execute(
            "DELETE FROM memory_recalls WHERE artifact_id = ?1",
            params![artifact_id],
        )?;
        // PF-R1 (V0040) — read the row's session_id BEFORE the delete so its
        // capture group's `is_newest` flag can be re-derived after: the
        // deleted row may have been the group's currently-flagged newest.
        let session_id: Option<String> = tx
            .query_row(
                "SELECT session_id FROM sessions WHERE artifact_id = ?1",
                params![artifact_id],
                |r| r.get(0),
            )
            .optional()?;
        let n = tx.execute(
            "DELETE FROM sessions WHERE artifact_id = ?1",
            params![artifact_id],
        )?;
        if let Some(sid) = session_id {
            recompute_is_newest(&tx, &sid)?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// R4 — replace the full research list for one session artifact in one tx.
    pub fn session_research_replace(
        &mut self,
        artifact_id_session: &str,
        research: &[SessionResearchRow],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM session_research WHERE artifact_id_session = ?1",
            params![artifact_id_session],
        )?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO session_research
                    (artifact_id_session, session_id, seq, kind, query)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for r in research {
                stmt.execute(params![
                    r.artifact_id_session,
                    r.session_id,
                    r.seq,
                    r.kind,
                    r.query,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// R4 — the research signals one session produced, in order. Newest capture
    /// only (#11): older Stop-hook captures re-record the same session, so a
    /// `session_id` join would return them N times.
    pub fn session_research_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionResearchRow>> {
        let mut map = self.session_research_for_sessions(&[session_id.to_string()])?;
        Ok(map.remove(session_id).unwrap_or_default())
    }

    /// Batched form of [`Self::session_research_for_session`]: one `IN (...)`
    /// query + newest-capture pred (#11) per row, keyed by `session_id`.
    pub fn session_research_for_sessions(
        &self,
        session_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<SessionResearchRow>>> {
        if session_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let placeholders = vec!["?"; session_ids.len()].join(",");
        let sql = format!(
            "SELECT artifact_id_session, session_id, seq, kind, query
             FROM session_research
             WHERE session_id IN ({placeholders})
               AND {}
             ORDER BY session_id ASC, seq ASC",
            // The correlation target MUST be qualified by the outer table's
            // own name (`session_research.session_id`), never a bare
            // `session_id` — the subquery's own `sessions AS s2` range var
            // ALSO has a `session_id` column, and SQLite's (standard SQL)
            // name resolution binds an unqualified reference to the
            // INNERMOST matching scope silently, no ambiguity error. A bare
            // `session_id` here would self-correlate against `s2` and
            // degenerate into "pick whichever session in the ENTIRE
            // `sessions` table has the globally largest started_at", not
            // "this row's own session's newest capture" — same pre-existing
            // bug class as `memory_recalls_counts_for_ids`'s MI-W1.R fix,
            // caught here because this fn batches MANY session_ids at once.
            newest_capture_pred("artifact_id_session", "session_research.session_id")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(session_ids.iter()), |r| {
                Ok(SessionResearchRow {
                    artifact_id_session: r.get(0)?,
                    session_id: r.get(1)?,
                    seq: r.get(2)?,
                    kind: r.get(3)?,
                    query: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out: std::collections::HashMap<String, Vec<SessionResearchRow>> =
            std::collections::HashMap::new();
        for row in rows {
            out.entry(row.session_id.clone()).or_default().push(row);
        }
        Ok(out)
    }

    /// W4/R8/ADD-2 — every `grok_job` research row across every session whose
    /// query is exactly `ulid` (the driver-side sniffer's join key), scoped
    /// to each matching session's OWN newest capture (#11 — the correlated
    /// form, same as `sessions_research_rollup`, since this scans across
    /// MANY sessions rather than one known `session_id`). Powers
    /// `GET /api/sessions/by-job/{ulid}` / `kb sessions by-job` — a Claude
    /// Code session that drove (or, from W5, was driven BY) a grokclaude job.
    pub fn session_research_by_job(&self, ulid: &str) -> Result<Vec<SessionResearchRow>> {
        let sql = format!(
            "SELECT artifact_id_session, session_id, seq, kind, query
             FROM session_research r
             WHERE r.kind = 'grok_job' AND r.query = ?1
               AND {}
             ORDER BY r.session_id ASC, r.seq ASC",
            newest_capture_pred("r.artifact_id_session", "r.session_id")
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(params![ulid], |r| {
                Ok(SessionResearchRow {
                    artifact_id_session: r.get(0)?,
                    session_id: r.get(1)?,
                    seq: r.get(2)?,
                    kind: r.get(3)?,
                    query: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Replace the full commits list for one session artifact in one tx.
    pub fn session_commits_replace(
        &mut self,
        artifact_id_session: &str,
        commits: &[SessionCommitRow],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM session_commits WHERE artifact_id_session = ?1",
            params![artifact_id_session],
        )?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO session_commits
                    (artifact_id_session, session_id, seq, kind, sha, subject,
                     sha_full, repo_root, resolved, author, parents, trailers)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            for c in commits {
                stmt.execute(params![
                    c.artifact_id_session,
                    c.session_id,
                    c.seq,
                    c.kind,
                    c.sha,
                    c.subject,
                    c.sha_full,
                    c.repo_root,
                    c.resolved,
                    c.author,
                    c.parents,
                    c.trailers,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The commits one session produced, in order. Newest capture only (#11).
    pub fn session_commits_for_session(&self, session_id: &str) -> Result<Vec<SessionCommitRow>> {
        let mut map = self.session_commits_for_sessions(&[session_id.to_string()])?;
        Ok(map.remove(session_id).unwrap_or_default())
    }

    /// Batched form of [`Self::session_commits_for_session`]: one `IN (...)`
    /// query + newest-capture pred (#11) per row, keyed by `session_id`.
    /// Empty input → empty map (no SQL). Missing sessions are simply absent.
    pub fn session_commits_for_sessions(
        &self,
        session_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<SessionCommitRow>>> {
        if session_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let placeholders = vec!["?"; session_ids.len()].join(",");
        let sql = format!(
            "SELECT artifact_id_session, session_id, seq, kind, sha, subject,
                    sha_full, repo_root, resolved, author, parents, trailers
             FROM session_commits
             WHERE session_id IN ({placeholders})
               AND {}
             ORDER BY session_id ASC, seq ASC",
            // Qualified by the outer table's own name — see the identical
            // rationale on `session_research_for_sessions` above (a bare
            // `session_id` here self-correlates against the subquery's own
            // `sessions AS s2` and collapses to the globally-newest session
            // across the WHOLE table, not each row's own session).
            newest_capture_pred("artifact_id_session", "session_commits.session_id")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(session_ids.iter()), |r| {
                Ok(SessionCommitRow {
                    artifact_id_session: r.get(0)?,
                    session_id: r.get(1)?,
                    seq: r.get(2)?,
                    kind: r.get(3)?,
                    sha: r.get(4)?,
                    subject: r.get(5)?,
                    sha_full: r.get(6)?,
                    repo_root: r.get(7)?,
                    resolved: r.get(8)?,
                    author: r.get(9)?,
                    parents: r.get(10)?,
                    trailers: r.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out: std::collections::HashMap<String, Vec<SessionCommitRow>> =
            std::collections::HashMap::new();
        for row in rows {
            out.entry(row.session_id.clone()).or_default().push(row);
        }
        Ok(out)
    }

    /// `SELECT` column list shared by [`Self::session_commits_by_sha_prefix`]
    /// and [`Self::session_commits_page`] — the `session_commits` columns in
    /// [`SessionCommitRow`]'s canonical order, plus the owning session's
    /// `started_at` (both queries JOIN `sessions` directly rather than the
    /// `session_commits_for_session` correlated-subquery pattern, since here
    /// the caller doesn't already know which `session_id` it wants).
    const COMMIT_JOIN_COLS: &'static str =
        "sc.artifact_id_session, sc.session_id, sc.seq, sc.kind, \
         sc.sha, sc.subject, sc.sha_full, sc.repo_root, sc.resolved, sc.author, \
         sc.parents, sc.trailers, s.started_at";

    fn map_commit_join_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(SessionCommitRow, i64)> {
        Ok((
            SessionCommitRow {
                artifact_id_session: r.get(0)?,
                session_id: r.get(1)?,
                seq: r.get(2)?,
                kind: r.get(3)?,
                sha: r.get(4)?,
                subject: r.get(5)?,
                sha_full: r.get(6)?,
                repo_root: r.get(7)?,
                resolved: r.get(8)?,
                author: r.get(9)?,
                parents: r.get(10)?,
                trailers: r.get(11)?,
            },
            r.get(12)?,
        ))
    }

    /// kb-code Wave 0 (W0.6) — `GET /api/sessions/by-commit?sha=`:
    /// `session_commits` rows whose `sha` OR `sha_full` (V0025) starts with
    /// `prefix` (SQLite `LIKE` is ASCII case-insensitive by default, so the
    /// route needn't pre-lowercase), scoped to each session's NEWEST capture
    /// (#11 — an older re-recorded capture would otherwise duplicate a
    /// match). `prefix` is expected pre-validated by the route (>=7 hex
    /// chars — no other characters can reach a `LIKE` pattern here since hex
    /// digits never collide with `%`/`_`). Joined in the SAME query with the
    /// owning session's `started_at`/`title`/`first_user_prompt` (co-located
    /// in this kb's sqlite db) so the route needs no second round trip to
    /// compute a display name.
    pub fn session_commits_by_sha_prefix(&self, prefix: &str) -> Result<Vec<SessionCommitMatch>> {
        let like = format!("{prefix}%");
        let sql = format!(
            "SELECT {}, s.title, s.first_user_prompt
             FROM session_commits sc
             JOIN sessions s ON s.artifact_id = sc.artifact_id_session
             WHERE (sc.sha LIKE ?1 OR sc.sha_full LIKE ?1)
               AND {}
             ORDER BY s.started_at DESC, sc.seq ASC",
            Self::COMMIT_JOIN_COLS,
            newest_capture_pred("sc.artifact_id_session", "sc.session_id")
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(params![like], |r| {
                let (commit, started_at) = Self::map_commit_join_row(r)?;
                Ok(SessionCommitMatch {
                    commit,
                    started_at,
                    title: r.get(13)?,
                    first_user_prompt: r.get(14)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// kb-code Wave 0 (W0.6) — `GET /api/sessions/commit-map`: every
    /// `session_commits` row scoped to its session's NEWEST capture (#11),
    /// optionally floored by `since` (session `started_at`), newest-first
    /// with a deterministic tiebreak, offset-paginated. Flat and cheap on
    /// purpose (no title/lance resolution) — feeds kb-code's wave-3
    /// sha→session join precomputation, which wants the whole table, not a
    /// per-row-enriched view.
    pub fn session_commits_page(
        &self,
        since: Option<i64>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<CommitMapRow>> {
        let sql = format!(
            "SELECT {}
             FROM session_commits sc
             JOIN sessions s ON s.artifact_id = sc.artifact_id_session
             WHERE (?1 IS NULL OR s.started_at >= ?1)
               AND {}
             ORDER BY s.started_at DESC, sc.session_id ASC, sc.seq ASC
             LIMIT ?2 OFFSET ?3",
            Self::COMMIT_JOIN_COLS,
            newest_capture_pred("sc.artifact_id_session", "sc.session_id")
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(params![since, limit, offset], |r| {
                let (commit, started_at) = Self::map_commit_join_row(r)?;
                Ok(CommitMapRow { commit, started_at })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Replace the full decisions log for one session artifact in one
    /// transaction (delete-then-insert; the indexer reruns on every reindex).
    pub fn session_decisions_replace(
        &mut self,
        artifact_id_session: &str,
        decisions: &[SessionDecisionRow],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM session_decisions WHERE artifact_id_session = ?1",
            params![artifact_id_session],
        )?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO session_decisions
                    (artifact_id_session, session_id, seq, kind, prompt, answer)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for d in decisions {
                stmt.execute(params![
                    d.artifact_id_session,
                    d.session_id,
                    d.seq,
                    d.kind,
                    d.prompt,
                    d.answer,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The decisions log for one session, in transcript order. Newest capture
    /// only (#11).
    pub fn session_decisions_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionDecisionRow>> {
        let mut map = self.session_decisions_for_sessions(&[session_id.to_string()])?;
        Ok(map.remove(session_id).unwrap_or_default())
    }

    /// Batched form of [`Self::session_decisions_for_session`]: one `IN (...)`
    /// query + newest-capture pred (#11) per row, keyed by `session_id`.
    pub fn session_decisions_for_sessions(
        &self,
        session_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<SessionDecisionRow>>> {
        if session_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let placeholders = vec!["?"; session_ids.len()].join(",");
        let sql = format!(
            "SELECT artifact_id_session, session_id, seq, kind, prompt, answer
             FROM session_decisions
             WHERE session_id IN ({placeholders})
               AND {}
             ORDER BY session_id ASC, seq ASC",
            // Qualified by the outer table's own name — see the identical
            // rationale on `session_research_for_sessions` above (a bare
            // `session_id` here self-correlates against the subquery's own
            // `sessions AS s2` and collapses to the globally-newest session
            // across the WHOLE table, not each row's own session).
            newest_capture_pred("artifact_id_session", "session_decisions.session_id")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(session_ids.iter()), |r| {
                Ok(SessionDecisionRow {
                    artifact_id_session: r.get(0)?,
                    session_id: r.get(1)?,
                    seq: r.get(2)?,
                    kind: r.get(3)?,
                    prompt: r.get(4)?,
                    answer: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out: std::collections::HashMap<String, Vec<SessionDecisionRow>> =
            std::collections::HashMap::new();
        for row in rows {
            out.entry(row.session_id.clone()).or_default().push(row);
        }
        Ok(out)
    }

    /// Replace the full `session_files` edge set for one session artifact in
    /// a single transaction (delete-then-insert): the indexer reruns the
    /// parse on every reindex, so the latest scan wins wholesale.
    pub fn session_files_replace(
        &mut self,
        artifact_id_session: &str,
        files: &[SessionFileRow],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM session_files WHERE artifact_id_session = ?1",
            params![artifact_id_session],
        )?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO session_files
                    (artifact_id_session, session_id, path, basename, action,
                     in_corpus, target_kb, target_artifact_id, via_subagent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for f in files {
                stmt.execute(params![
                    f.artifact_id_session,
                    f.session_id,
                    f.path,
                    f.basename,
                    f.action,
                    f.in_corpus as i64,
                    f.target_kb,
                    f.target_artifact_id,
                    f.via_subagent as i64,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// MI-W1.1 (revised, review-fix) — replace the full `memory_recalls` set
    /// for one CAPTURE (`artifact_id`) in one transaction (delete-then-
    /// insert), exactly like `session_files_replace`'s
    /// `artifact_id_session` scoping. The original version of this fn
    /// deleted by `session_id` directly on the claim that "a memory-session
    /// transcript is re-parsed in full on every capture, so the newest
    /// capture's derived set is always the superset and there is never more
    /// than one capture's rows alive for a given session id at once" — that
    /// claim assumed captures for one session always arrive in monotonic
    /// order and never interleave across artifact_ids, which nothing in the
    /// codebase guarantees (invariant #11's own multi-capture note is
    /// explicit that per-session reads must scope to the newest capture,
    /// not rely on write-time ordering). Scoping the delete+insert to THIS
    /// capture's own `artifact_id` means an out-of-order or concurrent
    /// capture can never delete a DIFFERENT capture's rows out from under
    /// it; every multi-row READ (`memory_recalls_for_session`,
    /// `memory_recalls_counts_for_ids`) instead filters to the newest
    /// capture via `newest_capture_pred` at query time.
    pub fn memory_recalls_replace(
        &mut self,
        artifact_id: &str,
        rows: &[MemoryRecallRow],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM memory_recalls WHERE artifact_id = ?1",
            params![artifact_id],
        )?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO memory_recalls
                    (memory_kb, memory_id, session_id, turn_id, recalled_at, artifact_id, used, pos)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for r in rows {
                stmt.execute(params![
                    r.memory_kb,
                    r.memory_id,
                    r.session_id,
                    r.turn_id,
                    r.recalled_at,
                    r.artifact_id,
                    r.used as i64,
                    r.pos,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// MI-W1.1 (revised) — every recalled hit for one session id, scoped to
    /// its NEWEST capture (#11 — mirrors `session_files_for_session`'s
    /// shape/predicate exactly: rows now persist per-capture, so a stale
    /// capture's rows can coexist with the live one until that stale
    /// capture's own `sessions` row is unlinked). Ordered by `recalled_at`
    /// (nulls last), then insertion order.
    pub fn memory_recalls_for_session(&self, session_id: &str) -> Result<Vec<MemoryRecallRow>> {
        let sql = format!(
            "SELECT memory_kb, memory_id, session_id, turn_id, recalled_at, artifact_id, used, pos
             FROM memory_recalls
             WHERE {}
             ORDER BY (recalled_at IS NULL) ASC, recalled_at ASC, rowid ASC",
            newest_capture_pred("artifact_id", "?1")
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(params![session_id], Self::memory_recall_row_from_query)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Shared row-mapper for the 8-column `memory_recalls` SELECT shape
    /// `memory_recalls_for_session` uses (same `used` INTEGER→bool
    /// convention as `via_subagent`/`truncated` elsewhere in this file).
    fn memory_recall_row_from_query(r: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRecallRow> {
        Ok(MemoryRecallRow {
            memory_kb: r.get(0)?,
            memory_id: r.get(1)?,
            session_id: r.get(2)?,
            turn_id: r.get(3)?,
            recalled_at: r.get(4)?,
            artifact_id: r.get(5)?,
            used: r.get::<_, i64>(6)? != 0,
            pos: r.get::<_, Option<i64>>(7)?.map(i64_as_u32_sat),
        })
    }

    /// MI-W1.2/W1.3 (revised, review-fix) — aggregate recall stats for a
    /// batch of memory ids within THIS kb's `memory_recalls` table (most kbs
    /// have none — only a kb that captures `memory-session` transcripts
    /// populates it). A caller wanting the true corpus-wide count fans out
    /// across every kb (invariant #28) and sums `count` / takes the max
    /// `last_recalled_at` per memory id across the per-kb partials.
    /// `memory_kb` narrows to hits recalled FROM that one memory corpus —
    /// the census route's `?kb=` scope; pass `None` to count a recall
    /// regardless of which corpus it named (defensive against a
    /// stale/mismatched `memory_kb` string in the ledger).
    ///
    /// Scoped to each matching row's OWN session's newest capture via the
    /// CORRELATED form of `newest_capture_pred` (same shape as
    /// `session_research_by_job` — this scans across MANY different
    /// recalling sessions, not one known `session_id`, so the predicate must
    /// re-resolve "newest" per row rather than once for a single bound id).
    /// Without this a memory recalled by a session that has since been
    /// re-captured (Stop-hook fires again, new `artifact_id`, stale rows
    /// left behind until THAT capture is unlinked — see
    /// `memory_recalls_replace`) would double-count: both the stale and the
    /// live capture's rows would otherwise satisfy `memory_id IN (...)`.
    pub fn memory_recalls_counts_for_ids(
        &self,
        memory_kb: Option<&str>,
        memory_ids: &[String],
    ) -> Result<Vec<MemoryRecallCount>> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; memory_ids.len()].join(",");
        let sql = format!(
            "SELECT memory_id, COUNT(*), MAX(recalled_at), SUM(used)
             FROM memory_recalls
             WHERE memory_id IN ({placeholders})
               AND {}{}
             GROUP BY memory_id",
            // The correlation target MUST be qualified by the outer table's
            // own name (`memory_recalls.session_id`), never a bare
            // `session_id` — the subquery's own `sessions AS s2` range var
            // ALSO has a `session_id` column, and SQLite's (standard SQL)
            // name resolution binds an unqualified reference to the
            // INNERMOST matching scope silently, no ambiguity error. A bare
            // `session_id` here would self-correlate against `s2` and
            // degenerate into "pick whichever session in the ENTIRE
            // `sessions` table has the globally largest started_at", not
            // "this row's own session's newest capture" — verified via a
            // failing unit test before this fix (a two-different-sessions
            // scenario collapsed to a single row).
            newest_capture_pred("artifact_id", "memory_recalls.session_id"),
            if memory_kb.is_some() {
                " AND memory_kb = ?"
            } else {
                ""
            }
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut binds: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(memory_ids.len() + 1);
        for id in memory_ids {
            binds.push(id);
        }
        // Push a reference to the PARAMETER itself (not a copied local) so
        // the borrow outlives `query_map` below — `Option<&str>: ToSql`
        // (rusqlite's blanket `Option<T: ToSql>` impl) serialises `Some` as
        // the value; the `is_some()` guard keeps the bind COUNT matching
        // the conditional `AND memory_kb = ?` clause in `sql` above.
        if memory_kb.is_some() {
            binds.push(&memory_kb);
        }
        let rows = stmt
            .query_map(binds.as_slice(), |r| {
                Ok(MemoryRecallCount {
                    memory_id: r.get(0)?,
                    count: i64_as_u32_sat(r.get(1)?),
                    last_recalled_at: r.get(2)?,
                    used_count: i64_as_u32_sat(r.get(3)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// MI-W4.2a — a coarse per-week injection HISTOGRAM (NOT a full
    /// per-event timeline — the design brief's own fallback for when
    /// per-injection timestamps aren't practical to expose in full:
    /// "add a server-side aggregate rather than faking it") for the
    /// `/memory` row's injection sparkline. Buckets by `(now_unix -
    /// recalled_at) / 1 week`, clamped in SQL (via `MIN(...,
    /// MEMORY_RECALL_WEEKLY_BUCKETS-1)`) so a row from years ago collapses
    /// into the LAST bucket ("N+ weeks ago") rather than growing the result
    /// set — the histogram width is fixed regardless of how far back the
    /// ledger goes. A `recalled_at IS NULL` row (the Turn's own timestamp
    /// didn't parse — see `derive_memory_recalls`) lands in bucket 0 rather
    /// than being dropped: the injection still happened, "sometime very
    /// recently" is a safer default than silently uncounting it. Same
    /// newest-capture scoping + optional `memory_kb` filter as
    /// `memory_recalls_counts_for_ids` (this is its per-week sibling); a
    /// caller wanting the true corpus-wide histogram fans out across every
    /// kb and sums same-bucket counts (invariant #28), exactly like that
    /// function's own doc comment describes for its aggregate.
    pub fn memory_recalls_weekly_for_ids(
        &self,
        memory_kb: Option<&str>,
        memory_ids: &[String],
        now_unix: i64,
    ) -> Result<Vec<MemoryRecallWeeklyRow>> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; memory_ids.len()].join(",");
        let sql = format!(
            "SELECT memory_id,
                    MIN(MAX(CAST((?  - COALESCE(recalled_at, ?)) / 604800 AS INTEGER), 0), {max_bucket}) AS wk,
                    COUNT(*)
             FROM memory_recalls
             WHERE memory_id IN ({placeholders})
               AND {}{}
             GROUP BY memory_id, wk",
            newest_capture_pred("artifact_id", "memory_recalls.session_id"),
            if memory_kb.is_some() {
                " AND memory_kb = ?"
            } else {
                ""
            },
            max_bucket = MEMORY_RECALL_WEEKLY_BUCKETS - 1,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut binds: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(memory_ids.len() + 3);
        // The two `?` in the bucket expression bind to the SAME `now_unix`
        // value (once for the subtraction, once as the `COALESCE` fallback
        // for a null `recalled_at` — which then makes the subtraction
        // collapse to exactly `0`, landing that row in bucket 0).
        binds.push(&now_unix);
        binds.push(&now_unix);
        for id in memory_ids {
            binds.push(id);
        }
        if memory_kb.is_some() {
            binds.push(&memory_kb);
        }
        let rows = stmt
            .query_map(binds.as_slice(), |r| {
                Ok(MemoryRecallWeeklyRow {
                    memory_id: r.get(0)?,
                    weeks_ago: r.get(1)?,
                    count: i64_as_u32_sat(r.get(2)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// CT-B2 — the memory-side reverse of `memory_recalls_for_session`:
    /// every session that recalled ONE memory (`memory_kb`/`memory_id`),
    /// newest-`recalled_at`-first, each row enriched with its recalling
    /// session's own `title`/`first_user_prompt` (co-located in this kb's
    /// sqlite — the same zero-round-trip join `session_commits_by_sha_prefix`
    /// uses, so the caller needs no second lookup to build a display name).
    ///
    /// Scoped to each row's OWN session's newest capture via the
    /// CORRELATED form of `newest_capture_pred` (same shape/reasoning as
    /// `memory_recalls_counts_for_ids` — this scans across MANY different
    /// recalling sessions, not one bound `session_id`, so the predicate must
    /// re-resolve "newest" per row rather than once for a single session).
    /// The correlation target is qualified `mr.session_id` for the exact
    /// reason documented on `memory_recalls_counts_for_ids`: the subquery's
    /// own `sessions AS s2` range var also has a `session_id` column, so an
    /// unqualified reference would silently self-correlate. The `JOIN
    /// sessions s ON s.artifact_id = mr.artifact_id` is safe to leave
    /// unfiltered by the same predicate: once `mr.artifact_id` is pinned to
    /// the session's newest capture, joining `sessions` on that exact
    /// `artifact_id` can only ever resolve to that same newest row.
    ///
    /// `limit` caps the result set (the route enforces its own cross-kb
    /// total cap on top of this per-kb one).
    pub fn memory_recalls_for_memory(
        &self,
        memory_kb: &str,
        memory_id: &str,
        limit: u32,
    ) -> Result<Vec<MemoryRecalledByRow>> {
        let sql = format!(
            "SELECT mr.session_id, mr.turn_id, mr.recalled_at, s.title, s.first_user_prompt,
                    s.started_at, mr.used, mr.pos
             FROM memory_recalls mr
             JOIN sessions s ON s.artifact_id = mr.artifact_id
             WHERE mr.memory_kb = ?1 AND mr.memory_id = ?2
               AND {}
             ORDER BY (mr.recalled_at IS NULL) ASC, mr.recalled_at DESC, mr.rowid ASC
             LIMIT ?3",
            newest_capture_pred("mr.artifact_id", "mr.session_id")
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(params![memory_kb, memory_id, limit], |r| {
                Ok(MemoryRecalledByRow {
                    session_id: r.get(0)?,
                    turn_id: r.get(1)?,
                    recalled_at: r.get(2)?,
                    title: r.get(3)?,
                    first_user_prompt: r.get(4)?,
                    started_at: r.get(5)?,
                    used: r.get::<_, i64>(6)? != 0,
                    pos: r.get::<_, Option<i64>>(7)?.map(i64_as_u32_sat),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// CT-F1 — replace THIS CAPTURE's `memory_commits` claims (V0038) in one
    /// transaction. Two halves, and the asymmetry is deliberate:
    ///
    /// - the DELETE is scoped to this capture's own `artifact_id` (the same
    ///   per-capture scoping `memory_recalls_replace` settled on, so an
    ///   out-of-order capture can never drop a sibling capture's rows), and
    /// - the INSERT is `OR REPLACE`, because the PK is the FACT
    ///   `(memory_id, sha_full)`, not the capture. Re-capturing the same
    ///   session simply rewrites the same row with the newer capture's
    ///   provenance, so the multi-capture fan-out (#11) can never
    ///   double-count here and every READ is free of `newest_capture_pred`
    ///   — duplicates are structurally impossible rather than filtered out.
    ///
    /// The one behaviour that follows from that: a row a PREVIOUS capture
    /// wrote and this one no longer derives survives under the older
    /// capture's `artifact_id` until that capture is unlinked. That is the
    /// honest outcome — the commit trailer really did exist in git history;
    /// git's history is the record, not this capture's re-reading of it.
    pub fn memory_commits_replace(
        &mut self,
        artifact_id: &str,
        rows: &[MemoryCommitRow],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM memory_commits WHERE artifact_id = ?1",
            params![artifact_id],
        )?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO memory_commits
                    (memory_id, sha_full, sha, subject, repo_root,
                     session_id, artifact_id, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for r in rows {
                stmt.execute(params![
                    r.memory_id,
                    r.sha_full,
                    r.sha,
                    r.subject,
                    r.repo_root,
                    r.session_id,
                    r.artifact_id,
                    r.recorded_at,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// CT-F1 — every commit that CITED one memory, newest-`recorded_at`
    /// first, within THIS kb's `memory_commits` table. The memory-side
    /// sibling of [`Self::memory_recalls_for_memory`] and, like it, fanned
    /// out across every kb by its route (invariant #28): the rows live with
    /// the RECORDING session's corpus, which is typically the sessions kb,
    /// not the memory's own.
    ///
    /// Deliberately NOT scoped by `newest_capture_pred` — see
    /// [`Self::memory_commits_replace`]: the `(memory_id, sha_full)` PK
    /// already collapses the multi-capture fan-out, so the predicate would
    /// only ever drop a row whose capture is stale but whose FACT (this
    /// commit cites this memory) is not.
    ///
    /// There is no `memory_kb` filter because the trailer grammar carries no
    /// kb (`Kb-Memory: <hex12>`); the caller is already scoped to one `{kb}`
    /// and the result is labelled a citation, never a proof.
    pub fn memory_commits_for_memory(
        &self,
        memory_id: &str,
        limit: u32,
    ) -> Result<Vec<MemoryCommitRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT memory_id, sha_full, sha, subject, repo_root,
                    session_id, artifact_id, recorded_at
             FROM memory_commits
             WHERE memory_id = ?1
             ORDER BY recorded_at DESC, sha_full ASC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![memory_id, limit], |r| {
                Ok(MemoryCommitRow {
                    memory_id: r.get(0)?,
                    sha_full: r.get(1)?,
                    sha: r.get(2)?,
                    subject: r.get(3)?,
                    repo_root: r.get(4)?,
                    session_id: r.get(5)?,
                    artifact_id: r.get(6)?,
                    recorded_at: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every file one session touched, ordered edits-before-reads then by
    /// basename, for the per-session file manifest (`GET /sessions/{sid}/files`).
    /// Newest capture only (#11): a multi-capture session would otherwise list
    /// each file once per capture (the "repeated edited/created" inflation).
    pub fn session_files_for_session(&self, session_id: &str) -> Result<Vec<SessionFileRow>> {
        let sql = format!(
            "SELECT artifact_id_session, session_id, path, basename, action,
                    in_corpus, target_kb, target_artifact_id, via_subagent
             FROM session_files
             WHERE {}
             ORDER BY (action = 'read') ASC, basename ASC, path ASC",
            newest_capture_pred("artifact_id_session", "?1")
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(params![session_id], Self::map_session_file)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every session edge pointing at one artifact, for the reverse
    /// "sessions that touched this file" lookup (A7). Filters to in-corpus
    /// rows resolved to this `target_artifact_id`. Newest capture only
    /// (#11) — multi-capture re-records the same edges, so without the
    /// pred a reverse lookup returns each touch once per Stop.
    pub fn session_files_for_artifact(
        &self,
        target_artifact_id: &str,
    ) -> Result<Vec<SessionFileRow>> {
        let sql = format!(
            "SELECT artifact_id_session, session_id, path, basename, action,
                    in_corpus, target_kb, target_artifact_id, via_subagent
             FROM session_files
             WHERE target_artifact_id = ?1 AND in_corpus = 1
               AND {}",
            // Qualified by the outer table's own name — see the identical
            // rationale on `session_research_for_sessions` above. This scan
            // is a reverse lookup across every session that ever touched
            // `target_artifact_id`, so it can legitimately span MANY
            // different session_ids at once — exactly the trigger condition
            // for the bare-`session_id` self-correlation bug.
            newest_capture_pred("artifact_id_session", "session_files.session_id")
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(params![target_artifact_id], Self::map_session_file)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// R2 (`kb why`) — every session edge whose file BASENAME matches. The
    /// robust key for "which sessions touched this file": most touched files
    /// are NOT kb artifacts (they're code), and the verbatim-stored `path`
    /// may be absolute while the caller has a source-relative one (or vice
    /// versa), so basename is the stable join. The route refines each row to
    /// Exact vs Fuzzy by aligning the full paths (TouchesConfidence).
    /// Newest capture only (#11) — same multi-capture inflation as the
    /// reverse-artifact lookup.
    pub fn session_files_for_basename(&self, basename: &str) -> Result<Vec<SessionFileRow>> {
        let sql = format!(
            "SELECT artifact_id_session, session_id, path, basename, action,
                    in_corpus, target_kb, target_artifact_id, via_subagent
             FROM session_files
             WHERE basename = ?1
               AND {}",
            // Qualified by the outer table's own name — see the identical
            // rationale on `session_research_for_sessions` above. This scan
            // is a reverse lookup across every session that ever touched a
            // file with this basename, so it can legitimately span MANY
            // different session_ids at once — exactly the trigger condition
            // for the bare-`session_id` self-correlation bug.
            newest_capture_pred("artifact_id_session", "session_files.session_id")
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(params![basename], Self::map_session_file)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// R2 (`kb why`) — batched session-metadata lookup for a SET of session
    /// ids: one `IN (...)` query instead of a per-session fan-out, bounding
    /// the WHY assembler's cost on a hot file. Returns the newest row per
    /// session id (same tie-break as `sessions_get`).
    pub fn sessions_get_many(&self, session_ids: &[String]) -> Result<Vec<SessionRow>> {
        if session_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; session_ids.len()].join(",");
        let sql = format!(
            "SELECT artifact_id, session_id, started_at, ended_at,
                    message_count, first_user_prompt, source_relative,
                    title, cwd, git_branch, files_read_count, files_edited_count,
                    token_total, tool_calls, model, error_count,
                    subagent_count, subagent_tokens, subagent_tool_calls,
                    subagent_files_edited, subagent_launched_unstatted,
                    project_key, repo_root, harness, cc_version,
                    last_assistant_text, all_cwds, commit_count, user_turns,
                    active_secs, substance
             FROM sessions
             WHERE session_id IN ({placeholders})
             ORDER BY started_at DESC, artifact_id ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(session_ids.iter()),
                Self::map_session_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // Keep the first (newest) row per session id — handles multi-capture.
        let mut seen = std::collections::BTreeSet::new();
        Ok(rows
            .into_iter()
            .filter(|r| seen.insert(r.session_id.clone()))
            .collect())
    }

    /// #11/recollect-R3 — resolve lance artifact ids to their sqlite session
    /// rows. `artifact_id` is the `sessions` table's PRIMARY KEY (the
    /// `ON CONFLICT(artifact_id)` in `sessions_upsert`), so this is a direct
    /// `IN (...)` lookup — no tie-break needed, unlike `sessions_get_many`'s
    /// session_id join (one artifact_id names exactly one capture). Callers
    /// join on `.session_id`, the JSONL-recovered CANONICAL id — never the
    /// lance `kb_session` meta value, which is a dirty capture-hook hint
    /// (trailing dash / truncation) and must not be used as a join key.
    pub fn sessions_get_by_artifact_ids(&self, artifact_ids: &[String]) -> Result<Vec<SessionRow>> {
        if artifact_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; artifact_ids.len()].join(",");
        let sql = format!(
            "SELECT artifact_id, session_id, started_at, ended_at,
                    message_count, first_user_prompt, source_relative,
                    title, cwd, git_branch, files_read_count, files_edited_count,
                    token_total, tool_calls, model, error_count,
                    subagent_count, subagent_tokens, subagent_tool_calls,
                    subagent_files_edited, subagent_launched_unstatted,
                    project_key, repo_root, harness, cc_version,
                    last_assistant_text, all_cwds, commit_count, user_turns,
                    active_secs, substance
             FROM sessions
             WHERE artifact_id IN ({placeholders})"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(artifact_ids.iter()),
                Self::map_session_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn map_session_file(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionFileRow> {
        Ok(SessionFileRow {
            artifact_id_session: r.get(0)?,
            session_id: r.get(1)?,
            path: r.get(2)?,
            basename: r.get(3)?,
            action: r.get(4)?,
            in_corpus: r.get::<_, i64>(5)? != 0,
            target_kb: r.get(6)?,
            target_artifact_id: r.get(7)?,
            via_subagent: r.get::<_, i64>(8)? != 0,
        })
    }

    /// List enrichment rows newest-first under the
    /// `(started_at DESC, artifact_id ASC)` order. `limit` caps the
    /// result; callers asking for "everything" pass `u32::MAX`.
    ///
    /// T5/X1 — keyset pagination. The cursor is the FULL sort key of the
    /// last surfaced row (`before` = its `started_at`, `before_id` = its
    /// `artifact_id`), so a page boundary that lands inside a same-
    /// `started_at` group (common when many sessions share a unix second,
    /// or across the cross-kb merge) neither drops nor duplicates rows:
    /// the next page is `started_at < before OR (started_at = before AND
    /// artifact_id > before_id)`. A bare `before` without `before_id`
    /// (legacy clients) degrades to the old strict `started_at < before`.
    /// `before = None` is page 1 (no upper bound).
    /// W3.A — `project`/`substance` are threaded in SQL WHERE (before LIMIT),
    /// exactly like `folder`/`q` above — adversary S-S1(b): a route-side
    /// post-filter here would desync the keyset cursor (a page could drop or
    /// duplicate rows depending on how many matches land on each side of the
    /// filtered-out boundary). `project` is `None`/empty for "no filter";
    /// `substance` is a csv-parsed set — empty means "no filter" (every
    /// session shown, including `NULL` — un-backfilled rows are NEVER
    /// hidden). A `NULL substance` row always passes a non-empty filter too
    /// (S1/R4: treat `NULL` as `"substantive"`).
    // W3.A pushed this to 7 params (8 incl. `&self`) — one over clippy's
    // threshold. Every param is a distinct, independently-optional filter
    // axis (cursor pair, folder, q, project, substance); a synthetic
    // `SessionsListFilter` bag would just move the sprawl into a type no
    // other caller needs — an allow is the more honest signal here (the
    // kb-server boot path takes the same call on an 8-input fn, see its
    // "Eight inputs" comment).
    #[allow(clippy::too_many_arguments)]
    pub fn sessions_list(
        &self,
        limit: u32,
        before: Option<i64>,
        before_id: Option<String>,
        folder: Option<&str>,
        q: Option<&str>,
        project: &crate::sessions::ProjectFilter,
        substance: &[String],
        harness: &[String],
    ) -> Result<Vec<SessionRow>> {
        const COLS: &str = "artifact_id, session_id, started_at, ended_at,
                            message_count, first_user_prompt, source_relative,
                            title, cwd, git_branch, files_read_count, files_edited_count,
                            token_total, tool_calls, model, error_count,
                            subagent_count, subagent_tokens, subagent_tool_calls,
                            subagent_files_edited, subagent_launched_unstatted,
                            project_key, repo_root, harness, cc_version,
                            last_assistant_text, all_cwds, commit_count, user_turns,
                            active_secs, substance";
        // Build the WHERE dynamically: the optional keyset cursor and the
        // optional folder (A1) filter are ANDed; placeholder indices are
        // assigned in bind order so the two stay in lockstep. The folder
        // filter must be IN the SQL (before LIMIT) so keyset pagination
        // within a folder neither drops nor duplicates rows.
        let mut conds: Vec<String> = Vec::new();
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        // #11 — collapse multi-capture: a long session is captured at every
        // Stop, so one `session_id` accrues many `sessions` rows (one per
        // capture). Keep only the NEWEST capture per session_id — the same row
        // `sessions_get`/`sessions_get_many` return — so the list shows each
        // session once. No bind params, so the placeholder numbering below is
        // unaffected; the keyset cursor then paginates over the deduped set.
        // PF-R1 (V0040) — self-referential (unaliased `sessions` compared
        // against its own session_id group), so this reads the materialized
        // flag directly rather than going through `newest_capture_pred`'s
        // subquery.
        conds.push("is_newest = 1".to_string());
        match (before, before_id) {
            (Some(b), Some(id)) => {
                binds.push(Box::new(b));
                let p = binds.len();
                binds.push(Box::new(id));
                let pid = binds.len();
                conds.push(format!(
                    "(started_at < ?{p} OR (started_at = ?{p} AND artifact_id > ?{pid}))"
                ));
            }
            (Some(b), None) => {
                binds.push(Box::new(b));
                let p = binds.len();
                conds.push(format!("started_at < ?{p}"));
            }
            (None, _) => {}
        }
        if let Some(f) = folder {
            binds.push(Box::new(f.to_string()));
            let p = binds.len();
            conds.push(format!("cwd = ?{p}"));
        }
        // P6 — sessions-scoped keyword search: case-insensitive substring over
        // the title, first prompt, and cwd. In SQL (before LIMIT) so it
        // composes with the keyset cursor + folder filter.
        if let Some(query) = q.filter(|s| !s.trim().is_empty()) {
            let like = format!("%{}%", query.trim().replace('%', "\\%").replace('_', "\\_"));
            binds.push(Box::new(like));
            let p = binds.len();
            conds.push(format!(
                "(title LIKE ?{p} ESCAPE '\\' OR first_user_prompt LIKE ?{p} ESCAPE '\\' \
                  OR cwd LIKE ?{p} ESCAPE '\\')"
            ));
        }
        // W3.A/P4 — `project=`: `project_key IN (keys)` OR, for un-derived
        // rows only (`project_key IS NULL`), a `cwd` prefix match against the
        // registry's declared roots. Mirrors `ProjectFilter::matches` (the
        // Rust-side twin used by the non-paginated aggregate routes).
        if !project.is_empty() {
            let mut sub: Vec<String> = Vec::new();
            if !project.keys.is_empty() {
                let placeholders: Vec<String> = project
                    .keys
                    .iter()
                    .map(|k| {
                        binds.push(Box::new(k.clone()));
                        format!("?{}", binds.len())
                    })
                    .collect();
                sub.push(format!("project_key IN ({})", placeholders.join(",")));
            }
            if !project.root_prefixes.is_empty() {
                let mut prefix_conds: Vec<String> = Vec::new();
                for prefix in &project.root_prefixes {
                    let esc = prefix.replace('%', "\\%").replace('_', "\\_");
                    binds.push(Box::new(esc.clone()));
                    let p1 = binds.len();
                    binds.push(Box::new(format!("{esc}/%")));
                    let p2 = binds.len();
                    prefix_conds.push(format!(
                        "(cwd LIKE ?{p1} ESCAPE '\\' OR cwd LIKE ?{p2} ESCAPE '\\')"
                    ));
                }
                sub.push(format!(
                    "(project_key IS NULL AND ({}))",
                    prefix_conds.join(" OR ")
                ));
            }
            if !sub.is_empty() {
                conds.push(format!("({})", sub.join(" OR ")));
            }
        }
        // W3.A/S1 — `substance=`: csv set over the triage enum. `NULL`
        // (un-backfilled) rows pass whenever `"substantive"` is in the
        // requested set (R4: never hide un-backfilled history behind a husk
        // filter). An empty `substance` slice means "no filter" (handled by
        // simply not adding a condition).
        if !substance.is_empty() {
            let wants_substantive = substance.iter().any(|s| s == "substantive");
            let placeholders: Vec<String> = substance
                .iter()
                .map(|s| {
                    binds.push(Box::new(s.clone()));
                    format!("?{}", binds.len())
                })
                .collect();
            if wants_substantive {
                conds.push(format!(
                    "(substance IN ({}) OR substance IS NULL)",
                    placeholders.join(",")
                ));
            } else {
                conds.push(format!("substance IN ({})", placeholders.join(",")));
            }
        }
        // W5/I — `harness=`: csv set, closed-set validated at the route
        // (`routes::sessions::parse_harness_csv`) so every value here is
        // already a real `kb_core::sessions::HARNESSES` member — no NULL
        // special-casing needed, `harness` is `NOT NULL DEFAULT 'claude'`.
        if !harness.is_empty() {
            let placeholders: Vec<String> = harness
                .iter()
                .map(|h| {
                    binds.push(Box::new(h.clone()));
                    format!("?{}", binds.len())
                })
                .collect();
            conds.push(format!("harness IN ({})", placeholders.join(",")));
        }
        binds.push(Box::new(limit as i64));
        let lim = binds.len();
        let where_clause = if conds.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conds.join(" AND "))
        };
        let sql = format!(
            "SELECT {COLS} FROM sessions {where_clause} \
             ORDER BY started_at DESC, artifact_id ASC LIMIT ?{lim}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
                Self::map_session_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Distinct working directories (the A1 folder facet) for this kb, with
    /// per-folder rollups (count, activity span, files edited, tokens) — the
    /// P6 per-project timeline header stats. Newest-active first. Rows with no
    /// `cwd` (old un-reindexed sessions) are excluded.
    ///
    /// Each session is scoped to its NEWEST capture (#11) via
    /// [`newest_capture_pred`]: without it a multi-capture session inflated
    /// `count` (and `SUM(files_edited_count)` / `SUM(token_total)`) once per
    /// Stop — the folder facet's long-standing double-count.
    pub fn sessions_folders(&self) -> Result<Vec<FolderStats>> {
        // PF-R1 (V0040) — self-referential, reads the materialized flag
        // directly (see `sessions_list`'s identical note).
        let sql = "SELECT cwd, COUNT(*), MAX(started_at), MIN(started_at),
                    COALESCE(SUM(files_edited_count), 0), COALESCE(SUM(token_total), 0)
             FROM sessions
             WHERE cwd IS NOT NULL AND cwd <> ''
               AND is_newest = 1
             GROUP BY cwd
             ORDER BY MAX(started_at) DESC";
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(FolderStats {
                    cwd: r.get(0)?,
                    count: r.get(1)?,
                    latest: r.get(2)?,
                    earliest: r.get(3)?,
                    edited_total: r.get(4)?,
                    token_total: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// W3.A/P4 — the `/api/sessions/projects` facet: per-project rollups
    /// PRE-registry-merge, grouped by `COALESCE(project_key, cwd)` (the
    /// P1-derived key, or a raw cwd for a session whose ladder never
    /// resolved anything). The route folds these across kbs, then applies
    /// the `[projects.*]` registry (multiple raw keys can collapse into one
    /// declared project — e.g. several subdir cwds under one repo). Newest-
    /// capture scoped (#11) so a multi-Stop session isn't counted once per
    /// capture.
    pub fn sessions_projects_stats(&self) -> Result<Vec<ProjectStatsRow>> {
        // PF-R1 (V0040) — self-referential, reads the materialized flag
        // directly (see `sessions_list`'s identical note).
        let sql = "SELECT COALESCE(project_key, cwd) AS key,
                    MIN(repo_root), MIN(cwd),
                    COUNT(*), MAX(started_at), MIN(started_at),
                    COALESCE(SUM(files_edited_count), 0), COALESCE(SUM(token_total), 0),
                    COALESCE(SUM(commit_count), 0),
                    SUM(CASE WHEN error_count > 0 THEN 1 ELSE 0 END),
                    COALESCE(SUM(active_secs), 0)
             FROM sessions
             WHERE (project_key IS NOT NULL OR (cwd IS NOT NULL AND cwd <> ''))
               AND is_newest = 1
             GROUP BY COALESCE(project_key, cwd)
             ORDER BY MAX(started_at) DESC";
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ProjectStatsRow {
                    key: r.get(0)?,
                    repo_root: r.get(1)?,
                    cwd_sample: r.get(2)?,
                    count: r.get(3)?,
                    latest: r.get(4)?,
                    earliest: r.get(5)?,
                    edited_total: r.get(6)?,
                    token_total: r.get(7)?,
                    commit_total: r.get(8)?,
                    error_sessions: r.get(9)?,
                    active_secs_total: r.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// W3.A/P4 — the harness breakdown behind the `harness_mix` field of the
    /// SAME `/api/sessions/projects` facet, same grouping key + newest-
    /// capture scope as [`sessions_projects_stats`] (a second query rather
    /// than a JSON aggregate — rusqlite's bundled sqlite3 doesn't guarantee
    /// `json_group_object`, and this keeps the SQL portable).
    pub fn sessions_projects_harness_mix(&self) -> Result<Vec<ProjectHarnessRow>> {
        // PF-R1 (V0040) — self-referential, reads the materialized flag
        // directly (see `sessions_list`'s identical note).
        let sql = "SELECT COALESCE(project_key, cwd) AS key, harness, COUNT(*)
             FROM sessions
             WHERE (project_key IS NOT NULL OR (cwd IS NOT NULL AND cwd <> ''))
               AND is_newest = 1
             GROUP BY COALESCE(project_key, cwd), harness";
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ProjectHarnessRow {
                    key: r.get(0)?,
                    harness: r.get(1)?,
                    count: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// R9 — research queries aggregated by `(cwd, kind, query)`: count, distinct
    /// sessions, and latest activity, for the per-project research rollup.
    /// Scopes each session to its NEWEST capture (#11) — a multi-capture session
    /// re-records the same research rows under every capture, so a bare
    /// `COUNT(*)` would inflate the per-query count N×. Folder-less rows are
    /// excluded (same rule as `sessions_folders`). Deterministic order; the
    /// route does cross-kb merge + per-folder top-N.
    pub fn sessions_research_rollup(&self, substance: &[String]) -> Result<Vec<ResearchRollupRow>> {
        let mut conds: Vec<String> = Vec::new();
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        conds.push(newest_capture_pred("r.artifact_id_session", "r.session_id"));
        // L1/F1 — substance filter: mirrors sessions_list (NULL treated as substantive).
        if !substance.is_empty() {
            let wants_substantive = substance.iter().any(|s| s == "substantive");
            let placeholders: Vec<String> = substance
                .iter()
                .map(|s| {
                    binds.push(Box::new(s.clone()));
                    format!("?{}", binds.len())
                })
                .collect();
            if wants_substantive {
                conds.push(format!(
                    "(s.substance IN ({}) OR s.substance IS NULL)",
                    placeholders.join(",")
                ));
            } else {
                conds.push(format!("s.substance IN ({})", placeholders.join(",")));
            }
        }
        let where_clause = format!(
            "s.cwd IS NOT NULL AND s.cwd <> '' AND r.query <> '' AND ({})",
            conds.join(" AND ")
        );
        let sql = format!(
            "SELECT s.cwd, r.kind, r.query,
                    COUNT(*), COUNT(DISTINCT r.session_id), MAX(s.started_at),
                    MIN(s.project_key)
             FROM session_research r
             JOIN sessions s ON s.artifact_id = r.artifact_id_session
             WHERE {where_clause}
             GROUP BY s.cwd, r.kind, r.query
             ORDER BY s.cwd ASC, COUNT(*) DESC, r.query ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
                |r| {
                    Ok(ResearchRollupRow {
                        cwd: r.get(0)?,
                        kind: r.get(1)?,
                        query: r.get(2)?,
                        count: r.get(3)?,
                        sessions: r.get(4)?,
                        latest: r.get(5)?,
                        project_key: r.get(6)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// R9 — the activity-funnel stage counts from the session_* tables (the
    /// `commented` stage is added route-side from review files, #6). `folder`
    /// filters on `sessions.cwd`; `None` aggregates across every project. Each
    /// stage reports total events + distinct sessions reaching it. Each session
    /// is scoped to its NEWEST capture (#11) so the `COUNT(*)` event totals
    /// aren't inflated N× by the re-recorded child rows of older captures.
    /// "Detected, not ground truth" (#10): `opened` mixes research artifact_open
    /// events with file reads — a coarse signal, surfaced honestly.
    /// W3.A — `project` is threaded into the SAME dynamic-WHERE approach
    /// `sessions_list` uses (SQL, not a post-fetch filter): funnel counts are
    /// per-child-table `COUNT`s, not raw session rows, so there's nothing to
    /// post-filter after the fact. PF-R1: one query per CHILD TABLE (three
    /// total), each folding its own several metrics into a single
    /// conditional-aggregation pass — not 9 separate round trips.
    pub fn sessions_funnel_counts(
        &self,
        folder: Option<&str>,
        project: &crate::sessions::ProjectFilter,
        substance: &[String],
    ) -> Result<FunnelCounts> {
        // The project predicate is built ONCE (placeholders start at `?2`,
        // since `?1` is always `folder`) and reused by every `count()` call
        // below — mirrors `sessions_list`'s WHERE fragment + `ProjectFilter::
        // matches`'s semantics (`project_key IN (keys)` OR, for un-derived
        // rows only, a `cwd` prefix match against the registry roots).
        let mut project_binds: Vec<String> = Vec::new();
        let project_pred = if project.is_empty() {
            "1=1".to_string()
        } else {
            let mut sub: Vec<String> = Vec::new();
            if !project.keys.is_empty() {
                let placeholders: Vec<String> = project
                    .keys
                    .iter()
                    .map(|k| {
                        project_binds.push(k.clone());
                        format!("?{}", 1 + project_binds.len())
                    })
                    .collect();
                sub.push(format!("s.project_key IN ({})", placeholders.join(",")));
            }
            if !project.root_prefixes.is_empty() {
                let mut prefix_conds: Vec<String> = Vec::new();
                for prefix in &project.root_prefixes {
                    let esc = prefix.replace('%', "\\%").replace('_', "\\_");
                    project_binds.push(esc.clone());
                    let p1 = 1 + project_binds.len();
                    project_binds.push(format!("{esc}/%"));
                    let p2 = 1 + project_binds.len();
                    prefix_conds.push(format!(
                        "(s.cwd LIKE ?{p1} ESCAPE '\\' OR s.cwd LIKE ?{p2} ESCAPE '\\')"
                    ));
                }
                sub.push(format!(
                    "(s.project_key IS NULL AND ({}))",
                    prefix_conds.join(" OR ")
                ));
            }
            format!("({})", sub.join(" OR "))
        };
        let project_binds = &project_binds;
        // Build substance filter (same pattern as sessions_list).
        let mut substance_binds: Vec<String> = Vec::new();
        let substance_pred = if substance.is_empty() {
            "1=1".to_string()
        } else {
            let wants_substantive = substance.iter().any(|s| s == "substantive");
            let placeholders: Vec<String> = substance
                .iter()
                .map(|s| {
                    substance_binds.push(s.clone());
                    format!("?{}", 1 + project_binds.len() + substance_binds.len())
                })
                .collect();
            if wants_substantive {
                format!(
                    "(s.substance IN ({}) OR s.substance IS NULL)",
                    placeholders.join(",")
                )
            } else {
                format!("s.substance IN ({})", placeholders.join(","))
            }
        };
        let substance_binds = &substance_binds;
        // PF-R1 — the original shape here was 9 separate `count()` round
        // trips (one SELECT each), all sharing this same `?1`/project/
        // substance/newest WHERE fragment but scanning THREE different
        // child tables (session_research ×3 metrics, session_files ×4,
        // session_commits ×2). Collapsed to THREE conditional-aggregation
        // passes — one per child table, each folding its own metrics into a
        // single `SUM(CASE WHEN …)`/`COUNT(DISTINCT CASE WHEN …)` scan —
        // combined via a final cross join of their single-row CTEs (SQLite
        // numbered placeholders like `?1`/`{project_pred}`/`{substance_pred}`
        // may be referenced more than once in one statement and are bound
        // only once, exactly like the old `count()`'s per-call reuse of
        // `?1` for `folder`). `session_commits` genuinely has no per-metric
        // condition to fold (`committed_events`/`committed_sessions` were
        // already an unconditional `1=1` scan), so its CTE needs no CASE at
        // all.
        //
        // `SUM(CASE WHEN … THEN 1 ELSE 0 END)` returns SQL NULL — not 0 —
        // when a CTE's own FROM/JOIN/WHERE matches literally zero rows (a
        // folder/project/substance combination with no session_research or
        // session_files rows at all); `COUNT(*)`/`COUNT(DISTINCT …)` never
        // has this problem (they return 0 over an empty input). Every SUM
        // below is wrapped in `COALESCE(…, 0)` so this collapsed form
        // answers exactly what the original per-metric `COUNT`-filtered-by-
        // WHERE statements did in every case, not just the non-empty one
        // (`sessions_funnel_counts_overall_and_per_folder`'s
        // `b.edited_events == 0` case pins the empty one).
        let newest = newest_capture_pred("c.artifact_id_session", "c.session_id");
        let sql = format!(
            "WITH research_agg AS (
                SELECT
                    COALESCE(SUM(CASE WHEN c.kind IN ('kb_search','web') THEN 1 ELSE 0 END), 0)
                        AS searched_events,
                    COUNT(DISTINCT CASE WHEN c.kind IN ('kb_search','web') THEN c.session_id END)
                        AS searched_sessions,
                    COALESCE(SUM(CASE WHEN c.kind = 'artifact_open' THEN 1 ELSE 0 END), 0)
                        AS opened_events_research
                FROM session_research c
                JOIN sessions s ON s.artifact_id = c.artifact_id_session
                WHERE (?1 IS NULL OR s.cwd = ?1) AND {project_pred}
                  AND {substance_pred} AND {newest}
             ),
             files_agg AS (
                SELECT
                    COALESCE(SUM(CASE WHEN c.action = 'read' THEN 1 ELSE 0 END), 0)
                        AS opened_events_files,
                    COUNT(DISTINCT CASE WHEN c.action = 'read' THEN c.session_id END)
                        AS opened_sessions,
                    COUNT(DISTINCT CASE WHEN c.action IN ('edit','write') THEN c.path END)
                        AS edited_events,
                    COUNT(DISTINCT CASE WHEN c.action IN ('edit','write') THEN c.session_id END)
                        AS edited_sessions
                FROM session_files c
                JOIN sessions s ON s.artifact_id = c.artifact_id_session
                WHERE (?1 IS NULL OR s.cwd = ?1) AND {project_pred}
                  AND {substance_pred} AND {newest}
             ),
             commits_agg AS (
                SELECT
                    COUNT(*) AS committed_events,
                    COUNT(DISTINCT c.session_id) AS committed_sessions
                FROM session_commits c
                JOIN sessions s ON s.artifact_id = c.artifact_id_session
                WHERE (?1 IS NULL OR s.cwd = ?1) AND {project_pred}
                  AND {substance_pred} AND {newest}
             )
             SELECT
                research_agg.searched_events, research_agg.searched_sessions,
                research_agg.opened_events_research + files_agg.opened_events_files,
                files_agg.opened_sessions, files_agg.edited_events, files_agg.edited_sessions,
                commits_agg.committed_events, commits_agg.committed_sessions
             FROM research_agg, files_agg, commits_agg"
        );
        let mut st = self.conn.prepare(&sql)?;
        let mut binds: Vec<&dyn rusqlite::ToSql> = vec![&folder];
        for b in project_binds.iter() {
            binds.push(b);
        }
        for b in substance_binds.iter() {
            binds.push(b);
        }
        let counts = st.query_row(rusqlite::params_from_iter(binds), |r| {
            Ok(FunnelCounts {
                searched_events: r.get(0)?,
                searched_sessions: r.get(1)?,
                opened_events: r.get(2)?,
                opened_sessions: r.get(3)?,
                edited_events: r.get(4)?,
                edited_sessions: r.get(5)?,
                committed_events: r.get(6)?,
                committed_sessions: r.get(7)?,
            })
        })?;
        Ok(counts)
    }

    /// R9 — distinct in-corpus `(session_id, target_kb, target_artifact_id)`
    /// triples a folder's sessions touched, for the funnel's `commented` stage
    /// (the route loads each artifact's review file — comments live in
    /// `.review/*`, not sqlite, #6). `folder = None` spans every project.
    /// Newest capture only (#11) — multi-capture re-records the same edges.
    #[allow(clippy::type_complexity)]
    pub fn session_files_in_folder(
        &self,
        folder: Option<&str>,
    ) -> Result<Vec<(String, String, String)>> {
        let sql = format!(
            "SELECT DISTINCT f.session_id, f.target_kb, f.target_artifact_id
             FROM session_files f
             JOIN sessions s ON s.artifact_id = f.artifact_id_session
             WHERE f.in_corpus = 1 AND f.target_kb IS NOT NULL
               AND f.target_artifact_id IS NOT NULL
               AND (?1 IS NULL OR s.cwd = ?1)
               AND {}",
            newest_capture_pred("f.artifact_id_session", "f.session_id")
        );
        let mut st = self.conn.prepare_cached(&sql)?;
        let rows = st
            .query_map(params![folder], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Fetch a single enrichment row by its Claude Code session id.
    /// Returns the most recent row when multiple artifacts in this kb
    /// share a session id (rare — only one capture per session
    /// normally, but the indexer doesn't enforce uniqueness on
    /// session_id since artifact_id is the natural primary key).
    pub fn sessions_get(&self, session_id: &str) -> Result<Option<SessionRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT artifact_id, session_id, started_at, ended_at,
                    message_count, first_user_prompt, source_relative,
                    title, cwd, git_branch, files_read_count, files_edited_count,
                    token_total, tool_calls, model, error_count,
                    subagent_count, subagent_tokens, subagent_tool_calls,
                    subagent_files_edited, subagent_launched_unstatted,
                    project_key, repo_root, harness, cc_version,
                    last_assistant_text, all_cwds, commit_count, user_turns,
                    active_secs, substance
             FROM sessions
             WHERE session_id = ?1
             ORDER BY started_at DESC, artifact_id ASC
             LIMIT 1",
        )?;
        let row = stmt
            .query_row(params![session_id], Self::map_session_row)
            .optional()?;
        Ok(row)
    }

    /// Map a `sessions` row in the canonical column order shared by
    /// `sessions_list`/`sessions_get`/`sessions_get_many`/
    /// `sessions_get_by_artifact_ids` (V0008 + V0017 + V0029). The
    /// positional `get(N)` order MUST match the SELECT column list in every
    /// caller — a drift silently corrupts the mapping.
    fn map_session_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
        Ok(SessionRow {
            artifact_id: r.get(0)?,
            session_id: r.get(1)?,
            started_at: r.get(2)?,
            ended_at: r.get(3)?,
            message_count: i64_as_u32_sat(r.get::<_, i64>(4)?),
            first_user_prompt: r.get(5)?,
            source_relative: r.get(6)?,
            title: r.get(7)?,
            cwd: r.get(8)?,
            git_branch: r.get(9)?,
            files_read_count: i64_as_u32_sat(r.get::<_, i64>(10)?),
            files_edited_count: i64_as_u32_sat(r.get::<_, i64>(11)?),
            token_total: i64_as_u64_sat(r.get::<_, i64>(12)?),
            tool_calls: i64_as_u32_sat(r.get::<_, i64>(13)?),
            model: r.get(14)?,
            error_count: i64_as_u32_sat(r.get::<_, i64>(15)?),
            subagent_count: i64_as_u32_sat(r.get::<_, i64>(16)?),
            subagent_tokens: i64_as_u64_sat(r.get::<_, i64>(17)?),
            subagent_tool_calls: i64_as_u32_sat(r.get::<_, i64>(18)?),
            subagent_files_edited: i64_as_u32_sat(r.get::<_, i64>(19)?),
            subagent_launched_unstatted: i64_as_u32_sat(r.get::<_, i64>(20)?),
            project_key: r.get(21)?,
            repo_root: r.get(22)?,
            harness: r.get(23)?,
            cc_version: r.get(24)?,
            last_assistant_text: r.get(25)?,
            all_cwds: r.get(26)?,
            commit_count: i64_as_u32_sat(r.get::<_, i64>(27)?),
            user_turns: i64_as_u32_sat(r.get::<_, i64>(28)?),
            active_secs: r.get(29)?,
            substance: r.get(30)?,
        })
    }

    // --- Artifact snapshots (V0013, Track V) ----------------------------
    //
    // One row per *distinct* indexed revision of an artifact's source.
    // Appended by the indexer's `SnapshotCaptureHook` when the content hash
    // changes, then pruned to the newest N per artifact. Backs the
    // Versions/Diff timeline for corpora not under git (and the auto-mode
    // fallback for untracked files).

    /// The most recent snapshot's content hash for an artifact, or `None`
    /// when it has none yet. The capture hook short-circuits when this
    /// equals the incoming hash (a byte-identical reindex).
    pub fn snapshot_latest_hash(&self, artifact_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT content_hash FROM artifact_snapshots
             WHERE artifact_id = ?1
             ORDER BY captured_at DESC, id DESC
             LIMIT 1",
        )?;
        let h = stmt
            .query_row(params![artifact_id], |r| r.get::<_, String>(0))
            .optional()?;
        Ok(h)
    }

    /// Append a snapshot row for one revision.
    pub fn snapshot_insert(
        &mut self,
        artifact_id: &str,
        content_hash: &str,
        raw_source: &str,
        captured_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO artifact_snapshots
                (artifact_id, content_hash, raw_source, captured_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![artifact_id, content_hash, raw_source, captured_at],
        )?;
        Ok(())
    }

    /// Newest-first snapshot metadata (no `raw_source` body — the timeline
    /// list stays cheap) for an artifact, capped at `limit`.
    pub fn snapshot_list(&self, artifact_id: &str, limit: u32) -> Result<Vec<SnapshotMeta>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, content_hash, captured_at FROM artifact_snapshots
             WHERE artifact_id = ?1
             ORDER BY captured_at DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![artifact_id, limit as i64], |r| {
                Ok(SnapshotMeta {
                    id: r.get(0)?,
                    content_hash: r.get(1)?,
                    captured_at: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The verbatim source text of one snapshot, by row id.
    pub fn snapshot_raw(&self, id: i64) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT raw_source FROM artifact_snapshots WHERE id = ?1")?;
        let raw = stmt
            .query_row(params![id], |r| r.get::<_, String>(0))
            .optional()?;
        Ok(raw)
    }

    /// Prune all but the newest `keep` snapshots for an artifact. Returns
    /// the number of rows deleted.
    pub fn snapshot_prune(&mut self, artifact_id: &str, keep: u32) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM artifact_snapshots
             WHERE artifact_id = ?1
               AND id NOT IN (
                 SELECT id FROM artifact_snapshots
                 WHERE artifact_id = ?1
                 ORDER BY captured_at DESC, id DESC
                 LIMIT ?2
               )",
            params![artifact_id, keep as i64],
        )?;
        Ok(n)
    }

    /// Drop every snapshot for an artifact — called by the indexer's delete
    /// pass when the source file is unlinked. Returns rows deleted.
    pub fn snapshots_delete_for_artifact(&mut self, artifact_id: &str) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM artifact_snapshots WHERE artifact_id = ?1",
            params![artifact_id],
        )?;
        Ok(n)
    }

    // --- F3a relocate: moves intent log + id rekey ----------------------

    /// Append a moves INTENT row (`completed_at` NULL). Call BEFORE any FS
    /// rename or storage mutation so a crash mid-relocate still leaves a
    /// durable suppress signal for the old path.
    pub fn moves_insert_intent(
        &mut self,
        old_id: &str,
        new_id: &str,
        old_rel: &str,
        new_rel: &str,
        moved_at: i64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO moves (old_id, new_id, old_rel, new_rel, moved_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![old_id, new_id, old_rel, new_rel, moved_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Stamp `completed_at` on one moves row (also done inside
    /// [`Self::cascade_relocate_doc`]; exposed for the abandon path).
    pub fn moves_mark_completed(&mut self, row_id: i64, completed_at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE moves SET completed_at = ?1 WHERE id = ?2",
            params![completed_at, row_id],
        )?;
        Ok(())
    }

    /// F3a — resolve a prior path or id through the moves log. Newest row
    /// wins, so a chain A→B then B→C is followed to completion (walk the
    /// chain, not just the latest single hop). Accepts either an old_id or
    /// an old_rel as `key`.
    ///
    /// Callers consult `moves` only after a live miss — the seed is a stale
    /// id/rel, and the walk aims for the live endpoint of the chain.
    pub fn moves_lookup(&self, key: &str) -> Result<Option<(String, String)>> {
        // Walk the chain (bounded) so A→B→C resolves A to C.
        // Seed: match either old_id or old_rel on the newest row.
        // Return seed's old_id so the cycle guard can treat the hop origin
        // as already visited (A→B then B→A must not ping-pong).
        let seed: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT old_id, new_id, new_rel FROM moves
                 WHERE old_id = ?1 OR old_rel = ?1
                 ORDER BY id DESC LIMIT 1",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((seed_old_id, mut cur_id, mut cur_rel)) = seed else {
            return Ok(None);
        };
        // Visited = hop origins we've already left (incl. the seed old_id).
        // When the next hop lands on a visited id, STOP at the CURRENT id
        // (breaks A↔B cycles without spinning 64 hops on the wrong end).
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        visited.insert(seed_old_id);
        // Follow new_id → further moves (newest first per hop).
        for _ in 0..64 {
            let next: Option<(String, String)> = self
                .conn
                .query_row(
                    "SELECT new_id, new_rel FROM moves
                     WHERE old_id = ?1
                     ORDER BY id DESC LIMIT 1",
                    params![cur_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match next {
                Some((nid, nrel)) if nid != cur_id => {
                    if visited.contains(&nid) {
                        break; // cycle — stay at current
                    }
                    visited.insert(cur_id.clone());
                    cur_id = nid;
                    cur_rel = nrel;
                }
                _ => break,
            }
        }
        Ok(Some((cur_id, cur_rel)))
    }

    /// True when a delete of `old_rel` should be suppressed: an incomplete
    /// intent row, or a row completed within `grace_secs` (covers the
    /// watcher debounce window after a successful relocate).
    pub fn moves_suppresses_delete(
        &self,
        old_rel: &str,
        now_unix: i64,
        grace_secs: i64,
    ) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM moves
             WHERE old_rel = ?1
               AND (completed_at IS NULL
                    OR completed_at >= ?2)",
            params![old_rel, now_unix - grace_secs],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Incomplete intent rows (`completed_at IS NULL`), oldest first — the
    /// startup replay walk order.
    pub fn moves_list_incomplete(&self) -> Result<Vec<MoveRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, old_id, new_id, old_rel, new_rel, moved_at, completed_at
             FROM moves WHERE completed_at IS NULL ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(MoveRow {
                id: r.get(0)?,
                old_id: r.get(1)?,
                new_id: r.get(2)?,
                old_rel: r.get(3)?,
                new_rel: r.get(4)?,
                moved_at: r.get(5)?,
                completed_at: r.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| crate::Error::Storage(format!("moves_list_incomplete: {e}")))
    }

    /// F3a — re-key every artifact-referencing sqlite table from `old_id` to
    /// `new_id` (and `old_rel`→`new_rel` on path-keyed tables) in ONE
    /// transaction, and stamp the moves intent row completed. Mirrors
    /// [`Self::cascade_delete_doc`]'s table set but UPDATE instead of DELETE,
    /// plus `list_entries`, `atlas_snapshot_points`, and `excluded_files`
    /// (which the delete cascade deliberately skips or path-keys).
    ///
    /// UNIQUE-conflict strategy: `UPDATE OR IGNORE` then `DELETE` leftovers
    /// under the old key, so a row already present at `new_id` wins and the
    /// orphaned old-key row is dropped (prefer keeping destination state) —
    /// **except `list_entries`**, which uses `UPDATE OR REPLACE` so the live
    /// source entry (note/position/read_override) replaces a stale tombstone
    /// already at `new_id` (V0015 `idx_list_entries_dedupe`). Positions are
    /// not renumbered (a hole is fine; invariant #25).
    ///
    /// Returns the DISTINCT `list_id`s whose entries were rekeyed (for
    /// `list.updated` SSE after the move).
    pub fn cascade_relocate_doc(
        &mut self,
        old_id: &str,
        new_id: &str,
        old_rel: &str,
        new_rel: &str,
        moves_row_id: i64,
        completed_at: i64,
    ) -> Result<Vec<String>> {
        let tx = self.conn.transaction()?;

        // --- edges (both directions; PK is src,dst,kind) ---
        // Drop rows that would collide after the src rekey, then rekey.
        tx.execute(
            "DELETE FROM edges WHERE src_artifact = ?1 AND EXISTS (
                SELECT 1 FROM edges e2
                WHERE e2.src_artifact = ?2 AND e2.dst_artifact = edges.dst_artifact
                  AND e2.kind = edges.kind
             )",
            params![old_id, new_id],
        )?;
        tx.execute(
            "UPDATE edges SET src_artifact = ?2 WHERE src_artifact = ?1",
            params![old_id, new_id],
        )?;
        tx.execute(
            "DELETE FROM edges WHERE dst_artifact = ?1 AND EXISTS (
                SELECT 1 FROM edges e2
                WHERE e2.dst_artifact = ?2 AND e2.src_artifact = edges.src_artifact
                  AND e2.kind = edges.kind
             )",
            params![old_id, new_id],
        )?;
        tx.execute(
            "UPDATE edges SET dst_artifact = ?2 WHERE dst_artifact = ?1",
            params![old_id, new_id],
        )?;

        // --- simple PK-on-artifact_id tables ---
        // doc_first_seen: rekey old→new; OR IGNORE on collision keeps the
        // destination's earlier first-seen (prefer keeping destination state).
        for table in [
            "corkboard",
            "pinned_memories",
            "memory_links_seeded",
            "doc_first_seen",
            "code_refs_docs",
        ] {
            // memory_links + code_refs have composite PKs — handled below.
            tx.execute(
                &format!("UPDATE OR IGNORE {table} SET artifact_id = ?2 WHERE artifact_id = ?1"),
                params![old_id, new_id],
            )?;
            tx.execute(
                &format!("DELETE FROM {table} WHERE artifact_id = ?1"),
                params![old_id],
            )?;
        }

        // code_refs: PK (artifact_id, ordinal). `UPDATE OR IGNORE` keeps a row
        // already present at `new_id` (destination wins, same rule as every
        // table above), then the leftovers under the old key are dropped.
        // Relocate deliberately preserves the embedding without re-indexing
        // (invariant #27/F3), so nothing would ever regenerate these rows —
        // leaving them under the dead id makes `kb refs <new-id>` return empty
        // forever and renders as doc-rot.
        tx.execute(
            "UPDATE OR IGNORE code_refs SET artifact_id = ?2 WHERE artifact_id = ?1",
            params![old_id, new_id],
        )?;
        tx.execute(
            "DELETE FROM code_refs WHERE artifact_id = ?1",
            params![old_id],
        )?;

        // memory_links: PK (artifact_id, linked_kb)
        tx.execute(
            "UPDATE OR IGNORE memory_links SET artifact_id = ?2 WHERE artifact_id = ?1",
            params![old_id, new_id],
        )?;
        tx.execute(
            "DELETE FROM memory_links WHERE artifact_id = ?1",
            params![old_id],
        )?;

        // artifact_snapshots: no unique on artifact_id alone — plain UPDATE.
        tx.execute(
            "UPDATE artifact_snapshots SET artifact_id = ?2 WHERE artifact_id = ?1",
            params![old_id, new_id],
        )?;

        // CT-F1 memory_commits: PK is (memory_id, sha_full), so `artifact_id`
        // carries no uniqueness — a plain UPDATE can't collide (same shape as
        // `artifact_snapshots` above). Relocate never re-indexes (#27/F3), so
        // nothing would ever regenerate these rows; leaving them under the
        // dead capture id would strand the memory→commit join forever and
        // silently defeat the cascade/sweep registrations too (both key on
        // this same column).
        //
        // `memory_id` is deliberately NOT rekeyed: it names a memory that
        // usually lives in a DIFFERENT kb than the one being relocated here,
        // and artifact ids can collide across corpora (#7 v2 / #28 v2), so a
        // same-kb rewrite would be a guess that could corrupt another
        // corpus's claim. Same posture — and the same accepted limitation —
        // as `memory_recalls.memory_id`: relocating a MEMORY (as opposed to a
        // capture) leaves its citations attached to the old id.
        tx.execute(
            "UPDATE memory_commits SET artifact_id = ?2 WHERE artifact_id = ?1",
            params![old_id, new_id],
        )?;

        // sessions children first (convention FK on artifact_id_session), then
        // sessions PK itself.
        for table in ["session_decisions", "session_commits", "session_research"] {
            tx.execute(
                &format!(
                    "UPDATE OR IGNORE {table} SET artifact_id_session = ?2 \
                     WHERE artifact_id_session = ?1"
                ),
                params![old_id, new_id],
            )?;
            tx.execute(
                &format!("DELETE FROM {table} WHERE artifact_id_session = ?1"),
                params![old_id],
            )?;
        }
        // session_files: rekey session PK AND reverse target_artifact_id.
        tx.execute(
            "UPDATE OR IGNORE session_files SET artifact_id_session = ?2 \
             WHERE artifact_id_session = ?1",
            params![old_id, new_id],
        )?;
        tx.execute(
            "DELETE FROM session_files WHERE artifact_id_session = ?1",
            params![old_id],
        )?;
        tx.execute(
            "UPDATE session_files SET target_artifact_id = ?2 \
             WHERE target_artifact_id = ?1",
            params![old_id, new_id],
        )?;

        // PF-R1 (V0040) — `is_newest`/`session_id` are deliberately NOT
        // rekeyed here: this UPDATE only ever rewrites `artifact_id` (and
        // `source_relative` below), so the materialized newest-capture flag
        // rides the row through the relocate untouched, same as every other
        // un-mentioned column. `cascade_relocate_doc_preserves_is_newest_flag`
        // pins it.
        tx.execute(
            "UPDATE OR IGNORE sessions SET artifact_id = ?2 WHERE artifact_id = ?1",
            params![old_id, new_id],
        )?;
        tx.execute(
            "DELETE FROM sessions WHERE artifact_id = ?1",
            params![old_id],
        )?;
        // sessions also carries source_relative — rewrite when it matched.
        tx.execute(
            "UPDATE sessions SET source_relative = ?2 WHERE source_relative = ?1",
            params![old_rel, new_rel],
        )?;

        // reading_sections + history (nullable artifact_id on history).
        tx.execute(
            "UPDATE reading_sections SET artifact_id = ?2 WHERE artifact_id = ?1",
            params![old_id, new_id],
        )?;
        tx.execute(
            "UPDATE history SET artifact_id = ?2 \
             WHERE artifact_id IS NOT NULL AND artifact_id = ?1",
            params![old_id, new_id],
        )?;

        // list_entries: collect affected list_ids BEFORE rekey (for SSE).
        // UPDATE OR REPLACE: source live entry wins over a tombstone already
        // at new_id (UNIQUE idx_list_entries_dedupe); positions not renumbered.
        let mut list_ids: Vec<String> = {
            let mut stmt = tx.prepare_cached(
                "SELECT DISTINCT list_id FROM list_entries WHERE artifact_id = ?1",
            )?;
            let rows = stmt.query_map(params![old_id], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| crate::Error::Storage(format!("list_entries list_ids: {e}")))?
        };
        list_ids.sort();
        list_ids.dedup();
        tx.execute(
            "UPDATE OR REPLACE list_entries SET artifact_id = ?2 WHERE artifact_id = ?1",
            params![old_id, new_id],
        )?;

        // atlas_snapshot_points: PK (snapshot_id, artifact_id).
        tx.execute(
            "UPDATE OR IGNORE atlas_snapshot_points SET artifact_id = ?2 \
             WHERE artifact_id = ?1",
            params![old_id, new_id],
        )?;
        tx.execute(
            "DELETE FROM atlas_snapshot_points WHERE artifact_id = ?1",
            params![old_id],
        )?;

        // excluded_files: path PK, old_rel → new_rel exact match.
        tx.execute(
            "UPDATE OR IGNORE excluded_files SET path = ?2 WHERE path = ?1",
            params![old_rel, new_rel],
        )?;
        tx.execute(
            "DELETE FROM excluded_files WHERE path = ?1",
            params![old_rel],
        )?;

        // Stamp the intent row completed in the SAME transaction.
        tx.execute(
            "UPDATE moves SET completed_at = ?1 WHERE id = ?2",
            params![completed_at, moves_row_id],
        )?;

        tx.commit()?;
        Ok(list_ids)
    }

    // --- R2 delete cascade + orphan sweep -------------------------------

    /// R2 — the sqlite side of the per-artifact delete cascade. Prunes every
    /// table in [`CASCADE_STEPS`] whose mode-gate includes `mode`, inside ONE
    /// transaction (all-or-nothing across the sqlite tables; the lance doc +
    /// chunk rows are dropped separately and FIRST by the storage actor,
    /// mirroring the `DropKbData` ordering). The step list is data-driven by
    /// [`CASCADE_STEPS`] so the touched-table set cannot drift from what
    /// [`cascade_cleanup_tables`] reports and the golden test pins.
    ///
    /// Returns the `sessions` row count (so the caller emits `session.deleted`,
    /// preserving the pre-R2 SSE surface). `KeepUserData` skips the two
    /// user-data tables (`history` + `reading_sections`); everything else goes
    /// in both modes. `list_entries` is NOT touched by either mode — a deleted
    /// artifact's list entry survives as a read-time tombstone.
    pub fn cascade_delete_doc(
        &mut self,
        artifact_id: &str,
        mode: crate::cascade::CascadeMode,
    ) -> Result<CascadeDbOutcome> {
        let tx = self.conn.transaction()?;
        let mut out = CascadeDbOutcome::default();
        for step in CASCADE_STEPS {
            if !step.applies(mode) {
                continue;
            }
            match step.shape {
                // Table names come from the compile-time `CASCADE_STEPS`
                // const, never user input — the interpolation is injection-safe.
                DeleteShape::ByArtifactId => {
                    // PF-R1 (V0040) — the `sessions` step also carries the
                    // materialized `is_newest` flag: read the row's
                    // session_id BEFORE the delete (the row being removed
                    // may be its group's currently-flagged newest capture)
                    // so the group can be re-derived after.
                    let sessions_group: Option<String> = if step.table == "sessions" {
                        tx.query_row(
                            "SELECT session_id FROM sessions WHERE artifact_id = ?1",
                            params![artifact_id],
                            |r| r.get(0),
                        )
                        .optional()?
                    } else {
                        None
                    };
                    let n = tx.execute(
                        &format!("DELETE FROM {} WHERE artifact_id = ?1", step.table),
                        params![artifact_id],
                    )?;
                    if step.table == "sessions" {
                        out.sessions_removed = n;
                        if let Some(sid) = sessions_group {
                            recompute_is_newest(&tx, &sid)?;
                        }
                    }
                    out.total_rows += n;
                }
                DeleteShape::ByArtifactIdSession => {
                    out.total_rows += tx.execute(
                        &format!("DELETE FROM {} WHERE artifact_id_session = ?1", step.table),
                        params![artifact_id],
                    )?;
                }
                DeleteShape::EdgesBothDirections => {
                    // Both directions: the deleted doc's outbound links AND the
                    // backlinks pointing at it are now orphaned (invariant: an
                    // explicit single-artifact delete cleans both immediately;
                    // the reconcile sweep only owns dead-`src` edges — see there).
                    out.total_rows += tx.execute(
                        "DELETE FROM edges WHERE src_artifact = ?1 OR dst_artifact = ?1",
                        params![artifact_id],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(out)
    }

    /// R2 — reconcile orphan backstop. Given the set of artifact ids to KEEP
    /// (every live lance id, unioned with any X2-exempt ids by the caller),
    /// prune the sqlite dependent tables the pre-v0.24 delete path leaked —
    /// `edges` (by dead `src`), `corkboard`, `pinned_memories`,
    /// `history` + `reading_sections` — of every row whose artifact id is NOT
    /// in `keep`. `list_entries` is excluded: an entry pointing at a deleted
    /// artifact is an intentional tombstone, not a leak. Cheap when clean: one
    /// `SELECT DISTINCT`
    /// scan + a set diff per table, and DELETEs only the genuine orphans.
    ///
    /// `sessions`/`artifact_snapshots`/`memory_links` are intentionally absent:
    /// the pre-R2 `process_delete` already cleaned those on unlink, so they
    /// never accumulated orphans (and sweeping `sessions` would have to reason
    /// about multi-capture, invariant #11 — out of scope for a leak backstop).
    /// Edges are swept by dead `src` only: a live-`src` → dead-`dst` edge is
    /// owned by `src`'s reindex (`record_edges` replaces the outbound set), so
    /// sweeping it would churn against edges the edge-record hook re-derives.
    pub fn sweep_orphans(
        &mut self,
        keep: &std::collections::HashSet<String>,
    ) -> Result<SweepOutcome> {
        let tx = self.conn.transaction()?;
        let mut out = SweepOutcome::default();
        for (table, col) in SWEEP_TABLES {
            // DISTINCT ids present in this table. `IS NOT NULL` is load-bearing
            // for `history` (its `artifact_id` is NULL on search rows, which
            // are not tied to any artifact and must NEVER be swept).
            let mut stmt = tx.prepare_cached(&format!(
                "SELECT DISTINCT {col} FROM {table} WHERE {col} IS NOT NULL"
            ))?;
            let present: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            let orphans: Vec<String> = present
                .into_iter()
                .filter(|id| !keep.contains(id))
                .collect();
            if orphans.is_empty() {
                continue;
            }
            // Delete the orphans (chunked IN-list so the statement stays bounded;
            // orphan counts are small — just the leaked ids).
            let mut removed = 0usize;
            for chunk in orphans.chunks(500) {
                let placeholders = vec!["?"; chunk.len()].join(",");
                let sql = format!("DELETE FROM {table} WHERE {col} IN ({placeholders})");
                removed += tx.execute(&sql, rusqlite::params_from_iter(chunk.iter()))?;
            }
            out.total_rows += removed;
            match *table {
                "edges" => out.edges_removed += removed,
                "corkboard" => out.corkboard_removed += removed,
                _ => {}
            }
        }
        tx.commit()?;
        Ok(out)
    }
}

// --- R2 delete-cascade table registry ----------------------------------------

/// How one cascaded table finds the rows to drop.
#[derive(Clone, Copy)]
enum DeleteShape {
    /// `DELETE FROM <table> WHERE artifact_id = ?1`
    ByArtifactId,
    /// `DELETE FROM <table> WHERE artifact_id_session = ?1` (the session
    /// child tables key on the sessions PK, not `session_id`).
    ByArtifactIdSession,
    /// `DELETE FROM edges WHERE src_artifact = ?1 OR dst_artifact = ?1`
    EdgesBothDirections,
}

/// One artifact-referencing table the per-artifact cascade prunes.
struct CascadeStep {
    table: &'static str,
    shape: DeleteShape,
    /// `true` ⇒ pruned in BOTH modes. `false` ⇒ a user-data table that
    /// [`crate::cascade::CascadeMode::KeepUserData`] preserves (today:
    /// `history` + `reading_sections`, mirrored on disk by the kept
    /// `.review` sidecar).
    keep_user_data_prunes: bool,
}

impl CascadeStep {
    fn applies(&self, mode: crate::cascade::CascadeMode) -> bool {
        matches!(mode, crate::cascade::CascadeMode::Full) || self.keep_user_data_prunes
    }
}

/// THE SINGLE SOURCE OF TRUTH for the per-artifact delete cascade's table set.
/// [`Db::cascade_delete_doc`] iterates this to build its transaction and
/// [`cascade_cleanup_tables`] projects it for the golden enumeration test —
/// so the two can never drift. Adding a new artifact-referencing table = one
/// entry here, which changes `cascade_cleanup_tables()` output and forces the
/// golden test's literal list to be updated consciously.
const CASCADE_STEPS: &[CascadeStep] = &[
    CascadeStep {
        table: "edges",
        shape: DeleteShape::EdgesBothDirections,
        keep_user_data_prunes: true,
    },
    CascadeStep {
        table: "corkboard",
        shape: DeleteShape::ByArtifactId,
        keep_user_data_prunes: true,
    },
    CascadeStep {
        table: "doc_first_seen",
        shape: DeleteShape::ByArtifactId,
        keep_user_data_prunes: true,
    },
    CascadeStep {
        table: "pinned_memories",
        shape: DeleteShape::ByArtifactId,
        keep_user_data_prunes: true,
    },
    CascadeStep {
        table: "memory_links",
        shape: DeleteShape::ByArtifactId,
        keep_user_data_prunes: true,
    },
    CascadeStep {
        table: "memory_links_seeded",
        shape: DeleteShape::ByArtifactId,
        keep_user_data_prunes: true,
    },
    CascadeStep {
        table: "artifact_snapshots",
        shape: DeleteShape::ByArtifactId,
        keep_user_data_prunes: true,
    },
    // DCB W1.A — code refs are DERIVED from the doc's bytes, never authored,
    // so `KeepUserData` must still drop them (same call `artifact_snapshots`
    // makes). Kept beside the other index-derived sibling tables.
    CascadeStep {
        table: "code_refs",
        shape: DeleteShape::ByArtifactId,
        keep_user_data_prunes: true,
    },
    CascadeStep {
        table: "code_refs_docs",
        shape: DeleteShape::ByArtifactId,
        keep_user_data_prunes: true,
    },
    // CT-F1 — `memory_commits` rows are DERIVED from a capture's own commit
    // trailers, never authored, so `KeepUserData` drops them too (the call
    // `code_refs`/`artifact_snapshots` make). The column is `artifact_id`
    // (the CAPTURE's id), not `memory_id`: deleting a capture retires the
    // claims that capture recorded; deleting the MEMORY is a different
    // lifecycle in (usually) a different kb, and is deliberately not
    // cascaded here — see the migration header.
    CascadeStep {
        table: "memory_commits",
        shape: DeleteShape::ByArtifactId,
        keep_user_data_prunes: true,
    },
    CascadeStep {
        table: "sessions",
        shape: DeleteShape::ByArtifactId,
        keep_user_data_prunes: true,
    },
    CascadeStep {
        table: "session_files",
        shape: DeleteShape::ByArtifactIdSession,
        keep_user_data_prunes: true,
    },
    CascadeStep {
        table: "session_decisions",
        shape: DeleteShape::ByArtifactIdSession,
        keep_user_data_prunes: true,
    },
    CascadeStep {
        table: "session_commits",
        shape: DeleteShape::ByArtifactIdSession,
        keep_user_data_prunes: true,
    },
    CascadeStep {
        table: "session_research",
        shape: DeleteShape::ByArtifactIdSession,
        keep_user_data_prunes: true,
    },
    // NB: `list_entries` is deliberately ABSENT. A reading-list entry whose
    // artifact was deleted must SURVIVE and render as a tombstone (derived at
    // read time from the missing lance doc — `routes/lists.rs`), so artifact
    // deletion never prunes list entries. They are removed only by explicit
    // user action (remove-entry / delete-list), never by the cascade or sweep.
    // User-data tables — kept by `KeepUserData`. `reading_sections` before
    // `history` so it's gone before its FK parent (robust even if the
    // `foreign_keys` pragma / ON DELETE CASCADE is ever off).
    CascadeStep {
        table: "reading_sections",
        shape: DeleteShape::ByArtifactId,
        keep_user_data_prunes: false,
    },
    CascadeStep {
        table: "history",
        shape: DeleteShape::ByArtifactId,
        keep_user_data_prunes: false,
    },
];

/// `(table, orphan-key column)` for the reconcile orphan sweep — the exact set
/// the pre-v0.24 `process_delete` leaked. [`Db::sweep_orphans`] iterates this;
/// [`sweep_cleanup_tables`] projects it for the golden test. Adding a table =
/// one entry here, which forces the golden test's literal to be updated.
const SWEEP_TABLES: &[(&str, &str)] = &[
    ("edges", "src_artifact"),
    ("corkboard", "artifact_id"),
    ("pinned_memories", "artifact_id"),
    ("reading_sections", "artifact_id"),
    ("history", "artifact_id"),
    // DCB W1.A — derived rows; an orphan here is a leak, never a tombstone.
    ("code_refs", "artifact_id"),
    ("code_refs_docs", "artifact_id"),
    // CT-F1 — same class: derived from a capture that no longer exists.
    // Swept on `artifact_id` ONLY. `memory_id` is deliberately NOT a sweep
    // key: it names a memory in (usually) a DIFFERENT kb, so this kb's
    // `keep` set — every live lance id HERE — would classify every single
    // row as an orphan and wipe the table on the first reconcile.
    ("memory_commits", "artifact_id"),
    // `list_entries` deliberately absent — see the note in `CASCADE_STEPS`:
    // an entry pointing at a deleted artifact is an intentional tombstone,
    // not a leak, so the orphan sweep must never reclaim it.
];

/// R2 — the exact ordered list of sqlite tables [`Db::cascade_delete_doc`]
/// touches for `mode`. Projected from [`CASCADE_STEPS`] (the transaction's own
/// driver) so the golden test and the code can't diverge.
pub fn cascade_cleanup_tables(mode: crate::cascade::CascadeMode) -> Vec<&'static str> {
    CASCADE_STEPS
        .iter()
        .filter(|s| s.applies(mode))
        .map(|s| s.table)
        .collect()
}

/// R2 — the exact list of sqlite tables the reconcile orphan sweep prunes.
/// Projected from [`SWEEP_TABLES`] (the sweep's own driver).
pub fn sweep_cleanup_tables() -> Vec<&'static str> {
    SWEEP_TABLES.iter().map(|(t, _)| *t).collect()
}

// --- Row types ---------------------------------------------------------------

/// One row from the F3a `moves` intent log (V0032).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRow {
    pub id: i64,
    pub old_id: String,
    pub new_id: String,
    pub old_rel: String,
    pub new_rel: String,
    pub moved_at: i64,
    pub completed_at: Option<i64>,
}

/// R2 — result of [`Db::cascade_delete_doc`].
#[derive(Debug, Clone, Default)]
pub struct CascadeDbOutcome {
    /// Rows removed from the `sessions` table (>0 ⇒ caller emits
    /// `session.deleted`, preserving the pre-R2 SSE).
    pub sessions_removed: usize,
    /// Total sqlite rows removed across every cascaded table.
    pub total_rows: usize,
}

/// R2 — result of [`Db::sweep_orphans`].
#[derive(Debug, Clone, Default)]
pub struct SweepOutcome {
    /// Orphaned `edges` rows removed — a non-zero value bumps the index
    /// generation (edge-count / gallery inputs changed).
    pub edges_removed: usize,
    /// Orphaned `corkboard` rows removed — also a generation input.
    pub corkboard_removed: usize,
    /// Total sqlite rows removed across every swept table (for the summary log).
    pub total_rows: usize,
}

#[derive(Debug, Clone)]
pub struct SourceRow {
    /// Internal slug as stored in the DB (string form).
    pub raw_slug: String,
    /// Round-trippable parsed slug. Until we have a `SourceSlug::parse` for
    /// already-normalised strings, callers can use `raw_slug` directly.
    pub slug: SourceSlug,
    pub path: PathBuf,
    pub added_at_unix: i64,
    pub paused: bool,
}

/// One `excluded_files` row (X2): a durable per-file exclusion.
#[derive(Debug, Clone)]
pub struct ExclusionRow {
    /// Source-relative, forward-slash path (normalised at write time).
    pub path: String,
    pub excluded_at_unix: i64,
    pub note: Option<String>,
}

/// One `atlas_labels` row (W1.B): one ranked c-TF-IDF term for one atlas
/// cluster. Mirrors `kb_core::atlas_labels::TermScore` plus the storage
/// key (`cluster`, `rank`) and the write-time stamp (`computed_at`) — see
/// `Db::set_atlas_labels`/`Db::atlas_labels`.
#[derive(Debug, Clone, PartialEq)]
pub struct AtlasLabelRow {
    /// Atlas cluster id (matches the lance `atlas_cluster` i16 column).
    pub cluster: i16,
    /// 1-based rank within the cluster (score desc / term asc tiebreak).
    pub rank: i64,
    pub term: String,
    /// Term count within this cluster.
    pub tf: f64,
    /// Term count across ALL clusters.
    pub ft: f64,
    /// `tf * ln(1 + A/ft)`.
    pub score: f64,
    /// Unix seconds — caller-supplied at write time (no clock inside the
    /// deterministic `atlas_labels::compute` path).
    pub computed_at: i64,
}

/// How many atlas frames a kb retains. The time-lapse is a recent-drift
/// scrubber, not an archive: 24 frames is roughly a month of daily
/// recomputes, and each frame stores one row per artifact, so the bound is
/// what keeps the sidecar from growing with `corpus_size × recompute_count`.
pub const DEFAULT_ATLAS_FRAMES_KEEP: usize = 24;

/// Provenance of an atlas frame (`atlas_snapshots.provenance`).
///
/// `Recorded` is what the recompute/recluster path writes: the frame was
/// captured BY the run that produced those coordinates — first-hand.
///
/// `Reconstructed` (W3 T-d, written by
/// `atlas::backfill_reconstructed_frames`) is a frame derived AFTER the fact:
/// today's embeddings laid out over the doc subset that existed at a past
/// `mtime_unix` cut point. **It is not history.** kb retains no past layout
/// and no past embedding, so a true historical layout cannot be re-derived;
/// a reconstructed frame answers the weaker question "where would these docs
/// have sat, if I had run the atlas then, knowing what I know now". Every
/// surface that shows a frame MUST show this word — a time-lapse that can't
/// tell first-hand from reconstructed is a lie by omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameProvenance {
    Recorded,
    Reconstructed,
}

impl FrameProvenance {
    pub fn as_str(self) -> &'static str {
        match self {
            FrameProvenance::Recorded => "recorded",
            FrameProvenance::Reconstructed => "reconstructed",
        }
    }
}

/// The caller-supplied half of an atlas frame — everything that ISN'T
/// derived from the points themselves (`point_count`, `cluster_count` and
/// `coord_hash` are all computed by [`Db::atlas_frame_insert`]).
#[derive(Debug, Clone)]
pub struct NewAtlasFrame {
    /// Unix seconds, CALLER-SUPPLIED. No clock may enter the deterministic
    /// layout path (crates/kb-core/CLAUDE.md invariant #3), so the frame's
    /// timestamp is threaded in from the route handler exactly like
    /// `atlas_labels`' `computed_at`.
    pub created_at_unix: i64,
    /// Algorithm label — `"umap"` / `"pca"` (recompute) or `"recluster"`
    /// (k-means re-run over preserved coordinates).
    pub layout: String,
    pub provenance: FrameProvenance,
}

/// One `atlas_snapshots` row: the metadata of a single time-lapse frame.
#[derive(Debug, Clone, PartialEq)]
pub struct AtlasFrameRow {
    pub id: i64,
    pub created_at_unix: i64,
    pub point_count: i64,
    /// Distinct cluster ids in this frame. NOT comparable across frames —
    /// see [`atlas_frame_coord_hash`] and V0028's header on renumbering.
    pub cluster_count: i64,
    pub layout: String,
    /// sha256 hex of the frame's geometry — the dedup key.
    pub coord_hash: String,
    /// `"recorded"` | `"reconstructed"` (see [`FrameProvenance`]).
    pub provenance: String,
}

/// One `atlas_snapshot_points` row: where one artifact sat in one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct AtlasFramePoint {
    pub artifact_id: String,
    pub x: f32,
    pub y: f32,
    /// Matches the lance `atlas_cluster` i16 **within this frame only**.
    /// `atlas::kmeans_lloyd` seeds centroids by array POSITION and reseeds
    /// empty clusters randomly, so cluster ids RENUMBER between frames: a
    /// time-lapse consumer must remap colours per frame rather than trust
    /// cluster identity across them.
    pub cluster: i16,
}

/// Deterministic sha256 (hex) over a frame's geometry: the id-sorted
/// `(artifact_id, x.to_bits(), y.to_bits(), cluster)` tuples, each field
/// length-delimited or fixed-width so no two distinct frames can collide by
/// concatenation.
///
/// Hashing the raw f32 BITS (not a formatted float) makes the comparison
/// exact — a recompute that reproduces bit-identical coordinates is not a
/// new frame, which is precisely the dedup [`Db::atlas_frame_insert`] wants.
/// Sorting by id makes the hash independent of the order the caller happened
/// to build the points in (the same canonicalisation `atlas::recompute_for_kb_with`
/// applies to its inputs).
pub fn atlas_frame_coord_hash(points: &[AtlasFramePoint]) -> String {
    use sha2::{Digest, Sha256};
    let mut idx: Vec<&AtlasFramePoint> = points.iter().collect();
    idx.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
    let mut h = Sha256::new();
    for p in idx {
        // Length prefix keeps ("ab","c") and ("a","bc") distinct.
        h.update((p.artifact_id.len() as u64).to_le_bytes());
        h.update(p.artifact_id.as_bytes());
        h.update(p.x.to_bits().to_le_bytes());
        h.update(p.y.to_bits().to_le_bytes());
        h.update(p.cluster.to_le_bytes());
    }
    format!("{:x}", h.finalize())
}

/// Shared prune body for [`Db::atlas_frame_insert`] (in-transaction) and
/// [`Db::atlas_frames_prune`]. Points go with their frame via the
/// `ON DELETE CASCADE` on `atlas_snapshot_points.snapshot_id`.
fn prune_atlas_frames(tx: &rusqlite::Transaction<'_>, keep: i64) -> Result<usize> {
    let n = tx.execute(
        "DELETE FROM atlas_snapshots WHERE id NOT IN (
             SELECT id FROM atlas_snapshots
             ORDER BY created_at_unix DESC, id DESC
             LIMIT ?1
         )",
        params![keep],
    )?;
    Ok(n)
}

#[derive(Debug, Clone)]
pub struct RunRow {
    pub id: String,
    pub source_slug: String,
    pub started_at_unix: i64,
    pub finished_at_unix: Option<i64>,
    pub ok_count: u32,
    pub err_count: u32,
}

#[derive(Debug, Clone)]
pub struct ErrorRow {
    pub id: String,
    pub kind: String,
    pub source_slug: String,
    pub path: PathBuf,
    pub message: String,
    pub content_hash: Option<String>,
    pub retry_count: u32,
    pub created_at_unix: i64,
}

/// CT-F5 — one `slo_snapshots` row: one indicator's reading at one run.
///
/// `indicator` and `status` are stored (and read back) as plain strings, not
/// as the `kb_core::slo` enums: the log must stay readable when an OLDER
/// binary reads rows a NEWER one wrote, so an unrecognised key renders as an
/// unknown row instead of failing the decode. Callers that need the typed
/// value go through `SloKey::parse` / `SloStatus::parse` and handle `None`.
///
/// `value` is `None` exactly when the indicator was `unknown` at that run —
/// the honest reading. It is never written as 0.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SloSnapshotRow {
    pub id: i64,
    pub taken_at_unix: i64,
    pub indicator: String,
    pub value: Option<f64>,
    pub target: Option<f64>,
    pub status: String,
}

/// DCB W1.A — one `code_refs_docs` row: the per-document extraction header,
/// written on EVERY extraction (including one that found nothing, so a
/// zero-ref doc stays distinguishable from an unscanned one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRefHeaderRow {
    pub artifact_id: String,
    /// The source's raw-bytes content hash at the LAST extraction (R14 —
    /// "hash at last extraction", not necessarily the doc's current bytes).
    pub doc_hash: String,
    /// Wall clock at the write that most recently CHANGED the extraction
    /// (R4). Advanced only when [`Db::record_code_refs`] returns `true`.
    pub extracted_at: i64,
    pub code_rev: Option<String>,
    pub ref_count: u32,
    pub group_count: u32,
    pub ungrouped_count: u32,
    pub truncated: bool,
}

/// DCB W1.A — one `code_refs` row. Every field is a HINT recorded from the
/// doc's own bytes; nothing here is a verdict about the repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRefRow {
    pub ordinal: u32,
    /// `kb_core::coderefs::CodeRefKind::as_str`.
    pub kind: String,
    pub raw_text: String,
    pub path_hint: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub line_spans: Option<String>,
    pub symbol_container: Option<String>,
    pub symbol_member: Option<String>,
    pub context: String,
    /// Space-joined identifier-shaped tokens.
    pub context_tokens: String,
    /// `None` = ungrouped (before the first `h2`/`h3`). No sentinel group row
    /// exists anywhere (R9).
    pub group_key: Option<String>,
    pub group_label: Option<String>,
    pub group_anchor: Option<String>,
    pub declared: bool,
}

/// DCB W1.A — header + its ordered rows: the unit both the per-doc route and
/// the cursor feed serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRefDoc {
    pub header: CodeRefHeaderRow,
    pub refs: Vec<CodeRefRow>,
}

/// One row from `edges_from`. `depth` is the BFS hop count from the
/// query starting id (1 = direct neighbor). Used by `routes::graph`
/// to render the cross-artifact view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeRow {
    pub from_id: String,
    pub to_id: String,
    pub kind: String,
    pub depth: u32,
}

/// Result of `history_record_open`. `id` is the visit's row id; the
/// SPA passes it back on subsequent scroll updates. `scroll_y` is the
/// prior saved scroll (0 for a brand-new visit). `is_new_visit` lets
/// the HTTP layer emit `history.recorded` SSE only on actual inserts
/// — bumps within the 30-minute gap are silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenResult {
    pub id: i64,
    pub scroll_y: i64,
    pub is_new_visit: bool,
}

/// One row from `history_list`. The discriminator `kind` is one of
/// `"open"`, `"search"`, `"comment"`; the optional fields are populated
/// according to that kind. The route handler enriches with title /
/// comment-body snippets before serving to the SPA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRow {
    pub id: i64,
    pub kind: String,
    pub artifact_id: Option<String>,
    pub query: Option<String>,
    pub comment_id: Option<String>,
    pub scroll_y: i64,
    pub scroll_max: i64,
    /// Per-visit high-water mark: the furthest `scroll_y` ever
    /// observed on this row. Always >= `scroll_y`. Drives the SPA's
    /// sticky "fully read" indicator (V0007).
    pub scroll_y_max: i64,
    pub started_at_unix: i64,
    pub updated_at_unix: i64,
    /// GC-B5 — who opened this row: `Some("web")`/`Some("cli")`, or `None`
    /// for rows written before the `source` column existed (treated as
    /// "web" by callers). Only meaningful for `kind = 'open'`.
    pub source: Option<String>,
    /// v0.34 X1 — attribution username (lowercase). Empty string means a
    /// pre-multi-user row not yet rewritten by [`Db::identity_backfill`].
    pub user: String,
}

/// W2.10 — one `(day, kind)` aggregate from `history_counts_by_day`. `day` is
/// a UTC `YYYY-MM-DD` string; `kind` is `"open"`, `"search"`, or `"comment"`
/// (the same discriminator as [`HistoryRow::kind`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayKindCount {
    pub day: String,
    pub kind: String,
    pub count: i64,
}

/// Inbound per-section reading beacon row (RP-track). Cumulative per-visit
/// values mapped from the HTTP body; `reading_upsert_sections` max-merges
/// them so resent beacons / remounts can't double-count.
#[derive(Debug, Clone)]
pub struct SectionDwell {
    pub section_id: String,
    pub section_idx: i64,
    pub section_text: String,
    pub level: i64,
    pub words: i64,
    pub content_px: i64,
    pub dwell_ms: i64,
    pub enters: i64,
}

/// One section of a visit's resume baseline (seed-on-open). Only the fields
/// the runtime needs to restore its in-memory accumulators — text / words /
/// content_px are re-measured from the live DOM, not seeded.
#[derive(Debug, Clone)]
pub struct ReadingSeedSection {
    pub section_id: String,
    pub dwell_ms: i64,
    pub enters: i64,
}

/// A visit's reading resume baseline returned by `reading_state_for_visit`.
/// Zero/empty for a brand-new visit.
#[derive(Debug, Clone, Default)]
pub struct ReadingResume {
    pub active_ms: i64,
    pub last_section: Option<String>,
    pub sections: Vec<ReadingSeedSection>,
}

/// One row from the `shares` registry (V0004). Records everything `kb
/// share list`, `--update` (find-by-target), and `revoke` (undo exactly
/// what was created) need. Cloudflare-only fields (`cf_*`,
/// `pages_project`, `access_*`) are `None` for GitHub Pages shares and
/// `github_repo` is `None` for Cloudflare shares; `gate` is `None` when
/// the share is public (ungated).
/// One row from the `corkboard` table (V0005). Anchored artifacts in
/// the current kb. The HTTP layer joins with lance to project title /
/// folder / source_relative for the cross-kb list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorkboardRow {
    pub artifact_id: String,
    pub created_at_unix: i64,
}

/// One row from the `lists` table (V0015, RL-track). The list header;
/// roll-ups (entry counts, minutes) are computed at the HTTP layer from
/// the entries + lance + reading progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRow {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub pinned: bool,
    pub archived: bool,
    pub created_at_unix: i64,
    pub updated_at_unix: i64,
}

/// One row from the `list_entries` table (V0015). `anchor_json` is the
/// canonical `review::Anchor` JSON (`None` = whole artifact); decode at
/// the kb-core layer. `position` is the dense 0-based display index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntryRow {
    pub id: String,
    pub list_id: String,
    pub kb: String,
    pub artifact_id: String,
    pub anchor_json: Option<String>,
    pub note: Option<String>,
    pub position: i64,
    pub read_override: Option<String>,
    pub words: Option<i64>,
    pub anchor_stale: bool,
    pub created_at_unix: i64,
    pub updated_at_unix: i64,
}

/// One row from the `sessions` table (V0008 + V0017). Enrichment metadata
/// for one memory-session artifact (Claude Code transcript or similar).
/// The HTTP layer (`/api/sessions`) cross-references with lance to
/// project the title + memory_count and serves the merged response.
///
/// V0017 added the transcript-derived identity fields (`title`, `cwd`,
/// `git_branch`) and the file-activity counts; they are `Default`-able so a
/// non-session caller (or an old un-reindexed row) reads sane zero/None
/// values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRow {
    pub artifact_id: String,
    pub session_id: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub message_count: u32,
    pub first_user_prompt: Option<String>,
    pub source_relative: String,
    /// aiTitle — the display name. `None` falls back to `first_user_prompt`.
    pub title: Option<String>,
    /// Modal working directory (the folder/project key, A1).
    pub cwd: Option<String>,
    /// First git branch the session ran on.
    pub git_branch: Option<String>,
    /// Count of files the session read.
    pub files_read_count: u32,
    /// Count of distinct files the session edited/wrote.
    pub files_edited_count: u32,
    /// V0018/S9 — total tokens (input + output) over assistant turns.
    pub token_total: u64,
    /// V0018/S9 — number of tool calls.
    pub tool_calls: u32,
    /// V0018/S9 — the model the session ran on.
    pub model: Option<String>,
    /// V0018/S9 — count of error tool-results (detected, not ground truth).
    pub error_count: u32,
    /// V0024/W0.2 — count of Agent/Task delegations that came back with real
    /// stats (a synchronous completion). See `subagent_launched_unstatted`
    /// for delegations that produced no numbers.
    pub subagent_count: u32,
    /// V0024/W0.2 — summed tokens over every completed-with-stats subagent.
    pub subagent_tokens: u64,
    /// V0024/W0.2 — summed tool-call counts over every completed-with-stats
    /// subagent.
    pub subagent_tool_calls: u32,
    /// V0024/W0.2 — summed edit-operation counts over every completed-with-
    /// stats subagent (detected, not ground truth).
    pub subagent_files_edited: u32,
    /// V0024/W0.2 — count of Agent/Task delegations with NO stats (an async
    /// stub, or any other agentId-bearing result missing `totalTokens`).
    /// Kept separate so a session that launched agents but got no numbers
    /// back never reads identically to a session that launched none.
    pub subagent_launched_unstatted: u32,
    /// V0029/P1 — the derived project key (`claude_project_slug(root)`).
    /// `None` when the derivation ladder found neither a resolved commit's
    /// repo root nor a cwd.
    pub project_key: Option<String>,
    /// V0029/P1 — the CONFIRMED git root behind `project_key` (ladder rung 2
    /// only — a rung-3 cwd-only key leaves this `None`).
    pub repo_root: Option<String>,
    /// V0029/R5 — the harness that produced this capture. `NOT NULL DEFAULT
    /// 'claude'` at the schema level: un-backfilled history is honestly
    /// Claude, never a NULL hole. See `kb_core::sessions::HARNESSES`.
    pub harness: String,
    /// V0029 — first non-empty top-level `version` field across the JSONL.
    pub cc_version: Option<String>,
    /// V0029/R3 — the closure quintuple: the session's last real assistant
    /// prose, head-capped at `LAST_ASSISTANT_TEXT_MAX_CHARS` (700 chars).
    pub last_assistant_text: Option<String>,
    /// V0029/P1 — JSON array of every distinct `cwd` the session visited.
    /// `None` when the session only ever touched one cwd (the plain `cwd`
    /// column already covers that case).
    pub all_cwds: Option<String>,
    /// V0029/P2 — count of `kind='commit'` rows this capture wrote to
    /// `session_commits` (push/tag excluded).
    pub commit_count: u32,
    /// V0029/P2 — the honest "real prompts" count (wrapper-skipping).
    pub user_turns: u32,
    /// V0029/R6/D4 — the honest ACTIVE duration in seconds (per-delta
    /// clamped sum).
    pub active_secs: i64,
    /// V0029/S1 — the deterministic triage enum
    /// (`"trivial"|"routine"|"substantive"`). `None` means un-backfilled —
    /// every reader MUST treat `None` as `"substantive"` (never hide
    /// un-backfilled history behind a husk filter).
    pub substance: Option<String>,
}

/// One row from the `session_decisions` table (V0018/S9) — a steering moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDecisionRow {
    pub artifact_id_session: String,
    pub session_id: String,
    pub seq: i64,
    /// `"question" | "plan"`.
    pub kind: String,
    pub prompt: String,
    pub answer: Option<String>,
}

/// Per-folder rollup (P6) — the timeline-header stats for one working dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderStats {
    pub cwd: String,
    pub count: i64,
    pub latest: i64,
    pub earliest: i64,
    pub edited_total: i64,
    pub token_total: i64,
}

/// W3.A/P4 — one `COALESCE(project_key, cwd)` group's rollup, PRE-registry-
/// merge. `key` is what `?project=` echoes/accepts for an auto-project (the
/// raw `project_key`, or a raw cwd for the rare pre-derivation row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectStatsRow {
    pub key: String,
    pub repo_root: Option<String>,
    pub cwd_sample: Option<String>,
    pub count: i64,
    pub latest: i64,
    pub earliest: i64,
    pub edited_total: i64,
    pub token_total: i64,
    pub commit_total: i64,
    pub error_sessions: i64,
    pub active_secs_total: i64,
}

/// W3.A/P4 — one `(key, harness)` count, folded into `ProjectOut.harness_mix`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectHarnessRow {
    pub key: String,
    pub harness: String,
    pub count: i64,
}

/// R9 — one `(cwd, kind, query)` research aggregate: how often a query was run
/// in a project, across how many sessions, and when last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResearchRollupRow {
    pub cwd: String,
    pub kind: String,
    pub query: String,
    pub count: i64,
    pub sessions: i64,
    pub latest: i64,
    /// W3.A — a representative `project_key` for this `(cwd, kind, query)`
    /// group (`MIN` over the group — project_key is stable per cwd in
    /// practice, so this is a single-value pick, not a real aggregate).
    /// Used by the route's `?project=` post-filter (research-rollup isn't
    /// cursor-paginated, so a Rust-side filter after the cross-kb merge is
    /// fine — unlike `sessions_list`'s SQL-WHERE requirement).
    pub project_key: Option<String>,
}

/// R9 — per-stage event + distinct-session counts for the activity funnel,
/// from the session_* tables (the `commented` stage is added route-side).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FunnelCounts {
    pub searched_events: i64,
    pub searched_sessions: i64,
    pub opened_events: i64,
    pub opened_sessions: i64,
    pub edited_events: i64,
    pub edited_sessions: i64,
    pub committed_events: i64,
    pub committed_sessions: i64,
}

/// One row from the `session_commits` table (V0019/P5, resolution columns
/// V0025/W0.4) — a git action.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionCommitRow {
    pub artifact_id_session: String,
    pub session_id: String,
    pub seq: i64,
    /// `"commit" | "push" | "tag"`.
    pub kind: String,
    /// Transcript-detected sha (possibly short; best-effort).
    pub sha: Option<String>,
    /// Transcript-detected subject, or the capture-time resolved TRUE
    /// subject when `resolved` — kept even when resolution failed, so a
    /// rebased-away sha still shows a best-effort subject.
    pub subject: Option<String>,
    /// V0025 — `kb sessions capture`'s one `git show -s` per detected sha
    /// succeeded. `false` for every transcript-only row (old captures,
    /// imports, unresolvable shas) — never re-resolved retroactively.
    pub sha_full: Option<String>,
    pub repo_root: Option<String>,
    pub resolved: bool,
    pub author: Option<String>,
    pub parents: Option<i64>,
    /// Newline-joined `"Key: value"` trailer lines (see the V0025 migration
    /// comment for why this isn't JSON), `None` when resolution found none
    /// or never ran.
    pub trailers: Option<String>,
}

/// kb-code Wave 0 (W0.6) — one [`SessionCommitRow`] matched by
/// [`Db::session_commits_by_sha_prefix`], carrying the owning session's
/// NEWEST-capture `started_at`/`title`/`first_user_prompt` (co-located in the
/// same kb sqlite db, so no second round trip is needed to render a display
/// name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCommitMatch {
    pub commit: SessionCommitRow,
    pub started_at: i64,
    pub title: Option<String>,
    pub first_user_prompt: Option<String>,
}

/// kb-code Wave 0 (W0.6) — one row of the `commit-map` bulk feed
/// ([`Db::session_commits_page`]): a [`SessionCommitRow`] (newest capture
/// only, #11) plus its owning session's `started_at`. Deliberately flat — no
/// title/lance resolution — since the bulk feed exists to be cheap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitMapRow {
    pub commit: SessionCommitRow,
    pub started_at: i64,
}

/// One row from the `session_research` table (V0020/R4) — a research /
/// tool-usage signal (kb/web search, subagent, skill/MCP, plan presentation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionResearchRow {
    pub artifact_id_session: String,
    pub session_id: String,
    pub seq: i64,
    /// `kb_search | web | skill | subagent | plan_span | artifact_open`.
    pub kind: String,
    pub query: String,
}

/// One row from the `session_files` table (V0017) — a single file a session
/// touched, with the action and (when the path resolved under a kb mount)
/// the target corpus + artifact id for the bidirectional link (A4/A6/A7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFileRow {
    pub artifact_id_session: String,
    pub session_id: String,
    pub path: String,
    pub basename: String,
    /// `"read" | "write" | "edit"` (see `kb_core::sessions::FileAction`).
    pub action: String,
    pub in_corpus: bool,
    pub target_kb: Option<String>,
    pub target_artifact_id: Option<String>,
    /// V0026/W0.5 — `true` when this touch was recovered from a subagent's
    /// own sidecar transcript (`<session-id>/subagents/agent-*.jsonl`)
    /// rather than the main thread. The main-thread row always wins a
    /// `(path, action)` collision, so a row is never both.
    pub via_subagent: bool,
}

/// One row from the `memory_recalls` table (V0035, MI-W1.1; `used` V0037,
/// CT-C5) — one memory hit a `kb-recall` UserPromptSubmit hook injected into
/// a captured session, derived from the transcript's `Item::MemoryInjection`
/// items (see `kb_core::sessions::view::derive_memory_recalls`, the pure
/// parse this row is built from). No FK: `memory_kb`/`memory_id` are
/// best-effort, parsed out of the injected hit's free text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecallRow {
    pub memory_kb: String,
    pub memory_id: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    /// Unix secs — the enclosing Turn's `ts`, a close-but-not-exact proxy for
    /// the injection's own JSONL-line timestamp (`Item::MemoryInjection`
    /// carries no per-item ts).
    pub recalled_at: Option<i64>,
    /// This capture's `sessions.artifact_id` — the scoping key `replace`
    /// deletes/inserts by (mirrors `SessionFileRow::artifact_id_session`)
    /// AND the join key every multi-session read correlates against via
    /// `newest_capture_pred` to exclude a stale, superseded capture's rows.
    pub artifact_id: String,
    /// CT-C5 (V0037) — did a turn strictly after this hit's own turn
    /// explicitly name the memory (id or title)? See
    /// `kb_core::sessions::view::DerivedRecall::used` for the exact
    /// definition and caveats (explicit reference only). SURFACED for
    /// display (census); never read by any scoring path.
    pub used: bool,
    /// MR1 (V0040) — the hit's rank in the pack that injected it (1 = top),
    /// read from the `kb-recall/1` marker's `pos=` pair alone. `None` on a
    /// pre-MR1 capture, a fallback-only parse, or a mangled value; see
    /// `kb_core::sessions::view::DerivedRecall::pos` for why the hit's
    /// position in the transcript is deliberately not used as a substitute.
    /// SURFACED for display; never read by any scoring path.
    pub pos: Option<u32>,
}

/// A `memory_id`'s aggregate recall stats within one sessions-corpus kb's
/// `memory_recalls` table — the census/recall-enrichment read shape
/// (`memory_recalls_counts_for_ids`). A caller fanning out across every kb
/// (invariant #28) sums `count`/`used_count` and takes the max
/// `last_recalled_at` across the per-kb partials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecallCount {
    pub memory_id: String,
    pub count: u32,
    pub last_recalled_at: Option<i64>,
    /// CT-C5 (V0037) — how many of `count`'s rows have `used = true`
    /// (explicit reference in a later turn). Always `<= count`.
    pub used_count: u32,
}

/// MI-W4.2a — one `(memory_id, weeks-ago bucket)` count from
/// `memory_recalls_weekly_for_ids`. `weeks_ago` is already clamped to
/// `[0, MEMORY_RECALL_WEEKLY_BUCKETS - 1]` (bucket `N-1` is a "N-1+ weeks
/// ago" catch-all) — see that fn's doc comment for why the clamping happens
/// in SQL rather than here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecallWeeklyRow {
    pub memory_id: String,
    pub weeks_ago: i64,
    pub count: u32,
}

/// Width of the [`Storage::memory_recalls_weekly_for_ids`] histogram — 8
/// weeks (~2 months) is enough runway to see a rising/falling injection
/// trend on a sparkline without the query scanning unbounded history.
pub const MEMORY_RECALL_WEEKLY_BUCKETS: i64 = 8;

/// CT-B2 — one row of [`Db::memory_recalls_for_memory`]: one recalling
/// session, enriched with that session's own `title`/`first_user_prompt`
/// (the same display-name inputs `SessionOut`'s `display_name_of` ladder
/// consumes) so the `recalled-by` route/CLI/SPA need no second lookup.
/// `title`/`first_user_prompt` reflect the row's session's NEWEST capture —
/// see the fn's own doc comment for the correlated-newest-capture scoping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecalledByRow {
    pub session_id: String,
    pub turn_id: Option<String>,
    pub recalled_at: Option<i64>,
    pub title: Option<String>,
    pub first_user_prompt: Option<String>,
    pub started_at: i64,
    /// CT-C5 (V0037) — did the recalling session go on to explicitly
    /// reference this memory in a later turn? See `DerivedRecall::used` for
    /// the exact (deliberately conservative) semantics.
    pub used: bool,
    /// MR1 (V0040) — this hit's rank in the pack that injected it (1 =
    /// top), or `None` when the capture's marker didn't carry one. See
    /// `MemoryRecallRow::pos`.
    pub pos: Option<u32>,
}

/// CT-F1 — one row of the `memory_commits` table (V0038): "this commit
/// cited this memory", recovered from a `Kb-Memory:` commit trailer the
/// capture's own `git show` already resolved (see the migration's header for
/// why this earns its own table and why an EMPTY result is a non-signal).
/// Used verbatim as BOTH the write shape (`memory_commits_replace`) and the
/// read shape (`memory_commits_for_memory`) — unlike `memory_recalls` there
/// is no display-enrichment join to add, since `sha`/`subject`/`repo_root`
/// are carried on the row itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCommitRow {
    /// The cited memory's 12-hex artifact id, verbatim from the trailer.
    pub memory_id: String,
    /// The citing commit's FULL sha (capture-time `git show`).
    pub sha_full: String,
    /// Short sha as the transcript detected it (display only).
    pub sha: Option<String>,
    pub subject: Option<String>,
    /// Which repo the sha lives in (`find_git_root` at capture time).
    pub repo_root: Option<String>,
    /// The session whose capture carried the trailer.
    pub session_id: String,
    /// That capture's `sessions.artifact_id` — this row's artifact-id
    /// lifecycle key (registered in all three id registries; see the
    /// migration header).
    pub artifact_id: String,
    /// When the row was DERIVED (indexer clock) — never a commit timestamp.
    pub recorded_at: i64,
}

/// One row's metadata from `artifact_snapshots` (V0013, Track V). The body
/// (`raw_source`) is fetched separately via `snapshot_raw` so the timeline
/// list stays cheap. `id` is the autoincrement row id, used as the
/// `index:<id>` version ref in `kb_core::versions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotMeta {
    pub id: i64,
    pub content_hash: String,
    pub captured_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareRow {
    pub name: String,
    pub target: String,
    pub host: String,
    pub deployed_url: String,
    pub gate: Option<String>,
    pub cf_account_id: Option<String>,
    pub pages_project: Option<String>,
    pub cf_deployment_id: Option<String>,
    pub access_app_id: Option<String>,
    pub access_policy_id: Option<String>,
    pub github_repo: Option<String>,
    pub created_at_unix: i64,
    pub updated_at_unix: i64,
}

impl From<rusqlite::Error> for crate::Error {
    fn from(e: rusqlite::Error) -> Self {
        crate::Error::Storage(format!("sqlite: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn db() -> Db {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.db");
        let db = Db::open(&path).unwrap();
        // Leak the tempdir so the path stays valid for the test's lifetime.
        // (This is a unit test; cleanup is OS-level.)
        std::mem::forget(tmp);
        db
    }

    /// Guard against two branches shipping the same `VNNNN__*.sql` version
    /// under different filenames. Git merges that cleanly (no textual
    /// conflict) and `refinery::embed_migrations!` only explodes at runtime
    /// on a fresh DB — the V0031 incident that broke ~70 tests. Fail at
    /// compile-test time instead, naming both colliding files.
    #[test]
    fn migration_versions_are_unique_and_well_formed() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");
        let entries =
            std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read migrations dir {dir}: {e}"));

        // version → first filename that claimed it
        let mut seen: HashMap<u32, String> = HashMap::new();
        let mut any = false;

        for entry in entries {
            let entry = entry.expect("read_dir entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".sql") {
                continue;
            }
            any = true;

            // V<digits>__<name>.sql — digits must be non-empty; name non-empty.
            let rest = name
                .strip_prefix('V')
                .and_then(|s| s.strip_suffix(".sql"))
                .unwrap_or("");
            let (ver_str, desc) = match rest.split_once("__") {
                Some(pair) => pair,
                None => panic!(
                    "migration filename {name:?} does not match V<digits>__<name>.sql \
                     (refinery would ignore or reject it)"
                ),
            };
            if ver_str.is_empty() || !ver_str.bytes().all(|b| b.is_ascii_digit()) || desc.is_empty()
            {
                panic!(
                    "migration filename {name:?} does not match V<digits>__<name>.sql \
                     (refinery would ignore or reject it)"
                );
            }
            let version: u32 = ver_str
                .parse()
                .unwrap_or_else(|_| panic!("migration version overflow in {name:?}"));

            if let Some(prior) = seen.get(&version) {
                panic!(
                    "duplicate migration version V{version}: {prior:?} and {name:?}. \
                     Two branches each adding the same VNNNN under different filenames \
                     merges clean in git and only fails at runtime when \
                     refinery::embed_migrations! loads a fresh DB (the V0031 incident)."
                );
            }
            seen.insert(version, name);
        }

        assert!(
            any,
            "no *.sql migrations found under {dir}; embed_migrations! would be empty"
        );
    }

    #[test]
    fn migrations_run_on_open() {
        let _ = db();
    }

    #[test]
    fn migrations_idempotent_on_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.db");
        let _db1 = Db::open(&path).unwrap();
        let _db2 = Db::open(&path).unwrap();
        // No panic = pass.
    }

    /// kb-sibling/1 — a volume forward-migrated by a NEWER binary must
    /// refuse to open, naming both epochs and the path. The history row is
    /// fabricated (migrations themselves are immutable) at
    /// `schema_epoch() + 1000`, a version no real migration will reach.
    // invariant:2 kb-sibling/1 schema-epoch boot refuse
    #[test]
    fn open_refuses_a_volume_whose_schema_epoch_is_ahead_of_this_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("index.db");
        let ahead = schema_epoch() + 1_000;
        {
            let db = Db::open(&path).expect("first open migrates normally");
            db.conn
                .execute(
                    "INSERT INTO refinery_schema_history (version, name, applied_on, checksum) \
                     VALUES (?1, 'from_a_newer_binary', '', '0')",
                    [ahead],
                )
                .unwrap();
        }
        let msg = match Db::open(&path) {
            Ok(_) => panic!("an ahead volume must refuse to open"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("refusing to boot"), "{msg}");
        assert!(msg.contains(&format!("V{ahead}")), "{msg}");
        assert!(msg.contains(&format!("V{}", schema_epoch())), "{msg}");
        assert!(msg.contains("index.db"), "{msg}");
    }

    /// The passing case at EQUAL epoch — the normal steady-state boot (the
    /// volume the running binary itself migrated).
    #[test]
    fn open_proceeds_when_the_volume_epoch_equals_the_binary_epoch() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("index.db");
        let db = Db::open(&path).unwrap();
        assert_eq!(
            crate::sibling::volume_epoch(&db.conn).unwrap(),
            Some(schema_epoch()),
            "a freshly migrated volume sits exactly at the binary epoch"
        );
        drop(db);
        Db::open(&path).expect("re-opening at an equal epoch must boot");
    }

    #[test]
    fn upsert_source_and_list() {
        let mut db = db();
        let slug = SourceSlug::from_path(Path::new("/tmp/canon"));
        db.upsert_source(&slug, Path::new("/tmp/canon"), 1700000000)
            .unwrap();

        let rows = db.list_sources().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].raw_slug, slug.as_str());
        assert_eq!(rows[0].path, PathBuf::from("/tmp/canon"));
        assert_eq!(rows[0].added_at_unix, 1700000000);
        assert!(!rows[0].paused);

        // GC-F3 — added_at ties break slug-ascending, not insertion order
        // (zeta is inserted before alpha but lists after it).
        let zeta = SourceSlug::from_path(Path::new("/tmp/zeta"));
        let alpha = SourceSlug::from_path(Path::new("/tmp/alpha"));
        db.upsert_source(&zeta, Path::new("/tmp/zeta"), 1700000000)
            .unwrap();
        db.upsert_source(&alpha, Path::new("/tmp/alpha"), 1700000000)
            .unwrap();
        let slugs: Vec<String> = db
            .list_sources()
            .unwrap()
            .into_iter()
            .map(|r| r.raw_slug)
            .collect();
        assert_eq!(slugs, ["tmp-alpha", "tmp-canon", "tmp-zeta"]);
    }

    #[test]
    fn upsert_source_idempotent() {
        let mut db = db();
        let slug = SourceSlug::from_path(Path::new("/tmp/canon"));
        db.upsert_source(&slug, Path::new("/tmp/canon"), 1700000000)
            .unwrap();
        db.upsert_source(&slug, Path::new("/tmp/canon"), 1700000999)
            .unwrap();
        let rows = db.list_sources().unwrap();
        assert_eq!(rows.len(), 1);
        // First write wins on added_at; ON CONFLICT updates only path.
        assert_eq!(rows[0].added_at_unix, 1700000000);
    }

    #[test]
    fn pause_unpause_source() {
        let mut db = db();
        let slug = SourceSlug::from_path(Path::new("/tmp/canon"));
        db.upsert_source(&slug, Path::new("/tmp/canon"), 1700000000)
            .unwrap();
        db.set_source_paused(&slug, true).unwrap();
        assert!(db.list_sources().unwrap()[0].paused);
        db.set_source_paused(&slug, false).unwrap();
        assert!(!db.list_sources().unwrap()[0].paused);
    }

    #[test]
    fn exclusion_add_list_remove_roundtrip() {
        let mut db = db();
        // Add is idempotent — the second insert reports false and the
        // original excluded_at/note win.
        assert!(db
            .add_exclusion("sub/drop.html", 1_700_000_000, Some("noisy"))
            .unwrap());
        assert!(!db
            .add_exclusion("sub/drop.html", 1_700_009_999, None)
            .unwrap());
        assert!(db.add_exclusion("later.md", 1_700_000_500, None).unwrap());

        let rows = db.list_exclusions().unwrap();
        assert_eq!(rows.len(), 2);
        // Newest first.
        assert_eq!(rows[0].path, "later.md");
        assert_eq!(rows[1].path, "sub/drop.html");
        assert_eq!(rows[1].excluded_at_unix, 1_700_000_000, "first write wins");
        assert_eq!(rows[1].note.as_deref(), Some("noisy"));

        assert!(db.remove_exclusion("sub/drop.html").unwrap());
        assert!(!db.remove_exclusion("sub/drop.html").unwrap(), "gone");
        assert_eq!(db.list_exclusions().unwrap().len(), 1);
    }

    // --- Atlas labels (W1.B, V0027) ---------------------------------------

    fn label_row(cluster: i16, rank: i64, term: &str) -> AtlasLabelRow {
        AtlasLabelRow {
            cluster,
            rank,
            term: term.to_string(),
            tf: rank as f64,
            ft: rank as f64 * 2.0,
            score: rank as f64 * 0.5,
            computed_at: 1_700_000_000,
        }
    }

    #[test]
    fn atlas_labels_migration_smoke_empty_table_on_fresh_db() {
        // V0027 must create the table even with zero rows ever written.
        let db = db();
        assert_eq!(db.atlas_labels().unwrap(), Vec::<AtlasLabelRow>::new());
    }

    #[test]
    fn atlas_labels_set_then_get_roundtrips_ordered_by_cluster_then_rank() {
        let mut db = db();
        db.set_atlas_labels(&[
            label_row(1, 2, "second"),
            label_row(0, 1, "alpha"),
            label_row(1, 1, "first"),
        ])
        .unwrap();

        let rows = db.atlas_labels().unwrap();
        assert_eq!(rows.len(), 3);
        // ORDER BY cluster, rank — independent of insertion order.
        assert_eq!(
            (rows[0].cluster, rows[0].rank, rows[0].term.as_str()),
            (0, 1, "alpha")
        );
        assert_eq!(
            (rows[1].cluster, rows[1].rank, rows[1].term.as_str()),
            (1, 1, "first")
        );
        assert_eq!(
            (rows[2].cluster, rows[2].rank, rows[2].term.as_str()),
            (1, 2, "second")
        );
        assert_eq!(rows[0].computed_at, 1_700_000_000);
    }

    #[test]
    fn atlas_labels_set_replaces_the_whole_table() {
        let mut db = db();
        db.set_atlas_labels(&[label_row(0, 1, "old-a"), label_row(0, 2, "old-b")])
            .unwrap();
        assert_eq!(db.atlas_labels().unwrap().len(), 2);

        // A fresh recompute's label set fully replaces the prior one — no
        // stale rows from a cluster that no longer exists survive.
        db.set_atlas_labels(&[label_row(5, 1, "new-only")]).unwrap();
        let rows = db.atlas_labels().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cluster, 5);
        assert_eq!(rows[0].term, "new-only");
    }

    #[test]
    fn atlas_labels_set_empty_clears_the_table() {
        let mut db = db();
        db.set_atlas_labels(&[label_row(0, 1, "a")]).unwrap();
        assert_eq!(db.atlas_labels().unwrap().len(), 1);
        db.set_atlas_labels(&[]).unwrap();
        assert!(db.atlas_labels().unwrap().is_empty());
    }

    // --- Atlas frames / time-lapse (W3 T-a, V0028) ------------------------

    fn frame_point(id: &str, x: f32, y: f32, cluster: i16) -> AtlasFramePoint {
        AtlasFramePoint {
            artifact_id: id.to_string(),
            x,
            y,
            cluster,
        }
    }

    fn new_frame(at: i64) -> NewAtlasFrame {
        NewAtlasFrame {
            created_at_unix: at,
            layout: "umap".into(),
            provenance: FrameProvenance::Recorded,
        }
    }

    #[test]
    fn atlas_frames_migration_smoke_empty_tables_on_fresh_db() {
        // V0028 must create both tables even with zero frames ever written —
        // and a fresh kb HAS zero frames: the time-lapse starts blind because
        // no past layout is recoverable.
        let db = db();
        assert!(db.atlas_frames(100).unwrap().is_empty());
        assert!(db.atlas_frame_points(1).unwrap().is_empty());
    }

    #[test]
    fn atlas_frame_insert_roundtrips_points_and_derives_counts() {
        let mut db = db();
        let pts = vec![
            frame_point("b", 0.25, 0.5, 1),
            frame_point("a", 0.0, 1.0, 0),
            frame_point("c", 1.0, 0.75, 1),
        ];
        let id = db
            .atlas_frame_insert(&new_frame(1_700_000_000), &pts)
            .unwrap();
        let id = id.expect("first frame must insert");

        let frames = db.atlas_frames(10).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].id, id);
        assert_eq!(frames[0].point_count, 3);
        // Two distinct cluster ids (0 and 1) — derived, not caller-supplied.
        assert_eq!(frames[0].cluster_count, 2);
        assert_eq!(frames[0].layout, "umap");
        assert_eq!(frames[0].provenance, "recorded");
        assert_eq!(frames[0].coord_hash, atlas_frame_coord_hash(&pts));

        // Read-back is id-sorted (the hash order), and f32 survives the REAL
        // round-trip bit-exactly.
        let back = db.atlas_frame_points(id).unwrap();
        assert_eq!(
            back,
            vec![
                frame_point("a", 0.0, 1.0, 0),
                frame_point("b", 0.25, 0.5, 1),
                frame_point("c", 1.0, 0.75, 1),
            ]
        );
        assert_eq!(atlas_frame_coord_hash(&back), frames[0].coord_hash);
    }

    #[test]
    fn atlas_frame_insert_skips_an_unchanged_recompute() {
        let mut db = db();
        let pts = vec![frame_point("a", 0.1, 0.2, 0)];
        assert!(db
            .atlas_frame_insert(&new_frame(1_700_000_000), &pts)
            .unwrap()
            .is_some());
        // Same geometry, later clock, different layout label: still not a
        // frame — a recompute that changed nothing is not a frame.
        let mut later = new_frame(1_700_009_999);
        later.layout = "pca".into();
        assert_eq!(db.atlas_frame_insert(&later, &pts).unwrap(), None);
        assert_eq!(db.atlas_frames(10).unwrap().len(), 1);

        // A single moved point IS a frame.
        let moved = vec![frame_point("a", 0.1, 0.2000001, 0)];
        assert!(db
            .atlas_frame_insert(&new_frame(1_700_010_000), &moved)
            .unwrap()
            .is_some());
        assert_eq!(db.atlas_frames(10).unwrap().len(), 2);

        // ...and so is a cluster RENUMBER at identical coordinates (the
        // renumbering caveat is real drift as far as the dedup is concerned).
        let renumbered = vec![frame_point("a", 0.1, 0.2000001, 7)];
        assert!(db
            .atlas_frame_insert(&new_frame(1_700_010_001), &renumbered)
            .unwrap()
            .is_some());
        assert_eq!(db.atlas_frames(10).unwrap().len(), 3);
    }

    #[test]
    fn atlas_frame_insert_prunes_to_the_keep_bound_and_cascades_points() {
        let mut db = db();
        let total = DEFAULT_ATLAS_FRAMES_KEEP + 5;
        let mut ids = Vec::new();
        for i in 0..total {
            let pts = vec![frame_point("a", i as f32 / 1000.0, 0.5, 0)];
            ids.push(
                db.atlas_frame_insert(&new_frame(1_700_000_000 + i as i64), &pts)
                    .unwrap()
                    .expect("each distinct geometry is a frame"),
            );
        }
        let frames = db.atlas_frames(1000).unwrap();
        assert_eq!(frames.len(), DEFAULT_ATLAS_FRAMES_KEEP);
        // Newest first, and the oldest 5 are gone.
        assert_eq!(frames[0].id, ids[total - 1]);
        assert_eq!(frames[DEFAULT_ATLAS_FRAMES_KEEP - 1].id, ids[5]);
        // Their points went with them (ON DELETE CASCADE).
        assert!(db.atlas_frame_points(ids[0]).unwrap().is_empty());
        assert_eq!(db.atlas_frame_points(ids[5]).unwrap().len(), 1);
    }

    #[test]
    fn atlas_frames_prune_is_explicit_and_cascades() {
        let mut db = db();
        let mut ids = Vec::new();
        for i in 0..4 {
            let pts = vec![frame_point("a", i as f32, 0.0, 0)];
            ids.push(
                db.atlas_frame_insert(&new_frame(1_700_000_000 + i), &pts)
                    .unwrap()
                    .unwrap(),
            );
        }
        assert_eq!(db.atlas_frames_prune(2).unwrap(), 2);
        let frames = db.atlas_frames(10).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].id, ids[3]);
        assert!(db.atlas_frame_points(ids[0]).unwrap().is_empty());
        assert_eq!(db.atlas_frame_points(ids[3]).unwrap().len(), 1);
        // Idempotent: pruning again to the same bound deletes nothing.
        assert_eq!(db.atlas_frames_prune(2).unwrap(), 0);
    }

    #[test]
    fn atlas_frame_coord_hash_is_order_independent_and_field_delimited() {
        let a = vec![frame_point("a", 0.1, 0.2, 0), frame_point("b", 0.3, 0.4, 1)];
        let reversed = vec![frame_point("b", 0.3, 0.4, 1), frame_point("a", 0.1, 0.2, 0)];
        assert_eq!(
            atlas_frame_coord_hash(&a),
            atlas_frame_coord_hash(&reversed)
        );

        // Length-prefixing: ("ab","c") and ("a","bc") must not collide.
        let split1 = vec![
            frame_point("ab", 0.0, 0.0, 0),
            frame_point("c", 0.0, 0.0, 0),
        ];
        let split2 = vec![
            frame_point("a", 0.0, 0.0, 0),
            frame_point("bc", 0.0, 0.0, 0),
        ];
        assert_ne!(
            atlas_frame_coord_hash(&split1),
            atlas_frame_coord_hash(&split2)
        );

        // Empty frame hashes stably (sha256 of nothing) rather than panicking.
        assert_eq!(atlas_frame_coord_hash(&[]), atlas_frame_coord_hash(&[]));
    }

    #[test]
    fn run_lifecycle() {
        let mut db = db();
        let slug = SourceSlug::from_path(Path::new("/tmp/canon"));
        db.upsert_source(&slug, Path::new("/tmp/canon"), 1700000000)
            .unwrap();

        let run = db.start_run(&slug, 1700000100).unwrap();
        let last = db.last_run_for_source(&slug).unwrap().unwrap();
        assert_eq!(last.id, run.as_str());
        assert!(last.finished_at_unix.is_none());
        assert_eq!(last.ok_count, 0);

        db.finish_run(&run, 5, 1, 1700000200).unwrap();
        let last = db.last_run_for_source(&slug).unwrap().unwrap();
        assert_eq!(last.finished_at_unix, Some(1700000200));
        assert_eq!(last.ok_count, 5);
        assert_eq!(last.err_count, 1);
    }

    #[test]
    fn record_error_dedup_by_path_hash() {
        let mut db = db();
        let slug = SourceSlug::from_path(Path::new("/tmp/canon"));
        db.upsert_source(&slug, Path::new("/tmp/canon"), 1700000000)
            .unwrap();
        let p = Path::new("/tmp/canon/bad.html");

        db.record_error("parse", &slug, p, "syntax", Some("h1"), 1700000100)
            .unwrap();
        db.record_error("parse", &slug, p, "syntax retry", Some("h1"), 1700000200)
            .unwrap();
        db.record_error("parse", &slug, p, "syntax retry 2", Some("h1"), 1700000300)
            .unwrap();

        let open = db.list_open_errors().unwrap();
        assert_eq!(open.len(), 1, "same (path, hash) tuple = single error row");
        assert_eq!(open[0].retry_count, 2, "two bumps → retry_count = 2");
        assert_eq!(open[0].message, "syntax retry 2");
    }

    #[test]
    fn record_error_separate_when_hash_differs() {
        let mut db = db();
        let slug = SourceSlug::from_path(Path::new("/tmp/canon"));
        db.upsert_source(&slug, Path::new("/tmp/canon"), 1700000000)
            .unwrap();
        let p = Path::new("/tmp/canon/changing.html");

        let id1 = db
            .record_error("parse", &slug, p, "v1 broken", Some("h1"), 1700000100)
            .unwrap();
        let id2 = db
            .record_error("parse", &slug, p, "v2 broken", Some("h2"), 1700000100)
            .unwrap();
        let open = db.list_open_errors().unwrap();
        assert_eq!(open.len(), 2, "different content hashes = different errors");
        // GC-F3 — same created_at: the tie breaks id-ascending (ErrorId is a
        // random token, so insertion order and id order are independent).
        let mut want = [id1.as_str().to_string(), id2.as_str().to_string()];
        want.sort();
        let got: Vec<&str> = open.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(got, want, "created_at tie must resolve id-ascending");
    }

    #[test]
    fn clear_errors_when_content_changes() {
        let mut db = db();
        let slug = SourceSlug::from_path(Path::new("/tmp/canon"));
        db.upsert_source(&slug, Path::new("/tmp/canon"), 1700000000)
            .unwrap();
        let p = Path::new("/tmp/canon/x.html");

        db.record_error("parse", &slug, p, "broken", Some("oldh"), 1700000100)
            .unwrap();
        let cleared = db.clear_errors_for_path_hash(p, "newh").unwrap();
        assert_eq!(cleared, 1);
        assert!(db.list_open_errors().unwrap().is_empty());
    }

    #[test]
    fn dismiss_error_marks_closed() {
        let mut db = db();
        let slug = SourceSlug::from_path(Path::new("/tmp/canon"));
        db.upsert_source(&slug, Path::new("/tmp/canon"), 1700000000)
            .unwrap();
        let p = Path::new("/tmp/canon/x.html");
        let id = db
            .record_error("parse", &slug, p, "x", None, 1700000100)
            .unwrap();
        db.dismiss_error(&id).unwrap();
        assert!(db.list_open_errors().unwrap().is_empty());
    }

    #[test]
    fn retry_count_returns_current_value() {
        let mut db = db();
        let slug = SourceSlug::from_path(Path::new("/tmp/canon"));
        db.upsert_source(&slug, Path::new("/tmp/canon"), 1700000000)
            .unwrap();
        let p = Path::new("/tmp/canon/x.html");
        assert_eq!(db.retry_count_for_path_hash(p, "h").unwrap(), 0);
        db.record_error("parse", &slug, p, "x", Some("h"), 1700000100)
            .unwrap();
        assert_eq!(db.retry_count_for_path_hash(p, "h").unwrap(), 0);
        db.record_error("parse", &slug, p, "x", Some("h"), 1700000200)
            .unwrap();
        assert_eq!(db.retry_count_for_path_hash(p, "h").unwrap(), 1);
        db.record_error("parse", &slug, p, "x", Some("h"), 1700000300)
            .unwrap();
        assert_eq!(db.retry_count_for_path_hash(p, "h").unwrap(), 2);
    }

    // --- edges (v0.3 F1) -------------------------------------------------

    fn link(to: &str) -> (String, String) {
        (to.to_string(), "link".to_string())
    }

    #[test]
    fn record_edges_then_outbound_depth_one() {
        let mut db = db();
        db.record_edges("a", &[link("b"), link("c")]).unwrap();
        let edges = db.edges_from("a", 1).unwrap();
        let dst: Vec<&str> = edges.iter().map(|e| e.to_id.as_str()).collect();
        assert_eq!(dst.len(), 2);
        assert!(dst.contains(&"b"));
        assert!(dst.contains(&"c"));
        for e in &edges {
            assert_eq!(e.depth, 1);
            assert_eq!(e.from_id, "a");
        }
    }

    #[test]
    fn backlinks_of_returns_inbound_linkers() {
        let mut db = db();
        // a → c, b → c, c → c (self), c → d
        db.record_edges("a", &[link("c")]).unwrap();
        db.record_edges("b", &[link("c"), link("d")]).unwrap();
        db.record_edges("c", &[link("c"), link("d")]).unwrap();
        let mut back: Vec<String> = db
            .backlinks_of("c")
            .unwrap()
            .into_iter()
            .map(|e| e.from_id)
            .collect();
        back.sort();
        // a + b link to c; the c→c self-edge is excluded.
        assert_eq!(back, vec!["a".to_string(), "b".to_string()]);
        // Every row points at the queried dst with depth 1.
        for e in db.backlinks_of("c").unwrap() {
            assert_eq!(e.to_id, "c");
            assert_eq!(e.depth, 1);
            assert_eq!(e.kind, "link");
        }
        // d has two linkers (b, c); a non-linked id has none.
        assert_eq!(db.backlinks_of("d").unwrap().len(), 2);
        assert!(db.backlinks_of("zz").unwrap().is_empty());
    }

    #[test]
    fn record_edges_replaces_previous_set() {
        let mut db = db();
        db.record_edges("a", &[link("b")]).unwrap();
        db.record_edges("a", &[link("c")]).unwrap();
        let edges = db.edges_from("a", 1).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to_id, "c");
    }

    #[test]
    fn record_edges_empty_clears_outbound() {
        let mut db = db();
        db.record_edges("a", &[link("b")]).unwrap();
        db.record_edges("a", &[]).unwrap();
        assert!(db.edges_from("a", 1).unwrap().is_empty());
    }

    #[test]
    fn edges_from_traverses_to_depth_two() {
        let mut db = db();
        db.record_edges("a", &[link("b")]).unwrap();
        db.record_edges("b", &[link("c")]).unwrap();
        let edges = db.edges_from("a", 2).unwrap();
        let pairs: Vec<(String, String, u32)> = edges
            .iter()
            .map(|e| (e.from_id.clone(), e.to_id.clone(), e.depth))
            .collect();
        assert_eq!(
            pairs,
            vec![("a".into(), "b".into(), 1), ("b".into(), "c".into(), 2),]
        );
    }

    #[test]
    fn edges_from_clamps_depth_at_three() {
        let mut db = db();
        db.record_edges("a", &[link("b")]).unwrap();
        db.record_edges("b", &[link("c")]).unwrap();
        db.record_edges("c", &[link("d")]).unwrap();
        db.record_edges("d", &[link("e")]).unwrap();
        // depth=10 should clamp to 3 — we should NOT see the d→e edge.
        let edges = db.edges_from("a", 10).unwrap();
        let dst: Vec<&str> = edges.iter().map(|e| e.to_id.as_str()).collect();
        assert!(dst.contains(&"d"));
        assert!(!dst.contains(&"e"));
    }

    #[test]
    fn edges_from_drops_self_loops() {
        let mut db = db();
        db.record_edges("a", &[link("a"), link("b")]).unwrap();
        let edges = db.edges_from("a", 1).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to_id, "b");
    }

    #[test]
    fn link_pairs_returns_only_link_kind_edges() {
        let mut db = db();
        db.record_edges(
            "a",
            &[
                ("b".to_string(), "link".to_string()),
                ("c".to_string(), "embed".to_string()),
            ],
        )
        .unwrap();
        db.record_edges("b", &[link("c")]).unwrap();
        let pairs = db.link_pairs().unwrap();
        assert_eq!(pairs.len(), 2);
        assert!(pairs.contains(&("a".into(), "b".into())));
        assert!(pairs.contains(&("b".into(), "c".into())));
        assert!(!pairs.contains(&("a".into(), "c".into())));
    }

    #[test]
    fn edges_from_handles_cycles() {
        let mut db = db();
        db.record_edges("a", &[link("b")]).unwrap();
        db.record_edges("b", &[link("a")]).unwrap();
        // Visiting `a` again at depth 2 must NOT loop back to b at depth 3.
        let edges = db.edges_from("a", 3).unwrap();
        // a → b at d=1; b → a at d=2 (a already visited as the start).
        // The b→a edge IS still emitted (visit-tracking is for queueing,
        // not for emitting), but the BFS must not re-explore from `a`.
        assert_eq!(edges.len(), 2);
    }

    // --- history (v0.6+ H1) ------------------------------------------------

    #[test]
    fn history_record_open_inserts_on_first_visit() {
        let mut db = db();
        let r = db
            .history_record_open("a1b2c3", 1_700_000_000, None, "operator")
            .unwrap();
        assert!(r.id > 0);
        assert_eq!(r.scroll_y, 0);
        assert!(r.is_new_visit);

        let rows = db.history_list(10, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "open");
        assert_eq!(rows[0].artifact_id.as_deref(), Some("a1b2c3"));
        assert_eq!(rows[0].scroll_y, 0);
    }

    // --- Reading progress (RP-track, V0014) ----------------------------

    fn dwell(id: &str, idx: i64, words: i64, dwell_ms: i64, enters: i64) -> SectionDwell {
        SectionDwell {
            section_id: id.to_string(),
            section_idx: idx,
            section_text: id.to_string(),
            level: 2,
            words,
            content_px: words * 5,
            dwell_ms,
            enters,
        }
    }

    // invariant:19 cumulative-idempotent
    #[test]
    fn reading_upsert_is_idempotent_and_monotonic() {
        let mut db = db();
        let v = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();
        let secs = vec![
            dwell("intro", 0, 100, 20_000, 1),
            dwell("body", 1, 200, 5_000, 1),
        ];
        db.reading_upsert_sections(v.id, "art", &secs, 1_700_000_010)
            .unwrap();
        // Re-send identical cumulative values → no change (dropped-beacon resend).
        db.reading_upsert_sections(v.id, "art", &secs, 1_700_000_020)
            .unwrap();
        let (rows, _) = db.reading_inputs_for_artifact("art", None).unwrap();
        assert_eq!(rows.len(), 2);
        let intro = rows.iter().find(|r| r.section_id == "intro").unwrap();
        assert_eq!(intro.dwell_ms, 20_000);
        assert_eq!(intro.enters, 1);
        // A stale beacon with SMALLER values must not regress (max-merge).
        db.reading_upsert_sections(
            v.id,
            "art",
            &[dwell("intro", 0, 100, 1_000, 1)],
            1_700_000_030,
        )
        .unwrap();
        let (rows, _) = db.reading_inputs_for_artifact("art", None).unwrap();
        let intro = rows.iter().find(|r| r.section_id == "intro").unwrap();
        assert_eq!(intro.dwell_ms, 20_000, "stale smaller value ignored");
        // A larger value DOES advance.
        db.reading_upsert_sections(
            v.id,
            "art",
            &[dwell("intro", 0, 100, 35_000, 2)],
            1_700_000_040,
        )
        .unwrap();
        let (rows, _) = db.reading_inputs_for_artifact("art", None).unwrap();
        let intro = rows.iter().find(|r| r.section_id == "intro").unwrap();
        assert_eq!(intro.dwell_ms, 35_000);
        assert_eq!(intro.enters, 2);
    }

    #[test]
    fn reading_set_active_gates_on_visit_and_max_merges() {
        let mut db = db();
        // Unknown visit → 0 rows (the endpoint's 404 gate).
        assert_eq!(
            db.reading_set_active(999, 1000, Some("x"), 1_700_000_000)
                .unwrap(),
            0
        );
        let v = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();
        assert_eq!(
            db.reading_set_active(v.id, 5_000, Some("intro"), 1_700_000_010)
                .unwrap(),
            1
        );
        // active_ms max-merges; a null last_section COALESCEs to the prior.
        db.reading_set_active(v.id, 3_000, None, 1_700_000_020)
            .unwrap();
        let st = db.reading_state_for_visit(v.id).unwrap();
        assert_eq!(st.active_ms, 5_000, "max-merge keeps the larger active_ms");
        assert_eq!(
            st.last_section.as_deref(),
            Some("intro"),
            "null keeps prior stop"
        );
        db.reading_set_active(v.id, 9_000, Some("risks"), 1_700_000_030)
            .unwrap();
        let st = db.reading_state_for_visit(v.id).unwrap();
        assert_eq!(st.active_ms, 9_000);
        assert_eq!(st.last_section.as_deref(), Some("risks"));
    }

    // invariant:19 seed-on-open
    #[test]
    fn reading_state_for_visit_seeds_resume() {
        let mut db = db();
        let v = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();
        db.reading_set_active(v.id, 12_000, Some("body"), 1_700_000_010)
            .unwrap();
        db.reading_upsert_sections(
            v.id,
            "art",
            &[dwell("intro", 0, 100, 8_000, 1)],
            1_700_000_010,
        )
        .unwrap();
        let st = db.reading_state_for_visit(v.id).unwrap();
        assert_eq!(st.active_ms, 12_000);
        assert_eq!(st.last_section.as_deref(), Some("body"));
        assert_eq!(st.sections.len(), 1);
        assert_eq!(st.sections[0].section_id, "intro");
        assert_eq!(st.sections[0].dwell_ms, 8_000);
        // A fresh, untouched visit seeds to empty/zero.
        let v2 = db
            .history_record_open("other", 1_700_000_000, None, "operator")
            .unwrap();
        let st2 = db.reading_state_for_visit(v2.id).unwrap();
        assert_eq!(st2.active_ms, 0);
        assert!(st2.last_section.is_none());
        assert!(st2.sections.is_empty());
    }

    #[test]
    fn reading_inputs_scoped_to_artifact() {
        let mut db = db();
        let a = db
            .history_record_open("aaa", 1_700_000_000, None, "operator")
            .unwrap();
        let b = db
            .history_record_open("bbb", 1_700_000_000, None, "operator")
            .unwrap();
        db.reading_upsert_sections(a.id, "aaa", &[dwell("s", 0, 10, 1_000, 1)], 1)
            .unwrap();
        db.reading_upsert_sections(b.id, "bbb", &[dwell("s", 0, 10, 2_000, 1)], 1)
            .unwrap();
        let (rows, visits) = db.reading_inputs_for_artifact("aaa", None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].dwell_ms, 1_000);
        assert_eq!(visits.len(), 1);
        assert_eq!(visits[0].visit_id, a.id);
    }

    #[test]
    fn reading_inputs_for_artifacts_batches_and_matches_single() {
        let mut db = db();
        let a = db
            .history_record_open("aaa", 1_700_000_000, None, "operator")
            .unwrap();
        let b = db
            .history_record_open("bbb", 1_700_000_000, None, "operator")
            .unwrap();
        db.reading_upsert_sections(
            a.id,
            "aaa",
            &[dwell("i", 0, 10, 1_000, 1), dwell("j", 1, 20, 2_000, 1)],
            1,
        )
        .unwrap();
        db.reading_upsert_sections(b.id, "bbb", &[dwell("s", 0, 10, 2_000, 1)], 1)
            .unwrap();

        // Batched result matches the per-artifact query, grouped by id.
        let map = db
            .reading_inputs_for_artifacts(&["aaa".to_string(), "bbb".to_string()], None)
            .unwrap();
        let (single_a, va) = db.reading_inputs_for_artifact("aaa", None).unwrap();
        let (single_b, vb) = db.reading_inputs_for_artifact("bbb", None).unwrap();
        let (batch_a, bva) = map.get("aaa").cloned().unwrap();
        let (batch_b, bvb) = map.get("bbb").cloned().unwrap();
        // Same per-artifact ordering (section_idx, visit_id) + visit rows.
        assert_eq!(
            batch_a
                .iter()
                .map(|r| r.section_id.clone())
                .collect::<Vec<_>>(),
            single_a
                .iter()
                .map(|r| r.section_id.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(batch_a.len(), 2);
        assert_eq!(batch_b.len(), single_b.len());
        assert_eq!(
            bva.iter().map(|v| v.visit_id).collect::<Vec<_>>(),
            va.iter().map(|v| v.visit_id).collect::<Vec<_>>()
        );
        assert_eq!(
            bvb.iter().map(|v| v.visit_id).collect::<Vec<_>>(),
            vb.iter().map(|v| v.visit_id).collect::<Vec<_>>()
        );
        // An id with no reading rows is simply absent; empty input → empty map.
        assert!(!map.contains_key("ccc"));
        assert!(db
            .reading_inputs_for_artifacts(&[], None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn reading_latest_for_artifact_completion() {
        let mut db = db();
        assert!(db
            .reading_latest_for_artifact("nope", "operator")
            .unwrap()
            .is_none());
        let v = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();
        db.history_update_scroll(v.id, 620, 1000, 1_700_000_010)
            .unwrap();
        db.reading_set_active(v.id, 1, Some("risks"), 1_700_000_010)
            .unwrap();
        let (pct, last, _ts) = db
            .reading_latest_for_artifact("art", "operator")
            .unwrap()
            .unwrap();
        assert_eq!(pct, 62);
        assert_eq!(last.as_deref(), Some("risks"));
    }

    #[test]
    fn reading_rollup_scroll_states_and_override_overlay() {
        use crate::lists::ReadState;
        let mut db = db();
        // No history, no overrides → empty rollup.
        assert!(db.reading_rollup("operator").unwrap().is_empty());

        // In-progress: opened + scrolled 62%.
        let v = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();
        db.history_update_scroll(v.id, 620, 1000, 1_700_000_010)
            .unwrap();
        // Fully read: scrolled to the bottom (≥ FULLY_READ_PCT).
        let d = db
            .history_record_open("done", 1_700_000_500, None, "operator")
            .unwrap();
        db.history_update_scroll(d.id, 1000, 1000, 1_700_000_510)
            .unwrap();
        // Opened but never scrolled (scroll_max stays 0) → InProgress, 0%.
        let _o = db
            .history_record_open("opened", 1_700_000_900, None, "operator")
            .unwrap();

        let roll = db.reading_rollup("operator").unwrap();
        assert_eq!(roll.len(), 3);
        assert_eq!(roll["art"].completion_pct, 62);
        assert_eq!(roll["art"].state, ReadState::InProgress);
        assert_eq!(roll["art"].last_opened_unix, Some(1_700_000_000));
        assert_eq!(roll["done"].completion_pct, 100);
        assert_eq!(roll["done"].state, ReadState::Read);
        assert_eq!(roll["opened"].completion_pct, 0);
        assert_eq!(roll["opened"].state, ReadState::InProgress);

        // A per-user list override (list_entry_user_state) beats the
        // scroll-derived state and can add an entry for an artifact that
        // was never opened. Legacy list_entries.read_override is frozen.
        db.list_create("L1", "reading", None, false, 1_700_000_000)
            .unwrap();
        let mk = |id: &str, art: &str| NewListEntry {
            id: id.into(),
            list_id: "L1".into(),
            kb: "canon".into(),
            artifact_id: art.into(),
            anchor_json: None,
            note: None,
            words: None,
            read_override: None,
        };
        db.list_entry_add(
            &mk("e_read", "art"),
            &PositionSpec::Last,
            "operator",
            1_700_000_000,
        )
        .unwrap();
        db.list_entry_add(
            &mk("e_unread", "ghost"),
            &PositionSpec::Last,
            "operator",
            1_700_000_000,
        )
        .unwrap();
        db.list_entry_set_user_override("e_read", "operator", Some("read"), 1_700_000_000)
            .unwrap();
        db.list_entry_set_user_override("e_unread", "operator", Some("unread"), 1_700_000_000)
            .unwrap();

        let roll = db.reading_rollup("operator").unwrap();
        assert_eq!(
            roll["art"].state,
            ReadState::Read,
            "override wins over the 62% scroll state"
        );
        assert_eq!(
            roll["art"].completion_pct, 62,
            "override leaves the scroll pct for the reading chip"
        );
        assert_eq!(roll["ghost"].state, ReadState::Unread);
        assert_eq!(
            roll["ghost"].last_opened_unix, None,
            "override-only row was never opened"
        );
    }

    #[test]
    fn reading_rollup_for_ids_scopes_window_and_override() {
        use crate::lists::ReadState;
        let mut db = db();
        // Empty candidate set → empty map, no scan.
        assert!(db
            .reading_rollup_for_ids(&["art".into()], "operator")
            .unwrap()
            .is_empty());
        assert!(db
            .reading_rollup_for_ids(&[], "operator")
            .unwrap()
            .is_empty());

        // art: 62% in-progress; done: 100% read; extra: opened, ignored below.
        let v = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();
        db.history_update_scroll(v.id, 620, 1000, 1_700_000_010)
            .unwrap();
        let d = db
            .history_record_open("done", 1_700_000_500, None, "operator")
            .unwrap();
        db.history_update_scroll(d.id, 1000, 1000, 1_700_000_510)
            .unwrap();
        let _e = db
            .history_record_open("extra", 1_700_000_900, None, "operator")
            .unwrap();

        // Override-only rows via list_entry_user_state: one on a candidate
        // (art), one off-set (ghost).
        db.list_create("L1", "reading", None, false, 1_700_000_000)
            .unwrap();
        let mk = |id: &str, art: &str| NewListEntry {
            id: id.into(),
            list_id: "L1".into(),
            kb: "canon".into(),
            artifact_id: art.into(),
            anchor_json: None,
            note: None,
            words: None,
            read_override: None,
        };
        db.list_entry_add(&mk("e_read", "art"), &PositionSpec::Last, "operator", 1)
            .unwrap();
        db.list_entry_add(&mk("e_ghost", "ghost"), &PositionSpec::Last, "operator", 1)
            .unwrap();
        db.list_entry_set_user_override("e_read", "operator", Some("read"), 1)
            .unwrap();
        db.list_entry_set_user_override("e_ghost", "operator", Some("unread"), 1)
            .unwrap();

        // Scoped rollup over {art, done, ghost} must byte-match the full
        // rollup filtered to those ids: extra is excluded by the window scope,
        // ghost's override survives, and art's override overlay still wins.
        let ids = vec!["art".to_string(), "done".to_string(), "ghost".to_string()];
        let scoped = db.reading_rollup_for_ids(&ids, "operator").unwrap();
        let full = db.reading_rollup("operator").unwrap();
        assert!(!scoped.contains_key("extra"), "off-set open row excluded");
        assert_eq!(scoped.len(), 3);
        for id in &ids {
            assert_eq!(
                scoped
                    .get(id)
                    .map(|r| (r.completion_pct, r.state, r.last_opened_unix)),
                full.get(id)
                    .map(|r| (r.completion_pct, r.state, r.last_opened_unix)),
                "scoped entry for {id} matches the full rollup"
            );
        }
        assert_eq!(
            scoped["art"].state,
            ReadState::Read,
            "override overlay scoped in"
        );
        assert_eq!(scoped["ghost"].last_opened_unix, None);
    }

    #[test]
    fn reading_latest_for_ids_batches_per_artifact() {
        let mut db = db();
        assert!(db
            .reading_latest_for_ids(&[], "operator")
            .unwrap()
            .is_empty());
        let a = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();
        db.history_update_scroll(a.id, 620, 1000, 1_700_000_010)
            .unwrap();
        db.reading_set_active(a.id, 1, Some("risks"), 1_700_000_010)
            .unwrap();
        let b = db
            .history_record_open("done", 1_700_000_500, None, "operator")
            .unwrap();
        db.history_update_scroll(b.id, 1000, 1000, 1_700_000_520)
            .unwrap();

        let map = db
            .reading_latest_for_ids(&["art".into(), "done".into(), "never".into()], "operator")
            .unwrap();
        assert_eq!(map.len(), 2, "never-opened id absent");
        // Each batched entry matches the single-id lookup.
        for id in ["art", "done"] {
            let single = db
                .reading_latest_for_artifact(id, "operator")
                .unwrap()
                .unwrap();
            let batched = map.get(id).unwrap();
            assert_eq!((batched.0, batched.1.clone(), batched.2), single);
        }
        assert!(!map.contains_key("never"));
    }

    #[test]
    fn reading_purge_cascades_and_counts() {
        let mut db = db();
        let v = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();
        db.reading_upsert_sections(
            v.id,
            "art",
            &[dwell("a", 0, 10, 1, 1), dwell("b", 1, 10, 1, 1)],
            1,
        )
        .unwrap();
        // 1 history row + 2 reading rows = 3 deleted, both tables emptied.
        let n = db.history_purge().unwrap();
        assert_eq!(n, 3, "count includes the cascaded reading rows");
        let (rows, visits) = db.reading_inputs_for_artifact("art", None).unwrap();
        assert!(rows.is_empty());
        assert!(visits.is_empty());
    }

    #[test]
    fn purge_kb_data_drops_reading() {
        let mut db = db();
        let v = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();
        db.reading_upsert_sections(v.id, "art", &[dwell("a", 0, 10, 1, 1)], 1)
            .unwrap();
        let n = db.purge_kb_data().unwrap();
        assert!(n >= 2, "history + reading rows counted");
        let (rows, _) = db.reading_inputs_for_artifact("art", None).unwrap();
        assert!(rows.is_empty());
    }

    // ---- R3 (v0.24) — opt-in age-based retention prune ----

    /// A history window removes visits OLDER than `now - window` and keeps
    /// every more-recent visit untouched.
    #[test]
    fn retention_prune_removes_old_history_keeps_recent() {
        const NOW: i64 = 1_700_000_000;
        const DAY: i64 = 86_400;
        let mut db = db();
        db.history_record_open("old", NOW - 40 * DAY, None, "operator")
            .unwrap();
        db.history_record_open("mid", NOW - 20 * DAY, None, "operator")
            .unwrap();
        db.history_record_open("fresh", NOW - DAY, None, "operator")
            .unwrap();
        // 30-day history window → only the 40-day-old visit is past the cutoff.
        let n = db.retention_prune(NOW, Some(30 * DAY), None).unwrap();
        assert_eq!(n, 1, "only the 40-day-old visit is pruned");
        let ids: Vec<String> = db
            .history_list(100, None, None, None)
            .unwrap()
            .into_iter()
            .filter_map(|r| r.artifact_id)
            .collect();
        assert!(!ids.iter().any(|i| i == "old"), "old visit gone");
        assert!(ids.iter().any(|i| i == "mid"), "20-day visit kept");
        assert!(ids.iter().any(|i| i == "fresh"), "1-day visit kept");
    }

    /// Both windows `None` → keep forever: even an ancient row is untouched.
    #[test]
    fn retention_prune_none_window_keeps_everything() {
        const NOW: i64 = 1_700_000_000;
        const DAY: i64 = 86_400;
        let mut db = db();
        let v = db
            .history_record_open("art", NOW - 999 * DAY, None, "operator")
            .unwrap();
        db.reading_upsert_sections(v.id, "art", &[dwell("a", 0, 10, 1, 1)], NOW - 999 * DAY)
            .unwrap();
        let n = db.retention_prune(NOW, None, None).unwrap();
        assert_eq!(n, 0, "no window set → nothing pruned");
        assert_eq!(db.history_list(100, None, None, None).unwrap().len(), 1);
        let (rows, _) = db.reading_inputs_for_artifact("art", None).unwrap();
        assert_eq!(rows.len(), 1, "ancient reading row survives with no window");
    }

    /// A history window's delete CASCADES its child reading rows, and the
    /// returned count includes those cascaded rows.
    #[test]
    fn retention_prune_history_window_cascades_child_sections() {
        const NOW: i64 = 1_700_000_000;
        const DAY: i64 = 86_400;
        let mut db = db();
        let v = db
            .history_record_open("art", NOW - 90 * DAY, None, "operator")
            .unwrap();
        db.reading_upsert_sections(
            v.id,
            "art",
            &[dwell("a", 0, 10, 1, 1), dwell("b", 1, 10, 1, 1)],
            NOW - 90 * DAY,
        )
        .unwrap();
        // 30-day window → the 90-day visit + BOTH child sections go.
        let n = db.retention_prune(NOW, Some(30 * DAY), None).unwrap();
        assert_eq!(n, 3, "1 history row + 2 cascaded reading rows");
        assert!(db.history_list(100, None, None, None).unwrap().is_empty());
        let (rows, _) = db.reading_inputs_for_artifact("art", None).unwrap();
        assert!(rows.is_empty());
    }

    // ---- GC-B6 — incremental auto-vacuum on retention prune ----

    /// A brand-new database gets incremental auto-vacuum for free at
    /// creation (no VACUUM needed — the file has no schema yet).
    #[test]
    fn fresh_db_enables_incremental_auto_vacuum() {
        let db = db();
        let mode: i64 = db
            .conn
            .pragma_query_value(None, "auto_vacuum", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, 2, "PRAGMA auto_vacuum reports INCREMENTAL");
    }

    /// Opens a database exactly the way pre-GC-B6 `Db::open` did (no
    /// `auto_vacuum` pragma), so tests can simulate the "existing kb sqlite
    /// file predating retention" case the guarded migration must handle.
    fn legacy_db(path: &Path) -> Db {
        let mut conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        embedded::migrations::runner().run(&mut conn).unwrap();
        Db { conn }
    }

    /// Reopening an EXISTING (already-migrated) database must NOT flip
    /// `auto_vacuum` for free — SQLite ignores the pragma on a non-empty
    /// database short of a full `VACUUM`, which `Db::open` deliberately
    /// does not run (that's `retention_prune`'s guarded, opt-in job).
    #[test]
    fn reopening_existing_db_leaves_auto_vacuum_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("legacy.db");
        {
            let db = legacy_db(&path);
            let mode: i64 = db
                .conn
                .pragma_query_value(None, "auto_vacuum", |row| row.get(0))
                .unwrap();
            assert_eq!(mode, 0, "legacy db starts life with auto_vacuum off");
        }
        let db = Db::open(&path).unwrap();
        let mode: i64 = db
            .conn
            .pragma_query_value(None, "auto_vacuum", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, 0, "Db::open never VACUUMs an existing db by itself");
    }

    /// Below the size threshold, an enabled retention prune leaves an
    /// existing `auto_vacuum=NONE` database alone (no surprise VACUUM on a
    /// lightly used kb).
    #[test]
    fn retention_prune_skips_auto_vacuum_upgrade_below_threshold() {
        const NOW: i64 = 1_700_000_000;
        const DAY: i64 = 86_400;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("legacy.db");
        let mut db = legacy_db(&path);
        db.history_record_open("art", NOW - 90 * DAY, None, "operator")
            .unwrap();

        let n = db.retention_prune(NOW, Some(30 * DAY), None).unwrap();
        assert_eq!(n, 1);
        let mode: i64 = db
            .conn
            .pragma_query_value(None, "auto_vacuum", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, 0, "tiny db stays under the upgrade threshold");
    }

    /// The load-bearing case: an existing (pre-retention) database that has
    /// grown past the size threshold gets a one-time VACUUM to incremental
    /// auto-vacuum, and pruning bulk-deleted rows actually reclaims disk —
    /// both the sqlite freelist and the file on disk shrink.
    #[test]
    fn retention_prune_reclaims_disk_on_existing_db() {
        const NOW: i64 = 1_700_000_000;
        const DAY: i64 = 86_400;
        const ROWS: i64 = 35_000;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("legacy.db");
        let mut db = legacy_db(&path);

        // Bulk-insert enough old history rows (one transaction, for test
        // speed) to push the file past AUTO_VACUUM_UPGRADE_MIN_BYTES.
        {
            let tx = db.conn.transaction().unwrap();
            for i in 0..ROWS {
                tx.execute(
                    "INSERT INTO history (kind, artifact_id, started_at, updated_at)
                     VALUES ('open', ?1, ?2, ?2)",
                    params![format!("bulk{i}"), NOW - 90 * DAY],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }

        // Force the WAL-resident bulk insert onto the main file so the
        // on-disk size below reflects reality (under WAL, writes land in
        // `-wal` and the main file doesn't grow until a checkpoint).
        let _ = db
            .conn
            .pragma(None, "wal_checkpoint", "TRUNCATE", |_row| Ok(()));
        let size_before = std::fs::metadata(&path).unwrap().len() as i64;
        assert!(
            size_before >= RetentionSection::AUTO_VACUUM_UPGRADE_MIN_BYTES,
            "test fixture must cross the upgrade threshold (got {size_before} bytes)"
        );
        let page_count_before: i64 = db
            .conn
            .pragma_query_value(None, "page_count", |row| row.get(0))
            .unwrap();

        // 30-day history window → every bulk row (90 days old) is pruned.
        let n = db.retention_prune(NOW, Some(30 * DAY), None).unwrap();
        assert_eq!(n, ROWS as usize);

        let mode: i64 = db
            .conn
            .pragma_query_value(None, "auto_vacuum", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, 2, "one-time upgrade to INCREMENTAL auto_vacuum");

        let freelist_after: i64 = db
            .conn
            .pragma_query_value(None, "freelist_count", |row| row.get(0))
            .unwrap();
        let page_count_after: i64 = db
            .conn
            .pragma_query_value(None, "page_count", |row| row.get(0))
            .unwrap();
        let size_after = std::fs::metadata(&path).unwrap().len() as i64;

        assert_eq!(
            freelist_after, 0,
            "the bounded incremental_vacuum comfortably covers this bulk \
             delete, so every freed page should be reclaimed (not just \
             marked free) — got {freelist_after} pages still on the freelist"
        );
        assert!(
            page_count_after < page_count_before,
            "page_count should drop once the freed pages are reclaimed \
             (before={page_count_before}, after={page_count_after})"
        );
        assert!(
            size_after < size_before,
            "file size should shrink after reclaiming the bulk delete \
             (before={size_before}, after={size_after})"
        );
    }

    /// An independent reading window prunes stale SECTION rows while KEEPING
    /// their (recent) parent visit — the more-aggressive-than-parent case.
    #[test]
    fn retention_prune_reading_window_keeps_recent_parent_visit() {
        const NOW: i64 = 1_700_000_000;
        const DAY: i64 = 86_400;
        let mut db = db();
        // A RECENT visit (well inside any history window) ...
        let v = db
            .history_record_open("art", NOW - DAY, None, "operator")
            .unwrap();
        // ... whose section rows were last observed long ago.
        db.reading_upsert_sections(v.id, "art", &[dwell("a", 0, 10, 1, 1)], NOW - 60 * DAY)
            .unwrap();
        // reading window 30d, history window OFF → prune the stale section,
        // keep the visit.
        let n = db.retention_prune(NOW, None, Some(30 * DAY)).unwrap();
        assert_eq!(n, 1, "the stale section row is pruned");
        let (rows, _) = db.reading_inputs_for_artifact("art", None).unwrap();
        assert!(rows.is_empty(), "stale section gone");
        assert_eq!(
            db.history_list(100, None, None, None).unwrap().len(),
            1,
            "the recent parent visit is kept"
        );
    }

    #[test]
    fn history_opens_in_window_filters_by_time() {
        let mut db = db();
        let _a = db
            .history_record_open("early", 1_000, None, "operator")
            .unwrap();
        let _b = db
            .history_record_open("mid", 2_000, None, "operator")
            .unwrap();
        let _c = db
            .history_record_open("late", 3_000, None, "operator")
            .unwrap();
        let rows = db.history_opens_in_window(1_500, 2_500, 100).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].artifact_id.as_deref(), Some("mid"));
    }

    #[test]
    fn history_comments_in_window_filters_by_time_and_kind() {
        let mut db = db();
        db.history_record_comment("artA", "c-1", 1_000, "operator")
            .unwrap();
        db.history_record_comment("artB", "c-2", 2_000, "operator")
            .unwrap();
        db.history_record_comment("artC", "c-3", 3_000, "operator")
            .unwrap();
        // An `open` row at the same time must NOT be returned (kind gate).
        db.history_record_open("artB", 2_000, None, "operator")
            .unwrap();
        let rows = db.history_comments_in_window(1_500, 2_500, 100).unwrap();
        assert_eq!(rows.len(), 1, "only the in-window comment, not the open");
        assert_eq!(rows[0].artifact_id.as_deref(), Some("artB"));
        assert_eq!(rows[0].comment_id.as_deref(), Some("c-2"));
        assert_eq!(rows[0].kind, "comment");
    }

    #[test]
    fn history_counts_by_day_groups_by_utc_day_and_kind() {
        let mut db = db();
        // Two opens on the same UTC day (2021-01-01), one search the same
        // day, and one comment the NEXT UTC day (2021-01-02) — day boundary
        // must split them even though the gap is a single second at the
        // 2021-01-01/02 midnight-UTC edge.
        db.history_record_open("a", 1_609_459_200, None, "operator")
            .unwrap(); // 2021-01-01 00:00:00Z
        db.history_record_open("b", 1_609_462_800, None, "operator")
            .unwrap(); // 2021-01-01 01:00:00Z
        db.history_record_search("q", 1_609_500_000, "operator")
            .unwrap(); // 2021-01-01, later
        db.history_record_comment("c", "cm-1", 1_609_545_600, "operator")
            .unwrap(); // 2021-01-02 00:00:00Z

        let rows = db
            .history_counts_by_day(1_609_459_200, 1_609_545_600)
            .unwrap();
        assert_eq!(
            rows,
            vec![
                DayKindCount {
                    day: "2021-01-01".into(),
                    kind: "open".into(),
                    count: 2,
                },
                DayKindCount {
                    day: "2021-01-01".into(),
                    kind: "search".into(),
                    count: 1,
                },
                DayKindCount {
                    day: "2021-01-02".into(),
                    kind: "comment".into(),
                    count: 1,
                },
            ],
            "ordered by day ASC then kind ASC"
        );
    }

    #[test]
    fn history_counts_by_day_window_bounds_are_inclusive() {
        let mut db = db();
        db.history_record_open("a", 999, None, "operator").unwrap();
        db.history_record_open("b", 1_000, None, "operator")
            .unwrap();
        db.history_record_open("c", 2_000, None, "operator")
            .unwrap();
        db.history_record_open("d", 2_001, None, "operator")
            .unwrap();
        let rows = db.history_counts_by_day(1_000, 2_000).unwrap();
        let total: i64 = rows.iter().map(|r| r.count).sum();
        assert_eq!(total, 2, "both from_unix and to_unix are inclusive bounds");
    }

    #[test]
    fn history_counts_by_day_empty_range_returns_empty() {
        let mut db = db();
        db.history_record_open("a", 1_000, None, "operator")
            .unwrap();
        let rows = db.history_counts_by_day(5_000, 6_000).unwrap();
        assert!(rows.is_empty());
    }

    // invariant:8 append-only
    #[test]
    fn history_record_open_within_30min_bumps_existing_row() {
        let mut db = db();
        let r1 = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();
        db.history_update_scroll(r1.id, 420, 1200, 1_700_000_100)
            .unwrap();
        // 10 minutes later — same visit.
        let r2 = db
            .history_record_open("art", 1_700_000_600, None, "operator")
            .unwrap();
        assert_eq!(r2.id, r1.id, "same visit → same row id");
        assert_eq!(r2.scroll_y, 420, "returns prior scroll for resume");
        assert!(!r2.is_new_visit, "bump within gap is not a new visit");

        let rows = db.history_list(10, None, None, None).unwrap();
        assert_eq!(rows.len(), 1, "no new row created within the gap");
    }

    // --- GC-B5: history.source (who opened it: web vs cli) -----------------

    #[test]
    fn history_record_open_stamps_source_on_insert() {
        let mut db = db();
        db.history_record_open("cli-art", 1_700_000_000, Some("cli"), "operator")
            .unwrap();
        db.history_record_open("web-art", 1_700_000_000, Some("web"), "operator")
            .unwrap();
        db.history_record_open("legacy-art", 1_700_000_000, None, "operator")
            .unwrap();

        let rows = db.history_list(10, None, Some("open"), None).unwrap();
        let by_artifact = |id: &str| -> Option<String> {
            rows.iter()
                .find(|r| r.artifact_id.as_deref() == Some(id))
                .and_then(|r| r.source.clone())
        };
        assert_eq!(by_artifact("cli-art"), Some("cli".to_string()));
        assert_eq!(by_artifact("web-art"), Some("web".to_string()));
        assert_eq!(
            by_artifact("legacy-art"),
            None,
            "no source ⇒ NULL, not \"web\""
        );
    }

    #[test]
    fn history_record_open_bump_does_not_overwrite_source() {
        let mut db = db();
        let r1 = db
            .history_record_open("art", 1_700_000_000, Some("cli"), "operator")
            .unwrap();
        // 10 minutes later, resumed via the SPA — still the same visit
        // row (append-only bump), so the ORIGINAL opener's source must
        // survive even though this call passes a different source.
        let r2 = db
            .history_record_open("art", 1_700_000_600, Some("web"), "operator")
            .unwrap();
        assert_eq!(r2.id, r1.id, "same visit → same row id");
        assert!(!r2.is_new_visit);

        let rows = db.history_list(10, None, Some("open"), None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].source.as_deref(),
            Some("cli"),
            "bump must not relabel who started the visit"
        );
    }

    #[test]
    fn history_record_open_after_30min_starts_new_row() {
        let mut db = db();
        let r1 = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();
        // 31 minutes later — new visit.
        let r2 = db
            .history_record_open("art", 1_700_000_000 + 31 * 60, None, "operator")
            .unwrap();
        assert_ne!(r2.id, r1.id);
        assert_eq!(r2.scroll_y, 0, "fresh visit starts at scroll 0");
        assert!(r2.is_new_visit, "past the gap is a new visit");

        let rows = db.history_list(10, None, None, None).unwrap();
        assert_eq!(rows.len(), 2);
    }

    /// v0.34 X1 — two users opening the same artifact within the 30-min
    /// gap get SEPARATE rows and independent scroll_y_max.
    #[test]
    fn history_record_open_two_users_within_gap_are_independent() {
        let mut db = db();
        let a = db
            .history_record_open("art", 1_700_000_000, None, "alice")
            .unwrap();
        let b = db
            .history_record_open("art", 1_700_000_100, None, "bob")
            .unwrap();
        assert_ne!(a.id, b.id, "different users must not share a visit row");
        assert!(a.is_new_visit && b.is_new_visit);

        db.history_update_scroll(a.id, 900, 1000, 1_700_000_200)
            .unwrap();
        db.history_update_scroll(b.id, 100, 1000, 1_700_000_200)
            .unwrap();

        // Alice's gap bump resumes her row, not Bob's.
        let a2 = db
            .history_record_open("art", 1_700_000_300, None, "alice")
            .unwrap();
        assert_eq!(a2.id, a.id);
        assert_eq!(a2.scroll_y, 900);
        assert!(!a2.is_new_visit);

        let rows = db.history_list(10, None, Some("open"), None).unwrap();
        assert_eq!(rows.len(), 2);
        let alice = rows.iter().find(|r| r.user == "alice").unwrap();
        let bob = rows.iter().find(|r| r.user == "bob").unwrap();
        assert_eq!(alice.scroll_y_max, 900);
        assert_eq!(bob.scroll_y_max, 100);
    }

    /// v0.34 X1 — rollup for user A ignores user B's visits.
    #[test]
    fn reading_rollup_is_per_user() {
        use crate::lists::ReadState;
        let mut db = db();
        let a = db
            .history_record_open("art", 1_700_000_000, None, "alice")
            .unwrap();
        db.history_update_scroll(a.id, 1000, 1000, 1_700_000_010)
            .unwrap(); // alice fully read
        let b = db
            .history_record_open("art", 1_700_000_100, None, "bob")
            .unwrap();
        db.history_update_scroll(b.id, 200, 1000, 1_700_000_110)
            .unwrap(); // bob 20%

        let alice_roll = db.reading_rollup("alice").unwrap();
        assert_eq!(alice_roll["art"].state, ReadState::Read);
        assert_eq!(alice_roll["art"].completion_pct, 100);

        let bob_roll = db.reading_rollup("bob").unwrap();
        assert_eq!(bob_roll["art"].state, ReadState::InProgress);
        assert_eq!(bob_roll["art"].completion_pct, 20);

        assert!(
            db.reading_rollup("carol").unwrap().is_empty(),
            "carol never opened anything"
        );
    }

    /// v0.34 X1 — identity_backfill is idempotent via the marker row;
    /// second run returns 0 without re-deriving.
    #[test]
    fn identity_backfill_is_idempotent() {
        let mut db = db();
        // Pre-multi-user shape: history row with user='' + legacy list
        // override on the frozen column (raw SQL — list_entry_add no
        // longer writes the legacy column).
        db.conn
            .execute(
                "INSERT INTO history (kind, artifact_id, scroll_y, scroll_max, started_at, updated_at, \"user\")
                 VALUES ('open', 'legacy', 0, 0, 100, 100, '')",
                [],
            )
            .unwrap();
        db.list_create("L1", "reading", None, false, 100).unwrap();
        db.list_entry_add(
            &NewListEntry {
                id: "e1".into(),
                list_id: "L1".into(),
                kb: "canon".into(),
                artifact_id: "legacy".into(),
                anchor_json: None,
                note: None,
                words: None,
                read_override: None,
            },
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
        db.conn
            .execute(
                "UPDATE list_entries SET read_override = 'read' WHERE id = 'e1'",
                [],
            )
            .unwrap();

        let n1 = db.identity_backfill("operator", 1_700_000_000).unwrap();
        assert!(n1 >= 2, "history rewrite + override copy; got {n1}");
        let n2 = db.identity_backfill("operator", 1_700_000_000).unwrap();
        assert_eq!(n2, 0, "second backfill must be a no-op via marker");

        let rows = db.history_list(10, None, None, Some("operator")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].user, "operator");
        let ovs = db
            .list_entry_user_overrides_for_list("L1", "operator")
            .unwrap();
        assert_eq!(ovs.get("e1").map(String::as_str), Some("read"));
    }

    /// v0.34 X1 — cleared overrides must stay cleared across reboots.
    /// Without the marker, a second `INSERT OR IGNORE` from the frozen
    /// legacy column would resurrect a user-cleared row.
    #[test]
    fn identity_backfill_does_not_resurrect_cleared_overrides() {
        let mut db = db();
        db.list_create("L1", "reading", None, false, 100).unwrap();
        db.list_entry_add(
            &NewListEntry {
                id: "e1".into(),
                list_id: "L1".into(),
                kb: "canon".into(),
                artifact_id: "legacy".into(),
                anchor_json: None,
                note: None,
                words: None,
                read_override: None,
            },
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
        // Simulate pre-multi-user legacy column still holding a value.
        db.conn
            .execute(
                "UPDATE list_entries SET read_override = 'read' WHERE id = 'e1'",
                [],
            )
            .unwrap();

        db.identity_backfill("operator", 1_700_000_000).unwrap();
        let ovs = db
            .list_entry_user_overrides_for_list("L1", "operator")
            .unwrap();
        assert_eq!(ovs.get("e1").map(String::as_str), Some("read"));

        // User clears the override (DELETE of the list_entry_user_state row).
        db.list_entry_set_user_override("e1", "operator", None, 1_700_000_100)
            .unwrap();
        assert!(db
            .list_entry_user_overrides_for_list("L1", "operator")
            .unwrap()
            .is_empty());

        // Boot again — marker short-circuits; override stays gone.
        let n = db.identity_backfill("operator", 1_700_000_200).unwrap();
        assert_eq!(n, 0, "marker must short-circuit");
        assert!(
            db.list_entry_user_overrides_for_list("L1", "operator")
                .unwrap()
                .is_empty(),
            "cleared override must not be resurrected from frozen legacy column"
        );
    }

    /// v0.34 X1 — list_entry_user_state override wins per-user; legacy
    /// column is ignored by rollup after the freeze. Pins exactly the
    /// panel catch: user A sets an override, user B's rollup unaffected.
    #[test]
    fn list_entry_user_override_is_per_user_and_legacy_ignored() {
        use crate::lists::ReadState;
        let mut db = db();
        db.list_create("L1", "reading", None, false, 100).unwrap();
        db.list_entry_add(
            &NewListEntry {
                id: "e1".into(),
                list_id: "L1".into(),
                kb: "canon".into(),
                artifact_id: "art".into(),
                anchor_json: None,
                note: None,
                words: None,
                read_override: None,
            },
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
        // Seed the frozen legacy column directly — rollup must NOT see it
        // without a list_entry_user_state row.
        db.conn
            .execute(
                "UPDATE list_entries SET read_override = 'read' WHERE id = 'e1'",
                [],
            )
            .unwrap();

        // Without user-state, rollup ignores the legacy column.
        assert!(
            db.reading_rollup("alice").unwrap().is_empty(),
            "legacy list_entries.read_override must not feed rollup"
        );

        // Alice sets unread; Bob leaves unset.
        db.list_entry_set_user_override("e1", "alice", Some("unread"), 200)
            .unwrap();
        let alice = db.reading_rollup("alice").unwrap();
        assert_eq!(alice["art"].state, ReadState::Unread);
        assert!(
            db.reading_rollup("bob").unwrap().is_empty(),
            "bob has no override and no visits"
        );

        // Per-user map for the assemble path (phase Y threads this into
        // derive_read_state's override arg).
        let ovs = db
            .list_entry_user_overrides_for_list("L1", "alice")
            .unwrap();
        assert_eq!(ovs.get("e1").map(String::as_str), Some("unread"));
        let bob_ovs = db.list_entry_user_overrides_for_list("L1", "bob").unwrap();
        assert!(bob_ovs.is_empty());

        // Clearing Alice's override removes it.
        db.list_entry_set_user_override("e1", "alice", None, 300)
            .unwrap();
        assert!(db.reading_rollup("alice").unwrap().is_empty());
    }

    /// v0.34 X1 — import with read markers lands per-user rows and leaves
    /// the legacy column NULL; entry patch writes the new table.
    #[test]
    fn list_import_and_entry_patch_write_user_state_not_legacy() {
        let mut db = db();
        db.list_create("l_aaa", "L", None, false, 100).unwrap();
        let mut e = nle("le_a", "l_aaa", "art1", None);
        e.read_override = Some("read".into());
        let n = db
            .list_import_entries("l_aaa", ImportMode::Replace, &[e], "alice", 200)
            .unwrap();
        assert_eq!(n, 1);
        let row = db.list_entry_get("le_a").unwrap().unwrap();
        assert!(
            row.read_override.is_none(),
            "legacy column must stay NULL after import"
        );
        let ovs = db
            .list_entry_user_overrides_for_list("l_aaa", "alice")
            .unwrap();
        assert_eq!(ovs.get("le_a").map(String::as_str), Some("read"));

        // Entry patch routes to user state for the passed user.
        db.list_entry_update(
            "le_a",
            &Patch::Keep,
            &Patch::Keep,
            &Patch::Set("unread".into()),
            "alice",
            300,
        )
        .unwrap();
        let row = db.list_entry_get("le_a").unwrap().unwrap();
        assert!(row.read_override.is_none(), "patch must not write legacy");
        let ovs = db
            .list_entry_user_overrides_for_list("l_aaa", "alice")
            .unwrap();
        assert_eq!(ovs.get("le_a").map(String::as_str), Some("unread"));
        // Other users unaffected.
        assert!(db
            .list_entry_user_overrides_for_list("l_aaa", "bob")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn history_update_scroll_only_targets_open_rows() {
        let mut db = db();
        let search_id = db
            .history_record_search("rust", 1_700_000_000, "operator")
            .unwrap();
        let n = db
            .history_update_scroll(search_id, 999, 999, 1_700_000_010)
            .unwrap();
        assert_eq!(n, 0, "scroll update on search row is a no-op");

        let r = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();
        let n = db
            .history_update_scroll(r.id, 555, 2000, 1_700_000_020)
            .unwrap();
        assert_eq!(n, 1);

        let rows = db.history_list(10, None, Some("open"), None).unwrap();
        assert_eq!(rows[0].scroll_y, 555);
        assert_eq!(rows[0].scroll_max, 2000);
    }

    #[test]
    fn history_update_scroll_keeps_high_water_mark() {
        // The user scrolls forward to the bottom, then back up to
        // re-read. scroll_y reflects the current (back-up) position
        // for resume; scroll_y_max stays at the furthest point reached
        // so the SPA's "fully read" ✓ chip stays sticky.
        let mut db = db();
        let r = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();

        // Scroll to ~near-bottom.
        db.history_update_scroll(r.id, 900, 1000, 1_700_000_010)
            .unwrap();
        let rows = db.history_list(10, None, Some("open"), None).unwrap();
        assert_eq!(rows[0].scroll_y, 900);
        assert_eq!(rows[0].scroll_y_max, 900);
        assert_eq!(rows[0].scroll_max, 1000);

        // Scroll back up to re-read the intro.
        db.history_update_scroll(r.id, 200, 1000, 1_700_000_020)
            .unwrap();
        let rows = db.history_list(10, None, Some("open"), None).unwrap();
        assert_eq!(rows[0].scroll_y, 200, "current position follows the user");
        assert_eq!(
            rows[0].scroll_y_max, 900,
            "high-water mark stays at the furthest point"
        );
        assert_eq!(rows[0].scroll_max, 1000);

        // Scroll forward past the prior peak — high-water mark bumps.
        db.history_update_scroll(r.id, 950, 1000, 1_700_000_030)
            .unwrap();
        let rows = db.history_list(10, None, Some("open"), None).unwrap();
        assert_eq!(rows[0].scroll_y, 950);
        assert_eq!(rows[0].scroll_y_max, 950, "high-water mark advances");
    }

    #[test]
    fn history_record_open_after_30min_resets_high_water_mark() {
        // A fresh visit row should NOT inherit the prior visit's
        // scroll_y_max — the chip resets per visit (per-visit decision;
        // see plan: when-reading-and-keeping-sharded-crayon).
        let mut db = db();
        let r1 = db
            .history_record_open("art", 1_700_000_000, None, "operator")
            .unwrap();
        db.history_update_scroll(r1.id, 950, 1000, 1_700_000_010)
            .unwrap();

        // 31 minutes later — new row.
        let r2 = db
            .history_record_open("art", 1_700_000_000 + 31 * 60, None, "operator")
            .unwrap();
        assert_ne!(r2.id, r1.id);
        assert!(r2.is_new_visit);

        let rows = db.history_list(10, None, Some("open"), None).unwrap();
        // Newest-first: rows[0] is the new visit, rows[1] the prior.
        assert_eq!(rows[0].id, r2.id);
        assert_eq!(rows[0].scroll_y, 0, "fresh visit starts at scroll 0");
        assert_eq!(
            rows[0].scroll_y_max, 0,
            "fresh visit starts with a clean high-water mark"
        );
        assert_eq!(rows[1].id, r1.id);
        assert_eq!(rows[1].scroll_y_max, 950, "prior visit's mark preserved");
    }

    #[test]
    fn history_record_search_dedups_within_5s() {
        let mut db = db();
        let id1 = db
            .history_record_search("borrow checker", 1_700_000_000, "operator")
            .unwrap();
        let id2 = db
            .history_record_search("borrow checker", 1_700_000_003, "operator")
            .unwrap();
        assert_eq!(id1, id2, "same query within 5s → same row");

        let rows = db.history_list(10, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
    }

    /// v0.34 X1 — search dedup is per (query, user): alice+bob within 5s
    /// → TWO rows; alice twice within 5s → ONE row.
    #[test]
    fn history_record_search_two_users_within_5s_are_independent() {
        let mut db = db();
        let a = db
            .history_record_search("borrow checker", 1_700_000_000, "alice")
            .unwrap();
        let b = db
            .history_record_search("borrow checker", 1_700_000_002, "bob")
            .unwrap();
        assert_ne!(a, b, "different users must not share a search-dedup row");

        let a2 = db
            .history_record_search("borrow checker", 1_700_000_004, "alice")
            .unwrap();
        assert_eq!(a2, a, "same user + same query within 5s → same row");

        let rows = db.history_list(10, None, Some("search"), None).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r.user == "alice"));
        assert!(rows.iter().any(|r| r.user == "bob"));
    }

    #[test]
    fn history_record_search_new_row_after_5s_or_different_query() {
        let mut db = db();
        db.history_record_search("rust", 1_700_000_000, "operator")
            .unwrap();
        db.history_record_search("rust", 1_700_000_010, "operator")
            .unwrap(); // > 5s
        db.history_record_search("python", 1_700_000_011, "operator")
            .unwrap(); // different query
        let rows = db.history_list(10, None, None, None).unwrap();
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn history_record_comment_always_inserts() {
        let mut db = db();
        db.history_record_comment("art", "c-1", 1_700_000_000, "operator")
            .unwrap();
        db.history_record_comment("art", "c-2", 1_700_000_010, "operator")
            .unwrap();
        let rows = db.history_list(10, None, Some("comment"), None).unwrap();
        assert_eq!(rows.len(), 2);
        // Newest first.
        assert_eq!(rows[0].comment_id.as_deref(), Some("c-2"));
        assert_eq!(rows[1].comment_id.as_deref(), Some("c-1"));
    }

    #[test]
    fn history_list_filters_by_kind_and_before_cursor() {
        let mut db = db();
        db.history_record_open("a", 1_700_000_000, None, "operator")
            .unwrap();
        db.history_record_search("q", 1_700_000_100, "operator")
            .unwrap();
        db.history_record_comment("a", "c-1", 1_700_000_200, "operator")
            .unwrap();
        db.history_record_open("b", 1_700_000_300, None, "operator")
            .unwrap();

        // No filter, no cursor.
        assert_eq!(db.history_list(10, None, None, None).unwrap().len(), 4);
        // Kind filter.
        assert_eq!(
            db.history_list(10, None, Some("open"), None).unwrap().len(),
            2
        );
        assert_eq!(
            db.history_list(10, None, Some("search"), None)
                .unwrap()
                .len(),
            1
        );
        // Cursor: only rows strictly older than the cursor.
        let older = db
            .history_list(10, Some(1_700_000_200), None, None)
            .unwrap();
        assert_eq!(older.len(), 2);
        assert!(older.iter().all(|r| r.started_at_unix < 1_700_000_200));
    }

    #[test]
    fn history_list_limit_clamps_rows() {
        let mut db = db();
        for i in 0..7 {
            db.history_record_search(&format!("q-{i}"), 1_700_000_000 + i, "operator")
                .unwrap();
        }
        let rows = db.history_list(3, None, None, None).unwrap();
        assert_eq!(rows.len(), 3);
        // Newest first — last inserted has the largest started_at.
        assert_eq!(rows[0].query.as_deref(), Some("q-6"));
    }

    // --- shares (kb share registry, V0004) --------------------------------

    fn sample_share(name: &str, target: &str) -> ShareRow {
        ShareRow {
            name: name.to_string(),
            target: target.to_string(),
            host: "cloudflare-pages".to_string(),
            deployed_url: format!("https://{name}.pages.dev"),
            gate: Some("email:example.com".to_string()),
            cf_account_id: Some("acct".to_string()),
            pages_project: Some(name.to_string()),
            cf_deployment_id: Some("dep-1".to_string()),
            access_app_id: Some("app-1".to_string()),
            access_policy_id: Some("pol-1".to_string()),
            github_repo: None,
            created_at_unix: 1_700_000_000,
            updated_at_unix: 1_700_000_000,
        }
    }

    #[test]
    fn shares_insert_list_get_delete_round_trip() {
        let mut db = db();
        let row = sample_share("kb-share-foo-abc123", "research/foo.html");
        db.shares_insert(&row).unwrap();

        let list = db.shares_list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], row);

        let got = db.shares_get("kb-share-foo-abc123").unwrap().unwrap();
        assert_eq!(got, row);
        assert!(db.shares_get("missing").unwrap().is_none());

        let n = db.shares_delete("kb-share-foo-abc123").unwrap();
        assert_eq!(n, 1);
        assert!(db.shares_list().unwrap().is_empty());
        assert_eq!(
            db.shares_delete("kb-share-foo-abc123").unwrap(),
            0,
            "deleting a missing share affects 0 rows"
        );
    }

    #[test]
    fn shares_insert_upsert_preserves_created_at() {
        let mut db = db();
        let mut row = sample_share("kb-share-bar-xyz", "research/bar.html");
        db.shares_insert(&row).unwrap();
        // Simulate --update: same name, new url + deployment + bumped updated_at.
        row.deployed_url = "https://kb-share-bar-xyz.pages.dev/v2".to_string();
        row.cf_deployment_id = Some("dep-2".to_string());
        row.updated_at_unix = 1_700_009_999;
        db.shares_insert(&row).unwrap();

        let list = db.shares_list().unwrap();
        assert_eq!(list.len(), 1, "upsert on name, not a second row");
        assert_eq!(
            list[0].created_at_unix, 1_700_000_000,
            "created_at preserved across --update"
        );
        assert_eq!(list[0].updated_at_unix, 1_700_009_999);
        assert_eq!(list[0].cf_deployment_id.as_deref(), Some("dep-2"));
    }

    #[test]
    fn shares_get_by_target_returns_most_recent() {
        let mut db = db();
        let mut a = sample_share("kb-share-t-1", "research/t.html");
        a.created_at_unix = 1_700_000_000;
        db.shares_insert(&a).unwrap();
        let mut b = sample_share("kb-share-t-2", "research/t.html");
        b.created_at_unix = 1_700_000_500;
        db.shares_insert(&b).unwrap();

        let got = db.shares_get_by_target("research/t.html").unwrap().unwrap();
        assert_eq!(got.name, "kb-share-t-2", "most recent by created_at");
        assert!(db.shares_get_by_target("nope").unwrap().is_none());
    }

    // --- doc_first_seen (v0.33 X2) ----------------------------------------

    #[test]
    fn first_seen_insert_ignore_is_idempotent() {
        let mut db = db();
        assert!(db.first_seen_is_empty().unwrap());
        assert!(db.first_seen_insert_ignore("art-a", 100).unwrap());
        assert!(!db.first_seen_insert_ignore("art-a", 999).unwrap());
        let map = db
            .first_seen_for_ids(&["art-a".into(), "missing".into()])
            .unwrap();
        assert_eq!(map.get("art-a"), Some(&100), "original ts preserved");
        assert!(!map.contains_key("missing"));
        assert!(!db.first_seen_is_empty().unwrap());
    }

    #[test]
    fn first_seen_seed_insert_or_ignore() {
        let mut db = db();
        let n = db
            .first_seen_seed(&[
                ("a".into(), 10),
                ("b".into(), 20),
                ("a".into(), 99), // duplicate id in batch — second OR IGNORE
            ])
            .unwrap();
        // Two successful inserts; the third conflicts with the first.
        assert_eq!(n, 2);
        let map = db.first_seen_for_ids(&["a".into(), "b".into()]).unwrap();
        assert_eq!(map.get("a"), Some(&10));
        assert_eq!(map.get("b"), Some(&20));
    }

    #[test]
    fn first_seen_coalesce_prefers_created_then_mtime_then_indexed() {
        assert_eq!(
            Db::first_seen_coalesce_ts(Some(1), Some(2), Some(3)),
            Some(1)
        );
        assert_eq!(Db::first_seen_coalesce_ts(None, Some(2), Some(3)), Some(2));
        assert_eq!(Db::first_seen_coalesce_ts(None, None, Some(3)), Some(3));
        assert_eq!(Db::first_seen_coalesce_ts(None, None, None), None);
    }

    #[test]
    fn first_seen_rekeys_on_relocate_cascade() {
        let mut db = db();
        db.first_seen_insert_ignore("oldid0000001", 42).unwrap();
        // Minimal moves intent so cascade can complete.
        let mid = db
            .moves_insert_intent("oldid0000001", "newid0000001", "old.html", "new.html", 1000)
            .unwrap();
        db.cascade_relocate_doc(
            "oldid0000001",
            "newid0000001",
            "old.html",
            "new.html",
            mid,
            1001,
        )
        .unwrap();
        let map = db
            .first_seen_for_ids(&["oldid0000001".into(), "newid0000001".into()])
            .unwrap();
        assert!(!map.contains_key("oldid0000001"));
        assert_eq!(map.get("newid0000001"), Some(&42));
    }

    // --- corkboard (anchor bookmarks, V0005) ------------------------------

    #[test]
    fn corkboard_starts_empty() {
        let db = db();
        assert!(db.corkboard_list().unwrap().is_empty());
        assert_eq!(db.corkboard_count().unwrap(), 0);
        assert!(!db.corkboard_contains("abc").unwrap());
    }

    #[test]
    fn corkboard_add_then_list_and_contains() {
        let mut db = db();
        assert!(db.corkboard_add("abcdef012345", 1_700_000_000).unwrap());
        assert!(db.corkboard_add("zzz000000000", 1_700_000_500).unwrap());
        let list = db.corkboard_list().unwrap();
        assert_eq!(list.len(), 2);
        // Newest first.
        assert_eq!(list[0].artifact_id, "zzz000000000");
        assert_eq!(list[1].artifact_id, "abcdef012345");
        assert!(db.corkboard_contains("abcdef012345").unwrap());
        assert_eq!(db.corkboard_count().unwrap(), 2);
    }

    #[test]
    fn corkboard_add_is_idempotent_and_preserves_original_created_at() {
        let mut db = db();
        assert!(db.corkboard_add("abc", 1_700_000_000).unwrap());
        // Second add returns false (already present) and must NOT overwrite
        // the original created_at, so the user's original pin time wins.
        assert!(!db.corkboard_add("abc", 1_700_999_999).unwrap());
        let list = db.corkboard_list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].created_at_unix, 1_700_000_000);
    }

    #[test]
    fn corkboard_remove_is_idempotent() {
        let mut db = db();
        db.corkboard_add("abc", 1_700_000_000).unwrap();
        assert!(db.corkboard_remove("abc").unwrap());
        // Second remove returns false (no-op); both calls leave the row gone.
        assert!(!db.corkboard_remove("abc").unwrap());
        assert!(db.corkboard_list().unwrap().is_empty());
    }

    #[test]
    fn corkboard_remove_nonexistent_returns_false() {
        let mut db = db();
        assert!(!db.corkboard_remove("never-pinned").unwrap());
    }

    #[test]
    fn pinned_memories_add_remove_and_set_round_trip() {
        let mut db = db();
        assert!(db.pinned_memories_set().unwrap().is_empty());
        assert!(db.pinned_memory_add("mem-a", 1_700_000_000).unwrap());
        // Idempotent.
        assert!(!db.pinned_memory_add("mem-a", 1_700_000_999).unwrap());
        assert!(db.pinned_memory_add("mem-b", 1_700_000_500).unwrap());
        let set = db.pinned_memories_set().unwrap();
        assert!(set.contains("mem-a"));
        assert!(set.contains("mem-b"));
        assert_eq!(set.len(), 2);
        assert!(db.pinned_memory_remove("mem-a").unwrap());
        assert!(!db.pinned_memory_remove("mem-a").unwrap());
        let set = db.pinned_memories_set().unwrap();
        assert_eq!(set.len(), 1);
        assert!(set.contains("mem-b"));
    }

    // --- Memory links (V0010 / V0011) -----------------------------------

    #[test]
    fn memory_links_start_empty() {
        let db = db();
        assert!(db.memory_links_for("nope").unwrap().is_empty());
        assert!(db.memory_links_all().unwrap().is_empty());
        assert!(!db.memory_links_seeded_has("nope").unwrap());
    }

    #[test]
    fn memory_link_add_is_idempotent_and_remove_round_trips() {
        let mut db = db();
        assert!(db.memory_link_add("mem-a", "kb-x", 1_700_000_000).unwrap());
        // Same (artifact, kb) pair — no-op.
        assert!(!db.memory_link_add("mem-a", "kb-x", 1_700_000_999).unwrap());
        assert!(db.memory_link_add("mem-a", "kb-y", 1_700_000_500).unwrap());
        assert!(db.memory_link_add("mem-a", "*", 1_700_000_500).unwrap());

        let mut links = db.memory_links_for("mem-a").unwrap();
        links.sort();
        assert_eq!(links, vec!["*", "kb-x", "kb-y"]);

        assert!(db.memory_link_remove("mem-a", "kb-x").unwrap());
        assert!(!db.memory_link_remove("mem-a", "kb-x").unwrap());
        let mut links = db.memory_links_for("mem-a").unwrap();
        links.sort();
        assert_eq!(links, vec!["*", "kb-y"]);
    }

    #[test]
    fn memory_links_replace_drops_then_inserts_atomically() {
        let mut db = db();
        db.memory_link_add("mem-a", "kb-x", 1_700_000_000).unwrap();
        db.memory_link_add("mem-a", "kb-y", 1_700_000_000).unwrap();
        db.memory_link_add("mem-a", "*", 1_700_000_000).unwrap();
        db.memory_links_replace(
            "mem-a",
            &["kb-z".to_string(), "kb-w".to_string()],
            false,
            1_700_000_500,
        )
        .unwrap();
        let mut links = db.memory_links_for("mem-a").unwrap();
        links.sort();
        assert_eq!(links, vec!["kb-w", "kb-z"]);
    }

    #[test]
    fn memory_links_replace_with_global_adds_sentinel() {
        let mut db = db();
        db.memory_links_replace("mem-a", &["kb-z".to_string()], true, 1_700_000_500)
            .unwrap();
        let mut links = db.memory_links_for("mem-a").unwrap();
        links.sort();
        assert_eq!(links, vec!["*", "kb-z"]);
    }

    #[test]
    fn memory_links_remove_all_clears_links_and_seeded_tombstone() {
        let mut db = db();
        db.memory_link_add("mem-a", "kb-x", 1_700_000_000).unwrap();
        db.memory_links_seeded_mark("mem-a", 1_700_000_000).unwrap();
        assert!(db.memory_links_seeded_has("mem-a").unwrap());

        db.memory_links_remove_all("mem-a").unwrap();
        assert!(db.memory_links_for("mem-a").unwrap().is_empty());
        assert!(!db.memory_links_seeded_has("mem-a").unwrap());
    }

    #[test]
    fn memory_links_all_groups_by_artifact() {
        let mut db = db();
        db.memory_link_add("mem-a", "kb-x", 1_700_000_000).unwrap();
        db.memory_link_add("mem-a", "*", 1_700_000_000).unwrap();
        db.memory_link_add("mem-b", "kb-y", 1_700_000_000).unwrap();
        let all = db.memory_links_all().unwrap();
        assert_eq!(all.len(), 2);
        let a = all.get("mem-a").unwrap();
        assert!(a.contains("*"));
        assert!(a.contains("kb-x"));
        let b = all.get("mem-b").unwrap();
        assert!(b.contains("kb-y"));
    }

    #[test]
    fn memory_links_seeded_mark_is_idempotent() {
        let mut db = db();
        db.memory_links_seeded_mark("mem-a", 1_700_000_000).unwrap();
        // Second call must not error.
        db.memory_links_seeded_mark("mem-a", 1_700_000_999).unwrap();
        assert!(db.memory_links_seeded_has("mem-a").unwrap());
    }

    #[test]
    fn corkboard_list_sorts_newest_first_with_id_tiebreak() {
        // Two rows with the same created_at must come back in a stable
        // order — the index `idx_corkboard_created` declares DESC on
        // `created_at` only; the secondary sort by `artifact_id ASC` is
        // enforced by the query. Pin this so a future migration that
        // reorders the index doesn't silently corrupt list ordering.
        let mut db = db();
        db.corkboard_add("aaa", 1_700_000_000).unwrap();
        db.corkboard_add("bbb", 1_700_000_000).unwrap();
        db.corkboard_add("ccc", 1_700_000_500).unwrap();
        let list = db.corkboard_list().unwrap();
        let ids: Vec<&str> = list.iter().map(|r| r.artifact_id.as_str()).collect();
        assert_eq!(ids, vec!["ccc", "aaa", "bbb"]);
    }

    // --- Sessions (V0008) ----------------------------------------------

    fn session_row(artifact_id: &str, sid: &str, started_at: i64) -> SessionRow {
        SessionRow {
            artifact_id: artifact_id.into(),
            session_id: sid.into(),
            started_at,
            ended_at: started_at + 600,
            message_count: 42,
            first_user_prompt: Some("hello there".into()),
            source_relative: format!("sessions/session-{started_at}-{sid}.html"),
            title: Some("a readable title".into()),
            cwd: Some("/home/user/project/kb".into()),
            git_branch: Some("main".into()),
            files_read_count: 3,
            files_edited_count: 2,
            token_total: 12_345,
            tool_calls: 9,
            model: Some("claude-opus-4-8".into()),
            error_count: 1,
            subagent_count: 1,
            subagent_tokens: 5_000,
            subagent_tool_calls: 4,
            subagent_files_edited: 1,
            subagent_launched_unstatted: 0,
            project_key: Some("-home-user-project-kb".into()),
            repo_root: Some("/home/user/project/kb".into()),
            harness: "claude".into(),
            cc_version: Some("2.1.0".into()),
            last_assistant_text: Some("Deploy is complete and verified.".into()),
            all_cwds: None,
            commit_count: 1,
            user_turns: 5,
            active_secs: 300,
            substance: Some("substantive".into()),
        }
    }

    #[test]
    fn sessions_research_rollup_groups_counts_and_excludes_cwdless() {
        let mut db = db();
        let row = |aid: &str, sid: &str, cwd: Option<&str>| SessionRow {
            cwd: cwd.map(|s| s.into()),
            ..session_row(aid, sid, 1_700_000_000)
        };
        db.sessions_upsert(&row("a1", "s-a1", Some("/p/a")))
            .unwrap();
        db.sessions_upsert(&row("a2", "s-a2", Some("/p/a")))
            .unwrap();
        db.sessions_upsert(&row("b1", "s-b1", Some("/p/b")))
            .unwrap();
        db.sessions_upsert(&row("n1", "s-n1", None)).unwrap();
        let research = |aid: &str, sid: &str, items: &[(&str, &str)]| -> Vec<SessionResearchRow> {
            items
                .iter()
                .enumerate()
                .map(|(i, (k, q))| SessionResearchRow {
                    artifact_id_session: aid.into(),
                    session_id: sid.into(),
                    seq: i as i64,
                    kind: (*k).into(),
                    query: (*q).into(),
                })
                .collect()
        };
        db.session_research_replace("a1", &research("a1", "s-a1", &[("kb_search", "reranker")]))
            .unwrap();
        db.session_research_replace("a2", &research("a2", "s-a2", &[("kb_search", "reranker")]))
            .unwrap();
        db.session_research_replace("b1", &research("b1", "s-b1", &[("kb_search", "invoice")]))
            .unwrap();
        db.session_research_replace("n1", &research("n1", "s-n1", &[("kb_search", "ignored")]))
            .unwrap();
        let roll = db.sessions_research_rollup(&[]).unwrap();
        let reranker = roll
            .iter()
            .find(|r| r.cwd == "/p/a" && r.query == "reranker")
            .expect("/p/a reranker aggregate");
        assert_eq!(reranker.count, 2, "run in both /p/a sessions");
        assert_eq!(reranker.sessions, 2);
        assert_eq!(reranker.kind, "kb_search");
        assert!(roll.iter().any(|r| r.cwd == "/p/b" && r.query == "invoice"));
        assert!(
            !roll.iter().any(|r| r.query == "ignored"),
            "folder-less session excluded"
        );
    }

    // --- W4/R8/ADD-2 — the grokclaude job join --------------------------

    #[test]
    fn session_research_by_job_finds_grok_job_rows_across_sessions() {
        let mut db = db();
        db.sessions_upsert(&session_row("a1", "s-a1", 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("b1", "s-b1", 1_700_000_100))
            .unwrap();
        let ulid = "01KY9XHCWKWBJSFDHW70G56HYY";
        db.session_research_replace(
            "a1",
            &[SessionResearchRow {
                artifact_id_session: "a1".into(),
                session_id: "s-a1".into(),
                seq: 0,
                kind: "grok_job".into(),
                query: ulid.into(),
            }],
        )
        .unwrap();
        // A different kind + a different ulid on another session must not
        // pollute the join (kind filter + exact query match).
        db.session_research_replace(
            "b1",
            &[
                SessionResearchRow {
                    artifact_id_session: "b1".into(),
                    session_id: "s-b1".into(),
                    seq: 0,
                    kind: "kb_search".into(),
                    query: ulid.into(),
                },
                SessionResearchRow {
                    artifact_id_session: "b1".into(),
                    session_id: "s-b1".into(),
                    seq: 1,
                    kind: "grok_job".into(),
                    query: "01KY7RB2CBNVC7SB7Z9XR4Y9D1".into(),
                },
            ],
        )
        .unwrap();

        let hits = db.session_research_by_job(ulid).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].session_id, "s-a1");
        assert_eq!(hits[0].kind, "grok_job");
        assert!(
            db.session_research_by_job("no-such-ulid")
                .unwrap()
                .is_empty(),
            "an unknown ulid must return no matches"
        );
    }

    #[test]
    fn session_research_by_job_scopes_to_newest_capture() {
        // #11 — two captures of ONE session_id; only the NEWER capture's
        // grok_job row must surface (mirrors funnel_and_rollup_dedup_multi_
        // capture's discipline, applied to the by-job join).
        let mut db = db();
        let sid = "s-multi";
        db.sessions_upsert(&session_row("old", sid, 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("new", sid, 1_700_000_500))
            .unwrap();
        let ulid = "01KY9XHCWKWBJSFDHW70G56HYY";
        db.session_research_replace(
            "old",
            &[SessionResearchRow {
                artifact_id_session: "old".into(),
                session_id: sid.into(),
                seq: 0,
                kind: "grok_job".into(),
                query: ulid.into(),
            }],
        )
        .unwrap();
        db.session_research_replace(
            "new",
            &[SessionResearchRow {
                artifact_id_session: "new".into(),
                session_id: sid.into(),
                seq: 0,
                kind: "grok_job".into(),
                query: ulid.into(),
            }],
        )
        .unwrap();

        let hits = db.session_research_by_job(ulid).unwrap();
        assert_eq!(
            hits.len(),
            1,
            "old capture must be collapsed away: {hits:?}"
        );
        assert_eq!(hits[0].artifact_id_session, "new");
    }

    #[test]
    fn funnel_and_rollup_dedup_multi_capture() {
        // #11 — two captures of ONE session (same session_id, newer started_at
        // on cap-new) each re-record the same research + read. The `COUNT(*)`
        // event totals must reflect the NEWEST capture only, not the union.
        let mut db = db();
        let row = |aid: &str, ts: i64| SessionRow {
            cwd: Some("/p/a".into()),
            ..session_row(aid, "s-dup", ts)
        };
        db.sessions_upsert(&row("cap-old", 1_700_000_000)).unwrap();
        db.sessions_upsert(&row("cap-new", 1_700_000_500)).unwrap();
        for cap in ["cap-old", "cap-new"] {
            db.session_research_replace(
                cap,
                &[SessionResearchRow {
                    artifact_id_session: cap.into(),
                    session_id: "s-dup".into(),
                    seq: 0,
                    kind: "kb_search".into(),
                    query: "reranker".into(),
                }],
            )
            .unwrap();
            db.session_files_replace(
                cap,
                &[SessionFileRow {
                    artifact_id_session: cap.into(),
                    session_id: "s-dup".into(),
                    path: "r1".into(),
                    basename: "r1".into(),
                    action: "read".into(),
                    in_corpus: false,
                    target_kb: None,
                    target_artifact_id: None,
                    via_subagent: false,
                }],
            )
            .unwrap();
        }
        // Rollup: the query is counted ONCE (newest capture), not twice.
        let roll = db.sessions_research_rollup(&[]).unwrap();
        let r = roll
            .iter()
            .find(|r| r.cwd == "/p/a" && r.query == "reranker")
            .expect("reranker aggregate");
        assert_eq!(
            r.count, 1,
            "newest capture only — not doubled by 2 captures"
        );
        assert_eq!(r.sessions, 1);
        // Funnel: search + open event totals reflect one capture, not the union.
        let f = db
            .sessions_funnel_counts(None, &Default::default(), &[])
            .unwrap();
        assert_eq!(f.searched_events, 1, "1 search, not 2");
        assert_eq!(f.searched_sessions, 1);
        assert_eq!(f.opened_events, 1, "1 read, not 2");
    }

    /// #11 — two captures of ONE session both touch the same in-corpus
    /// artifact under a folder. `session_files_in_folder` must return the
    /// triple once, from the newest capture only (not once per Stop).
    #[test]
    fn session_files_in_folder_dedup_multi_capture() {
        let mut db = db();
        let row = |aid: &str, ts: i64| SessionRow {
            cwd: Some("/p/a".into()),
            ..session_row(aid, "s-dup", ts)
        };
        db.sessions_upsert(&row("cap-old", 1_700_000_000)).unwrap();
        db.sessions_upsert(&row("cap-new", 1_700_000_500)).unwrap();
        for cap in ["cap-old", "cap-new"] {
            let mut f = session_file(cap, "s-dup", "kb/x.html", "edit");
            f.in_corpus = true;
            f.target_kb = Some("kb".into());
            f.target_artifact_id = Some("abc123def456".into());
            db.session_files_replace(cap, &[f]).unwrap();
        }
        let hits = db.session_files_in_folder(Some("/p/a")).unwrap();
        assert_eq!(
            hits.len(),
            1,
            "newest capture only — not doubled by 2 captures: {hits:?}"
        );
        assert_eq!(hits[0].0, "s-dup");
        assert_eq!(hits[0].1, "kb");
        assert_eq!(hits[0].2, "abc123def456");
        // The edge must come from the newest capture's artifact_id (cap-new).
        let by_art = db.session_files_for_artifact("abc123def456").unwrap();
        assert_eq!(by_art.len(), 1);
        assert_eq!(by_art[0].artifact_id_session, "cap-new");
    }

    /// #11 — reverse lookups collapse multi-capture edges to the newest
    /// capture: two Stops re-recording the same touch must not double the
    /// "sessions that touched this file" edge set.
    #[test]
    fn session_files_reverse_lookups_dedup_multi_capture() {
        let mut db = db();
        db.sessions_upsert(&session_row("cap-old", "s-dup", 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("cap-new", "s-dup", 1_700_000_500))
            .unwrap();
        for cap in ["cap-old", "cap-new"] {
            let mut f = session_file(cap, "s-dup", "kb/sub.html", "edit");
            f.in_corpus = true;
            f.target_kb = Some("kb".into());
            f.target_artifact_id = Some("abc123def456".into());
            db.session_files_replace(cap, &[f]).unwrap();
        }
        let by_art = db.session_files_for_artifact("abc123def456").unwrap();
        assert_eq!(by_art.len(), 1, "no duplicate reverse edges: {by_art:?}");
        assert_eq!(by_art[0].artifact_id_session, "cap-new");
        let by_base = db.session_files_for_basename("sub.html").unwrap();
        assert_eq!(by_base.len(), 1, "no duplicate basename edges: {by_base:?}");
        assert_eq!(by_base[0].artifact_id_session, "cap-new");
    }

    /// Batch child-row APIs match N singular lookups on a multi-session,
    /// multi-capture fixture (newest-capture semantics, #11).
    #[test]
    fn batch_child_row_apis_match_singular_on_multi_capture() {
        let mut db = db();
        // Session A: two captures — only newest's children should surface.
        db.sessions_upsert(&session_row("a-old", "sid-a", 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("a-new", "sid-a", 1_700_000_500))
            .unwrap();
        // Session B: single capture.
        db.sessions_upsert(&session_row("b-only", "sid-b", 1_700_000_200))
            .unwrap();
        for (cap, sid, seq_offset) in [
            ("a-old", "sid-a", 0i64),
            ("a-new", "sid-a", 0),
            ("b-only", "sid-b", 0),
        ] {
            db.session_commits_replace(
                cap,
                &[SessionCommitRow {
                    artifact_id_session: cap.into(),
                    session_id: sid.into(),
                    seq: seq_offset,
                    kind: "commit".into(),
                    sha: Some(format!("sha-{cap}")),
                    subject: Some(format!("subj-{cap}")),
                    ..Default::default()
                }],
            )
            .unwrap();
            db.session_decisions_replace(
                cap,
                &[SessionDecisionRow {
                    artifact_id_session: cap.into(),
                    session_id: sid.into(),
                    seq: seq_offset,
                    kind: "question".into(),
                    prompt: format!("q-{cap}"),
                    answer: Some("yes".into()),
                }],
            )
            .unwrap();
            db.session_research_replace(
                cap,
                &[SessionResearchRow {
                    artifact_id_session: cap.into(),
                    session_id: sid.into(),
                    seq: seq_offset,
                    kind: "kb_search".into(),
                    query: format!("q-{cap}"),
                }],
            )
            .unwrap();
        }
        // Overwrite a-new with a distinct newest-only payload so the batch
        // result is unambiguous (newest ≠ old).
        db.session_commits_replace(
            "a-new",
            &[SessionCommitRow {
                artifact_id_session: "a-new".into(),
                session_id: "sid-a".into(),
                seq: 0,
                kind: "commit".into(),
                sha: Some("sha-a-new".into()),
                subject: Some("newest only".into()),
                ..Default::default()
            }],
        )
        .unwrap();
        db.session_decisions_replace(
            "a-new",
            &[SessionDecisionRow {
                artifact_id_session: "a-new".into(),
                session_id: "sid-a".into(),
                seq: 0,
                kind: "question".into(),
                prompt: "newest-q".into(),
                answer: Some("yes".into()),
            }],
        )
        .unwrap();
        db.session_research_replace(
            "a-new",
            &[SessionResearchRow {
                artifact_id_session: "a-new".into(),
                session_id: "sid-a".into(),
                seq: 0,
                kind: "kb_search".into(),
                query: "newest-search".into(),
            }],
        )
        .unwrap();

        let ids = vec!["sid-a".to_string(), "sid-b".to_string()];
        let batch_c = db.session_commits_for_sessions(&ids).unwrap();
        let batch_d = db.session_decisions_for_sessions(&ids).unwrap();
        let batch_r = db.session_research_for_sessions(&ids).unwrap();
        for sid in &ids {
            assert_eq!(
                batch_c.get(sid).cloned().unwrap_or_default(),
                db.session_commits_for_session(sid).unwrap(),
                "commits batch == singular for {sid}"
            );
            assert_eq!(
                batch_d.get(sid).cloned().unwrap_or_default(),
                db.session_decisions_for_session(sid).unwrap(),
                "decisions batch == singular for {sid}"
            );
            assert_eq!(
                batch_r.get(sid).cloned().unwrap_or_default(),
                db.session_research_for_session(sid).unwrap(),
                "research batch == singular for {sid}"
            );
        }
        // Newest-capture collapse: sid-a's commit subject is the newest one.
        assert_eq!(batch_c["sid-a"][0].subject.as_deref(), Some("newest only"));
        assert_eq!(batch_r["sid-a"][0].query, "newest-search");
        // Empty input is a no-op.
        assert!(db.session_commits_for_sessions(&[]).unwrap().is_empty());
    }

    /// Regression for the pre-existing invariant #11 self-correlation bug
    /// (the MI-W1.R review pass found and fixed the same bug class in
    /// `memory_recalls_counts_for_ids`; this fn had it too, undetected,
    /// because `batch_child_row_apis_match_singular_on_multi_capture` above
    /// only ever compares the batch fn against the ALSO-buggy singular fn —
    /// both self-correlate identically, so the comparison passes even when
    /// both are wrong). Two sessions: `sid-b`'s single capture is the
    /// GLOBAL-newest row in the whole `sessions` table, while `sid-a` has an
    /// older STALE capture (deliberately carrying an inflated, newer-looking
    /// `seq`/`query` so a leak is unmistakable) plus its own true-newest
    /// capture, which is NOT the global newest. A bare, unqualified
    /// `session_id` in `newest_capture_pred`'s correlated subquery binds to
    /// the subquery's own `sessions AS s2` range var instead of the outer
    /// `session_research` row, so the predicate collapses to "whichever
    /// session in the ENTIRE table is globally newest" (`sid-b`) — `sid-a`
    /// then silently vanishes from the result map entirely, rather than
    /// surfacing its own newest capture's rows.
    #[test]
    fn session_research_for_sessions_scopes_each_session_to_its_own_newest_capture() {
        let mut db = db();
        db.sessions_upsert(&session_row("a-old", "sid-a", 1_700_000_100))
            .unwrap();
        db.sessions_upsert(&session_row("a-new", "sid-a", 1_700_000_500))
            .unwrap();
        // sid-b's ONLY capture — also the GLOBAL max `started_at` in the table.
        db.sessions_upsert(&session_row("b-only", "sid-b", 1_700_000_900))
            .unwrap();
        db.session_research_replace(
            "a-old",
            &[SessionResearchRow {
                artifact_id_session: "a-old".into(),
                session_id: "sid-a".into(),
                seq: 999,
                kind: "kb_search".into(),
                query: "STALE-SHOULD-NOT-LEAK".into(),
            }],
        )
        .unwrap();
        db.session_research_replace(
            "a-new",
            &[SessionResearchRow {
                artifact_id_session: "a-new".into(),
                session_id: "sid-a".into(),
                seq: 0,
                kind: "kb_search".into(),
                query: "live-a".into(),
            }],
        )
        .unwrap();
        db.session_research_replace(
            "b-only",
            &[SessionResearchRow {
                artifact_id_session: "b-only".into(),
                session_id: "sid-b".into(),
                seq: 0,
                kind: "kb_search".into(),
                query: "live-b".into(),
            }],
        )
        .unwrap();

        let batch = db
            .session_research_for_sessions(&["sid-a".to_string(), "sid-b".to_string()])
            .unwrap();
        assert_eq!(
            batch.get("sid-a").map(|r| r.len()),
            Some(1),
            "sid-a must NOT be dropped just because sid-b happens to be globally newest"
        );
        assert_eq!(
            batch["sid-a"][0].query, "live-a",
            "sid-a's OWN newest capture, not the stale one"
        );
        assert_eq!(batch["sid-b"][0].query, "live-b");
    }

    /// Regression for the same pre-existing bug class as
    /// `session_research_for_sessions_scopes_each_session_to_its_own_newest_capture`,
    /// applied to `session_commits_for_sessions`. See that test's doc comment
    /// for the mechanism.
    #[test]
    fn session_commits_for_sessions_scopes_each_session_to_its_own_newest_capture() {
        let mut db = db();
        db.sessions_upsert(&session_row("a-old", "sid-a", 1_700_000_100))
            .unwrap();
        db.sessions_upsert(&session_row("a-new", "sid-a", 1_700_000_500))
            .unwrap();
        db.sessions_upsert(&session_row("b-only", "sid-b", 1_700_000_900))
            .unwrap();
        db.session_commits_replace(
            "a-old",
            &[SessionCommitRow {
                artifact_id_session: "a-old".into(),
                session_id: "sid-a".into(),
                seq: 999,
                kind: "commit".into(),
                sha: Some("deadbeef00".into()),
                subject: Some("STALE-SHOULD-NOT-LEAK".into()),
                ..Default::default()
            }],
        )
        .unwrap();
        db.session_commits_replace(
            "a-new",
            &[SessionCommitRow {
                artifact_id_session: "a-new".into(),
                session_id: "sid-a".into(),
                seq: 0,
                kind: "commit".into(),
                sha: Some("aaaaaaaaaa".into()),
                subject: Some("live-a".into()),
                ..Default::default()
            }],
        )
        .unwrap();
        db.session_commits_replace(
            "b-only",
            &[SessionCommitRow {
                artifact_id_session: "b-only".into(),
                session_id: "sid-b".into(),
                seq: 0,
                kind: "commit".into(),
                sha: Some("bbbbbbbbbb".into()),
                subject: Some("live-b".into()),
                ..Default::default()
            }],
        )
        .unwrap();

        let batch = db
            .session_commits_for_sessions(&["sid-a".to_string(), "sid-b".to_string()])
            .unwrap();
        assert_eq!(
            batch.get("sid-a").map(|r| r.len()),
            Some(1),
            "sid-a must NOT be dropped just because sid-b happens to be globally newest"
        );
        assert_eq!(
            batch["sid-a"][0].subject.as_deref(),
            Some("live-a"),
            "sid-a's OWN newest capture, not the stale one"
        );
        assert_eq!(batch["sid-b"][0].subject.as_deref(), Some("live-b"));
    }

    /// Regression for the same pre-existing bug class as
    /// `session_research_for_sessions_scopes_each_session_to_its_own_newest_capture`,
    /// applied to `session_decisions_for_sessions`. See that test's doc
    /// comment for the mechanism.
    #[test]
    fn session_decisions_for_sessions_scopes_each_session_to_its_own_newest_capture() {
        let mut db = db();
        db.sessions_upsert(&session_row("a-old", "sid-a", 1_700_000_100))
            .unwrap();
        db.sessions_upsert(&session_row("a-new", "sid-a", 1_700_000_500))
            .unwrap();
        db.sessions_upsert(&session_row("b-only", "sid-b", 1_700_000_900))
            .unwrap();
        db.session_decisions_replace(
            "a-old",
            &[SessionDecisionRow {
                artifact_id_session: "a-old".into(),
                session_id: "sid-a".into(),
                seq: 999,
                kind: "question".into(),
                prompt: "STALE-SHOULD-NOT-LEAK".into(),
                answer: Some("stale-answer".into()),
            }],
        )
        .unwrap();
        db.session_decisions_replace(
            "a-new",
            &[SessionDecisionRow {
                artifact_id_session: "a-new".into(),
                session_id: "sid-a".into(),
                seq: 0,
                kind: "question".into(),
                prompt: "live-a".into(),
                answer: Some("live-a-answer".into()),
            }],
        )
        .unwrap();
        db.session_decisions_replace(
            "b-only",
            &[SessionDecisionRow {
                artifact_id_session: "b-only".into(),
                session_id: "sid-b".into(),
                seq: 0,
                kind: "question".into(),
                prompt: "live-b".into(),
                answer: Some("live-b-answer".into()),
            }],
        )
        .unwrap();

        let batch = db
            .session_decisions_for_sessions(&["sid-a".to_string(), "sid-b".to_string()])
            .unwrap();
        assert_eq!(
            batch.get("sid-a").map(|r| r.len()),
            Some(1),
            "sid-a must NOT be dropped just because sid-b happens to be globally newest"
        );
        assert_eq!(
            batch["sid-a"][0].prompt, "live-a",
            "sid-a's OWN newest capture, not the stale one"
        );
        assert_eq!(batch["sid-b"][0].prompt, "live-b");
    }

    /// V0031 indexes serve the by-sha and by-job equality/prefix lookups.
    #[test]
    fn session_commits_and_research_indexes_used_by_hot_queries() {
        let db = db();
        let plan_sha: String = db
            .conn
            .query_row(
                "EXPLAIN QUERY PLAN
                 SELECT sha FROM session_commits WHERE sha = ?1",
                params!["deadbeef"],
                |r| r.get::<_, String>(3),
            )
            .unwrap();
        assert!(
            plan_sha.contains("USING INDEX idx_session_commits_sha")
                || plan_sha.contains("idx_session_commits_sha"),
            "expected sha index seek, got: {plan_sha}"
        );
        let plan_full: String = db
            .conn
            .query_row(
                "EXPLAIN QUERY PLAN
                 SELECT sha_full FROM session_commits WHERE sha_full = ?1",
                params!["deadbeef"],
                |r| r.get::<_, String>(3),
            )
            .unwrap();
        assert!(
            plan_full.contains("idx_session_commits_sha_full"),
            "expected sha_full index seek, got: {plan_full}"
        );
        let plan_job: String = db
            .conn
            .query_row(
                "EXPLAIN QUERY PLAN
                 SELECT query FROM session_research
                 WHERE kind = 'grok_job' AND query = ?1",
                params!["01ARZ3NDEKTSV4RRFFQ69G5FAV"],
                |r| r.get::<_, String>(3),
            )
            .unwrap();
        assert!(
            plan_job.contains("idx_session_research_kind_query"),
            "expected kind+query index seek, got: {plan_job}"
        );
    }

    /// W0 (sessions-rethink P4.fix) — the folder facet was a bare
    /// `GROUP BY cwd` over EVERY capture: a session captured at 2 Stops
    /// counted twice and summed its `files_edited_count`/`token_total` twice.
    /// The golden pins the collapse: one row per session (the NEWEST capture),
    /// and every aggregate reads from that capture's values.
    #[test]
    fn sessions_folders_collapses_multi_capture_to_the_newest() {
        let mut db = db();
        let cap = |aid: &str, sid: &str, ts: i64, msgs: u32, edited: u32, tokens: u64| SessionRow {
            cwd: Some("/p/a".into()),
            message_count: msgs,
            files_edited_count: edited,
            token_total: tokens,
            ..session_row(aid, sid, ts)
        };
        // ONE session, two captures — the newer one is the superset.
        db.sessions_upsert(&cap("cap-old", "s-dup", 1_700_000_000, 10, 1, 100))
            .unwrap();
        db.sessions_upsert(&cap("cap-new", "s-dup", 1_700_000_500, 42, 7, 900))
            .unwrap();
        // A second, single-capture session in the same folder, and one
        // elsewhere so the grouping itself is still exercised.
        db.sessions_upsert(&cap("solo", "s-solo", 1_700_000_200, 5, 2, 50))
            .unwrap();
        db.sessions_upsert(&SessionRow {
            cwd: Some("/p/b".into()),
            ..session_row("other", "s-other", 1_700_000_300)
        })
        .unwrap();

        let folders = db.sessions_folders().unwrap();
        let a = folders
            .iter()
            .find(|f| f.cwd == "/p/a")
            .expect("/p/a folder row");
        assert_eq!(a.count, 2, "2 SESSIONS in /p/a, not 3 captures");
        assert_eq!(
            a.edited_total, 9,
            "newest capture's 7 + the solo session's 2 — the old capture's 1 is not added"
        );
        assert_eq!(
            a.token_total, 950,
            "900 (newest) + 50, never 100 + 900 + 50"
        );
        assert_eq!(
            a.latest, 1_700_000_500,
            "span still spans the newest capture"
        );
        assert_eq!(
            a.earliest, 1_700_000_200,
            "earliest is the solo session — the collapsed old capture is gone"
        );
        // Folders remain newest-active first, and the other project is intact.
        assert_eq!(folders.first().map(|f| f.cwd.as_str()), Some("/p/a"));
        assert!(folders.iter().any(|f| f.cwd == "/p/b" && f.count == 1));
    }

    fn commit_row(
        aid: &str,
        sid: &str,
        seq: i64,
        sha: Option<&str>,
        sha_full: Option<&str>,
    ) -> SessionCommitRow {
        SessionCommitRow {
            artifact_id_session: aid.into(),
            session_id: sid.into(),
            seq,
            kind: "commit".into(),
            sha: sha.map(String::from),
            subject: Some(format!("subject-{seq}")),
            sha_full: sha_full.map(String::from),
            resolved: sha_full.is_some(),
            ..Default::default()
        }
    }

    // --- session_commits_by_sha_prefix (Wave 0 / W0.6) ---------------------

    #[test]
    fn commits_by_sha_prefix_matches_short_sha_and_sha_full() {
        let mut db = db();
        db.sessions_upsert(&session_row("a1", "s-a1", 1_700_000_000))
            .unwrap();
        db.session_commits_replace(
            "a1",
            &[commit_row(
                "a1",
                "s-a1",
                0,
                Some("beadf00d"),
                Some("beadf00d1234567890abcdef1234567890abcdef"),
            )],
        )
        .unwrap();
        // A short-sha prefix match.
        let by_short = db.session_commits_by_sha_prefix("beadf00").unwrap();
        assert_eq!(by_short.len(), 1);
        assert_eq!(by_short[0].commit.session_id, "s-a1");
        assert_eq!(by_short[0].started_at, 1_700_000_000);
        // A sha_full prefix match, further into the string than `sha` covers.
        let by_full = db.session_commits_by_sha_prefix("beadf00d1234").unwrap();
        assert_eq!(by_full.len(), 1);
        assert_eq!(by_full[0].commit.session_id, "s-a1");
        // A prefix that matches neither column.
        assert!(db
            .session_commits_by_sha_prefix("ffffff0")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn commits_by_sha_prefix_scopes_to_newest_capture() {
        // #11 — two captures of one session; only the NEWEST capture's
        // commit row is a candidate match, even though the older capture
        // recorded the identical sha.
        let mut db = db();
        db.sessions_upsert(&session_row("cap-old", "s-dup", 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("cap-new", "s-dup", 1_700_000_500))
            .unwrap();
        db.session_commits_replace(
            "cap-old",
            &[commit_row("cap-old", "s-dup", 0, Some("deadbee"), None)],
        )
        .unwrap();
        db.session_commits_replace(
            "cap-new",
            &[commit_row("cap-new", "s-dup", 0, Some("deadbee"), None)],
        )
        .unwrap();
        let matches = db.session_commits_by_sha_prefix("deadbee").unwrap();
        assert_eq!(matches.len(), 1, "only the newest capture's row matches");
        assert_eq!(matches[0].commit.artifact_id_session, "cap-new");
    }

    #[test]
    fn commits_by_sha_prefix_returns_empty_on_no_match() {
        let mut db = db();
        db.sessions_upsert(&session_row("a1", "s-a1", 1_700_000_000))
            .unwrap();
        db.session_commits_replace("a1", &[commit_row("a1", "s-a1", 0, Some("cafebabe"), None)])
            .unwrap();
        assert!(db
            .session_commits_by_sha_prefix("0000000")
            .unwrap()
            .is_empty());
    }

    // --- session_commits_page (Wave 0 / W0.6 bulk feed) ---------------------

    #[test]
    fn commits_page_paginates_and_scopes_to_newest_capture() {
        let mut db = db();
        // Three distinct sessions, newest-first by started_at, one commit each.
        db.sessions_upsert(&session_row("a1", "s-a", 1_700_000_300))
            .unwrap();
        db.sessions_upsert(&session_row("b1", "s-b", 1_700_000_200))
            .unwrap();
        db.sessions_upsert(&session_row("c1", "s-c", 1_700_000_100))
            .unwrap();
        for (aid, sid) in [("a1", "s-a"), ("b1", "s-b"), ("c1", "s-c")] {
            db.session_commits_replace(aid, &[commit_row(aid, sid, 0, Some("abc0001"), None)])
                .unwrap();
        }
        // A duplicate OLDER capture of s-a must not double-count in the page.
        db.sessions_upsert(&session_row("a0", "s-a", 1_700_000_050))
            .unwrap();
        db.session_commits_replace("a0", &[commit_row("a0", "s-a", 0, Some("abc0001"), None)])
            .unwrap();

        let page1 = db.session_commits_page(None, 2, 0).unwrap();
        assert_eq!(page1.len(), 2, "limit caps the page");
        assert_eq!(page1[0].commit.session_id, "s-a", "newest-first");
        assert_eq!(
            page1[0].commit.artifact_id_session, "a1",
            "newest capture, not a0"
        );
        assert_eq!(page1[1].commit.session_id, "s-b");

        let page2 = db.session_commits_page(None, 2, 2).unwrap();
        assert_eq!(page2.len(), 1, "third row on the second page");
        assert_eq!(page2[0].commit.session_id, "s-c");

        // Total across pages is 3 rows — the duplicate older capture of s-a
        // never contributes a 4th.
        let all = db.session_commits_page(None, 10, 0).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn commits_page_since_filters_by_session_started_at() {
        let mut db = db();
        db.sessions_upsert(&session_row("a1", "s-a", 1_700_000_300))
            .unwrap();
        db.sessions_upsert(&session_row("b1", "s-b", 1_700_000_100))
            .unwrap();
        db.session_commits_replace("a1", &[commit_row("a1", "s-a", 0, Some("aaa0001"), None)])
            .unwrap();
        db.session_commits_replace("b1", &[commit_row("b1", "s-b", 0, Some("bbb0001"), None)])
            .unwrap();

        let all = db.session_commits_page(None, 10, 0).unwrap();
        assert_eq!(all.len(), 2);
        let since = db.session_commits_page(Some(1_700_000_200), 10, 0).unwrap();
        assert_eq!(since.len(), 1);
        assert_eq!(since[0].commit.session_id, "s-a");
    }

    #[test]
    fn sessions_funnel_counts_overall_and_per_folder() {
        let mut db = db();
        let row = |aid: &str, sid: &str, cwd: &str| SessionRow {
            cwd: Some(cwd.into()),
            ..session_row(aid, sid, 1_700_000_000)
        };
        db.sessions_upsert(&row("a1", "s-a1", "/p/a")).unwrap();
        db.sessions_upsert(&row("b1", "s-b1", "/p/b")).unwrap();
        let file = |aid: &str, sid: &str, path: &str, action: &str| SessionFileRow {
            artifact_id_session: aid.into(),
            session_id: sid.into(),
            path: path.into(),
            basename: path.into(),
            action: action.into(),
            in_corpus: false,
            target_kb: None,
            target_artifact_id: None,
            via_subagent: false,
        };
        // s-a1: 2 searches, 3 reads, 2 distinct edited paths (e1 edit+write, e2 edit), 1 commit.
        db.session_research_replace(
            "a1",
            &[
                SessionResearchRow {
                    artifact_id_session: "a1".into(),
                    session_id: "s-a1".into(),
                    seq: 0,
                    kind: "kb_search".into(),
                    query: "x".into(),
                },
                SessionResearchRow {
                    artifact_id_session: "a1".into(),
                    session_id: "s-a1".into(),
                    seq: 1,
                    kind: "web".into(),
                    query: "y".into(),
                },
            ],
        )
        .unwrap();
        db.session_files_replace(
            "a1",
            &[
                file("a1", "s-a1", "r1", "read"),
                file("a1", "s-a1", "r2", "read"),
                file("a1", "s-a1", "r3", "read"),
                file("a1", "s-a1", "e1", "edit"),
                file("a1", "s-a1", "e2", "edit"),
                file("a1", "s-a1", "e1", "write"),
            ],
        )
        .unwrap();
        db.session_commits_replace(
            "a1",
            &[SessionCommitRow {
                artifact_id_session: "a1".into(),
                session_id: "s-a1".into(),
                seq: 0,
                kind: "commit".into(),
                sha: Some("abc".into()),
                subject: Some("x".into()),
                ..Default::default()
            }],
        )
        .unwrap();
        // s-b1: one search (different folder).
        db.session_research_replace(
            "b1",
            &[SessionResearchRow {
                artifact_id_session: "b1".into(),
                session_id: "s-b1".into(),
                seq: 0,
                kind: "kb_search".into(),
                query: "z".into(),
            }],
        )
        .unwrap();

        let overall = db
            .sessions_funnel_counts(None, &Default::default(), &[])
            .unwrap();
        assert_eq!(overall.searched_events, 3, "a:2 + b:1");
        assert_eq!(overall.searched_sessions, 2);

        let a = db
            .sessions_funnel_counts(Some("/p/a"), &Default::default(), &[])
            .unwrap();
        assert_eq!(a.searched_events, 2);
        assert_eq!(a.searched_sessions, 1);
        assert_eq!(a.opened_events, 3, "3 file reads");
        assert_eq!(a.edited_events, 2, "distinct edited paths e1,e2");
        assert_eq!(a.committed_events, 1);
        // Folder narrows: /p/b sees only its own search.
        let b = db
            .sessions_funnel_counts(Some("/p/b"), &Default::default(), &[])
            .unwrap();
        assert_eq!(b.searched_events, 1);
        assert_eq!(b.edited_events, 0);
    }

    /// PF-R1 — every field the collapsed single-pass funnel query can
    /// produce, hand-computed against a fixture the OLD 9-statement
    /// implementation would have answered identically. Closes the gap the
    /// sibling test above leaves: it never exercises `opened_events` as a
    /// genuine SUM of TWO sources (`session_research.kind='artifact_open'`
    /// plus `session_files.action='read'`) landing on the SAME session,
    /// nor asserts `opened_sessions`/`edited_sessions`/`committed_sessions`
    /// at all.
    #[test]
    fn sessions_funnel_counts_every_field_matches_hand_computed_totals() {
        let mut db = db();
        let row = |aid: &str, sid: &str, cwd: &str| SessionRow {
            cwd: Some(cwd.into()),
            ..session_row(aid, sid, 1_700_000_000)
        };
        let file = |aid: &str, sid: &str, path: &str, action: &str| SessionFileRow {
            artifact_id_session: aid.into(),
            session_id: sid.into(),
            path: path.into(),
            basename: path.into(),
            action: action.into(),
            in_corpus: false,
            target_kb: None,
            target_artifact_id: None,
            via_subagent: false,
        };
        let research = |aid: &str, sid: &str, kind: &str| SessionResearchRow {
            artifact_id_session: aid.into(),
            session_id: sid.into(),
            seq: 0,
            kind: kind.into(),
            query: "q".into(),
        };
        let commit = |aid: &str, sid: &str| SessionCommitRow {
            artifact_id_session: aid.into(),
            session_id: sid.into(),
            seq: 0,
            kind: "commit".into(),
            sha: Some("abc".into()),
            subject: Some("x".into()),
            ..Default::default()
        };

        // Session A (/p/x): 1 artifact_open "open" + 2 file reads + 2
        // distinct edited paths (e1 edit, e2 write) + 1 commit.
        db.sessions_upsert(&row("a1", "s-a", "/p/x")).unwrap();
        db.session_research_replace("a1", &[research("a1", "s-a", "artifact_open")])
            .unwrap();
        db.session_files_replace(
            "a1",
            &[
                file("a1", "s-a", "r1", "read"),
                file("a1", "s-a", "r2", "read"),
                file("a1", "s-a", "e1", "edit"),
                file("a1", "s-a", "e2", "write"),
            ],
        )
        .unwrap();
        db.session_commits_replace("a1", &[commit("a1", "s-a")])
            .unwrap();

        // Session B (/p/x): 1 file read + 1 commit, no research, no edits.
        db.sessions_upsert(&row("b1", "s-b", "/p/x")).unwrap();
        db.session_files_replace("b1", &[file("b1", "s-b", "r3", "read")])
            .unwrap();
        db.session_commits_replace("b1", &[commit("b1", "s-b")])
            .unwrap();

        // Session C (/p/y, a DIFFERENT folder): 1 kb_search + 1 edit, no
        // reads, no commits — exercises folder narrowing on every field.
        db.sessions_upsert(&row("c1", "s-c", "/p/y")).unwrap();
        db.session_research_replace("c1", &[research("c1", "s-c", "kb_search")])
            .unwrap();
        db.session_files_replace("c1", &[file("c1", "s-c", "e9", "edit")])
            .unwrap();

        // --- /p/x only (A + B) ---
        let x = db
            .sessions_funnel_counts(Some("/p/x"), &Default::default(), &[])
            .unwrap();
        assert_eq!(x.searched_events, 0, "neither A nor B searched");
        assert_eq!(x.searched_sessions, 0);
        assert_eq!(
            x.opened_events, 4,
            "1 artifact_open (A) + 2 reads (A) + 1 read (B) = 4"
        );
        assert_eq!(x.opened_sessions, 2, "A and B both read something");
        assert_eq!(x.edited_events, 2, "distinct edited paths e1,e2 (A only)");
        assert_eq!(x.edited_sessions, 1, "only A edited");
        assert_eq!(x.committed_events, 2, "A:1 + B:1");
        assert_eq!(x.committed_sessions, 2, "A and B both committed");

        // --- overall (A + B + C) ---
        let overall = db
            .sessions_funnel_counts(None, &Default::default(), &[])
            .unwrap();
        assert_eq!(overall.searched_events, 1, "only C searched");
        assert_eq!(overall.searched_sessions, 1);
        assert_eq!(
            overall.opened_events, 4,
            "C's read-less edit contributes nothing to opened"
        );
        assert_eq!(overall.opened_sessions, 2);
        assert_eq!(
            overall.edited_events, 3,
            "e1,e2 (A) + e9 (C) — three distinct paths corpus-wide"
        );
        assert_eq!(overall.edited_sessions, 2, "A and C edited; B did not");
        assert_eq!(overall.committed_events, 2, "C committed nothing");
        assert_eq!(overall.committed_sessions, 2);
    }

    #[test]
    fn session_decisions_replace_round_trips_in_order() {
        let mut db = db();
        // The detail queries resolve the newest capture via the `sessions`
        // table (#11), so the parent row must exist (it always does in prod —
        // the enrich hook upserts it before the children).
        db.sessions_upsert(&session_row("art-1", "sid-a", 1_700_000_000))
            .unwrap();
        db.session_decisions_replace(
            "art-1",
            &[
                SessionDecisionRow {
                    artifact_id_session: "art-1".into(),
                    session_id: "sid-a".into(),
                    seq: 0,
                    kind: "question".into(),
                    prompt: "Scope?".into(),
                    answer: Some("Full".into()),
                },
                SessionDecisionRow {
                    artifact_id_session: "art-1".into(),
                    session_id: "sid-a".into(),
                    seq: 1,
                    kind: "plan".into(),
                    prompt: "plan approved".into(),
                    answer: None,
                },
            ],
        )
        .unwrap();
        let got = db.session_decisions_for_session("sid-a").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].prompt, "Scope?");
        assert_eq!(got[0].answer.as_deref(), Some("Full"));
        assert_eq!(got[1].kind, "plan");
        // Wholesale replace + cascade on sessions_delete.
        db.sessions_upsert(&session_row("art-1", "sid-a", 1_700_000_000))
            .unwrap();
        assert_eq!(db.sessions_delete("art-1").unwrap(), 1);
        assert!(db
            .session_decisions_for_session("sid-a")
            .unwrap()
            .is_empty());
    }

    fn session_file(art: &str, sid: &str, path: &str, action: &str) -> SessionFileRow {
        let basename = path.rsplit('/').next().unwrap_or(path).to_string();
        SessionFileRow {
            artifact_id_session: art.into(),
            session_id: sid.into(),
            path: path.into(),
            basename,
            action: action.into(),
            in_corpus: false,
            target_kb: None,
            target_artifact_id: None,
            via_subagent: false,
        }
    }

    #[test]
    fn sessions_list_folder_filter_with_compound_cursor_paginates() {
        // Adversarial: cursor + folder together — the only path where the
        // reused `?p` cursor placeholder co-exists with a separate folder
        // placeholder. Two cwds share one started_at; paginating within one
        // folder via the compound cursor must not drop/duplicate.
        let mut db = db();
        let mut mk = |art: &str, cwd: &str| {
            let mut r = session_row(art, art, 1_700_000_000);
            r.cwd = Some(cwd.into());
            db.sessions_upsert(&r).unwrap();
        };
        mk("a-alpha", "/p/alpha");
        mk("b-alpha", "/p/alpha");
        mk("c-alpha", "/p/alpha");
        mk("z-beta", "/p/beta"); // same started_at, different folder — excluded
        let p1 = db
            .sessions_list(
                2,
                None,
                None,
                Some("/p/alpha"),
                None,
                &Default::default(),
                &[],
                &[],
            )
            .unwrap();
        assert_eq!(p1.len(), 2);
        assert!(p1.iter().all(|r| r.cwd.as_deref() == Some("/p/alpha")));
        let last = p1.last().unwrap();
        let p2 = db
            .sessions_list(
                2,
                Some(last.started_at),
                Some(last.artifact_id.clone()),
                Some("/p/alpha"),
                None,
                &Default::default(),
                &[],
                &[],
            )
            .unwrap();
        assert_eq!(p2.len(), 1, "third alpha row only; beta excluded");
        assert_eq!(p2[0].artifact_id, "c-alpha");
        let ids1: Vec<_> = p1.iter().map(|r| r.artifact_id.as_str()).collect();
        assert!(!ids1.contains(&"c-alpha"), "no overlap across pages");
    }

    #[test]
    fn sessions_list_keyword_search_filters_title_prompt_cwd() {
        let mut db = db();
        let mut r1 = session_row("art-1", "sid-1", 1_700_000_100);
        r1.title = Some("Refactor the indexer".into());
        r1.first_user_prompt = Some("speed up reconcile".into());
        r1.cwd = Some("/p/kb".into());
        db.sessions_upsert(&r1).unwrap();
        let mut r2 = session_row("art-2", "sid-2", 1_700_000_200);
        r2.title = Some("Write the docs".into());
        r2.first_user_prompt = Some("authoring guide".into());
        r2.cwd = Some("/p/research".into());
        db.sessions_upsert(&r2).unwrap();
        // Matches title.
        let hits = db
            .sessions_list(
                10,
                None,
                None,
                None,
                Some("indexer"),
                &Default::default(),
                &[],
                &[],
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].artifact_id, "art-1");
        // Matches first_user_prompt.
        let hits = db
            .sessions_list(
                10,
                None,
                None,
                None,
                Some("authoring"),
                &Default::default(),
                &[],
                &[],
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].artifact_id, "art-2");
        // Matches cwd.
        let hits = db
            .sessions_list(
                10,
                None,
                None,
                None,
                Some("research"),
                &Default::default(),
                &[],
                &[],
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        // Empty query = no filter.
        assert_eq!(
            db.sessions_list(
                10,
                None,
                None,
                None,
                Some("  "),
                &Default::default(),
                &[],
                &[]
            )
            .unwrap()
            .len(),
            2
        );
        // A `%` in the query is escaped, not a wildcard.
        assert!(db
            .sessions_list(
                10,
                None,
                None,
                None,
                Some("%"),
                &Default::default(),
                &[],
                &[]
            )
            .unwrap()
            .is_empty());
    }

    // W5/I — harness facet
    #[test]
    fn sessions_list_harness_filter_narrows_the_set() {
        let mut db = db();
        let mut r1 = session_row("art-claude", "sid-claude", 1_700_000_100);
        r1.harness = "claude".into();
        db.sessions_upsert(&r1).unwrap();
        let mut r2 = session_row("art-grok", "sid-grok", 1_700_000_200);
        r2.harness = "grok".into();
        db.sessions_upsert(&r2).unwrap();
        let mut r3 = session_row("art-codex", "sid-codex", 1_700_000_300);
        r3.harness = "codex".into();
        db.sessions_upsert(&r3).unwrap();

        // A single harness.
        let hits = db
            .sessions_list(
                10,
                None,
                None,
                None,
                None,
                &Default::default(),
                &[],
                &["grok".to_string()],
            )
            .unwrap();
        assert_eq!(
            hits.iter()
                .map(|r| r.artifact_id.as_str())
                .collect::<Vec<_>>(),
            vec!["art-grok"]
        );

        // A csv set of two — OR semantics.
        let hits = db
            .sessions_list(
                10,
                None,
                None,
                None,
                None,
                &Default::default(),
                &[],
                &["grok".to_string(), "codex".to_string()],
            )
            .unwrap();
        let mut ids: Vec<&str> = hits.iter().map(|r| r.artifact_id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["art-codex", "art-grok"]);

        // Empty harness slice = no filter (all three).
        assert_eq!(
            db.sessions_list(10, None, None, None, None, &Default::default(), &[], &[])
                .unwrap()
                .len(),
            3
        );

        // Composes (AND) with substance/folder/q — here just re-confirm it
        // ANDs with a keyset limit rather than silently ignoring it.
        let hits = db
            .sessions_list(
                1,
                None,
                None,
                None,
                None,
                &Default::default(),
                &[],
                &["grok".to_string(), "codex".to_string()],
            )
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "limit still applies under the harness filter"
        );
    }

    // invariant:11 newest-capture
    #[test]
    fn multi_capture_collapses_to_newest_in_list_and_detail() {
        // #11 — one long session captured twice: same session_id, two
        // artifact_ids (the newer is a superset). The list must show it once,
        // and the per-session detail queries must return ONLY the newest
        // capture's rows — never the union, which double-lists every file /
        // decision (the "repeated edited/created" inflation).
        let mut db = db();
        let sid = "sid-multi";
        db.sessions_upsert(&session_row("cap-old", sid, 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("cap-new", sid, 1_700_000_500))
            .unwrap();

        // Both captures re-record the same two file touches…
        for cap in ["cap-old", "cap-new"] {
            db.session_files_replace(
                cap,
                &[
                    session_file(cap, sid, "/p/a.rs", "edit"),
                    session_file(cap, sid, "/p/a.rs", "read"),
                ],
            )
            .unwrap();
            db.session_decisions_replace(
                cap,
                &[SessionDecisionRow {
                    artifact_id_session: cap.into(),
                    session_id: sid.into(),
                    seq: 0,
                    kind: "plan".into(),
                    prompt: "plan approved".into(),
                    answer: None,
                }],
            )
            .unwrap();
        }

        // Detail queries return the newest capture only (2 files, not 4).
        let files = db.session_files_for_session(sid).unwrap();
        assert_eq!(files.len(), 2, "newest capture's rows only, not the union");
        assert!(files.iter().all(|f| f.artifact_id_session == "cap-new"));
        let decs = db.session_decisions_for_session(sid).unwrap();
        assert_eq!(decs.len(), 1, "decisions from the newest capture only");
        assert_eq!(decs[0].artifact_id_session, "cap-new");

        // The list collapses the two captures to one row (the newest).
        let list = db
            .sessions_list(50, None, None, None, None, &Default::default(), &[], &[])
            .unwrap();
        let mine: Vec<_> = list.iter().filter(|r| r.session_id == sid).collect();
        assert_eq!(mine.len(), 1, "one list row per session_id");
        assert_eq!(mine[0].artifact_id, "cap-new", "the newest capture");
    }

    #[test]
    fn session_commits_replace_round_trips_and_cascades() {
        let mut db = db();
        // Parent session row must exist — the detail query resolves the newest
        // capture via `sessions` (#11). Prod always has it (enrich hook order).
        db.sessions_upsert(&session_row("art-1", "sid-a", 1_700_000_000))
            .unwrap();
        db.session_commits_replace(
            "art-1",
            &[
                // V0025 — a fully resolved row (every new column populated).
                SessionCommitRow {
                    artifact_id_session: "art-1".into(),
                    session_id: "sid-a".into(),
                    seq: 0,
                    kind: "commit".into(),
                    sha: Some("abc1234".into()),
                    subject: Some("feat: x".into()),
                    sha_full: Some("abc1234def5678".into()),
                    repo_root: Some("/home/u/proj".into()),
                    resolved: true,
                    author: Some("kb-test <test@kb>".into()),
                    parents: Some(1),
                    trailers: Some("Kb-Session: sid-a".into()),
                },
                // An unresolved row (legacy/backfill shape) — every V0025
                // column defaults untouched.
                SessionCommitRow {
                    artifact_id_session: "art-1".into(),
                    session_id: "sid-a".into(),
                    seq: 1,
                    kind: "push".into(),
                    sha: None,
                    subject: Some("push".into()),
                    ..Default::default()
                },
            ],
        )
        .unwrap();
        let got = db.session_commits_for_session("sid-a").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].sha.as_deref(), Some("abc1234"));
        assert_eq!(got[0].sha_full.as_deref(), Some("abc1234def5678"));
        assert_eq!(got[0].repo_root.as_deref(), Some("/home/u/proj"));
        assert!(got[0].resolved);
        assert_eq!(got[0].author.as_deref(), Some("kb-test <test@kb>"));
        assert_eq!(got[0].parents, Some(1));
        assert_eq!(got[0].trailers.as_deref(), Some("Kb-Session: sid-a"));
        assert!(!got[1].resolved, "unresolved row keeps resolved=0");
        assert!(got[1].sha_full.is_none());
        assert!(got[1].trailers.is_none());
        db.sessions_upsert(&session_row("art-1", "sid-a", 1_700_000_000))
            .unwrap();
        assert_eq!(db.sessions_delete("art-1").unwrap(), 1);
        assert!(db.session_commits_for_session("sid-a").unwrap().is_empty());
    }

    fn memory_recall_row(
        memory_kb: &str,
        memory_id: &str,
        sid: &str,
        turn_id: &str,
        recalled_at: Option<i64>,
        artifact_id: &str,
    ) -> MemoryRecallRow {
        MemoryRecallRow {
            memory_kb: memory_kb.into(),
            memory_id: memory_id.into(),
            session_id: sid.into(),
            turn_id: Some(turn_id.into()),
            recalled_at,
            artifact_id: artifact_id.into(),
            used: false,
            pos: None,
        }
    }

    /// MI-W1.1 (revised) — `memory_recalls_replace` is delete-then-insert BY
    /// CAPTURE (`artifact_id`), exactly like `session_files_replace`:
    /// re-running it for the same capture (a re-index of the identical
    /// content) must replace that capture's row set wholesale, never
    /// accumulate duplicates — and a DIFFERENT capture's rows (a different
    /// session, or a different capture of the SAME session) must survive
    /// untouched by another capture's replace.
    #[test]
    fn memory_recalls_replace_round_trips_and_is_wholesale() {
        let mut db = db();
        db.sessions_upsert(&session_row("cap-1", "sid-a", 1_700_000_000))
            .unwrap();
        db.memory_recalls_replace(
            "cap-1",
            &[
                memory_recall_row("notes", "aaaaaaaaaaaa", "sid-a", "t-1", Some(100), "cap-1"),
                memory_recall_row("notes", "bbbbbbbbbbbb", "sid-a", "t-1", Some(100), "cap-1"),
            ],
        )
        .unwrap();
        db.sessions_upsert(&session_row("cap-9", "sid-b", 1_700_000_200))
            .unwrap();
        db.memory_recalls_replace(
            "cap-9",
            &[memory_recall_row(
                "notes",
                "cccccccccccc",
                "sid-b",
                "t-9",
                Some(200),
                "cap-9",
            )],
        )
        .unwrap();
        assert_eq!(db.memory_recalls_for_session("sid-a").unwrap().len(), 2);
        assert_eq!(db.memory_recalls_for_session("sid-b").unwrap().len(), 1);

        // sid-a gets re-captured — a NEWER capture (later started_at) with a
        // SMALLER derived set. The stale "cap-1" rows must stop being
        // surfaced, replaced by "cap-2"'s single row, not merged into 3.
        db.sessions_upsert(&session_row("cap-2", "sid-a", 1_700_000_150))
            .unwrap();
        db.memory_recalls_replace(
            "cap-2",
            &[memory_recall_row(
                "notes",
                "aaaaaaaaaaaa",
                "sid-a",
                "t-1",
                Some(150),
                "cap-2",
            )],
        )
        .unwrap();
        let rows = db.memory_recalls_for_session("sid-a").unwrap();
        assert_eq!(rows.len(), 1, "replace, not accumulate");
        assert_eq!(rows[0].artifact_id, "cap-2", "the NEWEST capture wins");
        // sid-b's rows are a different session_id — untouched by sid-a's replace.
        assert_eq!(db.memory_recalls_for_session("sid-b").unwrap().len(), 1);
    }

    /// CT-C5 (V0037) — `used` round-trips through `memory_recalls_replace` +
    /// `memory_recalls_for_session` exactly like every other column.
    #[test]
    fn memory_recalls_replace_round_trips_used() {
        let mut db = db();
        db.sessions_upsert(&session_row("cap-1", "sid-a", 1_700_000_000))
            .unwrap();
        db.memory_recalls_replace(
            "cap-1",
            &[
                MemoryRecallRow {
                    used: true,
                    ..memory_recall_row("notes", "aaaaaaaaaaaa", "sid-a", "t-1", Some(100), "cap-1")
                },
                memory_recall_row("notes", "bbbbbbbbbbbb", "sid-a", "t-1", Some(100), "cap-1"),
            ],
        )
        .unwrap();
        let rows = db.memory_recalls_for_session("sid-a").unwrap();
        assert_eq!(rows.len(), 2);
        let a = rows.iter().find(|r| r.memory_id == "aaaaaaaaaaaa").unwrap();
        let b = rows.iter().find(|r| r.memory_id == "bbbbbbbbbbbb").unwrap();
        assert!(a.used);
        assert!(!b.used);
    }

    /// MR1 (V0041) — `pos` round-trips through the ledger on BOTH per-row
    /// reads, and a `None` stays `None` rather than becoming a 0. The
    /// migration is ALTER-only with no default, so every pre-MR1 row reads
    /// back as an honest "rank unknown".
    #[test]
    fn memory_recalls_round_trip_pos_and_keep_none_none() {
        let mut db = db();
        db.sessions_upsert(&session_row("cap-1", "sid-a", 1_700_000_000))
            .unwrap();
        db.memory_recalls_replace(
            "cap-1",
            &[
                MemoryRecallRow {
                    pos: Some(1),
                    ..memory_recall_row("notes", "aaaaaaaaaaaa", "sid-a", "t-1", Some(100), "cap-1")
                },
                MemoryRecallRow {
                    pos: Some(5),
                    ..memory_recall_row("notes", "bbbbbbbbbbbb", "sid-a", "t-1", Some(101), "cap-1")
                },
                // A pre-MR1 / fallback-parsed hit: no rank to record.
                memory_recall_row("notes", "cccccccccccc", "sid-a", "t-1", Some(102), "cap-1"),
            ],
        )
        .unwrap();
        let rows = db.memory_recalls_for_session("sid-a").unwrap();
        assert_eq!(rows.len(), 3);
        let by = |id: &str| rows.iter().find(|r| r.memory_id == id).unwrap().pos;
        assert_eq!(by("aaaaaaaaaaaa"), Some(1));
        assert_eq!(by("bbbbbbbbbbbb"), Some(5));
        assert_eq!(
            by("cccccccccccc"),
            None,
            "absent rank stays absent, never 0"
        );

        // The memory-side reverse read carries it too (→ `recalled-by`).
        let back = db
            .memory_recalls_for_memory("notes", "aaaaaaaaaaaa", 10)
            .unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].pos, Some(1));
        let back_none = db
            .memory_recalls_for_memory("notes", "cccccccccccc", 10)
            .unwrap();
        assert_eq!(back_none.len(), 1);
        assert_eq!(back_none[0].pos, None);
    }

    /// MR1 (V0040) — the UPGRADE path: a row written by a pre-MR1 binary
    /// (which knew nothing of the `pos` column) reads back as
    /// `pos: None`, not `Some(0)`. Simulated the only way an in-tree test
    /// can — refinery always migrates a fresh DB to HEAD, so the "old
    /// binary" is modelled by an INSERT with the pre-V0040 column list,
    /// which is byte-for-byte the statement `memory_recalls_replace` used
    /// before this change. That the ALTER is nullable with no DEFAULT is
    /// what makes this hold; a `NOT NULL DEFAULT 0` would have silently
    /// claimed every legacy recall was the top hit.
    #[test]
    fn memory_recalls_pre_mr1_rows_read_back_as_pos_none() {
        let mut db = db();
        db.sessions_upsert(&session_row("cap-legacy", "sid-legacy", 1_700_000_000))
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO memory_recalls
                    (memory_kb, memory_id, session_id, turn_id, recalled_at, artifact_id, used)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    "notes",
                    "aaaaaaaaaaaa",
                    "sid-legacy",
                    "t-1",
                    100_i64,
                    "cap-legacy",
                    0_i64
                ],
            )
            .unwrap();
        let rows = db.memory_recalls_for_session("sid-legacy").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].pos, None,
            "a legacy row's rank is unknown, never a fabricated 0"
        );
        let back = db
            .memory_recalls_for_memory("notes", "aaaaaaaaaaaa", 10)
            .unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].pos, None);
    }

    /// CT-C5 — the CT-B2 "recalled by" read surfaces `used` per row and
    /// stays scoped to each row's OWN session's newest capture (a stale
    /// re-capture's rows must not leak in).
    #[test]
    fn memory_recalls_for_memory_surfaces_used_per_row_and_scopes_to_newest_capture() {
        let mut db = db();
        db.sessions_upsert(&session_row("cap-a", "sid-a", 1_700_000_000))
            .unwrap();
        db.memory_recalls_replace(
            "cap-a",
            &[MemoryRecallRow {
                used: true,
                ..memory_recall_row("notes", "aaaaaaaaaaaa", "sid-a", "t-1", Some(100), "cap-a")
            }],
        )
        .unwrap();
        // sid-b's stale capture — must never surface once superseded.
        db.sessions_upsert(&session_row("cap-b-stale", "sid-b", 1_700_000_100))
            .unwrap();
        db.memory_recalls_replace(
            "cap-b-stale",
            &[memory_recall_row(
                "notes",
                "aaaaaaaaaaaa",
                "sid-b",
                "t-1",
                Some(9_999),
                "cap-b-stale",
            )],
        )
        .unwrap();
        db.sessions_upsert(&session_row("cap-b-live", "sid-b", 1_700_000_200))
            .unwrap();
        db.memory_recalls_replace(
            "cap-b-live",
            &[memory_recall_row(
                "notes",
                "aaaaaaaaaaaa",
                "sid-b",
                "t-1",
                Some(300),
                "cap-b-live",
            )],
        )
        .unwrap();

        let rows = db
            .memory_recalls_for_memory("notes", "aaaaaaaaaaaa", 50)
            .unwrap();
        assert_eq!(rows.len(), 2, "sid-a's row + sid-b's LIVE capture only");
        // The stale capture's row (recalled_at 9_999) must be gone — sid-b
        // contributes only its live capture's row (recalled_at 300).
        let sid_b = rows.iter().find(|r| r.session_id == "sid-b").unwrap();
        assert_eq!(sid_b.recalled_at, Some(300));
        let used_flags: std::collections::HashSet<bool> = rows.iter().map(|r| r.used).collect();
        assert_eq!(used_flags, std::collections::HashSet::from([true, false]));

        // A different memory id: nothing.
        assert!(db
            .memory_recalls_for_memory("notes", "zzzzzzzzzzzz", 50)
            .unwrap()
            .is_empty());
        // A mismatched memory_kb filter: nothing.
        assert!(db
            .memory_recalls_for_memory("other-kb", "aaaaaaaaaaaa", 50)
            .unwrap()
            .is_empty());
    }

    /// The bug this milestone's revision fixes: captures for one session do
    /// NOT necessarily arrive/replace in monotonic `started_at` order (the
    /// original `memory_recalls_replace` assumed they did, keying the
    /// delete on `session_id` alone). Even when the STALE capture's own
    /// `memory_recalls_replace` call runs SECOND (after the newer one),
    /// `memory_recalls_for_session` must still resolve to whichever capture
    /// has the larger `started_at` — never "whichever was written last".
    #[test]
    fn memory_recalls_for_session_resolves_newest_by_started_at_not_write_order() {
        let mut db = db();
        db.sessions_upsert(&session_row("cap-late", "sid-a", 1_700_000_200))
            .unwrap();
        db.sessions_upsert(&session_row("cap-early", "sid-a", 1_700_000_100))
            .unwrap();

        // Write the LATER-started capture FIRST, the EARLIER-started one
        // SECOND — the opposite of wall-clock/insertion order.
        db.memory_recalls_replace(
            "cap-late",
            &[memory_recall_row(
                "notes",
                "bbbbbbbbbbbb",
                "sid-a",
                "t-2",
                Some(200),
                "cap-late",
            )],
        )
        .unwrap();
        db.memory_recalls_replace(
            "cap-early",
            &[memory_recall_row(
                "notes",
                "aaaaaaaaaaaa",
                "sid-a",
                "t-1",
                Some(100),
                "cap-early",
            )],
        )
        .unwrap();

        // Both captures' rows are alive in the table (neither delete
        // touched the other's artifact_id) — the READ must still pick
        // "cap-late" by started_at, even though "cap-early" was written
        // most recently.
        let rows = db.memory_recalls_for_session("sid-a").unwrap();
        assert_eq!(rows.len(), 1, "read scopes to ONE capture's rows");
        assert_eq!(
            rows[0].artifact_id, "cap-late",
            "started_at DESC picks the newest capture regardless of write order"
        );
        assert_eq!(rows[0].memory_id, "bbbbbbbbbbbb");
    }

    /// MI-W1.1 (revised) — `sessions_delete` cascades to `memory_recalls`,
    /// scoped to exactly the deleted capture's own `artifact_id` — a STALE
    /// capture being unlinked drops ONLY its own rows (verified via a raw
    /// count, since a read-time filter alone wouldn't distinguish "hidden"
    /// from "actually deleted") and never touches a different, live
    /// capture's rows for the same session_id.
    #[test]
    fn sessions_delete_drops_memory_recalls_for_that_capture_only() {
        let mut db = db();
        db.sessions_upsert(&session_row("cap-old", "sid-a", 1_700_000_000))
            .unwrap();
        db.memory_recalls_replace(
            "cap-old",
            &[memory_recall_row(
                "notes",
                "aaaaaaaaaaaa",
                "sid-a",
                "t-1",
                Some(100),
                "cap-old",
            )],
        )
        .unwrap();
        // A newer capture arrives (its own `sessions` row lands first, same
        // as production's session-capture → memory-recall-ledger hook
        // order) — its rows carry "cap-new", and "cap-old"'s rows are
        // UNTOUCHED (a different artifact_id, never deleted by another
        // capture's replace).
        db.sessions_upsert(&session_row("cap-new", "sid-a", 1_700_000_100))
            .unwrap();
        db.memory_recalls_replace(
            "cap-new",
            &[memory_recall_row(
                "notes",
                "aaaaaaaaaaaa",
                "sid-a",
                "t-1",
                Some(150),
                "cap-new",
            )],
        )
        .unwrap();

        // Both captures' rows exist in the table right now.
        let raw_count = |db: &Db, artifact_id: &str| -> i64 {
            db.conn
                .query_row(
                    "SELECT COUNT(*) FROM memory_recalls WHERE artifact_id = ?1",
                    params![artifact_id],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(raw_count(&db, "cap-old"), 1);
        assert_eq!(raw_count(&db, "cap-new"), 1);

        // Unlinking the STALE "cap-old" capture removes exactly its own row
        // and must NOT drop the live "cap-new" row.
        db.sessions_delete("cap-old").unwrap();
        assert_eq!(
            raw_count(&db, "cap-old"),
            0,
            "the stale capture's row is truly gone"
        );
        assert_eq!(
            raw_count(&db, "cap-new"),
            1,
            "the live capture's row survives"
        );
        assert_eq!(
            db.memory_recalls_for_session("sid-a").unwrap().len(),
            1,
            "read still resolves to the live capture"
        );

        // Unlinking the LIVE capture does drop it too.
        db.sessions_delete("cap-new").unwrap();
        assert_eq!(raw_count(&db, "cap-new"), 0);
        assert!(db.memory_recalls_for_session("sid-a").unwrap().is_empty());
    }

    /// MI-W1.2/W1.3 (revised) — `memory_recalls_counts_for_ids` groups by
    /// memory_id, SUMS the count across multiple recalling sessions'
    /// NEWEST captures, and takes the MAX `recalled_at`; the `memory_kb`
    /// filter narrows to one target corpus. A stale, superseded capture's
    /// rows must never contribute to the count (the double-count class this
    /// revision fixes) — proven here by giving the stale capture rows with
    /// a LARGER `recalled_at` than anything live: if the stale-exclusion
    /// were broken, `last_recalled_at`/`count` would pick them up and the
    /// test would catch it immediately.
    #[test]
    fn memory_recalls_counts_for_ids_sums_and_maxes() {
        let mut db = db();
        db.sessions_upsert(&session_row("cap-a", "sid-a", 1_700_000_000))
            .unwrap();
        db.memory_recalls_replace(
            "cap-a",
            &[MemoryRecallRow {
                used: true,
                ..memory_recall_row("notes", "aaaaaaaaaaaa", "sid-a", "t-1", Some(100), "cap-a")
            }],
        )
        .unwrap();
        // sid-b's FIRST (now stale) capture — deliberately carries the
        // LARGEST `recalled_at` values of the whole test, so a broken
        // stale-exclusion would surface as a wrong max/count below.
        db.sessions_upsert(&session_row("cap-b-stale", "sid-b", 1_700_000_100))
            .unwrap();
        db.memory_recalls_replace(
            "cap-b-stale",
            &[
                memory_recall_row(
                    "notes",
                    "aaaaaaaaaaaa",
                    "sid-b",
                    "t-1",
                    Some(9_999),
                    "cap-b-stale",
                ),
                memory_recall_row(
                    "other-kb",
                    "aaaaaaaaaaaa",
                    "sid-b",
                    "t-2",
                    Some(9_998),
                    "cap-b-stale",
                ),
            ],
        )
        .unwrap();
        // sid-b's LATER (live) re-capture — a newer `started_at`, supersedes
        // "cap-b-stale" for reads even though every row it wrote has a
        // SMALLER `recalled_at` than the stale rows above.
        db.sessions_upsert(&session_row("cap-b-live", "sid-b", 1_700_000_200))
            .unwrap();
        db.memory_recalls_replace(
            "cap-b-live",
            &[
                // used=false — proves used_count doesn't just mirror count.
                memory_recall_row(
                    "notes",
                    "aaaaaaaaaaaa",
                    "sid-b",
                    "t-1",
                    Some(300),
                    "cap-b-live",
                ),
                // A DIFFERENT memory_kb — must be excluded when the caller
                // filters to "notes", but counted when unfiltered.
                MemoryRecallRow {
                    used: true,
                    ..memory_recall_row(
                        "other-kb",
                        "aaaaaaaaaaaa",
                        "sid-b",
                        "t-2",
                        Some(500),
                        "cap-b-live",
                    )
                },
            ],
        )
        .unwrap();

        let counts = db
            .memory_recalls_counts_for_ids(Some("notes"), &["aaaaaaaaaaaa".to_string()])
            .unwrap();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].memory_id, "aaaaaaaaaaaa");
        assert_eq!(
            counts[0].count, 2,
            "summed across sid-a's row + sid-b's LIVE capture's notes row only"
        );
        assert_eq!(
            counts[0].last_recalled_at,
            Some(300),
            "max recalled_at across the two LIVE rows — the stale 9999/9998 never leak in"
        );
        // CT-C5 — used_count sums only the `used=true` rows, NOT count:
        // sid-a's row is used, sid-b-live's "notes" row is not.
        assert_eq!(
            counts[0].used_count, 1,
            "used_count must not just mirror count"
        );

        // No filter — widens to also count sid-b's live "other-kb" row.
        let unfiltered = db
            .memory_recalls_counts_for_ids(None, &["aaaaaaaaaaaa".to_string()])
            .unwrap();
        assert_eq!(
            unfiltered[0].count, 3,
            "sid-a + sid-b-live's notes row + sid-b-live's other-kb row"
        );
        assert_eq!(
            unfiltered[0].last_recalled_at,
            Some(500),
            "the stale capture's 9999/9998 recalled_at values never win the MAX"
        );
        assert_eq!(
            unfiltered[0].used_count, 2,
            "sid-a's row + sid-b-live's other-kb row are used; sid-b-live's notes row is not"
        );

        // An id with zero rows is simply absent, not a zero-count entry.
        let empty = db
            .memory_recalls_counts_for_ids(Some("notes"), &["zzzzzzzzzzzz".to_string()])
            .unwrap();
        assert!(empty.is_empty());
    }

    /// CT-B2 — `memory_recalls_for_memory` must collapse a re-captured
    /// session down to ONE row, not double-count across its stale and live
    /// captures. Mirrors `memory_recalls_counts_for_ids_sums_and_maxes`'s
    /// stale-vs-live setup but asserts on the per-row read instead of an
    /// aggregate.
    #[test]
    fn memory_recalls_for_memory_collapses_multi_capture_to_newest() {
        let mut db = db();
        db.sessions_upsert(&session_row("cap-stale", "sid-a", 1_700_000_000))
            .unwrap();
        db.memory_recalls_replace(
            "cap-stale",
            &[memory_recall_row(
                "notes",
                "aaaaaaaaaaaa",
                "sid-a",
                "t-1",
                Some(9_999),
                "cap-stale",
            )],
        )
        .unwrap();
        // A later re-capture of the SAME session — supersedes "cap-stale"
        // for reads even though its own row's `recalled_at` is smaller.
        db.sessions_upsert(&session_row("cap-live", "sid-a", 1_700_000_200))
            .unwrap();
        db.memory_recalls_replace(
            "cap-live",
            &[memory_recall_row(
                "notes",
                "aaaaaaaaaaaa",
                "sid-a",
                "t-2",
                Some(100),
                "cap-live",
            )],
        )
        .unwrap();

        let rows = db
            .memory_recalls_for_memory("notes", "aaaaaaaaaaaa", 50)
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "sid-a's stale capture must not surface a second row"
        );
        assert_eq!(rows[0].session_id, "sid-a");
        assert_eq!(rows[0].turn_id.as_deref(), Some("t-2"));
        assert_eq!(rows[0].recalled_at, Some(100));
        assert_eq!(rows[0].title.as_deref(), Some("a readable title"));
    }

    /// CT-B2 — multiple DIFFERENT sessions recalling the same memory come
    /// back newest-`recalled_at`-first.
    #[test]
    fn memory_recalls_for_memory_orders_across_sessions_newest_first() {
        let mut db = db();
        db.sessions_upsert(&session_row("cap-a", "sid-a", 1_700_000_000))
            .unwrap();
        db.memory_recalls_replace(
            "cap-a",
            &[memory_recall_row(
                "notes",
                "aaaaaaaaaaaa",
                "sid-a",
                "t-1",
                Some(100),
                "cap-a",
            )],
        )
        .unwrap();
        db.sessions_upsert(&session_row("cap-b", "sid-b", 1_700_000_100))
            .unwrap();
        db.memory_recalls_replace(
            "cap-b",
            &[memory_recall_row(
                "notes",
                "aaaaaaaaaaaa",
                "sid-b",
                "t-1",
                Some(500),
                "cap-b",
            )],
        )
        .unwrap();
        // A different memory entirely — must never show up.
        db.memory_recalls_replace(
            "cap-b",
            &[
                memory_recall_row("notes", "aaaaaaaaaaaa", "sid-b", "t-1", Some(500), "cap-b"),
                memory_recall_row("notes", "bbbbbbbbbbbb", "sid-b", "t-2", Some(900), "cap-b"),
            ],
        )
        .unwrap();

        let rows = db
            .memory_recalls_for_memory("notes", "aaaaaaaaaaaa", 50)
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session_id, "sid-b", "recalled_at 500 sorts first");
        assert_eq!(rows[1].session_id, "sid-a", "recalled_at 100 sorts second");

        // A memory_kb that doesn't match returns nothing.
        let wrong_kb = db
            .memory_recalls_for_memory("other-kb", "aaaaaaaaaaaa", 50)
            .unwrap();
        assert!(wrong_kb.is_empty());
    }

    /// CT-B2 — a memory id with zero ledger rows returns an empty vec, not
    /// an error.
    #[test]
    fn memory_recalls_for_memory_absent_memory_is_empty() {
        let db = db();
        let rows = db
            .memory_recalls_for_memory("notes", "zzzzzzzzzzzz", 50)
            .unwrap();
        assert!(rows.is_empty());
    }

    /// MI-W4.2a — `memory_recalls_weekly_for_ids` buckets by weeks-ago,
    /// clamps far-past rows into the LAST bucket instead of growing the
    /// histogram, and lands a null `recalled_at` in bucket 0 rather than
    /// dropping it.
    #[test]
    fn memory_recalls_weekly_for_ids_buckets_clamps_and_handles_null() {
        let mut db = db();
        let now = 1_700_000_000_i64;
        const WEEK: i64 = 604_800;
        db.sessions_upsert(&session_row("cap-a", "sid-a", now))
            .unwrap();
        db.memory_recalls_replace(
            "cap-a",
            &[
                // This week.
                memory_recall_row(
                    "notes",
                    "aaaaaaaaaaaa",
                    "sid-a",
                    "t-1",
                    Some(now - 100),
                    "cap-a",
                ),
                // ~2 weeks ago.
                memory_recall_row(
                    "notes",
                    "aaaaaaaaaaaa",
                    "sid-a",
                    "t-2",
                    Some(now - 2 * WEEK - 100),
                    "cap-a",
                ),
                // Far past (50 weeks ago) — must clamp into the LAST bucket,
                // never grow the histogram width.
                memory_recall_row(
                    "notes",
                    "aaaaaaaaaaaa",
                    "sid-a",
                    "t-3",
                    Some(now - 50 * WEEK),
                    "cap-a",
                ),
                // No timestamp at all — must land in bucket 0, not be dropped.
                memory_recall_row("notes", "aaaaaaaaaaaa", "sid-a", "t-4", None, "cap-a"),
            ],
        )
        .unwrap();

        let rows = db
            .memory_recalls_weekly_for_ids(Some("notes"), &["aaaaaaaaaaaa".to_string()], now)
            .unwrap();
        let bucket = |wk: i64| {
            rows.iter()
                .find(|r| r.weeks_ago == wk)
                .map(|r| r.count)
                .unwrap_or(0)
        };
        assert_eq!(
            bucket(0),
            2,
            "this-week row + the null-timestamp row: {rows:?}"
        );
        assert_eq!(bucket(2), 1, "the ~2-weeks-ago row: {rows:?}");
        assert_eq!(
            bucket(MEMORY_RECALL_WEEKLY_BUCKETS - 1),
            1,
            "the 50-weeks-ago row clamps into the last bucket: {rows:?}"
        );
        let total: u32 = rows.iter().map(|r| r.count).sum();
        assert_eq!(total, 4, "every row accounted for exactly once: {rows:?}");

        // Empty id list short-circuits without querying.
        assert!(db
            .memory_recalls_weekly_for_ids(None, &[], now)
            .unwrap()
            .is_empty());

        // An id with zero rows is simply absent.
        let empty = db
            .memory_recalls_weekly_for_ids(Some("notes"), &["zzzzzzzzzzzz".to_string()], now)
            .unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn session_files_replace_round_trips_and_is_wholesale() {
        let mut db = db();
        // Parent session row must exist — the manifest query resolves the newest
        // capture via `sessions` (#11). Prod always has it (enrich hook order).
        db.sessions_upsert(&session_row("art-1", "sid-a", 1_700_000_000))
            .unwrap();
        db.session_files_replace(
            "art-1",
            &[
                session_file("art-1", "sid-a", "/p/a.rs", "read"),
                session_file("art-1", "sid-a", "/p/b.rs", "edit"),
            ],
        )
        .unwrap();
        assert_eq!(db.session_files_for_session("sid-a").unwrap().len(), 2);
        // Replace is wholesale: the old set is gone, only the new one remains.
        db.session_files_replace(
            "art-1",
            &[session_file("art-1", "sid-a", "/p/c.rs", "write")],
        )
        .unwrap();
        let files = db.session_files_for_session("sid-a").unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/p/c.rs");
    }

    /// V0026/W0.5 — `via_subagent` round-trips through `session_files_replace`
    /// and every read path (`for_session`/`for_artifact`/`for_basename`); a
    /// plain `session_file(...)` fixture (via_subagent: false, the default)
    /// must never be misread as subagent-sourced.
    #[test]
    fn session_files_via_subagent_round_trips_on_every_read_path() {
        let mut db = db();
        db.sessions_upsert(&session_row("art-1", "sid-a", 1_700_000_000))
            .unwrap();
        let mut from_agent = session_file("art-1", "sid-a", "kb/sub.html", "edit");
        from_agent.via_subagent = true;
        from_agent.in_corpus = true;
        from_agent.target_kb = Some("kb".into());
        from_agent.target_artifact_id = Some("abc123def456".into());
        let from_main = session_file("art-1", "sid-a", "/p/main.rs", "read");
        db.session_files_replace("art-1", &[from_agent, from_main])
            .unwrap();

        let by_session = db.session_files_for_session("sid-a").unwrap();
        let agent_row = by_session.iter().find(|f| f.path == "kb/sub.html").unwrap();
        assert!(agent_row.via_subagent);
        let main_row = by_session.iter().find(|f| f.path == "/p/main.rs").unwrap();
        assert!(!main_row.via_subagent);

        let by_artifact = db.session_files_for_artifact("abc123def456").unwrap();
        assert_eq!(by_artifact.len(), 1);
        assert!(by_artifact[0].via_subagent);

        let by_basename = db.session_files_for_basename("sub.html").unwrap();
        assert_eq!(by_basename.len(), 1);
        assert!(by_basename[0].via_subagent);
    }

    #[test]
    fn session_files_for_artifact_filters_to_in_corpus_target() {
        let mut db = db();
        // Parent session required — newest-capture pred (#11) joins sessions.
        db.sessions_upsert(&session_row("art-1", "sid-a", 1_700_000_000))
            .unwrap();
        let mut in_corpus = session_file("art-1", "sid-a", "kb/x.html", "edit");
        in_corpus.in_corpus = true;
        in_corpus.target_kb = Some("kb".into());
        in_corpus.target_artifact_id = Some("abc123def456".into());
        let out_of_corpus = session_file("art-1", "sid-a", "/tmp/scratch.txt", "read");
        db.session_files_replace("art-1", &[in_corpus, out_of_corpus])
            .unwrap();
        // Reverse lookup finds the in-corpus edge only.
        let hits = db.session_files_for_artifact("abc123def456").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target_kb.as_deref(), Some("kb"));
    }

    /// Regression for the same pre-existing invariant #11 self-correlation
    /// bug the MI-W1.R review pass found in `memory_recalls_counts_for_ids`
    /// (see `session_research_for_sessions_scopes_each_session_to_its_own_newest_capture`
    /// for the full mechanism). `session_files_for_artifact` is a reverse
    /// lookup with NO `session_id` filter of its own — it can legitimately
    /// span every session that ever touched `target_artifact_id`, which is
    /// exactly the multi-session trigger condition. Two DIFFERENT sessions
    /// touch the same target: `sid-b`'s single capture is the GLOBAL-newest
    /// row in the whole `sessions` table; `sid-a` has an older STALE capture
    /// (deliberately carrying a suspicious path so a leak is unmistakable)
    /// plus its own true-newest capture, which is NOT the global newest. A
    /// bare `session_id` self-correlates against the subquery's own
    /// `sessions AS s2`, so the predicate collapses to "whichever session in
    /// the ENTIRE table is globally newest" (`sid-b`) — `sid-a`'s edge then
    /// silently vanishes from the reverse lookup entirely.
    #[test]
    fn session_files_for_artifact_scopes_each_session_to_its_own_newest_capture() {
        let mut db = db();
        db.sessions_upsert(&session_row("a-old", "sid-a", 1_700_000_100))
            .unwrap();
        db.sessions_upsert(&session_row("a-new", "sid-a", 1_700_000_500))
            .unwrap();
        // sid-b's ONLY capture — also the GLOBAL max `started_at` in the table.
        db.sessions_upsert(&session_row("b-only", "sid-b", 1_700_000_900))
            .unwrap();
        let mut stale = session_file("a-old", "sid-a", "/stale/should-not-leak.rs", "edit");
        stale.in_corpus = true;
        stale.target_kb = Some("kb".into());
        stale.target_artifact_id = Some("shared-target".into());
        db.session_files_replace("a-old", &[stale]).unwrap();
        let mut live_a = session_file("a-new", "sid-a", "/live/a-live.rs", "edit");
        live_a.in_corpus = true;
        live_a.target_kb = Some("kb".into());
        live_a.target_artifact_id = Some("shared-target".into());
        db.session_files_replace("a-new", &[live_a]).unwrap();
        let mut live_b = session_file("b-only", "sid-b", "/live/b-live.rs", "edit");
        live_b.in_corpus = true;
        live_b.target_kb = Some("kb".into());
        live_b.target_artifact_id = Some("shared-target".into());
        db.session_files_replace("b-only", &[live_b]).unwrap();

        let hits = db.session_files_for_artifact("shared-target").unwrap();
        let paths: std::collections::BTreeSet<&str> =
            hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(
            hits.len(),
            2,
            "sid-a's own-newest edge must NOT be dropped just because sid-b \
             happens to be globally newest; got {paths:?}"
        );
        assert!(paths.contains("/live/a-live.rs"));
        assert!(paths.contains("/live/b-live.rs"));
        assert!(!paths.contains("/stale/should-not-leak.rs"));
    }

    #[test]
    fn sessions_delete_also_drops_session_files() {
        let mut db = db();
        db.sessions_upsert(&session_row("art-1", "sid-a", 1_700_000_000))
            .unwrap();
        db.session_files_replace(
            "art-1",
            &[session_file("art-1", "sid-a", "/p/a.rs", "read")],
        )
        .unwrap();
        assert_eq!(db.sessions_delete("art-1").unwrap(), 1);
        assert!(db.session_files_for_session("sid-a").unwrap().is_empty());
    }

    #[test]
    fn session_files_for_basename_matches_and_uses_index() {
        let mut db = db();
        db.sessions_upsert(&session_row("art-1", "sid-a", 1_700_000_000))
            .unwrap();
        db.session_files_replace(
            "art-1",
            &[
                session_file("art-1", "sid-a", "/p/a.rs", "read"),
                session_file("art-1", "sid-a", "/other/a.rs", "edit"),
                session_file("art-1", "sid-a", "/p/b.rs", "write"),
            ],
        )
        .unwrap();
        // basename join is path-agnostic: both a.rs rows match, b.rs does not.
        let hits = db.session_files_for_basename("a.rs").unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.basename == "a.rs"));
        // The V0021 index must serve the equality lookup (SEARCH, not SCAN).
        let plan: String = db
            .conn
            .query_row(
                "EXPLAIN QUERY PLAN
                 SELECT artifact_id_session FROM session_files WHERE basename = ?1",
                params!["a.rs"],
                |r| r.get::<_, String>(3),
            )
            .unwrap();
        assert!(
            plan.contains("USING INDEX idx_session_files_basename"),
            "expected basename index seek, got: {plan}"
        );
    }

    /// Regression for the same pre-existing invariant #11 self-correlation
    /// bug as `session_files_for_artifact_scopes_each_session_to_its_own_newest_capture`
    /// (see that test's doc comment for the mechanism), applied to the
    /// basename reverse lookup. Two DIFFERENT sessions each edited a file
    /// with the same basename: `sid-b`'s single capture is the GLOBAL-newest
    /// row in the whole `sessions` table; `sid-a` has an older STALE capture
    /// (a suspicious path so a leak is unmistakable) plus its own
    /// true-newest capture, which is NOT the global newest.
    #[test]
    fn session_files_for_basename_scopes_each_session_to_its_own_newest_capture() {
        let mut db = db();
        db.sessions_upsert(&session_row("a-old", "sid-a", 1_700_000_100))
            .unwrap();
        db.sessions_upsert(&session_row("a-new", "sid-a", 1_700_000_500))
            .unwrap();
        // sid-b's ONLY capture — also the GLOBAL max `started_at` in the table.
        db.sessions_upsert(&session_row("b-only", "sid-b", 1_700_000_900))
            .unwrap();
        db.session_files_replace(
            "a-old",
            &[session_file(
                "a-old",
                "sid-a",
                "/stale/should-not-leak/shared.rs",
                "edit",
            )],
        )
        .unwrap();
        db.session_files_replace(
            "a-new",
            &[session_file("a-new", "sid-a", "/live/a/shared.rs", "edit")],
        )
        .unwrap();
        db.session_files_replace(
            "b-only",
            &[session_file("b-only", "sid-b", "/live/b/shared.rs", "edit")],
        )
        .unwrap();

        let hits = db.session_files_for_basename("shared.rs").unwrap();
        let paths: std::collections::BTreeSet<&str> =
            hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(
            hits.len(),
            2,
            "sid-a's own-newest edge must NOT be dropped just because sid-b \
             happens to be globally newest; got {paths:?}"
        );
        assert!(paths.contains("/live/a/shared.rs"));
        assert!(paths.contains("/live/b/shared.rs"));
        assert!(!paths.contains("/stale/should-not-leak/shared.rs"));
    }

    #[test]
    fn sessions_upsert_then_list_round_trips() {
        let mut db = db();
        let r = session_row("art-1", "sid-a", 1_700_000_000);
        db.sessions_upsert(&r).unwrap();
        let list = db
            .sessions_list(10, None, None, None, None, &Default::default(), &[], &[])
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], r);
    }

    #[test]
    fn sessions_upsert_is_idempotent_and_replaces_metadata() {
        let mut db = db();
        let mut r = session_row("art-1", "sid-a", 1_700_000_000);
        db.sessions_upsert(&r).unwrap();
        // Re-index bumps message_count + prompt preview; same artifact_id.
        r.message_count = 99;
        r.first_user_prompt = Some("rewritten".into());
        db.sessions_upsert(&r).unwrap();
        let list = db
            .sessions_list(10, None, None, None, None, &Default::default(), &[], &[])
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].message_count, 99);
        assert_eq!(list[0].first_user_prompt.as_deref(), Some("rewritten"));
    }

    #[test]
    fn sessions_delete_removes_row_and_is_idempotent() {
        let mut db = db();
        db.sessions_upsert(&session_row("art-1", "sid-a", 1_700_000_000))
            .unwrap();
        assert_eq!(db.sessions_delete("art-1").unwrap(), 1);
        assert!(db
            .sessions_list(10, None, None, None, None, &Default::default(), &[], &[])
            .unwrap()
            .is_empty());
        assert_eq!(db.sessions_delete("art-1").unwrap(), 0);
    }

    /// PF-R1 (V0040) — raw `is_newest` read, bypassing every read-side
    /// collapse so these tests exercise the MATERIALIZED flag itself, not a
    /// query that would still pass if maintenance silently no-opped.
    fn is_newest(db: &Db, artifact_id: &str) -> bool {
        db.conn
            .query_row(
                "SELECT is_newest FROM sessions WHERE artifact_id = ?1",
                params![artifact_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            != 0
    }

    // invariant:11 PF-R1-is-newest
    #[test]
    fn sessions_upsert_maintains_is_newest_across_multi_capture() {
        let mut db = db();
        // Three captures of ONE session, arriving OUT OF ORDER — invariant
        // #11 is explicit that nothing guarantees captures land in
        // started_at order, so maintenance must re-derive the winner by the
        // tie-break every time, not just trust write order.
        db.sessions_upsert(&session_row("cap-mid", "s-dup", 1_700_000_200))
            .unwrap();
        assert!(is_newest(&db, "cap-mid"), "the only capture so far");
        db.sessions_upsert(&session_row("cap-old", "s-dup", 1_700_000_000))
            .unwrap();
        assert!(
            is_newest(&db, "cap-mid"),
            "an OLDER capture arriving later must not steal the flag"
        );
        assert!(!is_newest(&db, "cap-old"));
        db.sessions_upsert(&session_row("cap-new", "s-dup", 1_700_000_500))
            .unwrap();
        assert!(
            is_newest(&db, "cap-new"),
            "the truly-newest capture wins regardless of arrival order"
        );
        assert!(!is_newest(&db, "cap-mid"));
        assert!(!is_newest(&db, "cap-old"));
        // The materialized flag agrees with what a read collapses to.
        assert_eq!(
            db.sessions_get("s-dup").unwrap().unwrap().artifact_id,
            "cap-new"
        );
        let list = db
            .sessions_list(10, None, None, None, None, &Default::default(), &[], &[])
            .unwrap();
        assert_eq!(
            list.len(),
            1,
            "3 captures collapse to 1 session in the list"
        );
        assert_eq!(list[0].artifact_id, "cap-new");
    }

    // invariant:11 PF-R1-is-newest
    #[test]
    fn sessions_upsert_reindex_repair_promotes_the_old_groups_remaining_row() {
        // A reindex can REPAIR an existing artifact_id's session_id (the
        // truncated-meta bug's fix path, invariant #11). Two captures start
        // in the SAME (truncated) group — "cap-new" is its flagged newest —
        // then a reindex repairs ONLY "cap-new"'s session_id to the full
        // id, leaving "cap-old" as the truncated group's sole remaining
        // member. That group must be RE-DERIVED, promoting "cap-old", or it
        // would be left with no flagged row at all despite still being a
        // live, readable session.
        let mut db = db();
        db.sessions_upsert(&session_row("cap-old", "sid-truncated", 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("cap-new", "sid-truncated", 1_700_000_500))
            .unwrap();
        assert!(is_newest(&db, "cap-new"));
        assert!(!is_newest(&db, "cap-old"));
        // Re-index recovers the FULL session id for "cap-new" only.
        db.sessions_upsert(&session_row(
            "cap-new",
            "sid-full-uuid-recovered",
            1_700_000_500,
        ))
        .unwrap();
        assert!(
            is_newest(&db, "cap-new"),
            "still its (new) group's only member"
        );
        assert!(
            is_newest(&db, "cap-old"),
            "the truncated group's remaining row must be promoted, not left flagless"
        );
        assert_eq!(
            db.sessions_get("sid-truncated")
                .unwrap()
                .unwrap()
                .artifact_id,
            "cap-old"
        );
        assert_eq!(
            db.sessions_get("sid-full-uuid-recovered")
                .unwrap()
                .unwrap()
                .artifact_id,
            "cap-new"
        );
    }

    // invariant:11 PF-R1-is-newest
    #[test]
    fn sessions_delete_of_newest_promotes_next_newest() {
        let mut db = db();
        db.sessions_upsert(&session_row("cap-old", "s-dup", 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("cap-new", "s-dup", 1_700_000_500))
            .unwrap();
        assert!(is_newest(&db, "cap-new"));
        db.sessions_delete("cap-new").unwrap();
        assert!(
            is_newest(&db, "cap-old"),
            "deleting the flagged newest promotes the next-newest"
        );
        assert_eq!(
            db.sessions_get("s-dup").unwrap().unwrap().artifact_id,
            "cap-old"
        );
        // Deleting the last remaining capture empties the group cleanly —
        // no panic, no orphan flag left behind.
        db.sessions_delete("cap-old").unwrap();
        assert!(db.sessions_get("s-dup").unwrap().is_none());
    }

    // invariant:11 PF-R1-is-newest
    #[test]
    fn sessions_delete_of_a_stale_capture_leaves_the_newest_flag_untouched() {
        let mut db = db();
        db.sessions_upsert(&session_row("cap-old", "s-dup", 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("cap-new", "s-dup", 1_700_000_500))
            .unwrap();
        db.sessions_delete("cap-old").unwrap();
        assert!(
            is_newest(&db, "cap-new"),
            "the live capture's flag survives an unrelated stale-capture delete"
        );
    }

    // invariant:11 PF-R1-is-newest
    #[test]
    fn cascade_delete_doc_of_newest_capture_promotes_next_newest() {
        // The `sessions` step of `cascade_delete_doc`'s `CASCADE_STEPS` loop
        // is a SEPARATE code path from `sessions_delete` (the indexer's
        // unlink cascade, invariant #2's per-artifact delete) — it needs its
        // own maintenance hook, tested independently here.
        let mut db = db();
        db.sessions_upsert(&session_row("cap-old", "s-dup", 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("cap-new", "s-dup", 1_700_000_500))
            .unwrap();
        let out = db
            .cascade_delete_doc("cap-new", crate::cascade::CascadeMode::Full)
            .unwrap();
        assert_eq!(out.sessions_removed, 1);
        assert!(
            is_newest(&db, "cap-old"),
            "cascade_delete_doc's sessions step also promotes the next-newest"
        );
        assert_eq!(
            db.sessions_get("s-dup").unwrap().unwrap().artifact_id,
            "cap-old"
        );
    }

    // invariant:11 PF-R1-is-newest invariant:2 relocate-never-reindexes
    #[test]
    fn cascade_relocate_doc_preserves_is_newest_flag() {
        // `cascade_relocate_doc` only ever rekeys `artifact_id` on the
        // `sessions` row (plus `source_relative`) — `is_newest`/`session_id`
        // must ride along untouched, never re-derived by the relocate tx.
        let mut db = db();
        db.sessions_upsert(&session_row("cap-old", "s-dup", 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("cap-new", "s-dup", 1_700_000_500))
            .unwrap();
        assert!(is_newest(&db, "cap-new"));
        let mid = db
            .moves_insert_intent("cap-new", "cap-relocated", "old.html", "new.html", 1_000)
            .unwrap();
        db.cascade_relocate_doc(
            "cap-new",
            "cap-relocated",
            "old.html",
            "new.html",
            mid,
            1_001,
        )
        .unwrap();
        assert!(
            is_newest(&db, "cap-relocated"),
            "the flag rides the rekeyed row"
        );
        let session_id: String = db
            .conn
            .query_row(
                "SELECT session_id FROM sessions WHERE artifact_id = ?1",
                params!["cap-relocated"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(session_id, "s-dup", "session_id also rides the row");
        assert_eq!(
            db.sessions_get("s-dup").unwrap().unwrap().artifact_id,
            "cap-relocated",
            "the collapsed read now resolves through the new id"
        );
        // The stale capture is untouched by the relocate of its sibling.
        assert!(!is_newest(&db, "cap-old"));
    }

    /// PF-R1 (V0040) — exercises the ACTUAL migration file's backfill SQL
    /// (via `include_str!`, never a hand-duplicated copy that could drift)
    /// against a hand-seeded pre-V0040 `sessions` shape, bypassing
    /// `Db::open`/refinery entirely (refinery's `embed_migrations!` bundles
    /// the whole embedded set atomically — there is no supported seam to
    /// stop the runner short of V0040 on a real `Db`). Pins the exact
    /// tie-break — `started_at DESC, artifact_id ASC` — every pre-existing
    /// read relied on before materialization.
    #[test]
    fn migration_v0040_backfill_matches_the_pre_materialization_tie_break() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                artifact_id TEXT PRIMARY KEY,
                session_id  TEXT NOT NULL,
                started_at  INTEGER NOT NULL
             );
             INSERT INTO sessions (artifact_id, session_id, started_at) VALUES
                ('cap-old', 'sid-a', 1700000000),
                ('cap-new', 'sid-a', 1700000500),
                ('cap-tie-a', 'sid-b', 1700000200),
                ('cap-tie-b', 'sid-b', 1700000200),
                ('solo', 'sid-c', 1700000300);",
        )
        .unwrap();
        conn.execute_batch(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/V0040__sessions_is_newest.sql"
        )))
        .unwrap();
        let flagged = |id: &str| -> i64 {
            conn.query_row(
                "SELECT is_newest FROM sessions WHERE artifact_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(flagged("cap-old"), 0);
        assert_eq!(flagged("cap-new"), 1, "later started_at wins");
        // sid-b: a started_at TIE breaks on artifact_id ASC —
        // "cap-tie-a" < "cap-tie-b" lexicographically.
        assert_eq!(flagged("cap-tie-a"), 1);
        assert_eq!(flagged("cap-tie-b"), 0);
        assert_eq!(
            flagged("solo"),
            1,
            "a session with one capture flags its only row"
        );
    }

    // invariant:11 recollect-R3
    #[test]
    fn sessions_get_by_artifact_ids_is_the_identity_join_not_session_id() {
        // #11 — `artifact_id` is the PK, `session_id` is a separate, possibly
        // dirty (capture-hook-truncated) column. The lookup must key on
        // `artifact_id` alone: two artifacts sharing a session_id (multi-
        // capture) each resolve to their OWN row, and an unknown id is
        // dropped rather than erroring.
        let mut db = db();
        db.sessions_upsert(&session_row("art-1", "sid-a", 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("art-2", "sid-b", 1_700_000_500))
            .unwrap();
        let rows = db
            .sessions_get_by_artifact_ids(&[
                "art-2".to_string(),
                "art-1".to_string(),
                "no-such-artifact".to_string(),
            ])
            .unwrap();
        assert_eq!(rows.len(), 2);
        let by_id: std::collections::BTreeMap<&str, &str> = rows
            .iter()
            .map(|r| (r.artifact_id.as_str(), r.session_id.as_str()))
            .collect();
        assert_eq!(by_id.get("art-1"), Some(&"sid-a"));
        assert_eq!(by_id.get("art-2"), Some(&"sid-b"));
        assert!(db.sessions_get_by_artifact_ids(&[]).unwrap().is_empty());
    }

    #[test]
    fn sessions_list_sorts_newest_first_with_id_tiebreak() {
        let mut db = db();
        db.sessions_upsert(&session_row("aaa", "sa", 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("bbb", "sb", 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("ccc", "sc", 1_700_000_500))
            .unwrap();
        let list = db
            .sessions_list(10, None, None, None, None, &Default::default(), &[], &[])
            .unwrap();
        let ids: Vec<&str> = list.iter().map(|r| r.artifact_id.as_str()).collect();
        assert_eq!(ids, vec!["ccc", "aaa", "bbb"]);
    }

    #[test]
    fn sessions_list_compound_cursor_paginates_same_second_without_loss() {
        // X1 — three sessions share one started_at; a page-1 of size 2 ends
        // mid-group. The keyset cursor (started_at, artifact_id) must carry
        // page 2 forward without dropping or repeating the boundary row.
        let mut db = db();
        let t = 1_700_000_000;
        db.sessions_upsert(&session_row("aaa", "sa", t)).unwrap();
        db.sessions_upsert(&session_row("bbb", "sb", t)).unwrap();
        db.sessions_upsert(&session_row("ccc", "sc", t)).unwrap();
        // Within one started_at the order is artifact_id ASC → aaa, bbb, ccc.
        let p1 = db
            .sessions_list(2, None, None, None, None, &Default::default(), &[], &[])
            .unwrap();
        let ids1: Vec<&str> = p1.iter().map(|r| r.artifact_id.as_str()).collect();
        assert_eq!(ids1, vec!["aaa", "bbb"]);
        let last = &p1[1];
        // Page 2 via the compound cursor → exactly the tail (ccc).
        let p2 = db
            .sessions_list(
                2,
                Some(last.started_at),
                Some(last.artifact_id.clone()),
                None,
                None,
                &Default::default(),
                &[],
                &[],
            )
            .unwrap();
        let ids2: Vec<&str> = p2.iter().map(|r| r.artifact_id.as_str()).collect();
        assert_eq!(
            ids2,
            vec!["ccc"],
            "compound cursor carries the same-second tail"
        );
        // The OLD strict `started_at < t` cursor would WRONGLY drop ccc
        // (it excludes every row at the boundary second) — the bug X1 fixes.
        let strict = db
            .sessions_list(
                2,
                Some(last.started_at),
                None,
                None,
                None,
                &Default::default(),
                &[],
                &[],
            )
            .unwrap();
        assert!(
            strict.is_empty(),
            "strict started_at<t drops the same-second tail (the pre-fix bug)"
        );
    }

    // invariant:11 newest-capture
    #[test]
    fn sessions_get_returns_latest_when_session_id_collides() {
        let mut db = db();
        db.sessions_upsert(&session_row("older", "sid-x", 1_700_000_000))
            .unwrap();
        db.sessions_upsert(&session_row("newer", "sid-x", 1_700_001_000))
            .unwrap();
        let row = db.sessions_get("sid-x").unwrap().unwrap();
        assert_eq!(row.artifact_id, "newer");
    }

    #[test]
    fn sessions_get_returns_none_for_unknown_session_id() {
        let db = db();
        assert!(db.sessions_get("never-existed").unwrap().is_none());
    }

    // --- Artifact snapshots (V0013, Track V) ---

    #[test]
    fn snapshot_insert_list_and_latest_hash() {
        let mut db = db();
        assert!(db.snapshot_latest_hash("art-1").unwrap().is_none());
        db.snapshot_insert("art-1", "hash-aaa", "<p>one</p>", 1_700_000_000)
            .unwrap();
        db.snapshot_insert("art-1", "hash-bbb", "<p>two</p>", 1_700_000_100)
            .unwrap();
        // latest = newest captured_at.
        assert_eq!(
            db.snapshot_latest_hash("art-1").unwrap().as_deref(),
            Some("hash-bbb")
        );
        let rows = db.snapshot_list("art-1", 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].content_hash, "hash-bbb"); // newest first
        assert_eq!(rows[1].content_hash, "hash-aaa");
        // raw body fetched by id.
        assert_eq!(
            db.snapshot_raw(rows[1].id).unwrap().as_deref(),
            Some("<p>one</p>")
        );
    }

    #[test]
    fn snapshot_prune_keeps_newest_n() {
        let mut db = db();
        for i in 0..5 {
            db.snapshot_insert("art-1", &format!("h{i}"), "body", 1_700_000_000 + i)
                .unwrap();
        }
        let deleted = db.snapshot_prune("art-1", 2).unwrap();
        assert_eq!(deleted, 3);
        let rows = db.snapshot_list("art-1", 10).unwrap();
        assert_eq!(rows.len(), 2);
        // The two newest (h4, h3) survive.
        assert_eq!(rows[0].content_hash, "h4");
        assert_eq!(rows[1].content_hash, "h3");
    }

    #[test]
    fn snapshots_delete_for_artifact_is_scoped_and_idempotent() {
        let mut db = db();
        db.snapshot_insert("art-1", "h1", "a", 1_700_000_000)
            .unwrap();
        db.snapshot_insert("art-2", "h2", "b", 1_700_000_000)
            .unwrap();
        assert_eq!(db.snapshots_delete_for_artifact("art-1").unwrap(), 1);
        assert_eq!(db.snapshots_delete_for_artifact("art-1").unwrap(), 0);
        // art-2 untouched.
        assert_eq!(db.snapshot_list("art-2", 10).unwrap().len(), 1);
    }

    #[test]
    fn snapshot_raw_none_for_missing_id() {
        let db = db();
        assert!(db.snapshot_raw(99999).unwrap().is_none());
    }

    // --- Reading lists (V0015, RL-track) ---------------------------------

    fn nle(id: &str, list: &str, artifact: &str, anchor: Option<&str>) -> NewListEntry {
        NewListEntry {
            id: id.into(),
            list_id: list.into(),
            kb: "canon".into(),
            artifact_id: artifact.into(),
            anchor_json: anchor.map(str::to_string),
            note: None,
            words: None,
            read_override: None,
        }
    }

    fn order_of(db: &Db, list: &str) -> Vec<(String, i64)> {
        db.list_entries_for_list(list)
            .unwrap()
            .into_iter()
            .map(|e| (e.id, e.position))
            .collect()
    }

    #[test]
    fn list_crud_round_trip() {
        let mut db = db();
        let row = db
            .list_create("l_aaa", "Async Rust", Some("in order"), false, 100)
            .unwrap();
        assert_eq!(row.title, "Async Rust");
        assert_eq!(row.description.as_deref(), Some("in order"));
        assert!(!row.pinned && !row.archived);
        assert_eq!((row.created_at_unix, row.updated_at_unix), (100, 100));

        db.list_create("l_bbb", "Later", None, true, 50).unwrap();
        // pinned first, then newest-touched.
        let all = db.lists_all().unwrap();
        assert_eq!(
            all.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec!["l_bbb", "l_aaa"]
        );

        let up = db
            .list_update(
                "l_aaa",
                Some("Async Rust, properly"),
                &Patch::Clear,
                Some(true),
                Some(true),
                200,
            )
            .unwrap()
            .unwrap();
        assert_eq!(up.title, "Async Rust, properly");
        assert!(up.description.is_none());
        assert!(up.pinned && up.archived);
        assert_eq!(up.updated_at_unix, 200);
        assert_eq!(up.created_at_unix, 100);

        // Patch::Keep preserves; missing list is Ok(None).
        let kept = db
            .list_update("l_aaa", None, &Patch::Keep, None, None, 300)
            .unwrap()
            .unwrap();
        assert_eq!(kept.title, "Async Rust, properly");
        assert!(db
            .list_update("l_nope", None, &Patch::Keep, None, None, 300)
            .unwrap()
            .is_none());

        assert!(db.list_delete("l_aaa").unwrap());
        assert!(!db.list_delete("l_aaa").unwrap());
        assert!(db.list_get("l_aaa").unwrap().is_none());
    }

    #[test]
    fn list_title_unique_nocase_conflicts() {
        let mut db = db();
        db.list_create("l_aaa", "Reading", None, false, 100)
            .unwrap();
        let err = db
            .list_create("l_bbb", "reading", None, false, 100)
            .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");

        db.list_create("l_ccc", "Other", None, false, 100).unwrap();
        let err = db
            .list_update("l_ccc", Some("READING"), &Patch::Keep, None, None, 200)
            .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        // Renaming to its own title (case change) is allowed.
        db.list_update("l_aaa", Some("READING"), &Patch::Keep, None, None, 200)
            .unwrap()
            .unwrap();
    }

    #[test]
    fn entry_append_before_after_positions() {
        let mut db = db();
        db.list_create("l_aaa", "L", None, false, 100).unwrap();
        db.list_entry_add(
            &nle("le_a", "l_aaa", "art1", None),
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
        db.list_entry_add(
            &nle("le_b", "l_aaa", "art2", None),
            &PositionSpec::Last,
            "operator",
            101,
        )
        .unwrap();
        db.list_entry_add(
            &nle("le_c", "l_aaa", "art3", None),
            &PositionSpec::Before("le_b".into()),
            "operator",
            102,
        )
        .unwrap();
        db.list_entry_add(
            &nle("le_d", "l_aaa", "art4", None),
            &PositionSpec::First,
            "operator",
            103,
        )
        .unwrap();
        db.list_entry_add(
            &nle("le_e", "l_aaa", "art5", None),
            &PositionSpec::At(2),
            "operator",
            104,
        )
        .unwrap();
        assert_eq!(
            order_of(&db, "l_aaa"),
            vec![
                ("le_d".to_string(), 0),
                ("le_a".to_string(), 1),
                ("le_e".to_string(), 2),
                ("le_c".to_string(), 3),
                ("le_b".to_string(), 4),
            ]
        );
        // Structural mutations bump the parent list.
        assert_eq!(db.list_get("l_aaa").unwrap().unwrap().updated_at_unix, 104);

        // Unknown list / unknown sibling are NotFound.
        let err = db
            .list_entry_add(
                &nle("le_x", "l_nope", "a", None),
                &PositionSpec::Last,
                "operator",
                105,
            )
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
        let err = db
            .list_entry_add(
                &nle("le_x", "l_aaa", "art9", None),
                &PositionSpec::After("le_nope".into()),
                "operator",
                105,
            )
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn entry_move_renumbers_dense() {
        let mut db = db();
        db.list_create("l_aaa", "L", None, false, 100).unwrap();
        for (i, id) in ["le_a", "le_b", "le_c", "le_d"].iter().enumerate() {
            db.list_entry_add(
                &nle(id, "l_aaa", &format!("art{i}"), None),
                &PositionSpec::Last,
                "operator",
                100,
            )
            .unwrap();
        }
        db.list_entry_move("l_aaa", "le_d", &PositionSpec::Before("le_b".into()), 200)
            .unwrap()
            .unwrap();
        assert_eq!(
            order_of(&db, "l_aaa"),
            vec![
                ("le_a".to_string(), 0),
                ("le_d".to_string(), 1),
                ("le_b".to_string(), 2),
                ("le_c".to_string(), 3),
            ]
        );
        db.list_entry_move("l_aaa", "le_a", &PositionSpec::Last, 201)
            .unwrap()
            .unwrap();
        assert_eq!(
            order_of(&db, "l_aaa")
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            vec!["le_d", "le_b", "le_c", "le_a"]
        );
        // Entry not in this list → Ok(None), no writes.
        assert!(db
            .list_entry_move("l_aaa", "le_nope", &PositionSpec::First, 202)
            .unwrap()
            .is_none());
    }

    #[test]
    fn entry_dedupe_unique_anchor_distinguishes() {
        let mut db = db();
        db.list_create("l_aaa", "L", None, false, 100).unwrap();
        db.list_entry_add(
            &nle("le_a", "l_aaa", "art1", None),
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
        // Same artifact, whole-artifact again → Conflict.
        let err = db
            .list_entry_add(
                &nle("le_b", "l_aaa", "art1", None),
                &PositionSpec::Last,
                "operator",
                101,
            )
            .unwrap_err();
        assert!(err.to_string().contains("already in the list"), "{err}");
        // Same artifact with a section anchor → fine.
        let sect = r#"{"kind":"section","id":"tuning","tag":null,"snippet":null}"#;
        db.list_entry_add(
            &nle("le_c", "l_aaa", "art1", Some(sect)),
            &PositionSpec::Last,
            "operator",
            102,
        )
        .unwrap();
        // Identical anchor again → Conflict.
        let err = db
            .list_entry_add(
                &nle("le_d", "l_aaa", "art1", Some(sect)),
                &PositionSpec::Last,
                "operator",
                103,
            )
            .unwrap_err();
        assert!(err.to_string().contains("already in the list"), "{err}");
        // Re-anchoring onto an existing target conflicts too.
        let err = db
            .list_entry_update(
                "le_a",
                &Patch::Keep,
                &Patch::Set((sect.to_string(), Some(40))),
                &Patch::Keep,
                "operator",
                104,
            )
            .unwrap_err();
        assert!(err.to_string().contains("already has that target"), "{err}");
        // The same target in a DIFFERENT list is fine.
        db.list_create("l_bbb", "M", None, false, 100).unwrap();
        db.list_entry_add(
            &nle("le_e", "l_bbb", "art1", Some(sect)),
            &PositionSpec::Last,
            "operator",
            105,
        )
        .unwrap();
    }

    #[test]
    fn entry_update_patches_fields() {
        let mut db = db();
        db.list_create("l_aaa", "L", None, false, 100).unwrap();
        db.list_entry_add(
            &nle("le_a", "l_aaa", "art1", None),
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
        let sect = r#"{"kind":"section","id":"tuning","tag":null,"snippet":null}"#;
        let up = db
            .list_entry_update(
                "le_a",
                &Patch::Set("read §tuning first".to_string()),
                &Patch::Set((sect.to_string(), Some(120))),
                &Patch::Set("read".to_string()),
                "operator",
                200,
            )
            .unwrap()
            .unwrap();
        assert_eq!(up.note.as_deref(), Some("read §tuning first"));
        assert_eq!(up.anchor_json.as_deref(), Some(sect));
        assert_eq!(up.words, Some(120));
        // Legacy column stays NULL; override lives in list_entry_user_state.
        assert!(up.read_override.is_none());
        assert!(!up.anchor_stale);
        assert_eq!(up.updated_at_unix, 200);
        let ovs = db
            .list_entry_user_overrides_for_list("l_aaa", "operator")
            .unwrap();
        assert_eq!(ovs.get("le_a").map(String::as_str), Some("read"));

        // Clear note + anchor + override; words reset with the anchor.
        let up = db
            .list_entry_update(
                "le_a",
                &Patch::Clear,
                &Patch::Clear,
                &Patch::Clear,
                "operator",
                300,
            )
            .unwrap()
            .unwrap();
        assert!(up.note.is_none() && up.anchor_json.is_none() && up.read_override.is_none());
        assert!(up.words.is_none());
        assert!(db
            .list_entry_user_overrides_for_list("l_aaa", "operator")
            .unwrap()
            .is_empty());
        // Missing entry → Ok(None).
        assert!(db
            .list_entry_update(
                "le_nope",
                &Patch::Keep,
                &Patch::Keep,
                &Patch::Keep,
                "operator",
                300
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn list_entries_remove_many_prunes_and_renumbers() {
        let mut db = db();
        db.list_create("l_aaa", "L", None, false, 100).unwrap();
        for (i, id) in ["le_a", "le_b", "le_c", "le_d"].iter().enumerate() {
            db.list_entry_add(
                &nle(id, "l_aaa", &format!("art{i}"), None),
                &PositionSpec::Last,
                "operator",
                100,
            )
            .unwrap();
        }
        let removed = db
            .list_entries_remove_many(
                "l_aaa",
                &["le_b".into(), "le_d".into(), "le_nope".into()],
                200,
            )
            .unwrap();
        assert_eq!(removed.len(), 2);
        assert_eq!(
            order_of(&db, "l_aaa"),
            vec![("le_a".to_string(), 0), ("le_c".to_string(), 1)]
        );
        // Empty set is a no-op.
        assert!(db
            .list_entries_remove_many("l_aaa", &[], 201)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn entry_remove_preserves_order() {
        let mut db = db();
        db.list_create("l_aaa", "L", None, false, 100).unwrap();
        for (i, id) in ["le_a", "le_b", "le_c"].iter().enumerate() {
            db.list_entry_add(
                &nle(id, "l_aaa", &format!("art{i}"), None),
                &PositionSpec::Last,
                "operator",
                100,
            )
            .unwrap();
        }
        let removed = db.list_entry_remove("l_aaa", "le_b", 200).unwrap().unwrap();
        assert_eq!(removed.id, "le_b");
        assert_eq!(
            order_of(&db, "l_aaa"),
            vec![("le_a".to_string(), 0), ("le_c".to_string(), 1)]
        );
        // Idempotent: gone → Ok(None). Wrong list → Ok(None).
        assert!(db
            .list_entry_remove("l_aaa", "le_b", 201)
            .unwrap()
            .is_none());
        db.list_create("l_bbb", "M", None, false, 100).unwrap();
        assert!(db
            .list_entry_remove("l_bbb", "le_a", 202)
            .unwrap()
            .is_none());
        assert!(db.list_entry_get("le_a").unwrap().is_some());
    }

    #[test]
    fn entries_for_artifact_lookup() {
        let mut db = db();
        db.list_create("l_aaa", "L", None, false, 100).unwrap();
        db.list_create("l_bbb", "M", None, false, 100).unwrap();
        db.list_entry_add(
            &nle("le_a", "l_aaa", "art1", None),
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
        let sect = r#"{"kind":"section","id":"x","tag":null,"snippet":null}"#;
        db.list_entry_add(
            &nle("le_b", "l_bbb", "art1", Some(sect)),
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
        db.list_entry_add(
            &nle("le_c", "l_bbb", "art2", None),
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
        let hits = db.list_entries_for_artifact("art1").unwrap();
        assert_eq!(
            hits.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["le_a", "le_b"]
        );
        assert!(db.list_entries_for_artifact("art9").unwrap().is_empty());
    }

    #[test]
    fn sync_resolution_sets_stale_and_words_without_touching_updated_at() {
        let mut db = db();
        db.list_create("l_aaa", "L", None, false, 100).unwrap();
        let sect = r#"{"kind":"section","id":"x","tag":null,"snippet":null}"#;
        let mut e = nle("le_a", "l_aaa", "art1", Some(sect));
        e.words = Some(50);
        db.list_entry_add(&e, &PositionSpec::Last, "operator", 100)
            .unwrap();

        let n = db
            .list_entries_sync_resolution(&[ResolutionUpdate {
                entry_id: "le_a".into(),
                anchor_stale: true,
                words: Some(42),
            }])
            .unwrap();
        assert_eq!(n, 1);
        let row = db.list_entry_get("le_a").unwrap().unwrap();
        assert!(row.anchor_stale);
        assert_eq!(row.words, Some(42));
        // Machine write never bumps updated_at (entry OR list).
        assert_eq!(row.updated_at_unix, 100);
        assert_eq!(db.list_get("l_aaa").unwrap().unwrap().updated_at_unix, 100);

        // words: None keeps the last-known estimate.
        db.list_entries_sync_resolution(&[ResolutionUpdate {
            entry_id: "le_a".into(),
            anchor_stale: false,
            words: None,
        }])
        .unwrap();
        let row = db.list_entry_get("le_a").unwrap().unwrap();
        assert!(!row.anchor_stale);
        assert_eq!(row.words, Some(42));
    }

    #[test]
    fn import_replace_and_append_modes() {
        let mut db = db();
        db.list_create("l_aaa", "L", None, false, 100).unwrap();
        db.list_entry_add(
            &nle("le_a", "l_aaa", "art1", None),
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();

        // Replace: wipes, loads in order, preserves created_at for a
        // round-tripped id, dedupes within the batch.
        let batch = vec![
            nle("le_b", "l_aaa", "art2", None),
            nle("le_a", "l_aaa", "art1", None), // same id as pre-import row
            nle("le_dup", "l_aaa", "art2", None), // duplicate target in batch
        ];
        let n = db
            .list_import_entries("l_aaa", ImportMode::Replace, &batch, "operator", 200)
            .unwrap();
        assert_eq!(n, 2);
        let rows = db.list_entries_for_list("l_aaa").unwrap();
        assert_eq!(
            rows.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["le_b", "le_a"]
        );
        assert_eq!(rows[0].created_at_unix, 200);
        assert_eq!(
            rows[1].created_at_unix, 100,
            "round-trip id keeps created_at"
        );
        assert_eq!(
            rows.iter().map(|e| e.position).collect::<Vec<_>>(),
            vec![0, 1]
        );

        // Append: keeps existing, skips duplicate targets, appends fresh.
        let batch = vec![
            nle("le_c", "l_aaa", "art2", None), // duplicate target → skipped
            nle("le_d", "l_aaa", "art3", None),
        ];
        let n = db
            .list_import_entries("l_aaa", ImportMode::Append, &batch, "operator", 300)
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            order_of(&db, "l_aaa")
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            vec!["le_b", "le_a", "le_d"]
        );
        // Unknown list → NotFound.
        let err = db
            .list_import_entries("l_nope", ImportMode::Replace, &[], "operator", 400)
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn import_remints_ids_that_live_in_another_list() {
        let mut db = db();
        db.list_create("l_aaa", "L", None, false, 100).unwrap();
        db.list_create("l_bbb", "M", None, false, 100).unwrap();
        db.list_entry_add(
            &nle("le_a", "l_aaa", "art1", None),
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
        // Importing l_aaa's exported entries (same ids) into l_bbb must
        // not trip the table-wide PK — it remints.
        let n = db
            .list_import_entries(
                "l_bbb",
                ImportMode::Replace,
                &[nle("le_a", "l_bbb", "art1", None)],
                "operator",
                200,
            )
            .unwrap();
        assert_eq!(n, 1);
        let rows = db.list_entries_for_list("l_bbb").unwrap();
        assert_eq!(rows.len(), 1);
        assert_ne!(rows[0].id, "le_a", "cross-list import remints the id");
        assert!(rows[0].id.starts_with("le_"));
        // The original is untouched.
        assert_eq!(db.list_entry_get("le_a").unwrap().unwrap().list_id, "l_aaa");
    }

    #[test]
    fn list_delete_cascades_entries() {
        let mut db = db();
        db.list_create("l_aaa", "L", None, false, 100).unwrap();
        db.list_entry_add(
            &nle("le_a", "l_aaa", "art1", None),
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
        assert!(db.list_delete("l_aaa").unwrap());
        assert!(db.list_entry_get("le_a").unwrap().is_none());
        assert!(db.list_entries_all().unwrap().is_empty());
    }

    #[test]
    fn bookmarks_table_is_gone() {
        // V0016 dropped the v0.13 table; a fresh DB must not recreate it.
        let db = db();
        let n: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='bookmarks'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn purge_kb_data_leaves_lists() {
        let mut db = db();
        db.list_create("l_aaa", "L", None, false, 100).unwrap();
        db.list_entry_add(
            &nle("le_a", "l_aaa", "art1", None),
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
        db.purge_kb_data().unwrap();
        // Lists are user-curated state — they survive the transient purge.
        assert!(db.list_get("l_aaa").unwrap().is_some());
        assert!(db.list_entry_get("le_a").unwrap().is_some());
    }

    // --- R2 delete cascade + orphan sweep -------------------------------

    /// Seed exactly one artifact-referencing row into every table the Full
    /// cascade touches (edges in BOTH directions), so a cascade can be asserted
    /// to leave nothing behind.
    fn seed_all_cascade_tables(db: &mut Db, id: &str) {
        db.record_edges(id, &[link("other_dst")]).unwrap(); // src = id
        db.record_edges("other_src", &[link(id)]).unwrap(); // dst = id
        db.corkboard_add(id, 100).unwrap();
        db.pinned_memory_add(id, 100).unwrap();
        db.memory_links_replace(id, &["kbx".to_string()], true, 100)
            .unwrap();
        db.memory_links_seeded_mark(id, 100).unwrap();
        db.snapshot_insert(id, "deadbeef0001", "raw", 100).unwrap();
        db.sessions_upsert(&SessionRow {
            artifact_id: id.into(),
            session_id: "sid-1".into(),
            started_at: 100,
            ended_at: 200,
            message_count: 3,
            first_user_prompt: Some("hi".into()),
            source_relative: "s.html".into(),
            title: None,
            cwd: None,
            git_branch: None,
            files_read_count: 0,
            files_edited_count: 0,
            token_total: 0,
            tool_calls: 0,
            model: None,
            error_count: 0,
            subagent_count: 0,
            subagent_tokens: 0,
            subagent_tool_calls: 0,
            subagent_files_edited: 0,
            subagent_launched_unstatted: 0,
            project_key: None,
            repo_root: None,
            harness: "claude".into(),
            cc_version: None,
            last_assistant_text: None,
            all_cwds: None,
            commit_count: 0,
            user_turns: 0,
            active_secs: 0,
            substance: None,
        })
        .unwrap();
        db.session_files_replace(
            id,
            &[SessionFileRow {
                artifact_id_session: id.into(),
                session_id: "sid-1".into(),
                path: "/p/a.rs".into(),
                basename: "a.rs".into(),
                action: "read".into(),
                in_corpus: false,
                target_kb: None,
                target_artifact_id: None,
                via_subagent: false,
            }],
        )
        .unwrap();
        db.session_decisions_replace(
            id,
            &[SessionDecisionRow {
                artifact_id_session: id.into(),
                session_id: "sid-1".into(),
                seq: 0,
                kind: "question".into(),
                prompt: "q".into(),
                answer: Some("a".into()),
            }],
        )
        .unwrap();
        db.session_commits_replace(
            id,
            &[SessionCommitRow {
                artifact_id_session: id.into(),
                session_id: "sid-1".into(),
                seq: 0,
                kind: "commit".into(),
                sha: Some("abc123".into()),
                subject: Some("s".into()),
                ..Default::default()
            }],
        )
        .unwrap();
        db.session_research_replace(
            id,
            &[SessionResearchRow {
                artifact_id_session: id.into(),
                session_id: "sid-1".into(),
                seq: 0,
                kind: "kb_search".into(),
                query: "q".into(),
            }],
        )
        .unwrap();
        let visit = db.history_record_open(id, 100, None, "operator").unwrap();
        db.reading_upsert_sections(visit.id, id, &[dwell("s1", 0, 10, 1_000, 1)], 100)
            .unwrap();
        db.list_create("l_1", "R2 List", None, false, 100).unwrap();
        db.list_entry_add(
            &nle("le_1", "l_1", id, None),
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
    }

    fn count_where(db: &Db, table: &str, col: &str, id: &str) -> i64 {
        db.conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE {col} = ?1"),
                params![id],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// Sum of every artifact-referencing row across the Full cascade tables.
    fn total_cascade_rows_for(db: &Db, id: &str) -> i64 {
        count_where(db, "edges", "src_artifact", id)
            + count_where(db, "edges", "dst_artifact", id)
            + count_where(db, "corkboard", "artifact_id", id)
            + count_where(db, "pinned_memories", "artifact_id", id)
            + count_where(db, "memory_links", "artifact_id", id)
            + count_where(db, "memory_links_seeded", "artifact_id", id)
            + count_where(db, "artifact_snapshots", "artifact_id", id)
            + count_where(db, "sessions", "artifact_id", id)
            + count_where(db, "session_files", "artifact_id_session", id)
            + count_where(db, "session_decisions", "artifact_id_session", id)
            + count_where(db, "session_commits", "artifact_id_session", id)
            + count_where(db, "session_research", "artifact_id_session", id)
            + count_where(db, "reading_sections", "artifact_id", id)
            + count_where(db, "history", "artifact_id", id)
    }

    #[test]
    fn cascade_full_removes_every_seeded_table() {
        let mut db = db();
        let id = "aaaaaaaaaaaa";
        seed_all_cascade_tables(&mut db, id);
        assert!(total_cascade_rows_for(&db, id) >= 15, "seed sanity");
        let out = db
            .cascade_delete_doc(id, crate::cascade::CascadeMode::Full)
            .unwrap();
        assert_eq!(
            total_cascade_rows_for(&db, id),
            0,
            "Full cascade removes a row from every cascaded table",
        );
        assert_eq!(out.sessions_removed, 1);
        // The list entry SURVIVES a Full delete — it renders as a tombstone
        // (invariant: artifact deletion never prunes list entries). The list
        // header survives too.
        assert_eq!(
            count_where(&db, "list_entries", "artifact_id", id),
            1,
            "list entry persists as a tombstone after Full delete",
        );
        assert!(db.list_get("l_1").unwrap().is_some());
    }

    #[test]
    fn cascade_keep_user_data_preserves_history_and_reading() {
        let mut db = db();
        let id = "bbbbbbbbbbbb";
        seed_all_cascade_tables(&mut db, id);
        db.cascade_delete_doc(id, crate::cascade::CascadeMode::KeepUserData)
            .unwrap();
        // Kept — the reading history + section dwell.
        assert_eq!(count_where(&db, "history", "artifact_id", id), 1);
        assert_eq!(count_where(&db, "reading_sections", "artifact_id", id), 1);
        // Everything else still goes, both edge directions included.
        assert_eq!(count_where(&db, "edges", "src_artifact", id), 0);
        assert_eq!(count_where(&db, "edges", "dst_artifact", id), 0);
        assert_eq!(count_where(&db, "corkboard", "artifact_id", id), 0);
        assert_eq!(count_where(&db, "sessions", "artifact_id", id), 0);
        // The list entry persists in BOTH modes — it is a tombstone, not data
        // the cascade owns.
        assert_eq!(count_where(&db, "list_entries", "artifact_id", id), 1);
    }

    #[test]
    fn cascade_cleanup_tables_are_pinned() {
        use crate::cascade::CascadeMode;
        // Golden: the exact set the transaction touches per mode. Adding an
        // artifact-referencing table to `CASCADE_STEPS` changes this output and
        // forces a conscious update here.
        assert_eq!(
            cascade_cleanup_tables(CascadeMode::Full),
            vec![
                "edges",
                "corkboard",
                "doc_first_seen",
                "pinned_memories",
                "memory_links",
                "memory_links_seeded",
                "artifact_snapshots",
                // DCB W1.A — deliberate addition (invariant #2's lifecycle
                // registry pin); derived rows, so KeepUserData drops them too.
                "code_refs",
                "code_refs_docs",
                // CT-F1 — deliberate addition (same invariant #2 lifecycle
                // pin): derived from the capture's own commit trailers.
                "memory_commits",
                "sessions",
                "session_files",
                "session_decisions",
                "session_commits",
                "session_research",
                "reading_sections",
                "history",
            ]
        );
        assert_eq!(
            cascade_cleanup_tables(CascadeMode::KeepUserData),
            vec![
                "edges",
                "corkboard",
                "doc_first_seen",
                "pinned_memories",
                "memory_links",
                "memory_links_seeded",
                "artifact_snapshots",
                "code_refs",
                "code_refs_docs",
                "memory_commits",
                "sessions",
                "session_files",
                "session_decisions",
                "session_commits",
                "session_research",
            ]
        );
        assert_eq!(
            sweep_cleanup_tables(),
            vec![
                "edges",
                "corkboard",
                "pinned_memories",
                "reading_sections",
                "history",
                // DCB W1.A — appended, matching SWEEP_TABLES' own order.
                "code_refs",
                "code_refs_docs",
                // CT-F1 — same class (derived rows), swept on the CAPTURE's
                // artifact_id only; `memory_id` is never a sweep key.
                "memory_commits",
            ]
        );
    }

    /// CT-F5 — `slo_snapshots` is deliberately ABSENT from all three
    /// artifact-id lifecycle registries, and this test exists so nobody
    /// "fixes" that.
    ///
    /// The rule (invariant #2) is that an ARTIFACT-ID-KEYED side table must be
    /// registered in `CASCADE_STEPS` + `SWEEP_TABLES` + the
    /// `cascade_relocate_doc` rekey tx, because none of them fires on an
    /// omission and relocate never re-indexes. `slo_snapshots` has no
    /// `artifact_id` and no artifact-derived key at all: a row is a
    /// WHOLE-CORPUS reading at a wall-clock instant. Deleting, moving or
    /// reindexing a document must NOT rewrite or reclaim a past reading —
    /// that would falsify the very history an append-only log exists to keep.
    /// So the absence IS the correct registration.
    // invariant:2 slo_snapshots is kb-keyed, not artifact-id-keyed
    #[test]
    fn slo_snapshots_is_deliberately_outside_the_artifact_id_lifecycle() {
        use crate::cascade::CascadeMode;
        for mode in [CascadeMode::Full, CascadeMode::KeepUserData] {
            assert!(
                !cascade_cleanup_tables(mode).contains(&"slo_snapshots"),
                "deleting a doc must never rewrite a past corpus-wide reading"
            );
        }
        assert!(
            !sweep_cleanup_tables().contains(&"slo_snapshots"),
            "an SLO reading is not an orphan just because no doc points at it"
        );
        // And the reading really does survive a cascade delete.
        let mut db = db();
        let r = crate::slo::build(
            "k",
            &crate::slo::SloInputs::default(),
            &crate::slo::SloTargets::default(),
            1_000,
        );
        db.slo_snapshot_append(1_000, &r.indicators).unwrap();
        db.cascade_delete_doc("aaaaaaaaaaaa", CascadeMode::Full)
            .unwrap();
        assert_eq!(
            db.slo_snapshots_list(100).unwrap().len(),
            4,
            "the log outlives every document it ever measured"
        );
    }

    // --- Code refs (DCB W1.A) ---------------------------------------------

    fn cr_header(id: &str, hash: &str, extracted_at: i64) -> CodeRefHeaderRow {
        CodeRefHeaderRow {
            artifact_id: id.to_string(),
            doc_hash: hash.to_string(),
            extracted_at,
            code_rev: Some("shopfront@bcd13a1d3".to_string()),
            ref_count: 1,
            group_count: 0,
            ungrouped_count: 1,
            truncated: false,
        }
    }

    fn cr_row(ordinal: u32, kind: &str, raw: &str) -> CodeRefRow {
        CodeRefRow {
            ordinal,
            kind: kind.to_string(),
            raw_text: raw.to_string(),
            path_hint: Some(raw.to_string()),
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
        }
    }

    #[test]
    fn record_code_refs_is_idempotent() {
        let mut db = db();
        let h = cr_header("aaaaaaaaaaaa", "hash1", 1_000);
        let rows = [cr_row(0, "path", "a.rb")];
        assert!(db.record_code_refs(&h, &rows).unwrap());
        // Byte-identical extraction, a LATER clock: nothing is written and
        // the stored `extracted_at` stays pinned to the first write (R4).
        let later = CodeRefHeaderRow {
            extracted_at: 9_999,
            ..h.clone()
        };
        assert!(!db.record_code_refs(&later, &rows).unwrap());
        let stored = db.code_refs_of("aaaaaaaaaaaa").unwrap().unwrap();
        assert_eq!(stored.header.extracted_at, 1_000);
    }

    #[test]
    fn record_code_refs_detects_change() {
        let mut db = db();
        let h = cr_header("aaaaaaaaaaaa", "hash1", 1_000);
        let rows = [cr_row(0, "path", "a.rb")];
        assert!(db.record_code_refs(&h, &rows).unwrap());
        // extracted_at ALONE is deliberately excluded from the comparison.
        assert!(!db
            .record_code_refs(
                &CodeRefHeaderRow {
                    extracted_at: 2_000,
                    ..h.clone()
                },
                &rows
            )
            .unwrap());
        // A changed doc_hash is a change.
        let h2 = CodeRefHeaderRow {
            doc_hash: "hash2".into(),
            extracted_at: 2_000,
            ..h.clone()
        };
        assert!(db.record_code_refs(&h2, &rows).unwrap());
        assert_eq!(
            db.code_refs_of("aaaaaaaaaaaa")
                .unwrap()
                .unwrap()
                .header
                .extracted_at,
            2_000
        );
        // So is a changed ref vector.
        assert!(db
            .record_code_refs(&h2, &[cr_row(0, "path", "b.rb")])
            .unwrap());
    }

    // --- CT-F5 corpus-health SLOs ---------------------------------------

    #[test]
    fn code_ref_shape_counts_splits_path_shapes_from_the_rest() {
        let mut db = db();
        // Four path-shaped kinds + four that name no local-tree path.
        let rows = vec![
            cr_row(0, "path", "a.rb"),
            cr_row(1, "path_line", "b.rb"),
            cr_row(2, "path_range", "c.rb"),
            cr_row(3, "path_list", "d.rb"),
            cr_row(4, "symbol_method", "Foo#bar"),
            cr_row(5, "symbol_const", "Foo::Bar"),
            cr_row(6, "issue", "o/r"),
            cr_row(7, "external", "gem-1.0/lib/a.rb"),
        ];
        let mut h = cr_header("aaaaaaaaaaaa", "hash1", 1_000);
        h.ref_count = rows.len() as u32;
        db.record_code_refs(&h, &rows).unwrap();
        assert_eq!(db.code_ref_shape_counts().unwrap(), (8, 4));
    }

    #[test]
    fn code_ref_shape_counts_are_zero_on_an_empty_table() {
        // The caller turns (0, 0) into an `unknown` indicator, never 0%.
        assert_eq!(db().code_ref_shape_counts().unwrap(), (0, 0));
    }

    #[test]
    fn sessions_newest_started_at_is_none_on_an_empty_table() {
        assert_eq!(db().sessions_newest_started_at().unwrap(), None);
    }

    #[test]
    fn sessions_newest_started_at_is_the_max() {
        let mut db = db();
        db.sessions_upsert(&session_row("a1", "s1", 1_000)).unwrap();
        db.sessions_upsert(&session_row("a2", "s2", 5_000)).unwrap();
        db.sessions_upsert(&session_row("a3", "s3", 3_000)).unwrap();
        assert_eq!(db.sessions_newest_started_at().unwrap(), Some(5_000));
    }

    #[test]
    fn session_ids_present_answers_membership_only() {
        let mut db = db();
        db.sessions_upsert(&session_row("a1", "s1", 1_000)).unwrap();
        // Multi-capture (invariant #11): two rows, one id — presence is
        // still one answer, never a duplicate.
        db.sessions_upsert(&session_row("a2", "s1", 2_000)).unwrap();
        let mut got = db
            .session_ids_present(&["s1".into(), "s-missing".into()])
            .unwrap();
        got.sort();
        assert_eq!(got, vec!["s1".to_string()]);
        assert!(db.session_ids_present(&[]).unwrap().is_empty());
    }

    #[test]
    fn recall_census_starts_null_and_reads_as_no_censused_captures() {
        let mut db = db();
        db.sessions_upsert(&session_row("a1", "s1", 1_000)).unwrap();
        // A capture with no census must NOT contribute a zero — the caller
        // needs (0,0,0,0) here so the indicator reads `unknown`.
        assert_eq!(
            db.sessions_recall_census_totals().unwrap(),
            (0, 0, 0, 0),
            "a NULL census is not a measured zero"
        );
    }

    #[test]
    fn recall_census_round_trips_and_sums() {
        let mut db = db();
        db.sessions_upsert(&session_row("a1", "s1", 1_000)).unwrap();
        db.sessions_upsert(&session_row("a2", "s2", 2_000)).unwrap();
        assert_eq!(db.sessions_set_recall_census("a1", 10, 2, 1).unwrap(), 1);
        assert_eq!(db.sessions_set_recall_census("a2", 5, 0, 0).unwrap(), 1);
        assert_eq!(db.sessions_recall_census_totals().unwrap(), (15, 2, 1, 2));
    }

    /// Invariant #11's multi-capture trap: one session captured twice must
    /// count ONCE, or a chatty session's injections weight the corpus-wide
    /// rate N times over.
    #[test]
    fn recall_census_totals_scope_to_the_newest_capture_per_session() {
        let mut db = db();
        db.sessions_upsert(&session_row("old", "s1", 1_000))
            .unwrap();
        db.sessions_upsert(&session_row("new", "s1", 9_000))
            .unwrap();
        db.sessions_set_recall_census("old", 3, 0, 3).unwrap();
        db.sessions_set_recall_census("new", 8, 1, 1).unwrap();
        assert_eq!(
            db.sessions_recall_census_totals().unwrap(),
            (8, 1, 1, 1),
            "only the newest capture of s1 may contribute"
        );
    }

    /// The designed no-op: no `sessions` row (its capture hook failed) means
    /// the UPDATE matches nothing and the census stays absent.
    #[test]
    fn setting_a_census_for_an_uncaptured_artifact_is_a_silent_no_op() {
        let mut db = db();
        assert_eq!(db.sessions_set_recall_census("ghost", 1, 1, 1).unwrap(), 0);
        assert_eq!(db.sessions_recall_census_totals().unwrap(), (0, 0, 0, 0));
    }

    /// `sessions_upsert` must never clobber the census back to NULL — the
    /// census columns are deliberately absent from its ON CONFLICT SET list,
    /// and a re-capture of the same artifact is the common case.
    #[test]
    fn a_recapture_upsert_does_not_clobber_the_census() {
        let mut db = db();
        db.sessions_upsert(&session_row("a1", "s1", 1_000)).unwrap();
        db.sessions_set_recall_census("a1", 7, 3, 2).unwrap();
        db.sessions_upsert(&session_row("a1", "s1", 1_500)).unwrap();
        assert_eq!(db.sessions_recall_census_totals().unwrap(), (7, 3, 2, 1));
    }

    #[test]
    fn slo_snapshots_append_only_and_read_newest_first() {
        use crate::slo::{SloInputs, SloTargets};
        let mut db = db();
        let first = crate::slo::build(
            "k",
            &SloInputs {
                coderef_total: 4,
                coderef_path_shaped: 3,
                ..Default::default()
            },
            &SloTargets {
                coderef_resolution_pct: Some(90.0),
                ..Default::default()
            },
            1_000,
        );
        assert_eq!(db.slo_snapshot_append(1_000, &first.indicators).unwrap(), 4);
        let second = crate::slo::build("k", &SloInputs::default(), &SloTargets::default(), 2_000);
        assert_eq!(
            db.slo_snapshot_append(2_000, &second.indicators).unwrap(),
            4
        );

        let rows = db.slo_snapshots_list(100).unwrap();
        assert_eq!(rows.len(), 8, "every run lands; nothing is deduped away");
        assert_eq!(rows[0].taken_at_unix, 2_000, "newest first");
        assert!(rows[..4].iter().all(|r| r.taken_at_unix == 2_000));

        // The identical-reading run is STILL appended (unlike atlas frames,
        // whose coord_hash skips a duplicate) — a flat line is the signal.
        assert_eq!(
            db.slo_snapshot_append(3_000, &second.indicators).unwrap(),
            4
        );
        assert_eq!(db.slo_snapshots_list(100).unwrap().len(), 12);

        // An `unknown` indicator stores a NULL value, never a fabricated 0.
        let unknown = db
            .slo_snapshots_list(100)
            .unwrap()
            .into_iter()
            .find(|r| r.taken_at_unix == 2_000 && r.indicator == "coderef_resolution_pct")
            .expect("row present");
        assert_eq!(unknown.value, None);
        assert_eq!(unknown.status, "unknown");

        // The first run's target rode along, so an old row stays readable
        // against the target it was actually judged by.
        let judged = db
            .slo_snapshots_list(100)
            .unwrap()
            .into_iter()
            .find(|r| r.taken_at_unix == 1_000 && r.indicator == "coderef_resolution_pct")
            .expect("row present");
        assert_eq!(judged.value, Some(75.0));
        assert_eq!(judged.target, Some(90.0));
        assert_eq!(judged.status, "warn");
    }

    #[test]
    fn slo_snapshots_list_respects_its_limit() {
        let mut db = db();
        let r = crate::slo::build(
            "k",
            &crate::slo::SloInputs::default(),
            &crate::slo::SloTargets::default(),
            1,
        );
        db.slo_snapshot_append(1, &r.indicators).unwrap();
        assert_eq!(db.slo_snapshots_list(2).unwrap().len(), 2);
        assert!(db.slo_snapshots_list(0).unwrap().is_empty());
    }

    #[test]
    fn record_code_refs_replaces_atomically() {
        let mut db = db();
        let id = "aaaaaaaaaaaa";
        let five: Vec<CodeRefRow> = (0..5).map(|i| cr_row(i, "path", "a.rb")).collect();
        let mut h = cr_header(id, "hash1", 1_000);
        h.ref_count = 5;
        db.record_code_refs(&h, &five).unwrap();
        assert_eq!(count_where(&db, "code_refs", "artifact_id", id), 5);
        let two: Vec<CodeRefRow> = (0..2).map(|i| cr_row(i, "path", "b.rb")).collect();
        h.ref_count = 2;
        h.doc_hash = "hash2".into();
        db.record_code_refs(&h, &two).unwrap();
        assert_eq!(count_where(&db, "code_refs", "artifact_id", id), 2);
        let doc = db.code_refs_of(id).unwrap().unwrap();
        assert_eq!(doc.refs, two);
    }

    #[test]
    fn code_refs_of_distinguishes_never_scanned_from_empty() {
        let mut db = db();
        assert!(db.code_refs_of("aaaaaaaaaaaa").unwrap().is_none());
        let h = CodeRefHeaderRow {
            code_rev: None,
            ref_count: 0,
            ungrouped_count: 0,
            ..cr_header("aaaaaaaaaaaa", "hash1", 1_000)
        };
        db.record_code_refs(&h, &[]).unwrap();
        let doc = db.code_refs_of("aaaaaaaaaaaa").unwrap().expect("scanned");
        assert!(doc.refs.is_empty());
        assert_eq!(doc.header, h);
    }

    /// Full-fidelity round-trip: every one of the `code_refs` table's 16
    /// columns populated with a DISTINCT, recognizable value, then read back
    /// via `code_refs_of` and asserted byte-identical field by field. This
    /// pins the column order between the `INSERT` and `SELECT` in
    /// `record_code_refs`/`code_refs_of` — several adjacent pairs are
    /// same-typed TEXT (`symbol_container`/`symbol_member`,
    /// `context`/`context_tokens`, `group_key`/`group_label`/`group_anchor`),
    /// so a copy-paste reorder on one side would compile clean and silently
    /// swap values instead of erroring.
    #[test]
    fn code_refs_round_trip_is_full_fidelity() {
        let mut db = db();
        let header = CodeRefHeaderRow {
            artifact_id: "aaaaaaaaaaaa".to_string(),
            doc_hash: "deadbeef0001".to_string(),
            extracted_at: 1_700_000_000,
            code_rev: Some("shopfront@bcd13a1d3+dirty".to_string()),
            ref_count: 1,
            group_count: 1,
            ungrouped_count: 0,
            truncated: true,
        };
        let row = CodeRefRow {
            ordinal: 3,
            kind: "path_list".to_string(),
            raw_text: "assets.js.erb:30,51-65,113-119".to_string(),
            path_hint: Some("assets.js.erb".to_string()),
            line_start: Some(30),
            line_end: Some(119),
            line_spans: Some("30,51-65,113-119".to_string()),
            symbol_container: Some("Checkout::UpdateCartService".to_string()),
            symbol_member: Some("item_attributes_for".to_string()),
            context: "see the cart update path".to_string(),
            context_tokens: "cart update_cart_service".to_string(),
            group_key: Some("kb-h-a1-token-identity".to_string()),
            group_label: Some("Token identity".to_string()),
            group_anchor: Some("kb-h-a1-token-identity".to_string()),
            declared: true,
        };
        assert!(db
            .record_code_refs(&header, std::slice::from_ref(&row))
            .unwrap());

        let stored = db.code_refs_of("aaaaaaaaaaaa").unwrap().expect("scanned");
        assert_eq!(stored.header, header);
        assert_eq!(stored.refs, vec![row.clone()]);
        // Field-by-field too, so a same-typed-column swap that happens to
        // still satisfy `PartialEq` on the struct as a whole (impossible
        // today, since every field differs, but this is the belt half of
        // belt-and-suspenders) can't hide.
        let got = &stored.refs[0];
        assert_eq!(got.ordinal, row.ordinal);
        assert_eq!(got.kind, row.kind);
        assert_eq!(got.raw_text, row.raw_text);
        assert_eq!(got.path_hint, row.path_hint);
        assert_eq!(got.line_start, row.line_start);
        assert_eq!(got.line_end, row.line_end);
        assert_eq!(got.line_spans, row.line_spans);
        assert_eq!(got.symbol_container, row.symbol_container);
        assert_eq!(got.symbol_member, row.symbol_member);
        assert_eq!(got.context, row.context);
        assert_eq!(got.context_tokens, row.context_tokens);
        assert_eq!(got.group_key, row.group_key);
        assert_eq!(got.group_label, row.group_label);
        assert_eq!(got.group_anchor, row.group_anchor);
        assert_eq!(got.declared, row.declared);
    }

    #[test]
    fn code_refs_feed_is_keyset_ordered() {
        let mut db = db();
        // Two docs share an `extracted_at` — the artifact_id tiebreak decides.
        for (id, ts) in [
            ("bbbbbbbbbbbb", 100),
            ("aaaaaaaaaaaa", 100),
            ("cccccccccccc", 200),
        ] {
            db.record_code_refs(&cr_header(id, "h", ts), &[cr_row(0, "path", "a.rb")])
                .unwrap();
        }
        let p1 = db.code_refs_feed(None, 2, true).unwrap();
        let ids1: Vec<&str> = p1.iter().map(|d| d.header.artifact_id.as_str()).collect();
        assert_eq!(ids1, vec!["aaaaaaaaaaaa", "bbbbbbbbbbbb"]);
        assert_eq!(p1[0].refs.len(), 1);
        let last = p1.last().unwrap();
        let p2 = db
            .code_refs_feed(
                Some((last.header.extracted_at, last.header.artifact_id.as_str())),
                2,
                true,
            )
            .unwrap();
        let ids2: Vec<&str> = p2.iter().map(|d| d.header.artifact_id.as_str()).collect();
        assert_eq!(
            ids2,
            vec!["cccccccccccc"],
            "no dup, no skip at the boundary"
        );
    }

    #[test]
    fn code_refs_feed_headers_only_mode() {
        let mut db = db();
        db.record_code_refs(
            &cr_header("aaaaaaaaaaaa", "h", 100),
            &[cr_row(0, "path", "a.rb")],
        )
        .unwrap();
        let page = db.code_refs_feed(None, 10, false).unwrap();
        assert_eq!(page.len(), 1);
        assert!(page[0].refs.is_empty(), "headers-only");
        assert_eq!(page[0].header.doc_hash, "h");
    }

    /// CT-B3 — `code_refs_by_target` is an exact `path_hint` match across
    /// the whole corpus: every doc citing `order.rb` comes back, a doc
    /// citing only `cart.rb` doesn't, and a doc with NO code_refs row at
    /// all is (correctly) invisible — this is a reverse index over
    /// EXTRACTED refs, not a corpus-wide doc scan.
    #[test]
    fn code_refs_by_target_is_an_exact_path_hint_match() {
        let mut db = db();
        db.record_code_refs(
            &cr_header("aaaaaaaaaaaa", "h1", 100),
            &[cr_row(0, "path", "order.rb")],
        )
        .unwrap();
        db.record_code_refs(
            &cr_header("bbbbbbbbbbbb", "h2", 200),
            &[cr_row(0, "path", "order.rb"), cr_row(1, "path", "cart.rb")],
        )
        .unwrap();
        db.record_code_refs(
            &cr_header("cccccccccccc", "h3", 300),
            &[cr_row(0, "path", "cart.rb")],
        )
        .unwrap();

        let hits = db.code_refs_by_target("order.rb", true).unwrap();
        let mut ids: Vec<&str> = hits.iter().map(|d| d.header.artifact_id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["aaaaaaaaaaaa", "bbbbbbbbbbbb"]);
        for d in &hits {
            assert!(
                d.refs
                    .iter()
                    .any(|r| r.path_hint.as_deref() == Some("order.rb")),
                "with_refs=true must populate each doc's refs"
            );
        }

        // A path nothing cites comes back empty, not an error.
        assert!(db.code_refs_by_target("nope.rb", true).unwrap().is_empty());

        // headers-only mode.
        let headers_only = db.code_refs_by_target("cart.rb", false).unwrap();
        assert_eq!(headers_only.len(), 2);
        assert!(headers_only.iter().all(|d| d.refs.is_empty()));
    }

    #[test]
    fn cascade_delete_removes_code_refs() {
        for mode in [
            crate::cascade::CascadeMode::Full,
            crate::cascade::CascadeMode::KeepUserData,
        ] {
            let mut db = db();
            let id = "aaaaaaaaaaaa";
            db.record_code_refs(&cr_header(id, "h", 100), &[cr_row(0, "path", "a.rb")])
                .unwrap();
            db.cascade_delete_doc(id, mode).unwrap();
            assert_eq!(
                count_where(&db, "code_refs", "artifact_id", id),
                0,
                "{mode:?}"
            );
            assert_eq!(
                count_where(&db, "code_refs_docs", "artifact_id", id),
                0,
                "{mode:?}"
            );
        }
    }

    // invariant:2 lifecycle-registries
    #[test]
    fn code_refs_rekey_on_relocate_cascade() {
        let mut db = db();
        db.record_code_refs(
            &cr_header("oldid0000001", "hash1", 42),
            &[cr_row(0, "path", "a.rb")],
        )
        .unwrap();
        let mid = db
            .moves_insert_intent("oldid0000001", "newid0000001", "old.html", "new.html", 1000)
            .unwrap();
        db.cascade_relocate_doc(
            "oldid0000001",
            "newid0000001",
            "old.html",
            "new.html",
            mid,
            1001,
        )
        .unwrap();
        assert!(db.code_refs_of("oldid0000001").unwrap().is_none());
        let moved = db.code_refs_of("newid0000001").unwrap().expect("rekeyed");
        assert_eq!(moved.refs.len(), 1);
        assert_eq!(moved.header.doc_hash, "hash1");
    }

    #[test]
    fn sweep_removes_orphan_code_refs() {
        let mut db = db();
        let live = "cccccccccccc";
        let orphan = "dddddddddddd";
        for id in [live, orphan] {
            db.record_code_refs(&cr_header(id, "h", 100), &[cr_row(0, "path", "a.rb")])
                .unwrap();
        }
        db.sweep_orphans(&std::collections::HashSet::from([live.to_string()]))
            .unwrap();
        assert_eq!(count_where(&db, "code_refs", "artifact_id", orphan), 0);
        assert_eq!(count_where(&db, "code_refs_docs", "artifact_id", orphan), 0);
        assert_eq!(count_where(&db, "code_refs", "artifact_id", live), 1);
        assert_eq!(count_where(&db, "code_refs_docs", "artifact_id", live), 1);
    }

    // --- CT-F1: memory_commits (V0038) ------------------------------------

    fn mc_row(memory_id: &str, sha_full: &str, artifact_id: &str) -> MemoryCommitRow {
        MemoryCommitRow {
            memory_id: memory_id.to_string(),
            sha_full: sha_full.to_string(),
            sha: Some(sha_full[..8.min(sha_full.len())].to_string()),
            subject: Some("fix the thing".to_string()),
            repo_root: Some("/home/user/project/kb".to_string()),
            session_id: "sess-1".to_string(),
            artifact_id: artifact_id.to_string(),
            recorded_at: 1_700_000_000,
        }
    }

    #[test]
    fn memory_commits_replace_round_trips_every_column() {
        let mut db = db();
        db.memory_commits_replace(
            "cap111111111",
            &[mc_row(
                "abc123def456",
                "deadbeef00112233445566778899aabbccddeeff",
                "cap111111111",
            )],
        )
        .unwrap();
        let got = db.memory_commits_for_memory("abc123def456", 10).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].sha_full, "deadbeef00112233445566778899aabbccddeeff");
        assert_eq!(got[0].sha.as_deref(), Some("deadbeef"));
        assert_eq!(got[0].subject.as_deref(), Some("fix the thing"));
        assert_eq!(got[0].repo_root.as_deref(), Some("/home/user/project/kb"));
        assert_eq!(got[0].session_id, "sess-1");
        assert_eq!(got[0].artifact_id, "cap111111111");
        assert_eq!(got[0].recorded_at, 1_700_000_000);
        // A memory nothing cites reads as an empty list, never an error.
        assert!(db
            .memory_commits_for_memory("ffffffffffff", 10)
            .unwrap()
            .is_empty());
    }

    /// Invariant #11's multi-capture fan-out: the SAME session is captured
    /// again (a new `artifact_id`) and re-derives the same trailer. The
    /// `(memory_id, sha_full)` PK must collapse that to ONE row — this is
    /// exactly why the reads need no `newest_capture_pred`.
    #[test]
    fn memory_commits_recapture_collapses_on_the_fact_pk() {
        let mut db = db();
        let sha = "deadbeef00112233445566778899aabbccddeeff";
        db.memory_commits_replace(
            "cap111111111",
            &[mc_row("abc123def456", sha, "cap111111111")],
        )
        .unwrap();
        let mut second = mc_row("abc123def456", sha, "cap222222222");
        second.recorded_at = 1_700_000_999;
        db.memory_commits_replace("cap222222222", &[second])
            .unwrap();

        let got = db.memory_commits_for_memory("abc123def456", 10).unwrap();
        assert_eq!(got.len(), 1, "one FACT, one row — never one per capture");
        assert_eq!(got[0].artifact_id, "cap222222222", "newest capture wins");
        assert_eq!(got[0].recorded_at, 1_700_000_999);
    }

    #[test]
    fn memory_commits_replace_is_scoped_to_this_capture() {
        let mut db = db();
        let sha_a = "aa00112233445566778899aabbccddeeff001122";
        let sha_b = "bb00112233445566778899aabbccddeeff001122";
        db.memory_commits_replace(
            "cap111111111",
            &[mc_row("aaaaaaaaaaaa", sha_a, "cap111111111")],
        )
        .unwrap();
        // A DIFFERENT capture's replace must not delete the first one's rows.
        db.memory_commits_replace(
            "cap222222222",
            &[mc_row("bbbbbbbbbbbb", sha_b, "cap222222222")],
        )
        .unwrap();
        assert_eq!(
            db.memory_commits_for_memory("aaaaaaaaaaaa", 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.memory_commits_for_memory("bbbbbbbbbbbb", 10)
                .unwrap()
                .len(),
            1
        );
        // Re-running THIS capture with an empty set clears only its own rows.
        db.memory_commits_replace("cap222222222", &[]).unwrap();
        assert_eq!(
            db.memory_commits_for_memory("aaaaaaaaaaaa", 10)
                .unwrap()
                .len(),
            1
        );
        assert!(db
            .memory_commits_for_memory("bbbbbbbbbbbb", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn memory_commits_for_memory_is_newest_first_and_capped() {
        let mut db = db();
        let mut rows = Vec::new();
        for i in 0..5u32 {
            let sha = format!("{i:040}");
            let mut r = mc_row("abc123def456", &sha, "cap111111111");
            r.recorded_at = 1_700_000_000 + i as i64;
            rows.push(r);
        }
        db.memory_commits_replace("cap111111111", &rows).unwrap();
        let got = db.memory_commits_for_memory("abc123def456", 3).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].recorded_at, 1_700_000_004);
        assert_eq!(got[2].recorded_at, 1_700_000_002);
    }

    // invariant:2 lifecycle-registries
    #[test]
    fn cascade_delete_removes_memory_commits() {
        for mode in [
            crate::cascade::CascadeMode::Full,
            crate::cascade::CascadeMode::KeepUserData,
        ] {
            let mut db = db();
            let cap = "cap111111111";
            db.memory_commits_replace(
                cap,
                &[mc_row(
                    "abc123def456",
                    "deadbeef00112233445566778899aabbccddeeff",
                    cap,
                )],
            )
            .unwrap();
            db.cascade_delete_doc(cap, mode).unwrap();
            assert_eq!(
                count_where(&db, "memory_commits", "artifact_id", cap),
                0,
                "{mode:?}"
            );
        }
    }

    // invariant:2 lifecycle-registries
    #[test]
    fn memory_commits_rekey_on_relocate_cascade() {
        let mut db = db();
        db.memory_commits_replace(
            "oldid0000001",
            &[mc_row(
                "abc123def456",
                "deadbeef00112233445566778899aabbccddeeff",
                "oldid0000001",
            )],
        )
        .unwrap();
        let mid = db
            .moves_insert_intent("oldid0000001", "newid0000001", "old.html", "new.html", 1000)
            .unwrap();
        db.cascade_relocate_doc(
            "oldid0000001",
            "newid0000001",
            "old.html",
            "new.html",
            mid,
            1001,
        )
        .unwrap();
        assert_eq!(
            count_where(&db, "memory_commits", "artifact_id", "oldid0000001"),
            0,
            "relocate never re-indexes (#27/F3) — a stranded row is forever"
        );
        let moved = db.memory_commits_for_memory("abc123def456", 10).unwrap();
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].artifact_id, "newid0000001");
        // `memory_id` is deliberately untouched by the rekey — see the
        // relocate tx's comment (cross-kb, and ids collide across corpora).
        assert_eq!(moved[0].memory_id, "abc123def456");
    }

    // invariant:2 lifecycle-registries
    #[test]
    fn sweep_removes_orphan_memory_commits() {
        let mut db = db();
        let live = "cccccccccccc";
        let orphan = "dddddddddddd";
        for cap in [live, orphan] {
            db.memory_commits_replace(cap, &[mc_row("abc123def456", &format!("{cap:0>40}"), cap)])
                .unwrap();
        }
        db.sweep_orphans(&std::collections::HashSet::from([live.to_string()]))
            .unwrap();
        assert_eq!(count_where(&db, "memory_commits", "artifact_id", orphan), 0);
        assert_eq!(count_where(&db, "memory_commits", "artifact_id", live), 1);
        // The surviving row's `memory_id` is NOT in `keep` (the memory lives
        // in another kb) and must never be treated as an orphan key.
        assert_eq!(
            db.memory_commits_for_memory("abc123def456", 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn sweep_removes_orphans_keeps_live_and_search_rows() {
        let mut db = db();
        let live = "cccccccccccc";
        let orphan = "dddddddddddd";
        db.corkboard_add(live, 100).unwrap();
        db.corkboard_add(orphan, 100).unwrap();
        db.record_edges(orphan, &[link("x")]).unwrap(); // src = orphan
        db.record_edges(live, &[link("y")]).unwrap(); // src = live
        let vo = db
            .history_record_open(orphan, 100, None, "operator")
            .unwrap();
        db.reading_upsert_sections(vo.id, orphan, &[dwell("s", 0, 1, 1, 1)], 100)
            .unwrap();
        // A search history row has a NULL artifact_id and must NEVER be swept.
        db.history_record_search("q", 100, "operator").unwrap();
        db.list_create("l_9", "L9", None, false, 100).unwrap();
        db.list_entry_add(
            &nle("le_o", "l_9", orphan, None),
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();
        db.list_entry_add(
            &nle("le_l", "l_9", live, None),
            &PositionSpec::Last,
            "operator",
            100,
        )
        .unwrap();

        let keep: std::collections::HashSet<String> = [live.to_string()].into_iter().collect();
        let out = db.sweep_orphans(&keep).unwrap();

        // Orphan dependents reclaimed.
        assert_eq!(count_where(&db, "corkboard", "artifact_id", orphan), 0);
        assert_eq!(count_where(&db, "edges", "src_artifact", orphan), 0);
        assert_eq!(count_where(&db, "history", "artifact_id", orphan), 0);
        assert_eq!(
            count_where(&db, "reading_sections", "artifact_id", orphan),
            0
        );
        // The orphan's list entry PERSISTS — the sweep never reclaims list
        // entries; it survives as a tombstone.
        assert_eq!(count_where(&db, "list_entries", "artifact_id", orphan), 1);
        // Live dependents untouched.
        assert_eq!(count_where(&db, "corkboard", "artifact_id", live), 1);
        assert_eq!(count_where(&db, "edges", "src_artifact", live), 1);
        assert_eq!(count_where(&db, "list_entries", "artifact_id", live), 1);
        // The NULL-artifact search row is preserved.
        let searches: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM history WHERE kind = 'search'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(searches, 1);
        assert!(out.edges_removed >= 1 && out.corkboard_removed >= 1);
    }

    #[test]
    fn sweep_is_a_noop_when_everything_is_live() {
        let mut db = db();
        let id = "eeeeeeeeeeee";
        db.corkboard_add(id, 100).unwrap();
        db.record_edges(id, &[link("z")]).unwrap();
        let keep: std::collections::HashSet<String> = [id.to_string()].into_iter().collect();
        let out = db.sweep_orphans(&keep).unwrap();
        assert_eq!(out.total_rows, 0);
        assert_eq!(count_where(&db, "corkboard", "artifact_id", id), 1);
    }
}
