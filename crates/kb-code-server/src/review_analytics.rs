//! PRR-R9 ("The PR Room," kb v0.39 T2, design-addendum-2 §C) — `GET
//! /api/reviews/analytics?repo=&from=&to=`: the disposition CALIBRATION
//! instrument. Deterministic, decomposed, never a quality verdict
//! (behavioral law, same posture as `behavioral::risk`/`review_inbox`'s own
//! scoring) — this route only counts and buckets what a human already
//! decided; it never itself judges a finding.
//!
//! # Route
//!
//! **Bearer** (read-only, same ordinary review-read gate as `/findings`/
//! `/inbox`): `GET /api/reviews/analytics` ([`analytics_route`]).
//!
//! # Aggregation surface
//!
//! Every term is named and always present (never a fabricated zero for
//! "unknown" — see [`AcceptanceRow::rate`] and [`LatencyOut`]'s own docs).
//! The data source is [`crate::store::Store::list_findings_for_analytics`]
//! (joins `review_findings` to `reviews` for the optional `repo` filter,
//! optionally windowed on `rf.created_at`) — **non-superseded findings
//! only feed every aggregate below**; [`AnalyticsCore::superseded_count`]
//! reports the excluded count separately (addendum §C: "non-superseded by
//! default, superseded reported separately") rather than silently dropping
//! the number.
//!
//! - `by_severity_disposition` — every `(severity, disposition)` cell,
//!   INCLUDING zero-count ones (same "report every variant even at zero"
//!   convention as `provenance::report::confidence_buckets`) — a
//!   severity×disposition combination that never occurred is a fact worth
//!   seeing, not an absent row.
//! - `acceptance` — per severity: `accepted = agree + fix-later`,
//!   `rejected = dispute`, `risk_accepted = waive`, `undecided = NULL
//!   disposition`. `rate = accepted / (accepted + rejected + risk_accepted)`
//!   — the DECIDED total only (undecided findings haven't been judged yet,
//!   so folding them into the denominator would understate a review still
//!   mid-triage) — `None` when that decided total is zero, never a
//!   fabricated `0.0` (design-addendum-2 §C, verbatim: "rate null when
//!   denominator 0"). **Judgment call, flagged**: the addendum doesn't spell
//!   out the denominator; "decided-only" is this unit's own reading —
//!   correct if a different one was intended.
//! - `by_category` — top 15 by finding count (ties broken by category name
//!   ascending, a full deterministic order), same disposition split as
//!   `acceptance`.
//! - `weekly` — ISO-week buckets (`crate::behavioral::iso_week_start_unix`,
//!   the SAME pure week-start fn `behavioral::bucket_timeseries` already
//!   uses — no second week-math implementation) of `created` (by
//!   `created_at`), `disposed` and `disputed` (both by `disposition_at`,
//!   `disputed` narrowed to `disposition = "dispute"`). No zero-fill — a
//!   week appears only when at least one of the three counts is nonzero,
//!   same convention as `bucket_timeseries`.
//! - `latency` — median + p90 seconds `disposition_at - created_at`,
//!   disposed rows only (`disposition_at.is_some()`). Nearest-rank
//!   percentile (see [`percentile`]'s own doc) — deterministic, no
//!   interpolation. `None`/`None` when zero disposed rows (`n == 0`), never
//!   a fabricated `0`.
//! - `publish` — `published_state = "published"` vs `"unpublished"` counts.
//! - `recurrence` — `(category, location_path)` pairs seen in
//!   `>= store::RECURRENCE_MIN_REVIEWS` distinct reviews
//!   ([`crate::store::Store::recurrence_pairs`], its OWN doc: this is also
//!   the frontier recurring-finding query — one shared store fn, two
//!   consumers, per addendum §C).

use crate::behavioral::iso_week_start_unix;
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{self, AnalyticsFindingRow, StoreBlocking};
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "review-analytics/1";

