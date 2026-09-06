//! `GET /api/story?repo=&path=[&symbol=]` (W3.4) — a file's (or one
//! symbol's) session TIMELINE: every session/commit that has ever left a
//! mark on it, ordered chronologically (oldest first — a "story" reads
//! start to end).
//!
//! # Two passes, one identity per group
//!
//! 1. **Owns-lines** — every CURRENT blame region's sha
//!    (`provenance::blame_regions`, whole file or `?symbol=`'s line range),
//!    resolved through the join ladder. A [`provenance::UNCOMMITTED_SHA`]
//!    region is skipped (no commit to attribute — `why`'s job, not
//!    `story`'s). Grouped by identity (session id when the ladder resolved
//!    one, else the bare sha) — `lines_touched` sums the CURRENT line count
//!    across every region sharing that identity, `first_seen` is the
//!    earliest region author-time.
//! 2. **Historical (drive-by)** — for each of those same regions, one
//!    bounded `git log -L` line-timeline sample at the region's own
//!    representative line (`provenance::line_timeline`,
//!    `blame::DEFAULT_MAX_ENTRIES`-capped). Any timeline sha that is NOT
//!    among the file's current owning shas is a superseded, drive-by touch
//!    — also ladder-resolved, grouped the SAME way. An identity already
//!    present from pass 1 is never duplicated: the group simply stays
//!    "owns-lines" (a session/commit that once drove by AND still owns a
//!    line elsewhere in the file is reported once, as an owner).
//!
//! This is v1's deliberately simple stand-in for "hunk birth vs drive-by":
//! it does NOT compare a commit's blame `orig_start`/`previous_sha` chain to
//! determine whether a sha truly ORIGINATED a line versus merely passed
//! through it. `status` is exactly `"owns-lines"` (the identity owns at
//! least one line in the file/symbol's CURRENT state) or `"historical"`
//! (every trace of it has since been overwritten, but the bounded timeline
//! still remembers it touched this file).
//!
//! # Symbol filter
//!
//! `?symbol=` restricts to that symbol's `[line_start, line_end]` (current
//! working-tree state, via `store::Store::symbols_for_repo` — the same
//! table `GET /api/symbols` reads). 404s when the named symbol doesn't
//! exist in `path`.
//!
//! # The attention-gap beat (CT-E2)
//!
//! A commit the join ladder resolved NO session for used to read exactly
//! like one with a rich session join — a full entry, silently missing its
//! `session_id`. The story now says so out loud instead: after the final
//! chronological sort, every maximal run of CONSECUTIVE session-less
//! entries is collapsed ([`collapse_uncovered_runs`]) into ONE
//! `status: "gap"` beat — "changed outside any captured session" — carrying
//! `commit_count`, the run's `first_seen`/`last_seen` author-time range,
//! and the summed `lines_touched`. One beat per RUN, never per commit: a
//! pre-kb-capture repo history is a single gap beat, not a story drowned in
//! per-commit noise. A single-commit gap keeps its `sha`/`subject` (there
//! is nothing to drown); a multi-commit gap carries neither (the per-commit
//! detail still lives in `GET /api/blame` / `GET /api/blame/timeline`).
//!
//! The gap is HONEST about what it knows, via `reason`:
//!
//! - `"no-captured-session"` — the ladder genuinely ran to completion
//!   (`via: "no-match"`): kb was consulted and no captured session records
//!   these commits.
//! - `"join-unavailable"` — the ladder could not consult kb at all
//!   (`via: "kb-unreachable"` / `"kb-disabled"`, or any via this build
//!   doesn't recognise — fail-honest): coverage is UNKNOWN here, not
//!   known-absent. Caveat: the ladder's `commit_sessions` cache TTLs
//!   `none`-confidence rows (`ladder::TTL_SECS`), so a `kb-unreachable`
//!   reason can outlive the outage by up to that long.
//!
//! A run is split where the reason changes — the two claims are different
//! statements and are never blended into one beat. When every change is
//! covered, [`collapse_uncovered_runs`] is the identity and the response is
//! byte-for-byte what it was before CT-E2 (every gap-only field is
//! `skip_serializing_if`-omitted).

