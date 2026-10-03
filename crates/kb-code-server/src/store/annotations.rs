//! Annotation threads, suggestions, and open-annotation counts.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    // --- annotations (W4.6 migration V0006; D-server migration V0008 adds
    // anchor kinds/threads/intents) -------------------------------------
    //
    // Deliberately NO `bump_generation()` calls in this section — same
    // rationale as the commit_sessions/transcripts sections above:
    // `generation` only invalidates the files/symbols search lanes' caches,
    // which annotations have nothing to do with.
    //
    // Every SELECT below spells out the SAME 18-column order (matching
    // `annotation_row_from`'s positional `r.get(0..17)` reads — V70-A10
    // appended `set_id` at index 16 after `side`, and V74-L3b appended
    // `trail_id` LAST at index 17, which is why the two queries that also
    // select a correlated `reply_count` now read it at 18) rather than
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
                 review_id, ps_number, side, set_id, trail_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                     ?18)",
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
                row.trail_id,
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
                    review_id, ps_number, side, set_id, trail_id
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
                    review_id, ps_number, side, set_id, trail_id
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
                        review_id, ps_number, side, set_id, trail_id
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

    /// V80-M0 — bind/rebind (`scope = Some((review_id, ps_number, side))`)
    /// or unbind (`scope = None`) a TOP-LEVEL annotation's review scope
    /// AFTER creation. Sets all three columns together (never a partial
    /// `COALESCE` like [`Self::update_annotation`] — a scope is one unit,
    /// not three independently-settable fields) and stamps `updated_at`.
    /// Never touches `anchor`/`anchor_kind`/`anchor2`/`parent_id`/`body`/
    /// `resolved`/`intent` — same "one route, one column family" discipline
    /// `update_annotation` already follows. Returns `true` iff a row with
    /// `id` existed (the route's 404 check) — the caller reads the PRIOR
    /// `review_id` via [`Self::get_annotation`] BEFORE calling this (single-
    /// writer `Mutex`, so that read-then-write is already atomic w.r.t. any
    /// other store call).
    pub fn update_annotation_review_scope(
        &self,
        id: &str,
        scope: Option<(i64, i64, &str)>,
        updated_at: i64,
    ) -> Result<bool> {
        let (review_id, ps_number, side) = match scope {
            Some((r, p, s)) => (Some(r), Some(p), Some(s)),
            None => (None, None, None),
        };
        let n = self.lock().execute(
            "UPDATE annotations SET
                review_id = ?2, ps_number = ?3, side = ?4, updated_at = ?5
             WHERE id = ?1 OR parent_id = ?1",
            params![id, review_id, ps_number, side, updated_at],
        )?;
        Ok(n > 0)
    }

    /// A7-2 — true iff a finding uses this annotation as its thread (the
    /// bind/unbind routes refuse such rows with 409).
    pub fn annotation_backs_finding(&self, id: &str) -> Result<bool> {
        let conn = self.lock();
        annotation_backs_finding_on(&conn, id)
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
                    a.review_id, a.ps_number, a.side, a.set_id, a.trail_id,
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
                Ok((annotation_row_from(r)?, r.get::<_, i64>(18)?))
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
                    a.review_id, a.ps_number, a.side, a.set_id, a.trail_id,
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
                Ok((annotation_row_from(r)?, r.get::<_, i64>(18)?))
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
                    review_id, ps_number, side, set_id, trail_id
             FROM annotations
             WHERE review_id = ?1
             ORDER BY created_at ASC, id ASC"
        } else {
            "SELECT id, repo_id, path, anchor, anchor_kind, anchor2, parent_id, intent,
                    body, author, created_at, updated_at, resolved,
                    review_id, ps_number, side, set_id, trail_id
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
                        review_id, ps_number, side, set_id, trail_id
                 FROM annotations
                 WHERE review_id IN ({placeholders})
                 ORDER BY review_id ASC, created_at ASC, id ASC"
            )
        } else {
            format!(
                "SELECT id, repo_id, path, anchor, anchor_kind, anchor2, parent_id, intent,
                        body, author, created_at, updated_at, resolved,
                        review_id, ps_number, side, set_id, trail_id
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

    /// v0.44 F9b — the annotation ids, among those bound to any of
    /// `review_ids`, that carry a stored suggestion (applied or not). One
    /// join instead of a `get_annotation_suggestion` per finding; the agent
    /// queue's "agreed but no suggestion yet" lane reads it.
    pub fn review_suggestion_annotation_ids(
        &self,
        review_ids: &[i64],
    ) -> Result<std::collections::HashSet<String>> {
        let mut out = std::collections::HashSet::new();
        if review_ids.is_empty() {
            return Ok(out);
        }
        let conn = self.lock();
        let placeholders = (0..review_ids.len())
            .map(|i| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT s.annotation_id
             FROM annotation_suggestions s
             JOIN annotations a ON a.id = s.annotation_id
             WHERE a.review_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let ids = stmt
            .query_map(rusqlite::params_from_iter(review_ids.iter()), |r| {
                r.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        out.extend(ids);
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
                PreparedAnnotationOp::BindReview {
                    id,
                    review_id,
                    ps_number,
                    side,
                } => {
                    let Some(cur) = get_annotation_on(&tx, id)? else {
                        return Err(StoreError::NotFound(format!("annotation {id}")));
                    };
                    if annotation_backs_finding_on(&tx, id)? {
                        return Err(StoreError::AnnotationIsFinding(id.clone()));
                    }
                    let same = cur.review_id == Some(*review_id)
                        && cur.ps_number == Some(*ps_number)
                        && cur.side.as_deref() == Some(side.as_str());
                    if !same {
                        update_annotation_review_scope_on(
                            &tx,
                            id,
                            Some((*review_id, *ps_number, side.as_str())),
                            now,
                        )?;
                        report.changed = true;
                    }
                }
                PreparedAnnotationOp::UnbindReview { id } => {
                    let Some(cur) = get_annotation_on(&tx, id)? else {
                        return Err(StoreError::NotFound(format!("annotation {id}")));
                    };
                    if annotation_backs_finding_on(&tx, id)? {
                        return Err(StoreError::AnnotationIsFinding(id.clone()));
                    }
                    if cur.review_id.is_some() {
                        update_annotation_review_scope_on(&tx, id, None, now)?;
                        report.changed = true;
                    }
                }
            }
            report.applied += 1;
        }
        tx.commit()?;
        Ok(report)
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
                    review_id, ps_number, side, set_id, trail_id
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

    /// Reply count for one annotation thread — the same correlated count
    /// `list_open_annotations` computes inline, exposed for a single id so
    /// `boards::resolve` can report a node thread's size without pulling
    /// every reply row.
    pub fn count_annotation_replies(&self, id: &str) -> Result<i64> {
        self.lock()
            .query_row(
                "SELECT COUNT(*) FROM annotations WHERE parent_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(Into::into)
    }
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

pub(super) fn insert_annotation_on(tx: &Transaction<'_>, row: &AnnotationRow) -> Result<()> {
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

pub(super) fn update_annotation_on(
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
         WHERE id = ?1 OR parent_id = ?1",
        params![id, review_id, ps_number, side, updated_at],
    )?;
    Ok(n > 0)
}

/// A7-2 — does a `review_findings` row use `annotation_id` as its thread?
fn annotation_backs_finding_on(conn: &rusqlite::Connection, annotation_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM review_findings WHERE annotation_id = ?1)",
        params![annotation_id],
        |r| r.get::<_, bool>(0),
    )?)
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

/// One `annotations` row (W4.6 migration V0006; D-server migration V0008
/// adds `anchor_kind`/`anchor2`/`parent_id`/`intent`; V4.C1 / V0023 adds
/// `review_id`/`ps_number`/`side`). `anchor`/`anchor2` are RAW JSON
/// exactly as stored — callers (`crate::annotations`) deserialize them,
/// never this module (the store stays free of kb-core's review types,
/// same "row types don't know about other modules' concerns" convention
/// `CommitSessionRow` (`store/transcripts.rs`) follows).
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
