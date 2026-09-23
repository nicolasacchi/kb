//! Claim rows.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    // --- claims (V73-K3, kbc-claim/1, V0035) -----------------------------

    /// Append one claim. Claims are never UPDATEd: the register is a
    /// record of what an agent said at a moment, and rewriting one would
    /// destroy the thing a human formed an opinion against (the same
    /// append-only posture `review_docs` states for its revisions).
    pub fn insert_claim(&self, row: &ClaimRow) -> Result<()> {
        self.lock().execute(
            "INSERT INTO claims (id, repo_id, subject_kind, subject, subject_path, review_id,
                                 kind, body_md, confidence, evidence_json, session_id, model,
                                 blob_sha, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                row.id,
                row.repo_id,
                row.subject_kind,
                row.subject,
                row.subject_path,
                row.review_id,
                row.kind,
                row.body_md,
                row.confidence,
                row.evidence_json,
                row.session_id,
                row.model,
                row.blob_sha,
                row.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_claim(&self, id: &str) -> Result<Option<ClaimRow>> {
        self.lock()
            .query_row(
                "SELECT id, repo_id, subject_kind, subject, subject_path, review_id, kind,
                        body_md, confidence, evidence_json, session_id, model, blob_sha,
                        created_at
                 FROM claims WHERE id = ?1",
                params![id],
                claim_row_from,
            )
            .optional()
            .map_err(Into::into)
    }

    /// The true count under `filter`, BEFORE paging — so a page can report
    /// what it is a page OF.
    pub fn count_claims(&self, filter: &ClaimFilter) -> Result<usize> {
        let (where_sql, args) = filter.where_clause();
        let conn = self.lock();
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let n: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM claims WHERE {where_sql}"),
            refs.as_slice(),
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// One page of claims under `filter`, NEWEST first (a claim register is
    /// read as "what has been said lately"), with `id` as the deterministic
    /// tiebreak so two claims stamped in the same second never swap places
    /// between two reads of the same page.
    pub fn list_claims(
        &self,
        filter: &ClaimFilter,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ClaimRow>> {
        let (where_sql, mut args) = filter.where_clause();
        args.push(Box::new(limit as i64));
        args.push(Box::new(offset as i64));
        let n = args.len();
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT id, repo_id, subject_kind, subject, subject_path, review_id, kind,
                    body_md, confidence, evidence_json, session_id, model, blob_sha, created_at
             FROM claims WHERE {where_sql}
             ORDER BY created_at DESC, id DESC
             LIMIT ?{} OFFSET ?{}",
            n - 1,
            n
        ))?;
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt
            .query_map(refs.as_slice(), claim_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
