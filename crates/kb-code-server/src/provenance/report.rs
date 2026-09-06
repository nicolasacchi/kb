//! `GET /api/provenance-report?repo=&max_count=` + `kb-code
//! provenance-report` (W3.3) — the provenance INSTRUMENT: kb-code's REAL
//! join-ladder measurement over a repo's commit history, walking `git log`
//! from `HEAD` (bounded, [`DEFAULT_MAX_COUNT`] by default) and running the
//! full 6-arm ladder (`join::ladder::resolve_commit`) per commit — the
//! cache does the heavy lifting on any re-run (`commit_sessions`, permanent
//! for `trailer`/`exact`, TTL'd for `fuzzy`/`none` — see that module's
//! doc).
//!
//! This SUPERSEDES kb-cli's W0.6 probe (`kb sessions provenance-report`,
//! `crates/kb-cli/src/commands/sessions.rs`) — that one stays in place
//! UNCHANGED: it measures a coarse trailer/recorded/pre-capture/non-session
//! split against kb's raw `commit-map` bulk feed (a pre-Wave-3 yardstick,
//! taken before the ladder existed), whereas THIS instrument reports the
//! ladder's own 4-valued `confidence` + free-form `via` taxonomy — exactly
//! what `kb-code why`/`story` would themselves report for any commit in the
//! walked window.
//!
//! # Report shape
//!
//! - `by_confidence` / `by_via` — counts + percentages across every walked
//!   commit.
//! - `trailer_coverage_by_week` — per ISO week (`chrono`'s `IsoWeek`,
//!   labelled `"<iso-year>-W<ww>"`), what fraction of that week's commits
//!   resolved at `confidence: "trailer"` — the trend line for "is the
//!   trailer dispatcher actually landing on every commit."
//! - `capture_era` — the SAME two breakdowns, restricted to commits authored
//!   at or after the EARLIEST `started_at` any resolved attribution
//!   carries (i.e., the oldest session capture the ladder found evidence
//!   of) — `None` when nothing in the walked window resolved a
//!   `started_at` at all (no capture evidence yet). Mirrors the W0.6
//!   probe's own `capture_era_start` derivation, adapted to the ladder's
//!   richer per-commit `started_at`.

use crate::config::RepoEntry;
use crate::join::ladder::{self, Attribution, Confidence};
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use chrono::Datelike;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// `git log`'s own bound (`--max-count`) when the caller doesn't override
/// it — generous enough to cover most repos' full capture-era history in
/// one pass without an unbounded walk.
pub const DEFAULT_MAX_COUNT: usize = 2000;

/// A defensive ceiling on an operator-supplied `?max_count=` — mirrors
/// `routes::blame_timeline`'s own `max` clamp precedent.
pub const MAX_ALLOWED_MAX_COUNT: usize = 20_000;