use crate::config::RepoEntry;
use crate::join::ladder::Attribution;
use crate::provenance;
use crate::routes::{find_repo, safe_rel_path, ApiError};
use crate::state::SharedState;
use crate::store::StoreBlocking;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Deserialize)]
pub struct StoryParams {
    pub repo: String,
    pub path: String,
    pub symbol: Option<String>,
}

/// A gap beat's `status` — the third value beside `"owns-lines"` /
/// `"historical"`; see the module doc's "attention-gap beat" section.
pub const STATUS_GAP: &str = "gap";

/// [`StoryEntry::reason`] — kb was consulted, no captured session records
/// these commits (`via: "no-match"`).
pub const GAP_REASON_NO_SESSION: &str = "no-captured-session";

/// [`StoryEntry::reason`] — the session join itself was unavailable
/// (`kb-unreachable`/`kb-disabled`): coverage UNKNOWN, not known-absent.
pub const GAP_REASON_JOIN_UNAVAILABLE: &str = "join-unavailable";

#[derive(Debug, Clone, Serialize)]
pub struct StoryEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub confidence: String,
    pub via: String,
    /// Unix seconds — the earliest author-time this identity is known to
    /// have touched the file/symbol under.
    pub first_seen: i64,
    /// Gap beats only (CT-E2): the LATEST author-time in the collapsed run
    /// (equal to `first_seen` for a single-commit gap). Absent on
    /// `owns-lines`/`historical` entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<i64>,
    pub lines_touched: u32,
    /// `"owns-lines"` | `"historical"` | [`STATUS_GAP`] — see the module
    /// doc.
    pub status: &'static str,
    /// Gap beats only: how many session-less commits this beat collapses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_count: Option<u32>,
    /// Gap beats only: [`GAP_REASON_NO_SESSION`] |
    /// [`GAP_REASON_JOIN_UNAVAILABLE`] — see the module doc's honesty
    /// contract.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoryOut {
    pub repo: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub entries: Vec<StoryEntry>,
}

/// One group's accumulated state — internal, converted to [`StoryEntry`] at
/// the end via [`From`].
struct StoryAgg {
    session_id: Option<String>,
    sha: Option<String>,
    display_name: Option<String>,
    subject: Option<String>,
    confidence: crate::join::ladder::Confidence,
    via: String,
    first_seen: i64,
    lines_touched: u32,
    owns_lines: bool,
}

impl From<StoryAgg> for StoryEntry {
    fn from(a: StoryAgg) -> Self {
        Self {
            session_id: a.session_id,
            sha: a.sha,
            display_name: a.display_name,
            subject: a.subject,
            confidence: a.confidence.as_str().to_string(),
            via: a.via,
            first_seen: a.first_seen,
            last_seen: None,
            lines_touched: a.lines_touched,
            status: if a.owns_lines {
                "owns-lines"
            } else {
                "historical"
            },
            commit_count: None,
            reason: None,
        }
    }
}

fn group_key(attribution: &Attribution, sha: &str) -> String {
    attribution
        .session_id
        .clone()
        .unwrap_or_else(|| sha.to_string())
}

/// The honest gap-reason mapping over the ladder's own "absence" via
/// taxonomy (single-sourced from `join::ladder`'s `pub(crate)` consts).
/// Only `no-match` can back the definite "no captured session" claim;
/// everything else — including a via this build doesn't recognise —
/// fail-honests to "the join was unavailable" (an unknown via is a row we
/// cannot vouch for, so the weaker claim is the truthful one).
fn gap_reason_for_via(via: &str) -> &'static str {
    use crate::join::ladder::{VIA_KB_DISABLED, VIA_KB_UNREACHABLE, VIA_NO_MATCH};
    match via {
        VIA_NO_MATCH => GAP_REASON_NO_SESSION,
        VIA_KB_UNREACHABLE | VIA_KB_DISABLED => GAP_REASON_JOIN_UNAVAILABLE,
        _ => GAP_REASON_JOIN_UNAVAILABLE,
    }
}

/// `true` for an entry the join ladder resolved no session for — the
/// entries the attention-gap collapse folds. Covered entries ALWAYS carry a
/// `session_id` (every resolving ladder arm sets one), so its absence is
/// the whole test.
fn is_uncovered(e: &StoryEntry) -> bool {
    e.session_id.is_none()
}

