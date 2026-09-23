//! Review document revisions.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    /// The newest revision of this review's document at `ps_number`, or
    /// `None` when it has none. "Newest wins" is the whole read rule —
    /// revisions are append-only (migration V0034).
    pub fn latest_review_doc(
        &self,
        review_id: i64,
        ps_number: i64,
    ) -> Result<Option<ReviewDocRow>> {
        self.lock()
            .query_row(
                &format!(
                    "SELECT {REVIEW_DOC_COLUMNS} FROM review_docs
                     WHERE review_id = ?1 AND ps_number = ?2
                     ORDER BY revision DESC LIMIT 1"
                ),
                params![review_id, ps_number],
                review_doc_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Every revision of this review's document, oldest first, across every
    /// patchset. The full record — nothing is ever rewritten, so this is a
    /// real history and not a reconstruction.
    pub fn list_review_docs(&self, review_id: i64) -> Result<Vec<ReviewDocRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {REVIEW_DOC_COLUMNS} FROM review_docs
             WHERE review_id = ?1 ORDER BY ps_number ASC, revision ASC"
        ))?;
        let rows = stmt
            .query_map(params![review_id], review_doc_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// V73-K1 — `kbc-review/1`'s ONE authoring transaction (design D9): the
    /// document revision, findings reconciled by FINGERPRINT (the SAME core
    /// [`Self::reconcile_findings_import`] uses, under
    /// [`FindingIdentity::Fingerprint`] instead of `Slug`), the report, and
    /// an optional review-level verdict — all inside one `BEGIN`/`COMMIT`.
    ///
    /// This is the v0 [`Self::compose_review`] one level up, and it keeps
    /// every one of that method's own guarantees: a failure partway through
    /// rolls back every prior write, so a caller never observes a document
    /// stored with no findings, or findings with no report. The report is
    /// pre-normalised by the caller through the SAME
    /// `reviews::normalize_report_shape` `PUT /report` uses; the verdict
    /// uses the SAME "no-op on an identical (state, note) pair" rule
    /// `set_review_verdict` does, re-implemented against `tx` because
    /// `self.lock()` is a non-reentrant `parking_lot::Mutex`.
    #[allow(clippy::too_many_arguments)]
    pub fn compose_review_doc(
        &self,
        doc: &NewReviewDoc,
        repo_id: i64,
        import_batch_id: &str,
        author: &str,
        findings: &[ImportedFinding],
        mode: FindingsImportMode,
        report_json: &str,
        verdict: Option<(&str, Option<&str>)>,
        now: i64,
    ) -> Result<ComposeDocOutcome> {
        let review_id = doc.review_id;
        let mut conn = self.lock();
        let tx = conn.transaction()?;

        let findings_outcome = reconcile_findings_import_on(
            &tx,
            review_id,
            repo_id,
            doc.ps_number,
            import_batch_id,
            author,
            findings,
            mode,
            FindingIdentity::Fingerprint,
            now,
        )?;

        let revision = insert_review_doc_on(&tx, doc, now)?;

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
                        params![review_id, verdict_state, note, now, doc.ps_number],
                    )?;
                    verdict_changed = true;
                }
            }
        }

        tx.commit()?;
        Ok(ComposeDocOutcome {
            findings: findings_outcome,
            revision,
            report_set: true,
            verdict_changed,
        })
    }
}
