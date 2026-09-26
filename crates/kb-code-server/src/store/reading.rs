//! Reading sets and their spans.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
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
}
