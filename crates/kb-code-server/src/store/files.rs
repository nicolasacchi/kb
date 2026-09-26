//! Repo and file mirror, plus file-open recency.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
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
    ///
    /// V77-P1: a thin wrapper over [`Store::upsert_file_with_mtime`] with
    /// `mtime = 0` ("unknown") — every ODB tree-walk caller (`ingest.rs`)
    /// has no filesystem mtime to offer (a git blob has none), and every
    /// existing test fixture calling this fn keeps writing the same
    /// "unknown" fingerprint it always implicitly did. Kept as the ONE
    /// unchanged 5-arg signature so the ~100 existing call sites across
    /// this crate need no edit.
    pub fn upsert_file(
        &self,
        repo_id: i64,
        path: &str,
        blob_hash: &str,
        lang: &str,
        size: u64,
    ) -> Result<()> {
        self.upsert_file_with_mtime(repo_id, path, blob_hash, lang, size, 0)
    }

    /// Same as [`Store::upsert_file`], but also records the filesystem
    /// `mtime` (unix seconds; `0` = unknown) — for the two `sink.rs`
    /// `std::fs`-reading paths only. `mtime` rides the SAME `ON CONFLICT`
    /// upsert as `blob_hash`/`size`, in the SAME statement, never a
    /// follow-up `UPDATE`: a reader must never be able to observe a row
    /// whose `mtime` fingerprint matches what it just stat'd while
    /// `blob_hash` is still from a stale write (or vice versa) — see
    /// `V0044__files_mtime.sql`'s header.
    pub fn upsert_file_with_mtime(
        &self,
        repo_id: i64,
        path: &str,
        blob_hash: &str,
        lang: &str,
        size: u64,
        mtime: u64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO files (repo_id, path, blob_hash, lang, size, mtime) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(repo_id, path) DO UPDATE SET
                blob_hash = excluded.blob_hash,
                lang = excluded.lang,
                size = excluded.size,
                mtime = excluded.mtime",
            params![repo_id, path, blob_hash, lang, size as i64, mtime as i64],
        )?;
        self.bump_generation();
        Ok(())
    }

    /// Every current `files` row for `repo_id`, path-ordered — the files
    /// search lane's (`search::files::FileIndex`) cache source, and the
    /// text lane's (`search::text::search_text`) working-tree walk list
    /// (see that module's doc: it reuses this table rather than a fresh
    /// filesystem walk). Also the boot ODB walk's fingerprint preload
    /// (`ingest::index_repo_working_tree`) — ONE query for the whole repo
    /// rather than a per-file lookup.
    pub fn list_files(&self, repo_id: i64) -> Result<Vec<FileRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path, blob_hash, lang, size, mtime FROM files WHERE repo_id = ?1 \
             ORDER BY path",
        )?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                Ok(FileRow {
                    path: r.get(0)?,
                    blob_hash: r.get(1)?,
                    lang: r.get(2)?,
                    size: r.get::<_, i64>(3)? as u64,
                    mtime: r.get::<_, i64>(4)? as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_file(&self, repo_id: i64, path: &str) -> Result<Option<FileRow>> {
        self.lock()
            .query_row(
                "SELECT path, blob_hash, lang, size, mtime FROM files \
                 WHERE repo_id = ?1 AND path = ?2",
                params![repo_id, path],
                |r| {
                    Ok(FileRow {
                        path: r.get(0)?,
                        blob_hash: r.get(1)?,
                        lang: r.get(2)?,
                        size: r.get::<_, i64>(3)? as u64,
                        mtime: r.get::<_, i64>(4)? as u64,
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
    ///
    /// ALSO prunes `comments` rows (V72-J1) — same path-keyed reasoning,
    /// and the reason the table is named in this ONE registry rather than
    /// relying on a foreign key it does not have.
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
        // V72-H4a — `lane_facts` joins the same id-lifecycle registry, for
        // the same reason `rails_edges` and `entity_defs` are here: its
        // reads are `(repo_id, path)`-keyed, so a deleted file's facts
        // would keep answering `GET /api/lanes/facts` forever, and the
        // per-request classing would have no current blob to grade them
        // against — every one of them a permanent orphan nobody can clear.
        // The owning `lane_runs` row is left alone deliberately: it is the
        // provenance of an ingest that really happened, and the retention
        // sweep is what ages it out.
        tx.execute(
            "DELETE FROM lane_facts WHERE repo_id = ?1 AND path = ?2",
            params![repo_id, path],
        )?;
        // V72-J1 — `comments` is `(repo_id, path)`-keyed for exactly the
        // same reason `rails_edges`/`entity_defs` are, so it joins the same
        // one id-lifecycle registry. `todo_items` (which this table
        // subsumes) rode `ON DELETE CASCADE` from `files(id)`; these rows
        // do not reference `files` at all, so the cascade is here in Rust,
        // inside the same transaction.
        tx.execute(
            "DELETE FROM comments WHERE repo_id = ?1 AND path = ?2",
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

    // --- the re-extract bill's own reads (V72-H2b) ------------------------

    /// `(lang, files, bytes)` per `files.lang` for one repo — the bill's
    /// exact half. One indexed GROUP BY over `(repo_id)`; the content skip
    /// markers (`unknown`/`binary`/`too-large`/`lfs`) come back as their
    /// own rows, which is what makes "how much of this repo the instrument
    /// cannot see" visible beside what a bump would cost.
    ///
    /// V77-P3 (Task 2, the E6 finding) — ordered by total bytes DESC (tie-
    /// broken by `lang` for determinism), not alphabetically: the
    /// `reextract-bill/1` sample pass (`reextract::build_bill`) walks this
    /// list to size each language's timed-sample allotment, and the E6
    /// finding was exactly a small-but-alphabetically-early language (HAML)
    /// starving a repo's actual DOMINANT language (Ruby) out of a shared
    /// clock. Iterating biggest-bytes-first means the dominant language's
    /// allotment no longer depends on where some other language happens to
    /// sit in the alphabet.
    pub fn files_by_lang(&self, repo_id: i64) -> Result<Vec<(String, u64, u64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT lang, COUNT(*), COALESCE(SUM(size), 0) FROM files \
             WHERE repo_id = ?1 GROUP BY lang ORDER BY COALESCE(SUM(size), 0) DESC, lang ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, i64>(2)? as u64,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The first `limit` paths of one language in `repo_id`, in `path`
    /// order. DETERMINISTIC on purpose: the bill's timed sample must be
    /// the same set on two consecutive runs, or its numbers cannot be
    /// compared to each other.
    pub fn sample_paths_for_lang(
        &self,
        repo_id: i64,
        lang: &str,
        limit: usize,
    ) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path FROM files WHERE repo_id = ?1 AND lang = ?2 ORDER BY path LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![repo_id, lang, limit as i64], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The integer `files.id` for `(repo_id, path)` — the join key for
    /// the `files`-keyed derived tables (`import_edges`; before V72-J1,
    /// `todo_items` too). V0012 gave `files` a free-standing INTEGER
    /// PRIMARY KEY for exactly this; pre-V0012 the table was keyed only by
    /// `(repo_id, path)`.
    pub fn file_id(&self, repo_id: i64, path: &str) -> Result<Option<i64>> {
        // PF-K1 — called once per caller-group in `hierarchy::callers_at`'s
        // fan-out; `prepare_cached` (identical SQL every call) avoids a
        // full re-parse/re-plan on every call.
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
