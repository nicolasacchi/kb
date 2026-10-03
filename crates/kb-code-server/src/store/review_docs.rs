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

/// One `review_docs` revision, as read back. `doc_md` is the WHOLE
/// document (front matter + body) byte-for-byte as it was composed — the
/// lossless record; every other column is a denormalised copy of a parsed
/// front-matter field and the document itself wins on a disagreement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDocRow {
    pub id: i64,
    pub review_id: i64,
    pub ps_number: i64,
    pub revision: i64,
    pub schema: String,
    pub tier: String,
    pub doc_md: String,
    pub summary_md: String,
    pub risk_level: Option<String>,
    pub risk_why: Option<String>,
    /// Raw JSON array (same "row types don't parse other modules' JSON"
    /// convention `AnnotationRow`/`ReviewFindingRow` already follow).
    pub omitted_json: String,
    pub author_json: Option<String>,
    pub byte_len: i64,
    pub created_at: i64,
}

/// A revision ready to append. `revision` is assigned by the store (the
/// caller never picks one), so two concurrent composes cannot both claim
/// the same number — the UNIQUE index would reject the second anyway, and
/// this way it never gets that far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReviewDoc {
    pub review_id: i64,
    pub ps_number: i64,
    pub schema: String,
    pub tier: String,
    pub doc_md: String,
    pub summary_md: String,
    pub risk_level: Option<String>,
    pub risk_why: Option<String>,
    pub omitted_json: String,
    pub author_json: Option<String>,
}

const REVIEW_DOC_COLUMNS: &str = "id, review_id, ps_number, revision, schema, tier, doc_md,
    summary_md, risk_level, risk_why, omitted_json, author_json, byte_len, created_at";

fn review_doc_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewDocRow> {
    Ok(ReviewDocRow {
        id: r.get(0)?,
        review_id: r.get(1)?,
        ps_number: r.get(2)?,
        revision: r.get(3)?,
        schema: r.get(4)?,
        tier: r.get(5)?,
        doc_md: r.get(6)?,
        summary_md: r.get(7)?,
        risk_level: r.get(8)?,
        risk_why: r.get(9)?,
        omitted_json: r.get(10)?,
        author_json: r.get(11)?,
        byte_len: r.get(12)?,
        created_at: r.get(13)?,
    })
}

/// Append one revision on an already-open transaction. Returns the
/// revision number it was given.
fn insert_review_doc_on(tx: &Transaction<'_>, d: &NewReviewDoc, now: i64) -> Result<i64> {
    let prev: i64 = tx.query_row(
        "SELECT COALESCE(MAX(revision), 0) FROM review_docs
         WHERE review_id = ?1 AND ps_number = ?2",
        params![d.review_id, d.ps_number],
        |r| r.get(0),
    )?;
    let revision = prev + 1;
    tx.execute(
        "INSERT INTO review_docs
            (review_id, ps_number, revision, schema, tier, doc_md, summary_md,
             risk_level, risk_why, omitted_json, author_json, byte_len, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            d.review_id,
            d.ps_number,
            revision,
            d.schema,
            d.tier,
            d.doc_md,
            d.summary_md,
            d.risk_level,
            d.risk_why,
            d.omitted_json,
            d.author_json,
            d.doc_md.len() as i64,
            now,
        ],
    )?;
    Ok(revision)
}

/// [`Store::compose_review_doc`]'s result — one field per write the
/// transaction performed.
#[derive(Debug, Clone)]
pub struct ComposeDocOutcome {
    pub findings: FindingsImportOutcome,
    pub revision: i64,
    pub report_set: bool,
    pub verdict_changed: bool,
}