/// One collapsed attention-gap beat from a non-empty run of consecutive
/// session-less entries — see the module doc. A single-commit run keeps its
/// `sha`/`subject`; a multi-commit run drops both (per-commit detail lives
/// in the blame/timeline surfaces). `lines_touched` is summed across the
/// run and keeps each member's own semantics (current lines for
/// `owns-lines` members, bounded-timeline touches for `historical` ones).
fn gap_beat(run: Vec<StoryEntry>) -> StoryEntry {
    debug_assert!(!run.is_empty());
    let first_seen = run.iter().map(|e| e.first_seen).min().unwrap_or(0);
    let last_seen = run.iter().map(|e| e.first_seen).max().unwrap_or(0);
    let lines_touched = run.iter().map(|e| e.lines_touched).sum();
    let reason = gap_reason_for_via(&run[0].via);
    let single = if run.len() == 1 { Some(&run[0]) } else { None };
    StoryEntry {
        session_id: None,
        sha: single.and_then(|e| e.sha.clone()),
        display_name: None,
        subject: single.and_then(|e| e.subject.clone()),
        confidence: crate::join::ladder::Confidence::None.as_str().to_string(),
        via: run[0].via.clone(),
        first_seen,
        last_seen: Some(last_seen),
        lines_touched,
        status: STATUS_GAP,
        commit_count: Some(run.len() as u32),
        reason: Some(reason),
    }
}

/// Collapse every maximal run of CONSECUTIVE session-less entries (in the
/// already-chronologically-sorted list) into one [`gap_beat`]; covered
/// entries pass through untouched, in place. A run additionally splits
/// where its [`gap_reason_for_via`] changes — "no captured session" and
/// "join unavailable" are different claims and are never blended into one
/// beat. The IDENTITY function on an all-covered list — that (plus the
/// `skip_serializing_if` on every gap-only field) is what keeps a fully
/// covered story byte-for-byte identical to its pre-CT-E2 shape.
fn collapse_uncovered_runs(entries: Vec<StoryEntry>) -> Vec<StoryEntry> {
    let mut out: Vec<StoryEntry> = Vec::with_capacity(entries.len());
    let mut run: Vec<StoryEntry> = Vec::new();
    for e in entries {
        if is_uncovered(&e) {
            if let Some(last) = run.last() {
                if gap_reason_for_via(&last.via) != gap_reason_for_via(&e.via) {
                    out.push(gap_beat(std::mem::take(&mut run)));
                }
            }
            run.push(e);
        } else {
            if !run.is_empty() {
                out.push(gap_beat(std::mem::take(&mut run)));
            }
            out.push(e);
        }
    }
    if !run.is_empty() {
        out.push(gap_beat(run));
    }
    out
}

