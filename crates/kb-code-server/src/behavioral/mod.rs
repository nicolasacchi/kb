//! V3.2-B1 — behavioral store: hotspots, coupling, ownership, age.
//! V3.2-B2 — provenance fusion: agent authorship, session pain, tainted
//! lines, session-coupling, pain-weighted hotspots, review-risk composite.
//!
//! **Design law (non-negotiable):** these are ATTENTION signals, never
//! quality verdicts. Every score is decomposable into its terms on the
//! wire — no opaque grade. Hotspot ≠ bad code. Missing input ⇒ `null`,
//! never a default that reads as "fine".
//!
//! # Data model
//!
//! Repo-addressed counters (migration V0017): `path_stats`, `author_stats`,
//! `cochange_pairs`, `behavioral_meta`. V0018 adds `session_signals` +
//! `author_stats.first_seen_unix`. Filled by a single-shape
//! `git log --numstat -M --format=…` walk — full window rebuild or
//! incremental `last_commit_sha..HEAD`. See [`ingest`].
//!
//! # Window semantics
//!
//! Full rebuilds (`backfill_repo` / `POST /api/behavioral/backfill`) are
//! exact for the configured `[behavioral] window_days`. Incremental
//! head-moved updates are **additive only** within the current window —
//! paths/pairs do not decay out until the next rebuild. Do NOT try to
//! subtract incrementally (wrong and unverifiable).
//!
//! # Complexity proxy
//!
//! Parse-free: `LOC + sum of leading-indent depths` on the working-tree
//! file (tabs count as 1 indent unit each; spaces as floor(n/2) so two
//! spaces ≈ one indent). Documented choice — not cyclomatic complexity,
//! not AST depth; a cheap attention weight that needs no language parser.
//!
//! # Session pain (V3.2-B2)
//!
//! Reachable without new kb-daemon work: `SessionOut.error_count` (count of
//! detected error tool-results on `GET /api/sessions/{id}`). No distinct
//! test-failure / fail_count field exists on the join/why wire — `fail_count`
//! is stored as 0 and never invented. Duration is stored but NOT used as a
//! pain proxy (long ≠ painful). Score is pure error evidence, decomposed.

mod ingest;
mod routes;

pub use ingest::{
    apply_commits, backfill_repo, incremental_update, parse_log_numstat, walk_commits,
    walk_commits_capped, BehavioralError, BehavioralStats, CommitDelta, SCHEMA as INGEST_SCHEMA,
};
pub use routes::{
    age_route, behavioral_backfill_route, coupling_route, hotspots_route, ownership_route,
    session_coupling_route, spawn_behavioral_worker, tainted_route, timeseries_route, AgeOut,
    CouplingOut, HotspotsOut, OwnershipOut, SCHEMA,
};

use std::path::Path;

// --- pure math (unit-tested, no daemon) -----------------------------------

/// Complexity proxy: LOC + summed leading-indent depth. See module doc.
pub fn complexity_proxy(source: &str) -> Complexity {
    let mut loc: u64 = 0;
    let mut indent_sum: u64 = 0;
    for line in source.lines() {
        // Skip pure-blank lines for both LOC and indent (attention signal
        // over source shape, not raw byte length).
        if line.trim().is_empty() {
            continue;
        }
        loc += 1;
        indent_sum += leading_indent_depth(line) as u64;
    }
    Complexity { loc, indent_sum }
}

/// Leading indent depth for one line: each tab = 1; each run of 2 spaces = 1
/// (odd trailing space does not add).
pub fn leading_indent_depth(line: &str) -> u32 {
    let mut depth = 0u32;
    let mut spaces = 0u32;
    for c in line.chars() {
        match c {
            '\t' => {
                if spaces > 0 {
                    depth += spaces / 2;
                    spaces = 0;
                }
                depth += 1;
            }
            ' ' => spaces += 1,
            _ => break,
        }
    }
    depth + spaces / 2
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Complexity {
    pub loc: u64,
    pub indent_sum: u64,
}

impl Complexity {
    pub fn total(self) -> u64 {
        self.loc.saturating_add(self.indent_sum)
    }
}

/// Read complexity for a working-tree path; missing/unreadable → zeros.
pub fn complexity_for_path(repo_root: &Path, path: &str) -> Complexity {
    let abs = repo_root.join(path);
    match std::fs::read_to_string(&abs) {
        Ok(s) => complexity_proxy(&s),
        Err(_) => Complexity {
            loc: 0,
            indent_sum: 0,
        },
    }
}

/// Dense ranks (1 = highest value). Ties share the same rank; next rank
/// skips (competition ranking: 1,2,2,4).
pub fn dense_ranks_desc(values: &[u64]) -> Vec<u32> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| values[b].cmp(&values[a]).then(a.cmp(&b)));
    let mut ranks = vec![0u32; values.len()];
    let mut rank = 1u32;
    for (i, &idx) in order.iter().enumerate() {
        if i > 0 && values[idx] != values[order[i - 1]] {
            rank = (i as u32) + 1;
        }
        ranks[idx] = rank;
    }
    ranks
}

