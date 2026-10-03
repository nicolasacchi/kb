//! SCIP ingest run ledger.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    /// Append one `scip_runs` row — called unconditionally on every
    /// successful (HTTP 200) `POST /api/scip/ingest`, regardless of
    /// `docs_accepted` (even an ingest that accepted zero documents — every
    /// doc stale/untracked/unsupported-lang — still honestly records "an
    /// ingest ran at this HEAD"; `ScipStatus::docs_covered` separately
    /// surfaces the zero, so `fresh = true` with `docs_covered = 0` is not
    /// misleading). Does NOT bump the store generation (mirrors
    /// `doc_lens_pins`' precedent, `pin_writes_do_not_bump_the_store_
    /// generation`): `scip_runs` feeds only `GET /api/repos`'s `ScipStatus`,
    /// never `FileIndex`/`SymbolIndex`'s generation-keyed search caches.
    pub fn record_scip_run(
        &self,
        repo_id: i64,
        head_sha: &str,
        ingested_at: i64,
        docs_accepted: i64,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO scip_runs (repo_id, head_sha, ingested_at, docs_accepted)
             VALUES (?1, ?2, ?3, ?4)",
            params![repo_id, head_sha, ingested_at, docs_accepted],
        )?;
        Ok(())
    }

    /// The most recent `scip_runs` row for `repo_id`, or `None` if this repo
    /// has never had a successful SCIP ingest — `ScipStatus`'s
    /// "never-ingested" case. Ties on `ingested_at` (a same-second double
    /// ingest) break toward the row inserted LAST (`rowid DESC` as the
    /// tiebreak) rather than an arbitrary one.
    pub fn latest_scip_run(&self, repo_id: i64) -> Result<Option<ScipRunRow>> {
        self.lock()
            .query_row(
                "SELECT head_sha, ingested_at, docs_accepted FROM scip_runs
                 WHERE repo_id = ?1 ORDER BY ingested_at DESC, rowid DESC LIMIT 1",
                params![repo_id],
                |r| {
                    Ok(ScipRunRow {
                        head_sha: r.get(0)?,
                        ingested_at: r.get(1)?,
                        docs_accepted: r.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }
}

// ── PRR-N12: scip runs ──────────────────────────────────────────────────

/// PRR-N12 (N1) — one `scip_runs` row: the outcome of one successful
/// `POST /api/scip/ingest` call, stamped by `crate::scip::scip_ingest_route`
/// (see migration V0025's doc). `head_sha` is the repo's git HEAD AT THE
/// MOMENT of that ingest call — compared against the repo's CURRENT HEAD by
/// `routes::repos`'s `ScipStatus::fresh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScipRunRow {
    pub head_sha: String,
    pub ingested_at: i64,
    pub docs_accepted: i64,
}

// ── end PRR-N12 ──────────────────────────────────────────────────────────