/// A finding's disposition is `NULL` in storage for "not yet decided" —
/// this label is how that state is SURFACED on the wire (never itself
/// written to `review_findings.disposition`).
pub const DISPOSITION_UNDECIDED: &str = "undecided";

/// Display order for the matrix/category splits — `store::DISPOSITIONS`'
/// own order, then `undecided` last (it isn't a real column value).
fn disposition_labels() -> [&'static str; 5] {
    [
        store::DISPOSITION_AGREE,
        store::DISPOSITION_DISPUTE,
        store::DISPOSITION_WAIVE,
        store::DISPOSITION_FIX_LATER,
        DISPOSITION_UNDECIDED,
    ]
}

fn disposition_label(d: Option<&str>) -> &str {
    d.unwrap_or(DISPOSITION_UNDECIDED)
}

#[derive(Debug, Deserialize, Default)]
pub struct AnalyticsParams {
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub from: Option<i64>,
    #[serde(default)]
    pub to: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeverityDispositionCell {
    pub severity: String,
    pub disposition: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AcceptanceRow {
    pub severity: String,
    pub accepted: i64,
    pub rejected: i64,
    pub risk_accepted: i64,
    pub undecided: i64,
    pub total: i64,
    /// See the module doc's "acceptance" section for exactly what this
    /// divides by, and why. `None` — never `Some(0.0)` — when nothing has
    /// been decided yet.
    pub rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CategoryRow {
    pub category: String,
    pub count: i64,
    pub accepted: i64,
    pub rejected: i64,
    pub risk_accepted: i64,
    pub undecided: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WeekBucket {
    pub week_start_unix: i64,
    pub created: i64,
    pub disposed: i64,
    pub disputed: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LatencyOut {
    pub n: i64,
    pub median_secs: Option<i64>,
    pub p90_secs: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PublishOut {
    pub published: i64,
    pub unpublished: i64,
}

/// Every aggregate except `recurrence` (which is its own store query, not
/// derived from this row set — see the module doc). Pure over an already-
/// fetched row set, so it's unit-testable without a DB or HTTP round trip
/// (mirrors `review_inbox::sort_inbox_rows`'s own "pure core, thin route"
/// shape).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnalyticsCore {
    pub total_findings: i64,
    pub superseded_count: i64,
    pub by_severity_disposition: Vec<SeverityDispositionCell>,
    pub acceptance: Vec<AcceptanceRow>,
    pub by_category: Vec<CategoryRow>,
    pub weekly: Vec<WeekBucket>,
    pub latency: LatencyOut,
    pub publish: PublishOut,
}

/// Nearest-rank percentile over an ASCENDING-sorted slice — the `ceil(p *
/// n)`-th smallest (1-indexed, clamped into range). Deterministic, no
/// interpolation between two elements (so the reported value is always
/// one that actually occurred). `None` on an empty slice — never a
/// fabricated `0`.
fn percentile(sorted_ascending: &[i64], p: f64) -> Option<i64> {
    if sorted_ascending.is_empty() {
        return None;
    }
    let n = sorted_ascending.len();
    let rank = ((p * n as f64).ceil() as usize).clamp(1, n);
    Some(sorted_ascending[rank - 1])
}

/// The addendum §C computation, pure. `rows` may include superseded
/// findings — this fn does the split (see [`AnalyticsCore::
/// superseded_count`]) rather than requiring the caller to pre-filter, so
/// a test fixture can hand in a mixed set directly.
pub(crate) fn compute_analytics(rows: &[AnalyticsFindingRow]) -> AnalyticsCore {
    let superseded_count = rows.iter().filter(|r| r.superseded).count() as i64;
    let active: Vec<&AnalyticsFindingRow> = rows.iter().filter(|r| !r.superseded).collect();
    let total_findings = active.len() as i64;

    // --- by_severity_disposition — every cell, zero included -------------
    let mut matrix_counts: BTreeMap<(&str, &str), i64> = BTreeMap::new();
    for sev in store::SEVERITIES {
        for disp in disposition_labels() {
            matrix_counts.insert((sev, disp), 0);
        }
    }
    for r in &active {
        *matrix_counts
            .entry((
                r.severity.as_str(),
                disposition_label(r.disposition.as_deref()),
            ))
            .or_insert(0) += 1;
    }
    let mut by_severity_disposition = Vec::with_capacity(store::SEVERITIES.len() * 5);
    for sev in store::SEVERITIES {
        for disp in disposition_labels() {
            let count = *matrix_counts.get(&(sev, disp)).unwrap_or(&0);
            by_severity_disposition.push(SeverityDispositionCell {
                severity: sev.to_string(),
                disposition: disp.to_string(),
                count,
            });
        }
    }

    // --- acceptance --------------------------------------------------------
    let mut acceptance = Vec::with_capacity(store::SEVERITIES.len());
    for sev in store::SEVERITIES {
        let (mut accepted, mut rejected, mut risk_accepted, mut undecided) =
            (0i64, 0i64, 0i64, 0i64);
        for r in active.iter().filter(|r| r.severity == sev) {
            match r.disposition.as_deref() {
                Some(d) if d == store::DISPOSITION_AGREE || d == store::DISPOSITION_FIX_LATER => {
                    accepted += 1
                }
                Some(store::DISPOSITION_DISPUTE) => rejected += 1,
                Some(store::DISPOSITION_WAIVE) => risk_accepted += 1,
                _ => undecided += 1,
            }
        }
        let total = accepted + rejected + risk_accepted + undecided;
        let decided = accepted + rejected + risk_accepted;
        let rate = if decided > 0 {
            Some(accepted as f64 / decided as f64)
        } else {
            None
        };
        acceptance.push(AcceptanceRow {
            severity: sev.to_string(),
            accepted,
            rejected,
            risk_accepted,
            undecided,
            total,
            rate,
        });
    }

    // --- by_category (top 15) ----------------------------------------------
    let mut cat_map: BTreeMap<String, (i64, i64, i64, i64, i64)> = BTreeMap::new();
    for r in &active {
        let e = cat_map.entry(r.category.clone()).or_insert((0, 0, 0, 0, 0));
        e.0 += 1;
        match r.disposition.as_deref() {
            Some(d) if d == store::DISPOSITION_AGREE || d == store::DISPOSITION_FIX_LATER => {
                e.1 += 1
            }
            Some(store::DISPOSITION_DISPUTE) => e.2 += 1,
            Some(store::DISPOSITION_WAIVE) => e.3 += 1,
            _ => e.4 += 1,
        }
    }
    let mut by_category: Vec<CategoryRow> = cat_map
        .into_iter()
        .map(
            |(category, (count, accepted, rejected, risk_accepted, undecided))| CategoryRow {
                category,
                count,
                accepted,
                rejected,
                risk_accepted,
                undecided,
            },
        )
        .collect();
    by_category.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.category.cmp(&b.category))
    });
    by_category.truncate(15);

    // --- weekly --------------------------------------------------------------
    let mut created_by_week: BTreeMap<i64, i64> = BTreeMap::new();
    let mut disposed_by_week: BTreeMap<i64, i64> = BTreeMap::new();
    let mut disputed_by_week: BTreeMap<i64, i64> = BTreeMap::new();
    for r in &active {
        *created_by_week
            .entry(iso_week_start_unix(r.created_at))
            .or_insert(0) += 1;
        if let Some(disposed_at) = r.disposition_at {
            let week = iso_week_start_unix(disposed_at);
            *disposed_by_week.entry(week).or_insert(0) += 1;
            if r.disposition.as_deref() == Some(store::DISPOSITION_DISPUTE) {
                *disputed_by_week.entry(week).or_insert(0) += 1;
            }
        }
    }
    let mut weeks: BTreeSet<i64> = BTreeSet::new();
    weeks.extend(created_by_week.keys());
    weeks.extend(disposed_by_week.keys());
    weeks.extend(disputed_by_week.keys());
    let weekly = weeks
        .into_iter()
        .map(|w| WeekBucket {
            week_start_unix: w,
            created: *created_by_week.get(&w).unwrap_or(&0),
            disposed: *disposed_by_week.get(&w).unwrap_or(&0),
            disputed: *disputed_by_week.get(&w).unwrap_or(&0),
        })
        .collect();

    // --- latency -------------------------------------------------------------
    let mut latencies: Vec<i64> = active
        .iter()
        .filter_map(|r| r.disposition_at.map(|d| d - r.created_at))
        .collect();
    latencies.sort_unstable();
    let latency = LatencyOut {
        n: latencies.len() as i64,
        median_secs: percentile(&latencies, 0.5),
        p90_secs: percentile(&latencies, 0.9),
    };

    // --- publish ---------------------------------------------------------------
    let published = active
        .iter()
        .filter(|r| r.published_state == "published")
        .count() as i64;
    let publish = PublishOut {
        published,
        unpublished: total_findings - published,
    };

    AnalyticsCore {
        total_findings,
        superseded_count,
        by_severity_disposition,
        acceptance,
        by_category,
        weekly,
        latency,
        publish,
    }
}

/// `GET /api/reviews/analytics?repo=&from=&to=` (design-addendum-2 §C).
/// Bearer. `repo` 404s on an unknown name (`find_repo`); absent scans every
/// finding across every configured repo. `from`/`to` are unix seconds over
/// `review_findings.created_at`, both optional.
pub async fn analytics_route(
    State(state): State<SharedState>,
    Query(params): Query<AnalyticsParams>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(r) = params.repo.as_deref() {
        find_repo(&state, r)?;
    }
    // 2026-08-31 incident (store.rs module doc): the findings scan +
    // recurrence-pairs query are contiguous store work — one blocking-pool
    // trip; `compute_analytics` is pure CPU, folded in between.
    let repo_param = params.repo.clone();
    let from = params.from;
    let to = params.to;
    let (core, recurrence) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let rows = store.list_findings_for_analytics(repo_param.as_deref(), from, to)?;
            let core = compute_analytics(&rows);
            let recurrence = store.recurrence_pairs(
                repo_param.as_deref(),
                from,
                to,
                store::RECURRENCE_MIN_REVIEWS,
            )?;
            Ok((core, recurrence))
        })
        .await?;
    let recurrence_out: Vec<serde_json::Value> = recurrence
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "category": r.category,
                "location_path": r.location_path,
                "review_count": r.review_count,
                "finding_count": r.finding_count,
                "review_ids": r.review_ids,
            })
        })
        .collect();

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "repo": params.repo,
            "from": params.from,
            "to": params.to,
            "total_findings": core.total_findings,
            "superseded_count": core.superseded_count,
            "by_severity_disposition": core.by_severity_disposition,
            "acceptance": core.acceptance,
            "by_category": core.by_category,
            "weekly": core.weekly,
            "latency": core.latency,
            "publish": core.publish,
            "recurrence": recurrence_out,
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn row(
        review_id: i64,
        severity: &str,
        category: &str,
        location_path: &str,
        disposition: Option<&str>,
        created_at: i64,
        disposition_at: Option<i64>,
        published_state: &str,
        superseded: bool,
    ) -> AnalyticsFindingRow {
        AnalyticsFindingRow {
            review_id,
            severity: severity.to_string(),
            category: category.to_string(),
            location_path: location_path.to_string(),
            disposition: disposition.map(|s| s.to_string()),
            disposition_at,
            published_state: published_state.to_string(),
            superseded,
            created_at,
        }
    }

    #[test]
    fn compute_analytics_is_deterministic_over_the_same_rows() {
        let rows = vec![
            row(
                1,
                "blocker",
                "Security",
                "a.rb",
                Some("agree"),
                1_000,
                Some(1_500),
                "unpublished",
                false,
            ),
            row(
                1,
                "concern",
                "Style",
                "b.rb",
                None,
                1_000,
                None,
                "unpublished",
                false,
            ),
            row(
                2,
                "blocker",
                "Security",
                "a.rb",
                Some("dispute"),
                2_000,
                Some(2_500),
                "published",
                false,
            ),
        ];
        let a = compute_analytics(&rows);
        let b = compute_analytics(&rows);
        assert_eq!(a, b, "same input rows must produce byte-identical output");
    }

    #[test]
    fn compute_analytics_matrix_reports_every_cell_even_at_zero() {
        let rows = vec![row(
            1,
            "blocker",
            "Security",
            "a.rb",
            Some("agree"),
            1_000,
            Some(1_500),
            "unpublished",
            false,
        )];
        let out = compute_analytics(&rows);
        assert_eq!(
            out.by_severity_disposition.len(),
            store::SEVERITIES.len() * 5
        );
        let zero_cell = out
            .by_severity_disposition
            .iter()
            .find(|c| c.severity == "ok" && c.disposition == "dispute")
            .expect("every (severity, disposition) cell must be present");
        assert_eq!(zero_cell.count, 0);
    }

    #[test]
    fn compute_analytics_acceptance_rate_is_null_not_zero_when_nothing_is_decided() {
        let rows = vec![
            row(
                1,
                "blocker",
                "Security",
                "a.rb",
                None,
                1_000,
                None,
                "unpublished",
                false,
            ),
            row(
                2,
                "blocker",
                "Security",
                "b.rb",
                None,
                1_000,
                None,
                "unpublished",
                false,
            ),
        ];
        let out = compute_analytics(&rows);
        let blocker = out
            .acceptance
            .iter()
            .find(|a| a.severity == "blocker")
            .unwrap();
        assert_eq!(blocker.undecided, 2);
        assert_eq!(blocker.rate, None, "no decided findings -> null, never 0.0");
    }

    #[test]
    fn compute_analytics_acceptance_rate_divides_by_decided_only_not_total() {
        let rows = vec![
            row(
                1,
                "blocker",
                "Security",
                "a.rb",
                Some("agree"),
                1_000,
                Some(1_100),
                "unpublished",
                false,
            ),
            row(
                2,
                "blocker",
                "Security",
                "b.rb",
                Some("dispute"),
                1_000,
                Some(1_100),
                "unpublished",
                false,
            ),
            row(
                3,
                "blocker",
                "Security",
                "c.rb",
                None,
                1_000,
                None,
                "unpublished",
                false,
            ),
        ];
        let out = compute_analytics(&rows);
        let blocker = out
            .acceptance
            .iter()
            .find(|a| a.severity == "blocker")
            .unwrap();
        assert_eq!(blocker.total, 3);
        assert_eq!(
            blocker.rate,
            Some(0.5),
            "1 accepted / (1 accepted + 1 rejected) decided"
        );
    }

    #[test]
    fn compute_analytics_excludes_superseded_from_every_aggregate_but_counts_it_separately() {
        let rows = vec![
            row(
                1,
                "blocker",
                "Security",
                "a.rb",
                Some("agree"),
                1_000,
                Some(1_100),
                "unpublished",
                false,
            ),
            row(
                1,
                "blocker",
                "Security",
                "a.rb",
                Some("agree"),
                1_000,
                Some(1_100),
                "unpublished",
                true,
            ),
        ];
        let out = compute_analytics(&rows);
        assert_eq!(out.total_findings, 1);
        assert_eq!(out.superseded_count, 1);
    }

    #[test]
    fn compute_analytics_by_category_caps_at_15_ranked_by_count_then_name() {
        let mut rows = Vec::new();
        for i in 0..20 {
            let category = format!("cat-{i:02}");
            // give cat-00 the most findings so it's guaranteed to rank first.
            let n = if i == 0 { 5 } else { 1 };
            for j in 0..n {
                rows.push(row(
                    j,
                    "concern",
                    &category,
                    "x.rb",
                    None,
                    1_000,
                    None,
                    "unpublished",
                    false,
                ));
            }
        }
        let out = compute_analytics(&rows);
        assert_eq!(out.by_category.len(), 15);
        assert_eq!(out.by_category[0].category, "cat-00");
        assert_eq!(out.by_category[0].count, 5);
        // Tied at count=1 -> alphabetical.
        assert_eq!(out.by_category[1].category, "cat-01");
    }

    #[test]
    fn compute_analytics_weekly_buckets_split_correctly_across_a_year_boundary() {
        use chrono::TimeZone;
        // 2025-12-29 is a Monday -> ISO week 2026-W01 (the ISO year rolls
        // over before the calendar year does). 2026-01-05 is the next
        // Monday -> ISO week 2026-W02. This exercises the exact case the
        // task calls out: bucketing across Dec 31 / Jan 1 must NOT merge
        // or misplace either week.
        let created_in_week_1 = chrono::Utc
            .with_ymd_and_hms(2025, 12, 30, 12, 0, 0)
            .unwrap()
            .timestamp();
        let created_in_week_2 = chrono::Utc
            .with_ymd_and_hms(2026, 1, 6, 12, 0, 0)
            .unwrap()
            .timestamp();
        let rows = vec![
            row(
                1,
                "concern",
                "Style",
                "a.rb",
                None,
                created_in_week_1,
                None,
                "unpublished",
                false,
            ),
            row(
                2,
                "concern",
                "Style",
                "b.rb",
                None,
                created_in_week_2,
                None,
                "unpublished",
                false,
            ),
        ];
        let out = compute_analytics(&rows);
        assert_eq!(
            out.weekly.len(),
            2,
            "two distinct ISO weeks, no zero-fill between them"
        );
        assert_eq!(out.weekly[0].created, 1);
        assert_eq!(out.weekly[1].created, 1);
        assert_eq!(
            out.weekly[1].week_start_unix - out.weekly[0].week_start_unix,
            7 * 86_400,
            "the two week starts must be exactly 7 days apart across the boundary"
        );
    }

    #[test]
    fn compute_analytics_latency_is_null_when_nothing_is_disposed() {
        let rows = vec![row(
            1,
            "concern",
            "Style",
            "a.rb",
            None,
            1_000,
            None,
            "unpublished",
            false,
        )];
        let out = compute_analytics(&rows);
        assert_eq!(out.latency.n, 0);
        assert_eq!(out.latency.median_secs, None);
        assert_eq!(out.latency.p90_secs, None);
    }

    #[test]
    fn compute_analytics_latency_median_and_p90_are_nearest_rank() {
        let rows = vec![
            row(
                1,
                "concern",
                "Style",
                "a.rb",
                Some("agree"),
                0,
                Some(10),
                "unpublished",
                false,
            ),
            row(
                2,
                "concern",
                "Style",
                "b.rb",
                Some("agree"),
                0,
                Some(20),
                "unpublished",
                false,
            ),
            row(
                3,
                "concern",
                "Style",
                "c.rb",
                Some("agree"),
                0,
                Some(30),
                "unpublished",
                false,
            ),
            row(
                4,
                "concern",
                "Style",
                "d.rb",
                Some("agree"),
                0,
                Some(40),
                "unpublished",
                false,
            ),
        ];
        let out = compute_analytics(&rows);
        // latencies sorted: [10, 20, 30, 40]
        assert_eq!(out.latency.n, 4);
        assert_eq!(
            out.latency.median_secs,
            Some(20),
            "ceil(0.5*4)=2nd smallest"
        );
        assert_eq!(out.latency.p90_secs, Some(40), "ceil(0.9*4)=4th smallest");
    }

    #[test]
    fn percentile_returns_none_on_empty_input() {
        assert_eq!(percentile(&[], 0.5), None);
    }
}
