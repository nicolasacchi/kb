//! Canvas sets and boards.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
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

    // --- kbc-canvas/1 boards (V74-L1, migration V0036) --------------------
    //
    // Deliberately NO `bump_generation()` on any write below. A board is a
    // set of REFERENCES into the index, never an input to it — invisible to
    // `FileIndex`/`SymbolIndex`'s generation-keyed caches, exactly like the
    // bookmarks and doc-lens sections above and for the identical reason.
    //
    // Every board read is a two-step (`get_canvas_board` then the three
    // child reads) rather than one join: a caller wants the parent to 404
    // on its own before it pays for the children, and a fan-out join would
    // have to de-duplicate the parent row per child.

    /// Board summaries for one repo, newest-updated first. Counts come from
    /// correlated scalar sub-queries — the `list_reading_sets` shape — so a
    /// board with no nodes reports a true 0 rather than vanishing from an
    /// inner join.
    ///
    /// V74-L3b — `kind` is REQUIRED, never defaulted. A tour is a
    /// `canvas_boards` row too (`tours::BOARD_KIND_TOUR`, migration
    /// V0039), so a read that forgot to say which family it wanted would
    /// silently list the other one; making the caller name it is what
    /// keeps `GET /api/boards` and `GET /api/tours` disjoint by
    /// construction rather than by convention.
    pub fn list_canvas_boards(
        &self,
        repo_id: i64,
        kind: &str,
        status: Option<&str>,
    ) -> Result<Vec<CanvasBoardSummaryRow>> {
        let conn = self.lock();
        let sql = "SELECT b.id, b.slug, b.title, b.status, b.revision, b.updated_unix,
                          (SELECT COUNT(*) FROM canvas_nodes n WHERE n.board_id = b.id),
                          (SELECT COUNT(*) FROM canvas_edges e WHERE e.board_id = b.id),
                          (SELECT COUNT(*) FROM canvas_steps s WHERE s.board_id = b.id)
                   FROM canvas_boards b
                   WHERE b.repo_id = ?1 AND b.kind = ?3 AND (?2 IS NULL OR b.status = ?2)
                   ORDER BY b.updated_unix DESC, b.slug ASC";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(params![repo_id, status, kind], |r| {
                Ok(CanvasBoardSummaryRow {
                    id: r.get(0)?,
                    slug: r.get(1)?,
                    title: r.get(2)?,
                    status: r.get(3)?,
                    revision: r.get(4)?,
                    updated_unix: r.get(5)?,
                    nodes: r.get(6)?,
                    edges: r.get(7)?,
                    steps: r.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// One board by its `(repo_id, slug)` identity — the pair
    /// `canvas_boards`' own UNIQUE constraint indexes — NARROWED to one
    /// `kind` (V74-L3b). That UNIQUE spans both kinds, so a slug names at
    /// most one row either way; the filter is what makes
    /// `GET /api/boards/{slug}` 404 honestly on a TOUR's slug instead of
    /// rendering a tour as a board.
    pub fn get_canvas_board(
        &self,
        repo_id: i64,
        kind: &str,
        slug: &str,
    ) -> Result<Option<CanvasBoardRow>> {
        self.lock()
            .query_row(
                "SELECT id, repo_id, slug, title, description_md, status, authored_ref,
                        content_hash, revision, created_unix, updated_unix
                 FROM canvas_boards WHERE repo_id = ?1 AND slug = ?2 AND kind = ?3",
                params![repo_id, slug, kind],
                canvas_board_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// A board's nodes, in authored order.
    pub fn canvas_board_nodes(&self, board_id: i64) -> Result<Vec<CanvasNodeRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT node_id, ordinal, kind, title, body_md, ref_json, group_id, thread_id,
                    anchor_snippet, pin_x, pin_y
             FROM canvas_nodes WHERE board_id = ?1 ORDER BY ordinal ASC",
        )?;
        let rows = stmt
            .query_map(params![board_id], |r| {
                Ok(CanvasNodeRow {
                    node_id: r.get(0)?,
                    ordinal: r.get(1)?,
                    kind: r.get(2)?,
                    title: r.get(3)?,
                    body_md: r.get(4)?,
                    ref_json: r.get(5)?,
                    group_id: r.get(6)?,
                    thread_id: r.get(7)?,
                    anchor_snippet: r.get(8)?,
                    pin_x: r.get(9)?,
                    pin_y: r.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// A board's edges, in authored order.
    pub fn canvas_board_edges(&self, board_id: i64) -> Result<Vec<CanvasEdgeRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT from_node, to_node, kind, label, provenance, trust
             FROM canvas_edges WHERE board_id = ?1 ORDER BY ordinal ASC",
        )?;
        let rows = stmt
            .query_map(params![board_id], |r| {
                Ok(CanvasEdgeRow {
                    from_node: r.get(0)?,
                    to_node: r.get(1)?,
                    kind: r.get(2)?,
                    label: r.get(3)?,
                    provenance: r.get(4)?,
                    trust: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// A board's walkthrough steps, in order.
    pub fn canvas_board_steps(&self, board_id: i64) -> Result<Vec<CanvasStepRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT node_id, caption, camera_json FROM canvas_steps \
             WHERE board_id = ?1 ORDER BY ordinal ASC",
        )?;
        let rows = stmt
            .query_map(params![board_id], |r| {
                Ok(CanvasStepRow {
                    node_id: r.get(0)?,
                    caption: r.get(1)?,
                    camera_json: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The idempotent upsert-by-slug behind `POST /api/boards/apply`, in ONE
    /// transaction.
    ///
    /// * A board whose `content_hash` is unchanged is a NO-OP: nothing is
    ///   written, `revision` does not move, `updated_unix` does not move,
    ///   and [`CanvasApplyOutcome::unchanged`] is `true`. That is the whole
    ///   idempotency contract, in one comparison rather than a field-by-field
    ///   diff that could disagree with itself.
    /// * A CHANGED board replaces its children wholesale (the
    ///   `replace_reading_set_spans` shape) and bumps `revision`. Nodes are
    ///   keyed by the author's own id, so a node that survives the re-apply
    ///   keeps its `thread_id` — which is what makes a node thread durable
    ///   across an edit.
    /// * A changed board that had been ACCEPTED or ARCHIVED is reset to the
    ///   status the apply asked for (D21: a human accepted a specific board,
    ///   not a slug), and the outcome says so.
    pub fn apply_canvas_board(
        &self,
        repo_id: i64,
        board: &NewCanvasBoard,
        nodes: &[NewCanvasNode],
        edges: &[NewCanvasEdge],
        steps: &[NewCanvasStep],
        now: i64,
    ) -> Result<CanvasApplyOutcome> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let existing: Option<(i64, String, i64, String)> = tx
            .query_row(
                "SELECT id, content_hash, revision, status FROM canvas_boards
                 WHERE repo_id = ?1 AND slug = ?2 AND kind = ?3",
                params![repo_id, board.slug, board.kind],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        // V74-L3b — the slug space is SHARED across kinds (`canvas_boards`'
        // UNIQUE spans both), so an apply that would collide with the OTHER
        // family must say so rather than fail on a constraint the caller
        // cannot see.
        if existing.is_none() {
            let taken: Option<String> = tx
                .query_row(
                    "SELECT kind FROM canvas_boards WHERE repo_id = ?1 AND slug = ?2",
                    params![repo_id, board.slug],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(other) = taken {
                tx.commit()?;
                return Err(StoreError::SlugTakenByOtherKind {
                    slug: board.slug.clone(),
                    kind: other,
                });
            }
        }

        if let Some((id, hash, revision, status)) = &existing {
            if *hash == board.content_hash {
                tx.commit()?;
                return Ok(CanvasApplyOutcome {
                    board_id: *id,
                    created: false,
                    unchanged: true,
                    revision: *revision,
                    status: status.clone(),
                    status_reset: false,
                });
            }
        }

        // A previously-accepted board whose CONTENT changed goes back to the
        // status the document asked for. Preserving `accepted` here would
        // let a re-apply smuggle unreviewed content under a human's earlier
        // approval — the exact thing D21's pending state exists to prevent.
        let status_reset = existing
            .as_ref()
            .is_some_and(|(_, _, _, s)| s != &board.status);

        let (board_id, created, revision) = match &existing {
            Some((id, _, revision, _)) => {
                let next = revision + 1;
                tx.execute(
                    "UPDATE canvas_boards SET title = ?2, description_md = ?3, status = ?4,
                        authored_ref = ?5, content_hash = ?6, revision = ?7, updated_unix = ?8
                     WHERE id = ?1",
                    params![
                        id,
                        board.title,
                        board.description_md,
                        board.status,
                        board.authored_ref,
                        board.content_hash,
                        next,
                        now
                    ],
                )?;
                tx.execute("DELETE FROM canvas_nodes WHERE board_id = ?1", params![id])?;
                tx.execute("DELETE FROM canvas_edges WHERE board_id = ?1", params![id])?;
                tx.execute("DELETE FROM canvas_steps WHERE board_id = ?1", params![id])?;
                (*id, false, next)
            }
            None => {
                tx.execute(
                    "INSERT INTO canvas_boards
                        (repo_id, slug, title, description_md, status, authored_ref,
                         content_hash, revision, created_unix, updated_unix, kind)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?8, ?9)",
                    params![
                        repo_id,
                        board.slug,
                        board.title,
                        board.description_md,
                        board.status,
                        board.authored_ref,
                        board.content_hash,
                        now,
                        board.kind
                    ],
                )?;
                (tx.last_insert_rowid(), true, 1)
            }
        };

        {
            let mut stmt = tx.prepare(
                "INSERT INTO canvas_nodes
                    (board_id, node_id, ordinal, kind, title, body_md, ref_json, group_id,
                     thread_id, anchor_snippet, pin_x, pin_y)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            for (i, n) in nodes.iter().enumerate() {
                stmt.execute(params![
                    board_id,
                    n.node_id,
                    i as i64,
                    n.kind,
                    n.title,
                    n.body_md,
                    n.ref_json,
                    n.group_id,
                    n.thread_id,
                    n.anchor_snippet,
                    n.pin_x,
                    n.pin_y
                ])?;
            }
        }
        {
            let mut stmt = tx.prepare(
                "INSERT INTO canvas_edges
                    (board_id, ordinal, from_node, to_node, kind, label, provenance, trust)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for (i, e) in edges.iter().enumerate() {
                stmt.execute(params![
                    board_id,
                    i as i64,
                    e.from_node,
                    e.to_node,
                    e.kind,
                    e.label,
                    e.provenance,
                    e.trust
                ])?;
            }
        }
        {
            let mut stmt = tx.prepare(
                "INSERT INTO canvas_steps (board_id, ordinal, node_id, caption, camera_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (i, s) in steps.iter().enumerate() {
                stmt.execute(params![
                    board_id,
                    i as i64,
                    s.node_id,
                    s.caption,
                    s.camera_json
                ])?;
            }
        }
        tx.commit()?;
        Ok(CanvasApplyOutcome {
            board_id,
            created,
            unchanged: false,
            revision,
            status: board.status.clone(),
            status_reset,
        })
    }

    /// The `accept` / `archive` transitions. Bumps `revision` — a status
    /// change IS a change to the board a reader is looking at. Returns the
    /// new row, or `None` when the slug does not exist.
    pub fn set_canvas_board_status(
        &self,
        repo_id: i64,
        kind: &str,
        slug: &str,
        status: &str,
        now: i64,
    ) -> Result<Option<CanvasBoardRow>> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE canvas_boards SET status = ?3, revision = revision + 1, updated_unix = ?4
             WHERE repo_id = ?1 AND slug = ?2 AND kind = ?5",
            params![repo_id, slug, status, now, kind],
        )?;
        if n == 0 {
            tx.commit()?;
            return Ok(None);
        }
        let row = tx
            .query_row(
                "SELECT id, repo_id, slug, title, description_md, status, authored_ref,
                        content_hash, revision, created_unix, updated_unix
                 FROM canvas_boards WHERE repo_id = ?1 AND slug = ?2 AND kind = ?3",
                params![repo_id, slug, kind],
                canvas_board_row_from,
            )
            .optional()?;
        tx.commit()?;
        Ok(row)
    }

    /// Delete a board and its three child tables. The children carry SQL
    /// `ON DELETE CASCADE` (V0036's own header argues why this family takes
    /// the `review_findings` route rather than `delete_reading_set`'s
    /// Rust-owned cascade), so this is one statement — but it still runs
    /// with `foreign_keys = ON`, which `Store::open` sets unconditionally.
    pub fn delete_canvas_board(&self, repo_id: i64, kind: &str, slug: &str) -> Result<bool> {
        let n = self.lock().execute(
            "DELETE FROM canvas_boards WHERE repo_id = ?1 AND slug = ?2 AND kind = ?3",
            params![repo_id, slug, kind],
        )?;
        Ok(n > 0)
    }

    /// V74-L1 — kbc-canvas/1 boards as kbc-seq/1 `board` projections. The
    /// SECOND source for that projection beside [`Self::seq_canvas_sets`];
    /// invariant 14's layer is unchanged (nothing is created, moved or
    /// merged here), and unlike a `canvas_sets` row a board reports a TRUE
    /// node count rather than the honest `null` an opaque payload forces.
    ///
    /// V74-L3b — `projection` is a PARAMETER now, because one table backs
    /// two projections: `canvas_boards.kind` is `board` or `tour` (V0039),
    /// and a tour must appear under kbc-seq/1's `tour` name, not under
    /// `board`. Still a layer: nothing is created, moved or merged, and
    /// `source` still names the table the row physically lives in.
    pub fn seq_canvas_boards(
        &self,
        repo_id: i64,
        kind: &str,
        projection: &str,
    ) -> Result<Vec<SeqProjectionRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT b.slug, b.title, b.updated_unix,
                    (SELECT COUNT(*) FROM canvas_nodes n WHERE n.board_id = b.id)
             FROM canvas_boards b WHERE b.repo_id = ?1 AND b.kind = ?2 ORDER BY b.slug ASC",
        )?;
        let rows = stmt
            .query_map(params![repo_id, kind], |r| {
                Ok(SeqProjectionRow {
                    projection: projection.to_string(),
                    id: r.get(0)?,
                    name: r.get(1)?,
                    size: Some(r.get(3)?),
                    ref_label: None,
                    workspace_id: None,
                    source: "canvas_boards",
                    updated_at: r.get(2)?,
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
}
