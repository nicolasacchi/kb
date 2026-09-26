//! Finding disposition analytics.
//!
//! Moved out of the store monolith. `Store`'s private connection and
//! the helpers it shares stay in the parent module; this child can
//! call them. Public paths stay `crate::store`.
use super::*;

impl Store {
    /// Every [`AnalyticsFindingRow`] (superseded INCLUDED — see the note
    /// below) joined to `reviews` for `repo` (or every repo when `None`),
    /// optionally windowed to `rf.created_at` in `[from, to]` (either bound
    /// optional). Oldest-first — `review_analytics`'s pure aggregation is
    /// order-independent, but a stable input order keeps its own test
    /// fixtures readable.
    ///
    /// `superseded` rows are DELIBERATELY still included here (unlike
    /// [`Self::list_review_findings`]'s default) — design-addendum-2 §C:
    /// "non-superseded by default, superseded reported separately." The
    /// caller (`review_analytics::compute_analytics`) does the split so
    /// the superseded count itself is a named, surfaced term rather than a
    /// silently dropped row.
    pub fn list_findings_for_analytics(
        &self,
        repo: Option<&str>,
        from: Option<i64>,
        to: Option<i64>,
    ) -> Result<Vec<AnalyticsFindingRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT rf.review_id, rf.severity, rf.category, rf.location_path,
                    rf.disposition, rf.disposition_at, rf.published_state,
                    rf.superseded, rf.created_at
             FROM review_findings rf
             JOIN reviews r ON r.id = rf.review_id
             WHERE (?1 IS NULL OR r.repo = ?1)
               AND (?2 IS NULL OR rf.created_at >= ?2)
               AND (?3 IS NULL OR rf.created_at <= ?3)
             ORDER BY rf.created_at ASC, rf.id ASC",
        )?;
        let rows = stmt
            .query_map(params![repo, from, to], analytics_finding_row_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// `(category, location_path)` pairs seen in `>= min_reviews` DISTINCT
    /// non-superseded findings' reviews, same `repo`/`from`/`to` filter as
    /// [`Self::list_findings_for_analytics`]. Descending by `review_count`,
    /// tied pairs broken by `category` then `location_path` ascending — a
    /// full, deterministic order (mirrors `sort_inbox_rows`'s own
    /// "same state -> byte-identical order" contract).
    pub fn recurrence_pairs(
        &self,
        repo: Option<&str>,
        from: Option<i64>,
        to: Option<i64>,
        min_reviews: i64,
    ) -> Result<Vec<RecurrenceRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT rf.category, rf.location_path,
                    COUNT(DISTINCT rf.review_id) AS review_count,
                    COUNT(*) AS finding_count,
                    GROUP_CONCAT(DISTINCT rf.review_id) AS review_ids
             FROM review_findings rf
             JOIN reviews r ON r.id = rf.review_id
             WHERE rf.superseded = 0
               AND (?1 IS NULL OR r.repo = ?1)
               AND (?2 IS NULL OR rf.created_at >= ?2)
               AND (?3 IS NULL OR rf.created_at <= ?3)
             GROUP BY rf.category, rf.location_path
             HAVING COUNT(DISTINCT rf.review_id) >= ?4
             ORDER BY review_count DESC, rf.category ASC, rf.location_path ASC",
        )?;
        let rows = stmt
            .query_map(params![repo, from, to, min_reviews], |r| {
                let ids_str: String = r.get(4)?;
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    ids_str,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (category, location_path, review_count, finding_count, ids_str) in rows {
            let mut review_ids: Vec<i64> = ids_str
                .split(',')
                .filter_map(|s| s.parse::<i64>().ok())
                .collect();
            review_ids.sort_unstable();
            out.push(RecurrenceRow {
                category,
                location_path,
                review_count,
                finding_count,
                review_ids,
            });
        }
        Ok(out)
    }
}
