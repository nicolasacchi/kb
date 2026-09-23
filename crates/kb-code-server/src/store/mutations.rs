//! Mutation ledger.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    // --- V70-A2 (SEC-20) — the append-only mutations audit ledger --------
    //
    // Deliberately the LAST two methods on this impl, and deliberately
    // only two: the ledger is written by exactly one middleware
    // (`security::audit::audit_mutations`) and read by exactly one route
    // (`GET /api/audit`). There is no update, no delete and no prune —
    // see `migrations/V0027__mutations_audit.sql`'s header for why
    // "append-only" is a contract rather than a trigger.

    /// Append one audited mutation. Never bumps `generation`: the ledger
    /// is not row-set state any search cache reads (the `UpsertChunks`
    /// precedent kb's invariant #2 records, applied here).
    pub fn insert_mutation(&self, m: &MutationIn) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO mutations
                 (ts_unix, route, method, admission, repo, target,
                  blob_before, blob_after, request_id, outcome)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                m.ts_unix,
                m.route,
                m.method,
                m.admission,
                m.repo,
                m.target,
                m.blob_before,
                m.blob_after,
                m.request_id,
                m.outcome,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// `GET /api/audit?since=&limit=` — newest first, `ts_unix >= since`,
    /// at most `limit` rows. The caller clamps `limit` (500 hard cap);
    /// this method takes whatever it is given so the clamp lives in ONE
    /// place (the route) rather than two that could drift.
    pub fn list_mutations(&self, since_unix: i64, limit: usize) -> Result<Vec<MutationRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, ts_unix, route, method, admission, repo, target,
                    blob_before, blob_after, request_id, outcome
             FROM mutations
             WHERE ts_unix >= ?1
             ORDER BY ts_unix DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![since_unix, limit as i64], |r| {
                Ok(MutationRow {
                    id: r.get(0)?,
                    ts_unix: r.get(1)?,
                    route: r.get(2)?,
                    method: r.get(3)?,
                    admission: r.get(4)?,
                    repo: r.get(5)?,
                    target: r.get(6)?,
                    blob_before: r.get(7)?,
                    blob_after: r.get(8)?,
                    request_id: r.get(9)?,
                    outcome: r.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