/// Hotspot score from two ranks (1 = hottest on that axis). Higher score
/// = more attention. `score = 0.5 * (1/churn_rank + 1/complexity_rank)`.
pub fn hotspot_score(churn_rank: u32, complexity_rank: u32) -> f64 {
    let c = churn_rank.max(1) as f64;
    let x = complexity_rank.max(1) as f64;
    0.5 * (1.0 / c + 1.0 / x)
}

/// ROSE-style confidence: `co_commits / revisions(path)`. Asymmetric —
/// conf(A⇒B) uses revisions(A); conf(B⇒A) uses revisions(B).
pub fn coupling_confidence(co_commits: i64, path_revisions: i64) -> f64 {
    if path_revisions <= 0 {
        return 0.0;
    }
    (co_commits as f64) / (path_revisions as f64)
}

/// Shannon entropy of a probability distribution (shares summing to ~1).
/// Returns 0 for empty or single-author.
pub fn shannon_entropy(shares: &[f64]) -> f64 {
    let mut h = 0.0;
    for &p in shares {
        if p > 0.0 {
            h -= p * p.ln();
        }
    }
    h
}

/// Bird-style major/minor split: major share > 5% of total commits.
pub const MAJOR_SHARE_THRESHOLD: f64 = 0.05;

/// Age buckets (label, max exclusive age in days). Last is catch-all.
pub const AGE_BUCKET_DEFS: &[(&str, Option<u64>)] = &[
    ("<7d", Some(7)),
    ("<30d", Some(30)),
    ("<90d", Some(90)),
    ("<1y", Some(365)),
    ("older", None),
];

/// Assign each line's age (days) into buckets. `now_unix` anchors "today".
pub fn age_buckets(line_ages_days: &[f64], now_unix: i64, timestamps: &[i64]) -> AgeBucketResult {
    let _ = now_unix; // ages already computed relative to now
    let mut buckets: Vec<(String, u64)> = AGE_BUCKET_DEFS
        .iter()
        .map(|(l, _)| ((*l).to_string(), 0u64))
        .collect();
    for &age in line_ages_days {
        let age_u = if age < 0.0 { 0.0 } else { age };
        let mut placed = false;
        for (i, (_, max)) in AGE_BUCKET_DEFS.iter().enumerate() {
            if let Some(m) = max {
                if age_u < *m as f64 {
                    buckets[i].1 += 1;
                    placed = true;
                    break;
                }
            }
        }
        if !placed {
            let last = buckets.len() - 1;
            buckets[last].1 += 1;
        }
    }
    let lines = line_ages_days.len() as u64;
    let (oldest, newest) = if timestamps.is_empty() {
        (None, None)
    } else {
        (
            timestamps.iter().copied().min(),
            timestamps.iter().copied().max(),
        )
    };
    let median_age_days = if line_ages_days.is_empty() {
        0.0
    } else {
        let mut sorted = line_ages_days.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = sorted.len() / 2;
        if sorted.len() % 2 == 0 && sorted.len() >= 2 {
            (sorted[mid - 1] + sorted[mid]) / 2.0
        } else {
            sorted[mid]
        }
    };
    AgeBucketResult {
        lines,
        oldest_unix: oldest,
        newest_unix: newest,
        median_age_days,
        buckets,
    }
}

#[derive(Debug, Clone)]
pub struct AgeBucketResult {
    pub lines: u64,
    pub oldest_unix: Option<i64>,
    pub newest_unix: Option<i64>,
    pub median_age_days: f64,
    pub buckets: Vec<(String, u64)>,
}