#[derive(Debug, Deserialize)]
pub struct ProvenanceReportParams {
    pub repo: String,
    pub max_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BucketOut {
    pub label: String,
    pub count: usize,
    pub pct: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WeekTrailerCoverageOut {
    /// ISO week label, `"<iso-year>-W<ww>"` (e.g. `"2026-W29"`).
    pub week: String,
    pub commits: usize,
    pub trailer_commits: usize,
    pub pct: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureEraOut {
    /// Unix seconds — the earliest `started_at` any resolved attribution in
    /// the walked window carries.
    pub start: i64,
    pub commit_count: usize,
    pub by_confidence: Vec<BucketOut>,
    pub by_via: Vec<BucketOut>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProvenanceReportOut {
    pub repo: String,
    pub total_commits: usize,
    pub max_count: usize,
    /// `true` when `total_commits == max_count` — a best-effort signal
    /// ("there may be more history past the walked window"), not a proof:
    /// a repo with EXACTLY `max_count` commits looks truncated too (`git
    /// log --max-count` reports no boundary marker of its own).
    pub truncated: bool,
    pub by_confidence: Vec<BucketOut>,
    pub by_via: Vec<BucketOut>,
    pub trailer_coverage_by_week: Vec<WeekTrailerCoverageOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_era: Option<CaptureEraOut>,
}

struct LogCommit {
    sha: String,
    /// Unix seconds.
    author_time: i64,
}

/// `GET /api/provenance-report?repo=&max_count=`.
pub async fn provenance_report_route(
    State(state): State<SharedState>,
    Query(params): Query<ProvenanceReportParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let max_count = params
        .max_count
        .unwrap_or(DEFAULT_MAX_COUNT)
        .clamp(1, MAX_ALLOWED_MAX_COUNT);
    let out = run(&state, repo, repo_id, max_count).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// The instrument's entry point, called from [`provenance_report_route`]
/// above. `kb-code provenance-report` itself is daemon-only (like every
/// other W3.x verb) and has no in-process path that calls this directly;
/// it's `pub` purely so a future direct caller (or this module's own
/// tests) doesn't need to go through HTTP to exercise it.
pub async fn run(
    state: &SharedState,
    repo: &RepoEntry,
    repo_id: i64,
    max_count: usize,
) -> Result<ProvenanceReportOut, ApiError> {
    let commits = walk_commits(&repo.path, max_count).await?;
    let total = commits.len();
    let truncated = total >= max_count && total > 0;

    let mut attributions = Vec::with_capacity(total);
    for c in &commits {
        let a = ladder::resolve_commit(repo, repo_id, &c.sha, &state.store, &state.kb_client).await;
        attributions.push(a);
    }

    let by_confidence = confidence_buckets(&attributions);
    let by_via = via_buckets(&attributions);
    let trailer_coverage_by_week = weekly_trailer_coverage(&commits, &attributions);

    let capture_era_start = attributions.iter().filter_map(|a| a.started_at).min();
    let capture_era = capture_era_start.map(|start| {
        let era_attrs: Vec<Attribution> = commits
            .iter()
            .zip(attributions.iter())
            .filter(|(c, _)| c.author_time >= start)
            .map(|(_, a)| a.clone())
            .collect();
        CaptureEraOut {
            start,
            commit_count: era_attrs.len(),
            by_confidence: confidence_buckets(&era_attrs),
            by_via: via_buckets(&era_attrs),
        }
    });

    Ok(ProvenanceReportOut {
        repo: repo.name.clone(),
        total_commits: total,
        max_count,
        truncated,
        by_confidence,
        by_via,
        trailer_coverage_by_week,
        capture_era,
    })
}

/// Blocking `git log --format=%H %at --max-count=<n> HEAD`, newest-first
/// (git's own default order) — run inside `spawn_blocking`, mirroring
/// every other git-subprocess call in this crate (see `blame::timeline`'s
/// module doc for the same rationale).
async fn walk_commits(repo_root: &Path, max_count: usize) -> Result<Vec<LogCommit>, ApiError> {
    let repo_root = repo_root.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<Vec<LogCommit>, ApiError> {
        let max = max_count.to_string();
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo_root)
            .args(["log", "--format=%H %at", "--max-count", &max, "HEAD"])
            .output()
            .map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("spawn git log: {e}"),
                )
            })?;
        if !out.status.success() {
            return Err(ApiError::bad_request(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let commits = text
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(2, ' ');
                let sha = parts.next()?.to_string();
                let author_time: i64 = parts.next()?.trim().parse().ok()?;
                Some(LogCommit { sha, author_time })
            })
            .collect();
        Ok(commits)
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("git log task panicked: {e}"),
        )
    })?
}

fn pct(n: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (n as f64) / (total as f64) * 100.0
    }
}

/// The 4-valued `join::ladder::Confidence` enum, IN ORDER
/// (`trailer, exact, fuzzy, none`) — every bucket always present, even at
/// zero, so a client can render a stable table.
fn confidence_buckets(attrs: &[Attribution]) -> Vec<BucketOut> {
    let total = attrs.len();
    [
        Confidence::Trailer,
        Confidence::Exact,
        Confidence::Fuzzy,
        Confidence::None,
    ]
    .iter()
    .map(|c| {
        let n = attrs.iter().filter(|a| a.confidence == *c).count();
        BucketOut {
            label: c.as_str().to_string(),
            count: n,
            pct: pct(n, total),
        }
    })
    .collect()
}

