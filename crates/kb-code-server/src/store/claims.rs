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

/// V73-K3 — reads the 14-column order every `claims` SELECT in this file
/// uses. One mapper, so a column added to the table cannot be picked up by
/// one read and missed by another.
fn claim_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ClaimRow> {
    Ok(ClaimRow {
        id: r.get(0)?,
        repo_id: r.get(1)?,
        subject_kind: r.get(2)?,
        subject: r.get(3)?,
        subject_path: r.get(4)?,
        review_id: r.get(5)?,
        kind: r.get(6)?,
        body_md: r.get(7)?,
        confidence: r.get(8)?,
        evidence_json: r.get(9)?,
        session_id: r.get(10)?,
        model: r.get(11)?,
        blob_sha: r.get(12)?,
        created_at: r.get(13)?,
    })
}

/// One `claims` row (V73-K3 / V0035, `kbc-claim/1`). There is deliberately
/// no `trust`/`state` column: the Ladder is computed per request by
/// `crate::claims::ladder_state` from `blob_sha` against the file's live
/// blob — root invariant #2's "kb-code mints classes, nothing is cached",
/// applied to the agent's own prose. `evidence_json` is RAW JSON exactly as
/// stored (a JSON array of kbc refs), the same "row types don't parse other
/// modules' JSON" convention `AnnotationRow` follows.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimRow {
    pub id: String,
    pub repo_id: i64,
    pub subject_kind: String,
    pub subject: String,
    pub subject_path: Option<String>,
    pub review_id: Option<i64>,
    pub kind: String,
    pub body_md: String,
    /// The AGENT'S declared confidence, verbatim. Never a ranking term.
    pub confidence: Option<f64>,
    pub evidence_json: String,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub blob_sha: Option<String>,
    pub created_at: i64,
}

/// The `claims` read filter. Every field is AND-ed; `None` means "no
/// constraint". One struct rather than five query variants so
/// [`Store::count_claims`] and [`Store::list_claims`] cannot drift apart —
/// a `total` computed under a different predicate than the page it
/// describes is the classic paging lie.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimFilter {
    pub repo_id: i64,
    pub subject: Option<String>,
    pub subject_kind: Option<String>,
    pub subject_path: Option<String>,
    pub review_id: Option<i64>,
    pub kind: Option<String>,
}

impl ClaimFilter {
    /// The shared `WHERE` text + its bound values. ONE builder, so the
    /// count and the page are the same predicate by construction.
    fn where_clause(&self) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut sql = String::from("repo_id = ?1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(self.repo_id)];
        let optional: [(&str, Option<Box<dyn rusqlite::ToSql>>); 5] = [
            (
                "subject",
                self.subject
                    .clone()
                    .map(|v| Box::new(v) as Box<dyn rusqlite::ToSql>),
            ),
            (
                "subject_kind",
                self.subject_kind
                    .clone()
                    .map(|v| Box::new(v) as Box<dyn rusqlite::ToSql>),
            ),
            (
                "subject_path",
                self.subject_path
                    .clone()
                    .map(|v| Box::new(v) as Box<dyn rusqlite::ToSql>),
            ),
            (
                "review_id",
                self.review_id
                    .map(|v| Box::new(v) as Box<dyn rusqlite::ToSql>),
            ),
            (
                "kind",
                self.kind
                    .clone()
                    .map(|v| Box::new(v) as Box<dyn rusqlite::ToSql>),
            ),
        ];
        // The placeholder number IS the argument's position, so the two can
        // never drift the way a separately-incremented counter can.
        for (col, value) in optional {
            if let Some(v) = value {
                args.push(v);
                sql.push_str(&format!(" AND {col} = ?{}", args.len()));
            }
        }
        (sql, args)
    }
}
