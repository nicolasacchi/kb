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

    /// v0.44 F9b — what a human already decided about the SAME paths in
    /// OTHER reviews of `repo`: the unsuperseded findings with disposition
    /// `dispute` or `waive` located on one of `paths`, excluding
    /// `exclude_review_id`. Exact path match (a renamed file does not
    /// carry); ordered `(path, review_id, slug)`. Chunked so a large change
    /// set never nears SQLite's bound-variable limit. The `review context`
    /// bundle's "other reviews" section reads it; surfaced, never scored.
    pub fn other_review_judgements(
        &self,
        repo: &str,
        exclude_review_id: i64,
        paths: &[String],
    ) -> Result<Vec<OtherReviewJudgement>> {
        let mut out: Vec<OtherReviewJudgement> = Vec::new();
        let conn = self.lock();
        for chunk in paths.chunks(400) {
            let placeholders = (0..chunk.len())
                .map(|i| format!("?{}", i + 3))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT f.review_id, r.state, f.slug, f.severity, f.title,
                        f.location_path, f.disposition, f.disposition_note,
                        f.disposition_by, f.disposition_at
                 FROM review_findings f JOIN reviews r ON r.id = f.review_id
                 WHERE r.repo = ?1 AND f.review_id != ?2 AND f.superseded = 0
                   AND f.disposition IN ('dispute', 'waive')
                   AND f.location_path IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut binds: Vec<rusqlite::types::Value> = Vec::with_capacity(2 + chunk.len());
            binds.push(rusqlite::types::Value::Text(repo.to_string()));
            binds.push(rusqlite::types::Value::Integer(exclude_review_id));
            for p in chunk {
                binds.push(rusqlite::types::Value::Text(p.clone()));
            }
            let rows = stmt
                .query_map(rusqlite::params_from_iter(binds), |r| {
                    Ok(OtherReviewJudgement {
                        review_id: r.get(0)?,
                        review_state: r.get(1)?,
                        slug: r.get(2)?,
                        severity: r.get(3)?,
                        title: r.get(4)?,
                        path: r.get(5)?,
                        disposition: r.get(6)?,
                        note: r.get(7)?,
                        by: r.get(8)?,
                        at: r.get(9)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            out.extend(rows);
        }
        out.sort_by(|a, b| (&a.path, a.review_id, &a.slug).cmp(&(&b.path, b.review_id, &b.slug)));
        Ok(out)
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

/// Reads the 31-column order every `review_findings` SELECT in this file uses
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
pub(super) fn reconcile_findings_import_on(
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

/// One finding from ANOTHER review that a human disputed or waived, on a
/// path the current review also changes (`Store::other_review_judgements`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherReviewJudgement {
    pub review_id: i64,
    pub review_state: String,
    pub slug: String,
    pub severity: String,
    pub title: String,
    pub path: String,
    pub disposition: String,
    pub note: Option<String>,
    pub by: Option<String>,
    pub at: Option<i64>,
}