/// The free-form `via` taxonomy — label-sorted (`BTreeMap`) rather than a
/// fixed order (a future arm needs no report-side update, per `join::
/// ladder`'s own "via stays extensible" doc).
fn via_buckets(attrs: &[Attribution]) -> Vec<BucketOut> {
    let total = attrs.len();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for a in attrs {
        *counts.entry(a.via.clone()).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .map(|(label, n)| BucketOut {
            label,
            count: n,
            pct: pct(n, total),
        })
        .collect()
}

fn weekly_trailer_coverage(
    commits: &[LogCommit],
    attrs: &[Attribution],
) -> Vec<WeekTrailerCoverageOut> {
    let mut per_week: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (c, a) in commits.iter().zip(attrs.iter()) {
        let week = iso_week_label(c.author_time);
        let entry = per_week.entry(week).or_insert((0, 0));
        entry.0 += 1;
        if a.confidence == Confidence::Trailer {
            entry.1 += 1;
        }
    }
    per_week
        .into_iter()
        .map(
            |(week, (commits, trailer_commits))| WeekTrailerCoverageOut {
                week,
                commits,
                trailer_commits,
                pct: pct(trailer_commits, commits),
            },
        )
        .collect()
}

fn iso_week_label(unix_seconds: i64) -> String {
    let dt = chrono::DateTime::from_timestamp(unix_seconds, 0)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).expect("epoch is representable"));
    let iso = dt.iso_week();
    format!("{}-W{:02}", iso.year(), iso.week())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attribution(confidence: Confidence, via: &str, started_at: Option<i64>) -> Attribution {
        Attribution {
            schema: ladder::SCHEMA,
            confidence,
            via: via.to_string(),
            session_id: None,
            kb: None,
            display_name: None,
            started_at,
            sha: "deadbeef".to_string(),
        }
    }

    #[test]
    fn confidence_buckets_reports_every_variant_even_at_zero() {
        let attrs = vec![
            attribution(Confidence::Trailer, "commit-trailer", None),
            attribution(Confidence::Trailer, "commit-trailer", None),
            attribution(Confidence::None, "no-match", None),
        ];
        let buckets = confidence_buckets(&attrs);
        assert_eq!(buckets.len(), 4);
        assert_eq!(buckets[0].label, "trailer");
        assert_eq!(buckets[0].count, 2);
        assert!((buckets[0].pct - (200.0 / 3.0)).abs() < 1e-9);
        assert_eq!(buckets[1].label, "exact");
        assert_eq!(buckets[1].count, 0);
        assert_eq!(buckets[1].pct, 0.0);
        assert_eq!(buckets[3].label, "none");
        assert_eq!(buckets[3].count, 1);
    }

    #[test]
    fn via_buckets_groups_and_sorts_by_label() {
        let attrs = vec![
            attribution(Confidence::Fuzzy, "time-window", None),
            attribution(Confidence::Fuzzy, "subject", None),
            attribution(Confidence::Fuzzy, "subject", None),
        ];
        let buckets = via_buckets(&attrs);
        let labels: Vec<&str> = buckets.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, vec!["subject", "time-window"]);
        assert_eq!(buckets[0].count, 2);
        assert_eq!(buckets[1].count, 1);
    }

    #[test]
    fn iso_week_label_matches_a_known_date() {
        // 2026-07-17 is in ISO week 29 of 2026 (verified against `date
        // -d 2026-07-17 +%G-W%V`).
        let ts = chrono::NaiveDate::from_ymd_opt(2026, 7, 17)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        assert_eq!(iso_week_label(ts), "2026-W29");
    }

    #[test]
    fn weekly_trailer_coverage_groups_by_iso_week_and_computes_pct() {
        let day1 = chrono::NaiveDate::from_ymd_opt(2026, 7, 13)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let day2 = day1 + 86_400;
        let commits = vec![
            LogCommit {
                sha: "a".to_string(),
                author_time: day1,
            },
            LogCommit {
                sha: "b".to_string(),
                author_time: day2,
            },
        ];
        let attrs = vec![
            attribution(Confidence::Trailer, "commit-trailer", None),
            attribution(Confidence::None, "no-match", None),
        ];
        let weeks = weekly_trailer_coverage(&commits, &attrs);
        assert_eq!(weeks.len(), 1, "both commits fall in the same ISO week");
        assert_eq!(weeks[0].commits, 2);
        assert_eq!(weeks[0].trailer_commits, 1);
        assert_eq!(weeks[0].pct, 50.0);
    }

    #[test]
    fn pct_of_zero_total_is_zero_not_nan() {
        assert_eq!(pct(0, 0), 0.0);
    }
}
