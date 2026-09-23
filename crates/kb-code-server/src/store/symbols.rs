//! Blob-keyed symbols, imports, calls, occurrences, highlights, chunks, and the stale-salt sweep.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    /// One PAGE of the bill's derived-row census: the next `page` distinct
    /// `files.blob_hash` values after `after`, and the row count each
    /// derived table holds for them.
    ///
    /// Paged for the reason [`Store::sweep_stale_salt_page`] is (V72-B0):
    /// the un-paged `blob_hash IN (SELECT blob_hash FROM files)` shape is
    /// O(every live blob) random seeks per table and would hold the
    /// store's single connection mutex for the whole of it — on a
    /// production-sized mirror that is a self-inflicted outage, and a
    /// measurement tool that takes the daemon down is not a measurement
    /// tool. Returns the cursor to resume from, `None` at the end.
    pub fn derived_row_census_page(&self, after: Option<&str>, page: usize) -> Result<CensusPage> {
        let conn = self.lock();
        let blobs: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT blob_hash FROM files WHERE blob_hash > ?1 \
                 ORDER BY blob_hash LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![after.unwrap_or(""), page as i64], |r| r.get(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        if blobs.is_empty() {
            return Ok(CensusPage {
                counts: Vec::new(),
                blobs: 0,
                next: None,
            });
        }
        let slots = blobs.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut counts: Vec<(&'static str, u64)> = Vec::new();
        for table in BILL_TABLES {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE blob_hash IN ({slots})");
            let n: i64 =
                conn.query_row(&sql, rusqlite::params_from_iter(blobs.iter()), |r| r.get(0))?;
            counts.push((table, n as u64));
        }
        let next = if blobs.len() < page {
            None
        } else {
            blobs.last().cloned()
        };
        Ok(CensusPage {
            counts,
            blobs: blobs.len(),
            next,
        })
    }

    // --- derived_status: the per-family "extracted" marker (V72-H2b) -----
    //
    // The cache-hit gates used to be ROW-COUNT questions, which cannot tell
    // "not derived yet" from "derived, and zero rows was the honest
    // answer" — so every zero-symbol file re-parsed on every visit (an ERB
    // template, an SCSS file, a comment-only Rust file). These four fns are
    // the fix: the marker's EXISTENCE is the gate, `rows` is bookkeeping
    // for the re-extract bill and never consulted by one.

    /// `true` if `(blob_hash, family, salt)` has been derived — the gate
    /// `ingest::index_file` uses, deliberately independent of how many rows
    /// the derivation produced.
    pub fn is_derived(
        &self,
        blob_hash: &str,
        family: crate::lang::SaltFamily,
        salt: &str,
    ) -> Result<bool> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM derived_status \
             WHERE blob_hash = ?1 AND family = ?2 AND salt = ?3",
            params![blob_hash, family.as_str(), salt],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// `true` iff BOTH `(blob_hash, Symbol, symbol_salt)` and
    /// `(blob_hash, Highlight, highlight_salt)` are derived — one lock
    /// acquisition for both `SELECT COUNT` queries, so the two checks the
    /// V77-P1 boot/live-edit fast paths make before skipping a read cannot
    /// observe two different instants of the store between them (the
    /// residual TOCTOU is narrower, not gone: a concurrent writer could
    /// still invalidate one marker in the gap between this fn returning
    /// `true` and the caller acting on it — see `ingest::walk_dir`'s and
    /// `sink::handle_upsert`'s own doc comments on the skip decision for
    /// why that residual window is accepted rather than closed with a
    /// second lock spanning the caller's own `continue`/`return`).
    pub fn is_derived_pair(
        &self,
        blob_hash: &str,
        symbol_salt: &str,
        highlight_salt: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        let symbol_n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM derived_status \
             WHERE blob_hash = ?1 AND family = ?2 AND salt = ?3",
            params![
                blob_hash,
                crate::lang::SaltFamily::Symbol.as_str(),
                symbol_salt
            ],
            |r| r.get(0),
        )?;
        if symbol_n == 0 {
            return Ok(false);
        }
        let highlight_n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM derived_status \
             WHERE blob_hash = ?1 AND family = ?2 AND salt = ?3",
            params![
                blob_hash,
                crate::lang::SaltFamily::Highlight.as_str(),
                highlight_salt
            ],
            |r| r.get(0),
        )?;
        Ok(highlight_n > 0)
    }

    /// V77-P3 (Task 0) — every LIVE `derived_status` row (i.e. keyed by one
    /// of TODAY's registered salts, in EITHER family) as
    /// `(blob_hash, family, salt)`. ONE query, bounded by the number of
    /// registered languages (`lang::ALL_LANGS.len() * 2` VALUES rows), not
    /// by corpus size — the boot walk's preload for
    /// `ingest::DerivedPreload` (see that struct's doc for why the boot
    /// walk wants this instead of one `is_derived_pair` round trip per
    /// unchanged file). Not repo-scoped: `derived_status` is
    /// content-addressed and shared across every configured repo (ADR-2),
    /// so there is no `repo_id` to filter by — the same tradeoff
    /// `sweep_stale_salt_page` already accepts for this table, just
    /// without that fn's paging (this query is bounded by SALT count, not
    /// row count, so it stays cheap regardless of corpus size).
    pub fn derived_status_for_current_salts(&self) -> Result<Vec<(String, String, String)>> {
        let symbol_salts = crate::lang::current_salts(crate::lang::SaltFamily::Symbol);
        let highlight_salts = crate::lang::current_salts(crate::lang::SaltFamily::Highlight);
        let symbol_values = symbol_salts
            .iter()
            .map(|_| "(?)")
            .collect::<Vec<_>>()
            .join(",");
        let highlight_values = highlight_salts
            .iter()
            .map(|_| "(?)")
            .collect::<Vec<_>>()
            .join(",");
        let symbol_family = crate::lang::SaltFamily::Symbol.as_str();
        let highlight_family = crate::lang::SaltFamily::Highlight.as_str();
        let sql = format!(
            "WITH cur_symbol(salt) AS (VALUES {symbol_values}), \
                  cur_highlight(salt) AS (VALUES {highlight_values}) \
             SELECT blob_hash, family, salt FROM derived_status \
             WHERE (family = '{symbol_family}' AND salt IN (SELECT salt FROM cur_symbol)) \
                OR (family = '{highlight_family}' AND salt IN (SELECT salt FROM cur_highlight))"
        );
        let conn = self.lock();
        let mut stmt = conn.prepare(&sql)?;
        let mut bind: Vec<&str> = symbol_salts;
        bind.extend(highlight_salts.iter().copied());
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bind.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The recorded row count for a derivation, or `None` when there is no
    /// marker. `Some(0)` and `None` are DIFFERENT answers and the whole
    /// reason this table exists.
    pub fn derived_rows(
        &self,
        blob_hash: &str,
        family: crate::lang::SaltFamily,
        salt: &str,
    ) -> Result<Option<i64>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT rows FROM derived_status \
                 WHERE blob_hash = ?1 AND family = ?2 AND salt = ?3",
                params![blob_hash, family.as_str(), salt],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Record a derivation outside a derived-table write — the direct door
    /// for tests and for a future pass that derives nothing at all. The
    /// production writers ([`Store::replace_symbols`],
    /// [`Store::put_highlights`]) mark inside their OWN transaction via
    /// [`mark_derived_in`], so the marker and the rows it describes can
    /// never land apart.
    pub fn mark_derived(
        &self,
        blob_hash: &str,
        family: crate::lang::SaltFamily,
        salt: &str,
        rows: usize,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        mark_derived_in(&tx, blob_hash, family, salt, rows)?;
        tx.commit()?;
        Ok(())
    }

    // --- symbols (blob-keyed) -------------------------------------------

    /// `true` if `(blob_hash, salt)` already has derived symbol ROWS.
    ///
    /// **Not the cache gate.** It was until V72-H2b, and `COUNT(*) > 0`
    /// cannot distinguish "not derived" from "derived, zero symbols", so
    /// every zero-symbol blob re-parsed on every visit. The gate is now
    /// [`Store::is_derived`]; this stays as the honest "are there rows"
    /// question its name asks, for callers that mean exactly that.
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
        // V72-H2b — the SYMBOL family's derivation marker, in the same
        // transaction as the rows it describes, so "derived" and "these
        // are the rows" can never disagree. An empty `symbols` slice is a
        // real derivation with a real marker; that is the fix.
        mark_derived_in(
            &tx,
            blob_hash,
            crate::lang::SaltFamily::Symbol,
            salt,
            symbols.len(),
        )?;
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
        let (cte, salts) = current_salt_cte(crate::lang::SaltFamily::Symbol);
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
            let salt = lang.map(|l| l.symbol_salt).unwrap_or("");
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
            let salt = lang.map(|l| l.symbol_salt).unwrap_or("");
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
        let (cte, salts) = current_salt_cte(crate::lang::SaltFamily::Symbol);
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
        let (cte, salts) = current_salt_cte(crate::lang::SaltFamily::Symbol);
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
        let (cte, salts) = current_salt_cte(crate::lang::SaltFamily::Symbol);
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
        // V72-H2b: each table is swept against the salt set of the family
        // that keys IT. Sweeping `highlights` against the SYMBOL salts
        // would find every current highlight row "not in cur" and delete
        // it the moment a symbol-salted sibling existed — the exact damage
        // this sweep exists to undo.
        let mut counts = StaleSaltSweepCounts::default();
        for (table, family, family_value) in SWEEP_TABLES {
            let (cte, salts) = current_salt_cte(*family);
            let n = sweep_stale_salt_table(&tx, &cte, &salts, &blobs, table, *family_value)?;
            match *table {
                "symbols" => counts.symbols += n,
                "highlights" => counts.highlights += n,
                "occurrences" => counts.occurrences += n,
                _ => counts.derived_status += n,
            }
        }
        let StaleSaltSweepCounts {
            symbols,
            highlights,
            occurrences,
            derived_status,
        } = counts;
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
                derived_status,
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
        // V72-H2b — the HIGHLIGHT family's marker, same transaction, same
        // reason as `replace_symbols`'s. `salt` here is a
        // `LangInfo::highlight_salt`; passing a symbol salt would key the
        // marker under a string the highlight gate never asks about.
        mark_derived_in(
            &tx,
            blob_hash,
            crate::lang::SaltFamily::Highlight,
            salt,
            spans.len(),
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

    /// [`Self::symbols_for_repo`] narrowed to one path PREFIX — the same
    /// current-salt discipline (invariant 11), one `LIKE 'prefix%'` more.
    ///
    /// `prefix` is a caller-side CONSTANT (`app/controllers/`), never a
    /// user-supplied string: `LIKE`'s `%`/`_` wildcards are not escaped
    /// here, so a caller that ever wants to pass a query param must escape
    /// them first. The narrowing is the point — the `rails/1` index needs
    /// the methods of a few hundred controller files, and materialising a
    /// monolith's ~200k symbol rows to filter in Rust is the shape
    /// kbc-tree/1's own whole-repo reads already refused.
    pub fn symbols_for_repo_under_prefix(
        &self,
        repo_id: i64,
        prefix: &str,
    ) -> Result<Vec<(String, Symbol)>> {
        let conn = self.lock();
        let (cte, salts) = current_salt_cte(crate::lang::SaltFamily::Symbol);
        let sql = format!(
            "{cte}
             SELECT f.path, s.ordinal, s.name, s.kind, s.line_start, s.line_end,
                    s.col_start, s.col_end, s.container, s.signature, s.doc,
                    s.param_min, s.param_max
             FROM files f
             JOIN symbols s ON s.blob_hash = f.blob_hash
             WHERE f.repo_id = ?
               AND f.path LIKE ? || '%'
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
        bind.push(Box::new(prefix.to_string()));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bind.iter()), |r| {
                Ok((r.get::<_, String>(0)?, symbol_from_row(r, 1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
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
}