/// Canonical cochange pair order: `(min, max)` by path string.
pub fn ordered_pair(a: &str, b: &str) -> (String, String) {
    if a < b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

// --- V3.2-B2 session pain + review-risk (pure math) ----------------------

/// Prefix for dual-author agent rows in `author_stats`.
pub const SESSION_AUTHOR_PREFIX: &str = "session:";

/// Error-count scale for pain normalization: `min(1, error_count / N)`.
/// Documented choice — attention weight, not a defect prophecy.
pub const PAIN_ERROR_SCALE: f64 = 10.0;

/// Whether `author` is a dual-written session row.
pub fn is_session_author(author: &str) -> bool {
    author.starts_with(SESSION_AUTHOR_PREFIX)
}

/// Strip `session:` prefix; `None` if not a session author.
pub fn session_id_from_author(author: &str) -> Option<&str> {
    author
        .strip_prefix(SESSION_AUTHOR_PREFIX)
        .filter(|s| !s.is_empty())
}

/// Session pain from stored signals. Pure; never uses duration as a proxy.
///
/// Score = mean of the AVAILABLE normalized terms, renormalized over what
/// exists — the same rule as `review_risk_score`. This matters because
/// `fail_count` is structurally UNPOPULATED today: no test-failure field
/// exists on the sessions wire (see the module doc's investigation), so
/// the store writes 0. Averaging that hard zero in would halve every real
/// error signal — a session with maximal tool-error evidence would score
/// 0.5 and read as "medium pain" when the only evidence available says
/// "maximal". A term with no information must be ABSENT, not zero
/// (caught in review, 2026-08-02).
///
/// Callers pass `None` for a term that was not measured. No row at all →
/// the route returns `pain: null` (unknown ≠ zero).
#[derive(Debug, Clone, PartialEq)]
pub struct PainTerms {
    pub error_count: i64,
    /// `None` while no test-failure evidence exists on the wire.
    pub fail_count: Option<i64>,
    pub error_norm: f64,
    /// `None` mirrors `fail_count` — absent, never a 0.0 that dilutes.
    pub fail_norm: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PainScore {
    pub score: f64,
    pub terms: PainTerms,
}

/// Map a STORED `session_signals.fail_count` to a pain term.
///
/// The column exists so a real test-failure signal can land without a
/// migration, but nothing populates it today (no such field on the
/// sessions wire), so the store writes 0. A stored 0 therefore means
/// "not measured", not "zero failures" — return `None` so it stays out
/// of the score instead of halving the error evidence. The day a real
/// producer writes non-zero values, this becomes `Some(f)` for those
/// rows automatically.
pub fn stored_fail_term(fail_count: i64) -> Option<i64> {
    (fail_count > 0).then_some(fail_count)
}

pub fn pain_from_signals(error_count: i64, fail_count: Option<i64>) -> PainScore {
    let error_norm = ((error_count.max(0) as f64) / PAIN_ERROR_SCALE).min(1.0);
    let fail_norm = fail_count.map(|f| ((f.max(0) as f64) / PAIN_ERROR_SCALE).min(1.0));
    // Mean over AVAILABLE terms only — an unmeasured term never dilutes a
    // measured one. See the type doc.
    let mut sum = error_norm;
    let mut n = 1.0f64;
    if let Some(fnorm) = fail_norm {
        sum += fnorm;
        n += 1.0;
    }
    let score = (sum / n).clamp(0.0, 1.0);
    PainScore {
        score,
        terms: PainTerms {
            error_count,
            fail_count,
            error_norm,
            fail_norm,
        },
    }
}

/// Review-risk term weights (documented). Only AVAILABLE terms enter the
/// sum; score is renormalized over what exists. No terms ⇒ `None` (not 0).
pub const RISK_W_RELATIVE_CHURN: f64 = 0.30;
pub const RISK_W_OWNERSHIP_MINOR: f64 = 0.20;
pub const RISK_W_HOTSPOT_RANK: f64 = 0.25;
pub const RISK_W_AGENT_FIRST_TOUCH: f64 = 0.15;
pub const RISK_W_SESSION_PAIN: f64 = 0.10;

/// Optional per-term inputs for one review file. `None` = missing input.
#[derive(Debug, Clone, Default)]
pub struct RiskTermInputs {
    pub relative_churn: Option<f64>,
    pub ownership_minor: Option<f64>,
    pub hotspot_rank: Option<f64>,
    pub agent_first_touch: Option<bool>,
    pub session_pain: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RiskTermsOut {
    pub relative_churn: Option<f64>,
    pub ownership_minor: Option<f64>,
    pub hotspot_rank: Option<f64>,
    pub agent_first_touch: Option<bool>,
    pub session_pain: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RiskScore {
    pub score: f64,
    pub terms: RiskTermsOut,
    pub inputs_missing: Vec<&'static str>,
}

/// Weighted sum of available risk terms, renormalized over present weights.
/// Returns `None` when no term is computable (`risk: null` on the wire).
pub fn review_risk_score(inputs: &RiskTermInputs) -> Option<RiskScore> {
    let mut missing: Vec<&'static str> = Vec::new();
    let mut weight_sum = 0.0;
    let mut weighted = 0.0;

    let relative_churn = inputs.relative_churn;
    match relative_churn {
        Some(v) => {
            weight_sum += RISK_W_RELATIVE_CHURN;
            weighted += RISK_W_RELATIVE_CHURN * v.clamp(0.0, 1.0);
        }
        None => missing.push("relative_churn"),
    }
    let ownership_minor = inputs.ownership_minor;
    match ownership_minor {
        Some(v) => {
            weight_sum += RISK_W_OWNERSHIP_MINOR;
            weighted += RISK_W_OWNERSHIP_MINOR * v.clamp(0.0, 1.0);
        }
        None => missing.push("ownership_minor"),
    }
    let hotspot_rank = inputs.hotspot_rank;
    match hotspot_rank {
        Some(v) => {
            weight_sum += RISK_W_HOTSPOT_RANK;
            weighted += RISK_W_HOTSPOT_RANK * v.clamp(0.0, 1.0);
        }
        None => missing.push("hotspot_rank"),
    }
    let agent_first_touch = inputs.agent_first_touch;
    match agent_first_touch {
        Some(b) => {
            weight_sum += RISK_W_AGENT_FIRST_TOUCH;
            weighted += RISK_W_AGENT_FIRST_TOUCH * if b { 1.0 } else { 0.0 };
        }
        None => missing.push("agent_first_touch"),
    }
    let session_pain = inputs.session_pain;
    match session_pain {
        Some(v) => {
            weight_sum += RISK_W_SESSION_PAIN;
            weighted += RISK_W_SESSION_PAIN * v.clamp(0.0, 1.0);
        }
        None => missing.push("session_pain"),
    }

    if weight_sum <= 0.0 {
        return None;
    }
    Some(RiskScore {
        score: (weighted / weight_sum).clamp(0.0, 1.0),
        terms: RiskTermsOut {
            relative_churn,
            ownership_minor,
            hotspot_rank,
            agent_first_touch,
            session_pain,
        },
        inputs_missing: missing,
    })
}

/// Relative churn (Nagappan-Ball): `(added+deleted) / file_loc`.
/// Caps at 1.0. `None` when `file_loc == 0` (empty/missing file).
pub fn relative_churn(added: i64, deleted: i64, file_loc: u64) -> Option<f64> {
    if file_loc == 0 {
        return None;
    }
    let delta = (added.saturating_add(deleted)).max(0) as f64;
    Some((delta / file_loc as f64).min(1.0))
}

/// Normalize a dense rank (1 = hottest) into 0..1 attention: `1/rank`.
pub fn hotspot_rank_norm(rank: u32, total: usize) -> f64 {
    let _ = total;
    let r = rank.max(1) as f64;
    (1.0 / r).clamp(0.0, 1.0)
}

/// Extract the "new" path from a `--numstat` path field (handles renames).
pub fn numstat_final_path(raw: &str) -> String {
    let s = raw.trim();
    // `old => new` (full rename)
    if let Some(idx) = s.find(" => ") {
        // Prefer brace form when present: `dir/{old => new}/rest`
        if let Some(open) = s.find('{') {
            if let Some(close_rel) = s[open..].find('}') {
                let inner = &s[open + 1..open + close_rel];
                if let Some(arrow) = inner.find(" => ") {
                    let new = &inner[arrow + 4..];
                    let prefix = &s[..open];
                    let suffix = &s[open + close_rel + 1..];
                    return format!("{prefix}{new}{suffix}");
                }
            }
        }
        return s[idx + 4..].trim().to_string();
    }
    s.to_string()
}

// --- V3.4-C1 time-series (pure bucketing; no storage) --------------------

/// Default weeks for `GET /api/behavioral/timeseries` when `weeks` omitted.
pub const TIMESERIES_DEFAULT_WEEKS: u32 = 26;
/// Hard upper bound on the `weeks` query param.
pub const TIMESERIES_MAX_WEEKS: u32 = 104;
/// Hard cap on commits walked for a single timeseries request (mirrors the
/// need for a bounded walk; backfill itself is window-bounded via
/// `window_days` rather than a commit count, so this is the timeseries
/// route's own bound). Exceeded ⇒ `truncated: true`.
pub const TIMESERIES_MAX_COMMITS: usize = 50_000;

/// ISO week start (Monday 00:00 UTC) for an author-time unix timestamp.
///
/// Unix epoch day 0 is Thursday 1970-01-01. Weekday index with Monday=0 is
/// `(days_since_epoch + 3) rem 7`. Deterministic pure math — no locale.
pub fn iso_week_start_unix(commit_unix: i64) -> i64 {
    let days = commit_unix.div_euclid(86_400);
    let weekday = (days + 3).rem_euclid(7); // 0 = Monday
    (days - weekday) * 86_400
}

/// One week bucket of activity (attention signal — never a "health" grade).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TimeseriesBucket {
    pub week_start_unix: i64,
    pub commits: u64,
    /// `adds + dels` over the scoped files in the bucket.
    pub churn: i64,
    pub authors: u64,
}

/// Bucket commits into author-time ISO weeks (Monday UTC). Only weeks that
/// contain at least one matching commit appear (no zero-fill). Ordered
/// ascending by `week_start_unix` (total order).
///
/// When `path` is `Some`, a commit contributes only if it touches that
/// exact final path (post-`numstat_final_path` / `-M` rename detection —
/// **no** `git log --follow`; same rename posture as ingest). Churn is
/// scoped to matching files; authors/commits count only commits that
/// touch the path.
pub fn bucket_timeseries(
    commits: &[ingest::CommitDelta],
    path: Option<&str>,
) -> Vec<TimeseriesBucket> {
    use std::collections::{BTreeMap, BTreeSet};
    // week_start → (commit count, churn, authors set)
    let mut map: BTreeMap<i64, (u64, i64, BTreeSet<String>)> = BTreeMap::new();
    for c in commits {
        let mut churn = 0i64;
        let mut touches = false;
        for (p, add, del) in &c.files {
            let matched = match path {
                None => true,
                Some(want) => p == want,
            };
            if matched {
                touches = true;
                churn = churn.saturating_add(add.saturating_add(*del));
            }
        }
        if !touches {
            continue;
        }
        let week = iso_week_start_unix(c.commit_unix);
        let entry = map.entry(week).or_default();
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.saturating_add(churn);
        entry.2.insert(c.author.clone());
    }
    map.into_iter()
        .map(
            |(week_start_unix, (commits, churn, authors))| TimeseriesBucket {
                week_start_unix,
                commits,
                churn,
                authors: authors.len() as u64,
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complexity_proxy_counts_loc_and_indent() {
        let src = "fn a() {\n    let x = 1;\n\tlet y = 2;\n}\n";
        let c = complexity_proxy(src);
        // 4 non-blank lines; "    " → 2, "\t" → 1
        assert_eq!(c.loc, 4);
        assert_eq!(c.indent_sum, 3);
        assert_eq!(c.total(), 7);
    }

    #[test]
    fn dense_ranks_desc_handles_ties() {
        // values: 10, 5, 10, 1 → ranks 1, 3, 1, 4
        let ranks = dense_ranks_desc(&[10, 5, 10, 1]);
        assert_eq!(ranks, vec![1, 3, 1, 4]);
    }

    #[test]
    fn hotspot_score_higher_for_better_ranks() {
        let top = hotspot_score(1, 1);
        let mid = hotspot_score(2, 2);
        let mixed = hotspot_score(1, 10);
        assert!(top > mid);
        assert!(top > mixed);
        assert!((top - 1.0).abs() < 1e-9);
    }

    #[test]
    fn coupling_confidence_is_asymmetric_ratio() {
        assert!((coupling_confidence(3, 10) - 0.3).abs() < 1e-9);
        assert!((coupling_confidence(3, 5) - 0.6).abs() < 1e-9);
        assert_eq!(coupling_confidence(1, 0), 0.0);
    }

    #[test]
    fn shannon_entropy_zero_for_single_author() {
        assert!((shannon_entropy(&[1.0])).abs() < 1e-12);
        // two equal authors → ln(2)
        let h = shannon_entropy(&[0.5, 0.5]);
        assert!((h - std::f64::consts::LN_2).abs() < 1e-9);
    }

    #[test]
    fn age_buckets_partition_lines() {
        // ages: 1, 10, 40, 200, 400 days
        let ages = [1.0, 10.0, 40.0, 200.0, 400.0];
        let ts = [0i64; 5];
        let r = age_buckets(&ages, 0, &ts);
        assert_eq!(r.lines, 5);
        let map: std::collections::BTreeMap<_, _> = r.buckets.into_iter().collect();
        assert_eq!(map["<7d"], 1);
        assert_eq!(map["<30d"], 1);
        assert_eq!(map["<90d"], 1);
        assert_eq!(map["<1y"], 1);
        assert_eq!(map["older"], 1);
    }

    #[test]
    fn ordered_pair_canonicalizes() {
        assert_eq!(ordered_pair("b.rs", "a.rs"), ("a.rs".into(), "b.rs".into()));
    }

    #[test]
    fn numstat_final_path_handles_renames() {
        assert_eq!(numstat_final_path("src/a.rs"), "src/a.rs");
        assert_eq!(numstat_final_path("old.rs => new.rs"), "new.rs");
        assert_eq!(numstat_final_path("dir/{old.rs => new.rs}"), "dir/new.rs");
    }

    #[test]
    fn pain_from_signals_decomposes_error_evidence_only() {
        // fail term UNMEASURED (today's reality): the score is the error
        // evidence alone. It must NOT be halved by an absent term —
        // maximal error evidence must read as maximal, not "medium".
        let p = pain_from_signals(10, None);
        assert!((p.terms.error_norm - 1.0).abs() < 1e-9);
        assert!(p.terms.fail_norm.is_none());
        assert!(p.terms.fail_count.is_none());
        assert!((p.score - 1.0).abs() < 1e-9);
        let zero = pain_from_signals(0, None);
        assert!((zero.score - 0.0).abs() < 1e-9);
    }

    #[test]
    fn pain_from_signals_means_over_both_terms_once_fail_is_measured() {
        // The day a producer writes real failure counts, both terms count.
        let p = pain_from_signals(10, Some(0));
        assert!((p.terms.error_norm - 1.0).abs() < 1e-9);
        assert_eq!(p.terms.fail_norm, Some(0.0));
        assert!((p.score - 0.5).abs() < 1e-9); // (1+0)/2
    }

    #[test]
    fn stored_fail_term_treats_zero_as_not_measured() {
        // The column exists so a real producer can land without a
        // migration; nothing writes it today, so a stored 0 means
        // "not measured", not "zero failures".
        assert_eq!(stored_fail_term(0), None);
        assert_eq!(stored_fail_term(3), Some(3));
    }

    #[test]
    fn review_risk_null_when_no_terms() {
        assert!(review_risk_score(&RiskTermInputs::default()).is_none());
    }

    #[test]
    fn review_risk_renormalizes_over_available_terms() {
        // Only relative_churn = 1.0 → score must be 1.0 (sole available term).
        let only_churn = review_risk_score(&RiskTermInputs {
            relative_churn: Some(1.0),
            ..Default::default()
        })
        .unwrap();
        assert!((only_churn.score - 1.0).abs() < 1e-9);
        assert_eq!(
            only_churn.inputs_missing,
            vec![
                "ownership_minor",
                "hotspot_rank",
                "agent_first_touch",
                "session_pain"
            ]
        );

        // churn=1.0 (w=0.30) + minor=0.0 (w=0.20) → 0.30/0.50 = 0.6
        let two = review_risk_score(&RiskTermInputs {
            relative_churn: Some(1.0),
            ownership_minor: Some(0.0),
            ..Default::default()
        })
        .unwrap();
        assert!((two.score - 0.6).abs() < 1e-9);
        assert!(two.inputs_missing.contains(&"session_pain"));
    }

    #[test]
    fn relative_churn_none_on_empty_file() {
        assert!(relative_churn(10, 5, 0).is_none());
        let c = relative_churn(50, 50, 100).unwrap();
        assert!((c - 1.0).abs() < 1e-9);
    }

    #[test]
    fn session_author_prefix_helpers() {
        assert!(is_session_author("session:abc"));
        assert!(!is_session_author("alice@ex.com"));
        assert_eq!(session_id_from_author("session:abc"), Some("abc"));
        assert_eq!(session_id_from_author("alice"), None);
    }

    #[test]
    fn iso_week_start_is_monday_utc() {
        // 2024-01-15 (Mon) 12:00 UTC → that Monday 00:00
        // 2024-01-15 is a Monday.
        let mon = 1_705_320_000i64; // 2024-01-15T12:00:00Z
        let start = iso_week_start_unix(mon);
        assert_eq!(start, 1_705_276_800); // 2024-01-15T00:00:00Z
                                          // Same week, Wednesday
        let wed = mon + 2 * 86_400;
        assert_eq!(iso_week_start_unix(wed), start);
        // Previous Sunday falls in the prior week
        let sun = mon - 86_400;
        assert_eq!(iso_week_start_unix(sun), start - 7 * 86_400);
    }

    #[test]
    fn timeseries_buckets_golden_three_weeks() {
        // Three Mondays: 2024-01-01 is Monday; 2024-01-08; 2024-01-15.
        let w1 = 1_704_067_200i64; // 2024-01-01T00:00:00Z
        let w2 = w1 + 7 * 86_400;
        let w3 = w2 + 7 * 86_400;
        let commits = vec![
            CommitDelta {
                sha: "a".into(),
                author: "alice@ex.com".into(),
                commit_unix: w1 + 3600,
                files: vec![("a.rs".into(), 2, 0)],
            },
            CommitDelta {
                sha: "b".into(),
                author: "bob@ex.com".into(),
                commit_unix: w1 + 7200,
                files: vec![("a.rs".into(), 1, 1), ("b.rs".into(), 3, 0)],
            },
            CommitDelta {
                sha: "c".into(),
                author: "alice@ex.com".into(),
                commit_unix: w2 + 100,
                files: vec![("a.rs".into(), 5, 2)],
            },
            CommitDelta {
                sha: "d".into(),
                author: "carol@ex.com".into(),
                commit_unix: w3 + 100,
                files: vec![("b.rs".into(), 1, 0)],
            },
        ];
        let buckets = bucket_timeseries(&commits, None);
        assert_eq!(buckets.len(), 3);
        assert_eq!(
            buckets[0],
            TimeseriesBucket {
                week_start_unix: w1,
                commits: 2,
                // alice: a.rs +2/0; bob: a.rs +1/-1 + b.rs +3/0 → 2+1+1+3 = 7
                churn: 7,
                authors: 2,
            }
        );
        assert_eq!(
            buckets[1],
            TimeseriesBucket {
                week_start_unix: w2,
                commits: 1,
                churn: 7,
                authors: 1,
            }
        );
        assert_eq!(
            buckets[2],
            TimeseriesBucket {
                week_start_unix: w3,
                commits: 1,
                churn: 1,
                authors: 1,
            }
        );

        // Path scope: only a.rs
        let scoped = bucket_timeseries(&commits, Some("a.rs"));
        assert_eq!(scoped.len(), 2);
        assert_eq!(scoped[0].commits, 2);
        // a.rs only: alice +2/0 + bob +1/-1 → 4 (b.rs excluded)
        assert_eq!(scoped[0].churn, 4);
        assert_eq!(scoped[0].authors, 2);
        assert_eq!(scoped[1].week_start_unix, w2);
        assert_eq!(scoped[1].churn, 7);
        // w3 only touched b.rs → absent
    }

    #[test]
    fn timeseries_bucket_determinism() {
        let w1 = 1_704_067_200i64;
        let commits = vec![
            CommitDelta {
                sha: "z".into(),
                author: "b@ex.com".into(),
                commit_unix: w1 + 100,
                files: vec![("x.rs".into(), 1, 0)],
            },
            CommitDelta {
                sha: "a".into(),
                author: "a@ex.com".into(),
                commit_unix: w1 + 200,
                files: vec![("x.rs".into(), 2, 0)],
            },
        ];
        let a = bucket_timeseries(&commits, None);
        let b = bucket_timeseries(&commits, None);
        assert_eq!(a, b);
        let json_a = serde_json::to_string(&a).unwrap();
        let json_b = serde_json::to_string(&b).unwrap();
        assert_eq!(json_a, json_b);
    }
}