/// `GET /api/story?repo=&path=[&symbol=]`.
pub async fn story(
    State(state): State<SharedState>,
    Query(params): Query<StoryParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?;
    let symbol_range = match params.symbol.as_deref() {
        Some(name) => {
            // `resolve_symbol_range` keeps its own `&SharedState` signature
            // (a sync helper) — the wrap happens here, at the async
            // boundary (store.rs's 2026-08-31 incident note).
            let state_c = state.clone();
            let path_c = path.to_string();
            let name_c = name.to_string();
            Some(
                state
                    .store
                    .run_blocking(move |_store| {
                        resolve_symbol_range(&state_c, repo_id, &path_c, &name_c)
                    })
                    .await?,
            )
        }
        None => None,
    };
    let mut out = build_story(&state, repo, repo_id, path, symbol_range).await?;
    out.symbol = params.symbol;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// The named symbol's current `(line_start, line_end)` in `path` — 404 when
/// no symbol of that name is currently indexed for this file.
fn resolve_symbol_range(
    state: &SharedState,
    repo_id: i64,
    path: &str,
    symbol: &str,
) -> Result<(u32, u32), ApiError> {
    let symbols = state.store.symbols_for_repo(repo_id)?;
    symbols
        .into_iter()
        .find(|(p, s)| p == path && s.name == symbol)
        .map(|(_, s)| (s.line_start, s.line_end))
        .ok_or_else(|| ApiError::not_found(format!("no symbol {symbol:?} in {path:?}")))
}

/// `pub(crate)` — [`crate::agentview::pack`] reuses this to build each
/// packed file's "recent story entries" section without going through
/// HTTP; `symbol_range: None` (a whole-file story) is pack's only use.
pub(crate) async fn build_story(
    state: &SharedState,
    repo: &RepoEntry,
    repo_id: i64,
    path: &str,
    symbol_range: Option<(u32, u32)>,
) -> Result<StoryOut, ApiError> {
    let regions = provenance::blame_regions(state, repo, repo_id, path, symbol_range).await?;

    let mut sha_cache: HashMap<String, Attribution> = HashMap::new();
    let mut owns: BTreeMap<String, StoryAgg> = BTreeMap::new();
    let mut owning_shas: HashSet<String> = HashSet::new();

    for region in &regions {
        if region.sha == provenance::UNCOMMITTED_SHA {
            continue;
        }
        owning_shas.insert(region.sha.clone());
        let attribution =
            provenance::resolve_sha_cached(repo, repo_id, &region.sha, state, &mut sha_cache).await;
        let key = group_key(&attribution, &region.sha);
        let entry = owns.entry(key).or_insert_with(|| StoryAgg {
            session_id: attribution.session_id.clone(),
            sha: attribution.session_id.is_none().then(|| region.sha.clone()),
            display_name: attribution.display_name.clone(),
            subject: Some(region.subject.clone()),
            confidence: attribution.confidence,
            via: attribution.via.clone(),
            first_seen: region.author_time,
            lines_touched: 0,
            owns_lines: true,
        });
        entry.lines_touched += region.count;
        entry.first_seen = entry.first_seen.min(region.author_time);
    }

    let mut historical: BTreeMap<String, StoryAgg> = BTreeMap::new();
    for region in &regions {
        if region.sha == provenance::UNCOMMITTED_SHA {
            continue;
        }
        let timeline = provenance::line_timeline(state, repo, path, region.final_start).await?;
        for entry_t in timeline {
            if owning_shas.contains(&entry_t.sha) {
                continue; // already covered as owns-lines
            }
            let attribution =
                provenance::resolve_sha_cached(repo, repo_id, &entry_t.sha, state, &mut sha_cache)
                    .await;
            let key = group_key(&attribution, &entry_t.sha);
            if owns.contains_key(&key) {
                continue; // this identity already owns a line elsewhere
            }
            let bucket = historical.entry(key).or_insert_with(|| StoryAgg {
                session_id: attribution.session_id.clone(),
                sha: attribution
                    .session_id
                    .is_none()
                    .then(|| entry_t.sha.clone()),
                display_name: attribution.display_name.clone(),
                subject: Some(entry_t.subject.clone()),
                confidence: attribution.confidence,
                via: attribution.via.clone(),
                first_seen: entry_t.author_time,
                lines_touched: 0,
                owns_lines: false,
            });
            bucket.lines_touched += 1;
            bucket.first_seen = bucket.first_seen.min(entry_t.author_time);
        }
    }

    let mut all: Vec<(String, StoryAgg)> = owns.into_iter().chain(historical).collect();
    all.sort_by(|(ka, a), (kb, b)| a.first_seen.cmp(&b.first_seen).then_with(|| ka.cmp(kb)));

    let entries =
        collapse_uncovered_runs(all.into_iter().map(|(_, a)| StoryEntry::from(a)).collect());
    Ok(StoryOut {
        repo: repo.name.clone(),
        path: path.to_string(),
        symbol: None,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::join::ladder::Confidence;

    fn attribution(session_id: Option<&str>, confidence: Confidence, via: &str) -> Attribution {
        Attribution {
            schema: crate::join::ladder::SCHEMA,
            confidence,
            via: via.to_string(),
            session_id: session_id.map(str::to_string),
            kb: None,
            display_name: None,
            started_at: None,
            sha: "deadbeef".to_string(),
        }
    }

    #[test]
    fn group_key_prefers_session_id_over_sha() {
        let a = attribution(Some("sess-1"), Confidence::Exact, "by-commit");
        assert_eq!(group_key(&a, "deadbeef"), "sess-1");
    }

    #[test]
    fn group_key_falls_back_to_sha_when_no_session_resolved() {
        let a = attribution(None, Confidence::None, "no-match");
        assert_eq!(group_key(&a, "deadbeef"), "deadbeef");
    }

    #[test]
    fn story_entry_from_agg_labels_status_by_owns_lines() {
        let owning = StoryAgg {
            session_id: Some("sess-1".to_string()),
            sha: None,
            display_name: Some("fixed the gizmo".to_string()),
            subject: Some("fix it".to_string()),
            confidence: Confidence::Trailer,
            via: "commit-trailer".to_string(),
            first_seen: 1_700_000_000,
            lines_touched: 5,
            owns_lines: true,
        };
        let entry: StoryEntry = owning.into();
        assert_eq!(entry.status, "owns-lines");
        assert_eq!(entry.session_id.as_deref(), Some("sess-1"));
        assert_eq!(entry.sha, None);

        let historical = StoryAgg {
            session_id: None,
            sha: Some("cafef00d".to_string()),
            display_name: None,
            subject: Some("an old drive-by edit".to_string()),
            confidence: Confidence::None,
            via: "no-match".to_string(),
            first_seen: 1_600_000_000,
            lines_touched: 1,
            owns_lines: false,
        };
        let entry2: StoryEntry = historical.into();
        assert_eq!(entry2.status, "historical");
        assert_eq!(entry2.sha.as_deref(), Some("cafef00d"));
        assert_eq!(entry2.session_id, None);
    }

    // --- the attention-gap collapse (CT-E2) --------------------------------

    /// A covered entry — the shape every resolving ladder arm produces
    /// (`session_id` present).
    fn covered(session_id: &str, first_seen: i64, lines: u32) -> StoryEntry {
        StoryEntry {
            session_id: Some(session_id.to_string()),
            sha: None,
            display_name: Some(format!("session {session_id}")),
            subject: Some(format!("subject of {session_id}")),
            confidence: "trailer".to_string(),
            via: "commit-trailer".to_string(),
            first_seen,
            last_seen: None,
            lines_touched: lines,
            status: "owns-lines",
            commit_count: None,
            reason: None,
        }
    }

    /// A session-less entry — what a `Confidence::None` resolution groups
    /// into (keyed, and identified, by its bare sha).
    fn uncovered(sha: &str, via: &str, first_seen: i64, lines: u32) -> StoryEntry {
        StoryEntry {
            session_id: None,
            sha: Some(sha.to_string()),
            display_name: None,
            subject: Some(format!("subject of {sha}")),
            confidence: "none".to_string(),
            via: via.to_string(),
            first_seen,
            last_seen: None,
            lines_touched: lines,
            status: "historical",
            commit_count: None,
            reason: None,
        }
    }

    /// The load-bearing "additive" pin: an all-covered story is BYTE-FOR-BYTE
    /// unchanged by the collapse — the function is the identity and no
    /// gap-only field ever serializes.
    #[test]
    fn all_covered_story_is_byte_for_byte_unchanged_by_the_collapse() {
        let entries = vec![
            covered("sess-a", 100, 3),
            covered("sess-b", 200, 1),
            covered("sess-c", 300, 7),
        ];
        let before = serde_json::to_string(&entries).unwrap();
        let after = serde_json::to_string(&collapse_uncovered_runs(entries)).unwrap();
        assert_eq!(before, after);
        assert!(
            !after.contains("commit_count")
                && !after.contains("reason")
                && !after.contains("last_seen"),
            "no gap-only field may serialize on a fully covered story: {after}"
        );
    }

    /// A run of consecutive session-less commits collapses to ONE gap beat
    /// carrying the count, the author-time range, and the summed lines —
    /// never one beat per commit.
    #[test]
    fn uncovered_run_collapses_to_one_gap_beat_with_count_and_range() {
        let entries = vec![
            uncovered("aaa1", "no-match", 100, 2),
            uncovered("bbb2", "no-match", 200, 1),
            uncovered("ccc3", "no-match", 300, 4),
        ];
        let out = collapse_uncovered_runs(entries);
        assert_eq!(out.len(), 1);
        let gap = &out[0];
        assert_eq!(gap.status, STATUS_GAP);
        assert_eq!(gap.commit_count, Some(3));
        assert_eq!(gap.first_seen, 100);
        assert_eq!(gap.last_seen, Some(300));
        assert_eq!(gap.lines_touched, 7);
        assert_eq!(gap.reason, Some(GAP_REASON_NO_SESSION));
        // Multi-commit gap: per-commit identity is dropped (the beat speaks
        // for the RUN; per-commit detail lives in blame/timeline).
        assert_eq!(gap.sha, None);
        assert_eq!(gap.subject, None);
        assert_eq!(gap.session_id, None);
    }

    /// Mixed coverage: covered beats pass through in place, gap beats sit
    /// exactly where their runs sat — chronological order is preserved.
    #[test]
    fn mixed_coverage_orders_beats_correctly() {
        let entries = vec![
            covered("sess-a", 100, 3),
            uncovered("aaa1", "no-match", 200, 1),
            uncovered("bbb2", "no-match", 300, 2),
            covered("sess-b", 400, 5),
            uncovered("ccc3", "no-match", 500, 1),
        ];
        let out = collapse_uncovered_runs(entries);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].session_id.as_deref(), Some("sess-a"));
        assert_eq!(out[1].status, STATUS_GAP);
        assert_eq!(out[1].commit_count, Some(2));
        assert_eq!((out[1].first_seen, out[1].last_seen), (200, Some(300)));
        assert_eq!(out[2].session_id.as_deref(), Some("sess-b"));
        assert_eq!(out[3].status, STATUS_GAP);
        // A single-commit gap keeps its sha + subject — nothing to drown.
        assert_eq!(out[3].commit_count, Some(1));
        assert_eq!(out[3].sha.as_deref(), Some("ccc3"));
        assert_eq!(out[3].subject.as_deref(), Some("subject of ccc3"));
        assert_eq!((out[3].first_seen, out[3].last_seen), (500, Some(500)));
    }

    /// The honesty split: `no-match` backs the definite "no captured
    /// session" claim; `kb-unreachable`/`kb-disabled` (and any unknown via
    /// — fail-honest) only back "join unavailable", and a reason change
    /// SPLITS the run rather than blending the two claims.
    #[test]
    fn gap_reason_distinguishes_no_session_from_join_unavailable_and_splits_runs() {
        assert_eq!(gap_reason_for_via("no-match"), GAP_REASON_NO_SESSION);
        assert_eq!(
            gap_reason_for_via("kb-unreachable"),
            GAP_REASON_JOIN_UNAVAILABLE
        );
        assert_eq!(
            gap_reason_for_via("kb-disabled"),
            GAP_REASON_JOIN_UNAVAILABLE
        );
        assert_eq!(
            gap_reason_for_via("some-future-via"),
            GAP_REASON_JOIN_UNAVAILABLE
        );

        let entries = vec![
            uncovered("aaa1", "no-match", 100, 1),
            uncovered("bbb2", "kb-unreachable", 200, 1),
            uncovered("ccc3", "kb-unreachable", 300, 1),
        ];
        let out = collapse_uncovered_runs(entries);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].reason, Some(GAP_REASON_NO_SESSION));
        assert_eq!(out[0].commit_count, Some(1));
        assert_eq!(out[1].reason, Some(GAP_REASON_JOIN_UNAVAILABLE));
        assert_eq!(out[1].commit_count, Some(2));
        assert_eq!(out[1].via, "kb-unreachable");
    }

    /// The gap beat's wire shape: gap-only fields serialize on a gap beat
    /// (and `session_id`/`display_name` stay absent).
    #[test]
    fn gap_beat_serializes_count_range_and_reason() {
        let out = collapse_uncovered_runs(vec![
            uncovered("aaa1", "no-match", 100, 2),
            uncovered("bbb2", "no-match", 300, 1),
        ]);
        let v = serde_json::to_value(&out[0]).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "confidence": "none",
                "via": "no-match",
                "first_seen": 100,
                "last_seen": 300,
                "lines_touched": 3,
                "status": "gap",
                "commit_count": 2,
                "reason": "no-captured-session"
            })
        );
    }
}
