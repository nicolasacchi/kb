//! Trail rows, steps, and retention.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    // --- V74-L3b: kbc-trail/1 -------------------------------------------
    //
    // Every function here is bounded and paged where it could not be.
    // `trails`/`trail_steps` are the only tables in this crate whose rows
    // describe a PERSON rather than a repository, so three of them
    // (`purge_trails`, `sweep_trail_retention_page`, `delete_trail`) exist
    // solely to remove rows, and the one agent-facing read
    // (`aggregate_trail_steps`) groups by day and never selects
    // `entered_at` at all. See `trails`'s module doc for the four
    // structural properties those choices implement.

    /// The persisted opt-in mode, or `None` on a volume that has never
    /// opted in — which every read treats as `off`. The absence of a row
    /// is the absence of a decision (D17: "off on first boot").
    pub fn trails_state(&self) -> Result<Option<(String, i64)>> {
        self.lock()
            .query_row(
                "SELECT mode, changed_unix FROM trails_state WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Set the opt-in mode. The ONLY writer is the loopback-only, audited
    /// `POST /api/trails/state`.
    pub fn set_trails_state(&self, mode: &str, now: i64) -> Result<()> {
        self.lock().execute(
            "INSERT INTO trails_state (id, mode, changed_unix) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET mode = ?1, changed_unix = ?2",
            params![mode, now],
        )?;
        Ok(())
    }

    /// The trail a recorded step belongs to right now: the repo's
    /// unforked trail for `day`, created if it does not exist yet. One
    /// transaction, so two concurrent batches on a day boundary cannot
    /// mint two trails for one day (the partial unique index
    /// `idx_trails_day` is the assertion behind this).
    pub fn current_trail_for_day(
        &self,
        repo_id: i64,
        day: &str,
        session_hint: Option<&str>,
        now: i64,
    ) -> Result<String> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM trails
                 WHERE repo_id = ?1 AND day = ?2 AND parent_id IS NULL",
                params![repo_id, day],
                |r| r.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => id,
            None => {
                let id = crate::trails::new_trail_id();
                tx.execute(
                    "INSERT INTO trails
                        (id, repo_id, origin, title, day, parent_id, parent_ordinal,
                         session_hint, created_unix, updated_unix)
                     VALUES (?1, ?2, ?3, NULL, ?4, NULL, NULL, ?5, ?6, ?6)",
                    params![
                        id,
                        repo_id,
                        crate::trails::ORIGIN_RECORDED,
                        day,
                        session_hint,
                        now
                    ],
                )?;
                id
            }
        };
        tx.commit()?;
        Ok(id)
    }

    /// Append a batch of steps to one trail, in ONE transaction, after the
    /// per-trail cap. Returns `(appended, total_after)`.
    ///
    /// The cap is a REFUSAL, not a truncation: a caller that would cross
    /// [`crate::trails::MAX_STEPS_PER_TRAIL`] gets `NotFound`-free,
    /// explicit feedback from the route with both numbers. This function
    /// reports the total so the route can make that call before writing.
    pub fn append_trail_steps(
        &self,
        trail_id: &str,
        steps: &[NewTrailStep],
        now: i64,
    ) -> Result<(usize, i64)> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let next: i64 = tx.query_row(
            "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM trail_steps WHERE trail_id = ?1",
            params![trail_id],
            |r| r.get(0),
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO trail_steps
                    (trail_id, ordinal, via, path, line_start, line_end, symbol, blob_sha,
                     entered_at, dwell_secs, day, note)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            for (i, s) in steps.iter().enumerate() {
                stmt.execute(params![
                    trail_id,
                    next + i as i64,
                    s.via,
                    s.path,
                    s.line_start,
                    s.line_end,
                    s.symbol,
                    s.blob_sha,
                    s.entered_at,
                    s.dwell_secs,
                    s.day,
                    s.note
                ])?;
            }
        }
        tx.execute(
            "UPDATE trails SET updated_unix = ?2 WHERE id = ?1",
            params![trail_id, now],
        )?;
        tx.commit()?;
        Ok((steps.len(), next + steps.len() as i64))
    }

    /// How many steps a trail already holds — the cap check's input.
    pub fn trail_step_count(&self, trail_id: &str) -> Result<i64> {
        self.lock()
            .query_row(
                "SELECT COUNT(*) FROM trail_steps WHERE trail_id = ?1",
                params![trail_id],
                |r| r.get(0),
            )
            .map_err(Into::into)
    }

    /// Create a trail outright — an AUTHORED one (`POST /api/trails`) or a
    /// FORK (`POST /api/trails/{id}/fork`). Both carry a NULL `day`, which
    /// is what keeps them out of `idx_trails_day`'s one-per-day rule.
    pub fn create_trail(&self, repo_id: i64, t: &NewTrail, now: i64) -> Result<String> {
        let id = crate::trails::new_trail_id();
        self.lock().execute(
            "INSERT INTO trails
                (id, repo_id, origin, title, day, parent_id, parent_ordinal, session_hint,
                 created_unix, updated_unix)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, ?8)",
            params![
                id,
                repo_id,
                t.origin,
                t.title,
                t.parent_id,
                t.parent_ordinal,
                t.session_hint,
                now
            ],
        )?;
        Ok(id)
    }

    /// Trails in one repo, newest first, with their step counts.
    pub fn list_trails(&self, repo_id: i64, limit: usize) -> Result<Vec<TrailSummaryRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT t.id, t.origin, t.title, t.day, t.parent_id, t.parent_ordinal,
                    t.created_unix, t.updated_unix,
                    (SELECT COUNT(*) FROM trail_steps s WHERE s.trail_id = t.id),
                    (SELECT COALESCE(SUM(s.dwell_secs), 0) FROM trail_steps s
                      WHERE s.trail_id = t.id)
             FROM trails t WHERE t.repo_id = ?1
             ORDER BY t.created_unix DESC, t.id ASC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![repo_id, limit as i64], |r| {
                Ok(TrailSummaryRow {
                    id: r.get(0)?,
                    origin: r.get(1)?,
                    title: r.get(2)?,
                    day: r.get(3)?,
                    parent_id: r.get(4)?,
                    parent_ordinal: r.get(5)?,
                    created_unix: r.get(6)?,
                    updated_unix: r.get(7)?,
                    steps: r.get(8)?,
                    dwell_secs: r.get(9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// One trail by id, scoped to a repo so a caller cannot read another
    /// repo's trail by guessing an id.
    pub fn get_trail(&self, repo_id: i64, id: &str) -> Result<Option<TrailSummaryRow>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT t.id, t.origin, t.title, t.day, t.parent_id, t.parent_ordinal,
                    t.created_unix, t.updated_unix,
                    (SELECT COUNT(*) FROM trail_steps s WHERE s.trail_id = t.id),
                    (SELECT COALESCE(SUM(s.dwell_secs), 0) FROM trail_steps s
                      WHERE s.trail_id = t.id)
             FROM trails t WHERE t.repo_id = ?1 AND t.id = ?2",
            params![repo_id, id],
            |r| {
                Ok(TrailSummaryRow {
                    id: r.get(0)?,
                    origin: r.get(1)?,
                    title: r.get(2)?,
                    day: r.get(3)?,
                    parent_id: r.get(4)?,
                    parent_ordinal: r.get(5)?,
                    created_unix: r.get(6)?,
                    updated_unix: r.get(7)?,
                    steps: r.get(8)?,
                    dwell_secs: r.get(9)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// One trail's steps, in order, optionally from an ordinal (the
    /// FORK read: everything from the branch point on).
    pub fn trail_steps(
        &self,
        trail_id: &str,
        from_ordinal: i64,
        limit: usize,
    ) -> Result<Vec<TrailStepRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT ordinal, via, path, line_start, line_end, symbol, blob_sha,
                    entered_at, dwell_secs, day, note
             FROM trail_steps WHERE trail_id = ?1 AND ordinal >= ?2
             ORDER BY ordinal ASC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![trail_id, from_ordinal, limit as i64], |r| {
                Ok(TrailStepRow {
                    ordinal: r.get(0)?,
                    via: r.get(1)?,
                    path: r.get(2)?,
                    line_start: r.get(3)?,
                    line_end: r.get(4)?,
                    symbol: r.get(5)?,
                    blob_sha: r.get(6)?,
                    entered_at: r.get(7)?,
                    dwell_secs: r.get(8)?,
                    day: r.get(9)?,
                    note: r.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The ONLY agent-facing read: counts per `(path, symbol)` over a DAY
    /// window. `entered_at` is never selected, never grouped on and never
    /// returned — D17's "no ordering below the day", as a query rather than
    /// a promise. `since`/`until` are DAY strings (`YYYY-MM-DD`) for the
    /// same reason.
    pub fn aggregate_trail_steps(
        &self,
        repo_id: i64,
        since: Option<&str>,
        until: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TrailAggregateRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT s.path, s.symbol, COUNT(*), COALESCE(SUM(s.dwell_secs), 0),
                    COUNT(DISTINCT s.day), MIN(s.day), MAX(s.day)
             FROM trail_steps s
             JOIN trails t ON t.id = s.trail_id
             WHERE t.repo_id = ?1
               AND (?2 IS NULL OR s.day >= ?2)
               AND (?3 IS NULL OR s.day <= ?3)
             GROUP BY s.path, s.symbol
             ORDER BY COUNT(*) DESC, s.path ASC, s.symbol ASC
             LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![repo_id, since, until, limit as i64], |r| {
                Ok(TrailAggregateRow {
                    path: r.get(0)?,
                    symbol: r.get(1)?,
                    steps: r.get(2)?,
                    dwell_secs: r.get(3)?,
                    days: r.get(4)?,
                    first_day: r.get(5)?,
                    last_day: r.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// WHOLESALE purge, in one transaction: every trail in a repo (or
    /// every one created strictly before `before_unix`). Returns
    /// `(trails, steps)` removed.
    ///
    /// `annotations` rows carrying a `trail_id` are deliberately LEFT
    /// ALONE — see `trails`'s module doc and invariant 23(a): a dissent
    /// note is the human's own authored words, not derived movement data.
    pub fn purge_trails(&self, repo_id: i64, before_unix: Option<i64>) -> Result<(usize, usize)> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let steps: i64 = tx.query_row(
            "SELECT COUNT(*) FROM trail_steps s JOIN trails t ON t.id = s.trail_id
             WHERE t.repo_id = ?1 AND (?2 IS NULL OR t.created_unix < ?2)",
            params![repo_id, before_unix],
            |r| r.get(0),
        )?;
        // The child rows go first and explicitly: `trail_steps`' FK carries
        // ON DELETE CASCADE, but deleting them by the same predicate makes
        // the count above and the rows removed provably the same set.
        tx.execute(
            "DELETE FROM trail_steps WHERE trail_id IN
                (SELECT id FROM trails WHERE repo_id = ?1 AND (?2 IS NULL OR created_unix < ?2))",
            params![repo_id, before_unix],
        )?;
        let trails = tx.execute(
            "DELETE FROM trails WHERE repo_id = ?1 AND (?2 IS NULL OR created_unix < ?2)",
            params![repo_id, before_unix],
        )?;
        tx.commit()?;
        Ok((trails, steps as usize))
    }

    /// Delete ONE trail and its steps.
    pub fn delete_trail(&self, repo_id: i64, id: &str) -> Result<bool> {
        let n = self.lock().execute(
            "DELETE FROM trails WHERE repo_id = ?1 AND id = ?2",
            params![repo_id, id],
        )?;
        Ok(n > 0)
    }

    /// ONE PAGE of the retention sweep: up to `page` trails older than
    /// `cutoff_unix`, with their steps, in one short transaction. Returns
    /// `((trails, steps), more)` — the `sweep_lane_retention_page` shape,
    /// and for the same V72-B0 reason: this store has ONE connection
    /// mutex, so a background pass must never hold it for an unbounded
    /// stretch.
    pub fn sweep_trail_retention_page(
        &self,
        cutoff_unix: i64,
        page: usize,
    ) -> Result<((usize, usize), bool)> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let ids: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT id FROM trails WHERE created_unix < ?1 ORDER BY created_unix ASC LIMIT ?2",
            )?;
            let v = stmt
                .query_map(params![cutoff_unix, page as i64], |r| r.get(0))?
                .collect::<std::result::Result<Vec<String>, _>>()?;
            v
        };
        if ids.is_empty() {
            tx.commit()?;
            return Ok(((0, 0), false));
        }
        let mut steps = 0usize;
        let mut trails = 0usize;
        for id in &ids {
            steps += tx.execute("DELETE FROM trail_steps WHERE trail_id = ?1", params![id])?;
            trails += tx.execute("DELETE FROM trails WHERE id = ?1", params![id])?;
        }
        tx.commit()?;
        let more = ids.len() == page;
        Ok(((trails, steps), more))
    }

    /// A trail's DISSENT notes — `annotations` rows carrying its id, the
    /// drain behind `kb-code trail notes`. Parents and replies alike carry
    /// the same `trail_id` (`routes::inherit_scope_field`), which is what
    /// keeps this one predicate rather than a two-step.
    pub fn trail_notes(&self, trail_id: &str, limit: usize) -> Result<Vec<TrailNoteRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, parent_id, author, intent, body, path, resolved, created_at
             FROM annotations WHERE trail_id = ?1
             ORDER BY created_at ASC, id ASC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![trail_id, limit as i64], |r| {
                Ok(TrailNoteRow {
                    id: r.get(0)?,
                    parent_id: r.get(1)?,
                    author: r.get(2)?,
                    intent: r.get(3)?,
                    body: r.get(4)?,
                    path: r.get(5)?,
                    resolved: r.get::<_, i64>(6)? != 0,
                    created_at: r.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
