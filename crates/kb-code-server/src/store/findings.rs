//! Review findings import, disposition, and compose.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
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

    /// V80-M5 — a finding that ADOPTS an existing annotation as its thread,
    /// rather than minting one. A single `INSERT` (no `annotations` row to
    /// write, unlike [`Self::insert_review_finding`]) — the "the human
    /// comment's PEER" contract: same `review_findings` table, `origin =
    /// "manual"`, the caller-supplied `annotation_id` reused verbatim. A
    /// race against another adoption of the SAME annotation (or an ordinary
    /// double-submit) hits `annotation_id`'s own UNIQUE index and surfaces
    /// as [`StoreError::AnnotationAlreadyFinding`] (409), never a raw sqlite
    /// panic — see [`annotation_finding_conflict_or`]. Returns the new
    /// `review_findings.id`.
    pub fn insert_review_finding_adopting(
        &self,
        f: &AdoptedReviewFinding,
        now: i64,
    ) -> Result<i64> {
        insert_review_finding_adopting_on(&self.lock(), f, now)
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
            FindingIdentity::Slug,
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
            FindingIdentity::Slug,
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
