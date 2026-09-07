//! PRR-R4 ("The PR Room," kb v0.39 T2, Phase 4) — `GET /api/reviews/inbox`
//! (design doc §2 row 13): a cross-repo, cross-review attention queue over
//! every PR-bound AND plain review. Bearer — same ordinary review-read gate
//! as `/comments`/`/distill`/`/findings`. Response shape follows the SAME
//! house convention `crate::review_findings` already deviated to (`{schema,
//! reviews: [...]}` rather than the design doc's bare `[...]` sketch — every
//! sibling read route wraps its array the same way).
//!
//! # Scoring — named terms, never a daemon-authored verdict
//!
//! `score = unanswered_questions * 2 + unresolved_findings` (design doc's
//! own formula), descending; ties broken by `updated_at` descending, then
//! `review_id` ascending — a final, fully deterministic tiebreak the design
//! doc doesn't name, added here so two reviews tied on BOTH the score AND
//! `updated_at` still sort byte-identically across repeated calls (the
//! "same state -> byte-identical order" contract Phase 4's own test plan
//! asks for). [`sort_inbox_rows`] is the pure, unit-tested core — never a
//! quality verdict, mirrors `behavioral::risk`'s own "every score decomposes
//! into named terms" law.
//!
//! # `unanswered_questions` — derivation (approximate, documented per the
//! milestone brief's own invitation to do so)
//!
//! The design doc names the term but not its derivation; the milestone
//! brief elaborates "open question-intent threads whose last voice is not
//! the asker." Read LITERALLY, that would never count a question with ZERO
//! replies (its only voice trivially IS the asker), which defeats the
//! purpose of an attention queue — a completely unanswered question with no
//! replies at all is exactly the case most needing surfacing. [`thread_is_
//! unanswered`] instead implements the common review-tool "awaiting reply"
//! convention (GitHub's own "awaiting your reply" framing): a question
//! thread counts as unanswered when its most recent voice — the question
//! itself if it has zero replies, else its most recent reply — is the SAME
//! author as the original asker, i.e. nobody but the asker has spoken most
//! recently. **This is a deliberate, documented DEVIATION from the brief's
//! literal wording** — flagged here and in this unit's own report for the
//! operator to correct if a different reading was intended.
//!
//! # `pr_head_drift` — LOCAL-only, by design
//!
//! Design doc §2 row 13, verbatim: "`pr_head_drift` uses ONLY the cached
//! local half of the staleness probe (no live GitHub call per row — an
//! N-request storm on one inbox load is not acceptable)." Computed as
//! `stored pr_head_sha != latest local ps tip` — the SAME local comparison
//! `GET /reviews/{id}/pr-status`'s own local half makes (`reviews::
//! local_pr_status`, duplicated here as a two-line inline comparison rather
//! than imported, since importing a `fn` whose OTHER half — the live
//! GitHub call — this route explicitly must never perform would invite a
//! future edit to "helpfully" wire the two together). `None` for a review
//! with no PR binding at all (nothing to compare, never coerced to a
//! misleading `false`).

use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{self, AnnotationRow, StoreBlocking};
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::collections::HashMap;

pub const SCHEMA: &str = "review-inbox/1";

/// Fixed ceiling on rows returned — same "plain fixed cap, not a paginated
/// surface" convention as `github::MAX_PRS`/`MAX_COMMENTS`.
pub const INBOX_MAX_LIMIT: usize = 200;
pub const INBOX_DEFAULT_LIMIT: usize = 50;

#[derive(Debug, Deserialize, Default)]
pub struct InboxParams {
    #[serde(default)]
    pub repo: Option<String>,
    /// `open` (default) | `closed` | `all`.
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One inbox row, BEFORE JSON composition — the shape [`sort_inbox_rows`]
/// operates on, so the scoring/tiebreak logic is unit-testable without an
/// HTTP round trip or a repo/daemon. `pub` (PF-K1, widened from
/// `pub(crate)`) so `tests/measure/latency.rs`'s PR Room budget can call
/// [`compose_rows`] directly, in-process, without a daemon.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxRow {
    pub review_id: i64,
    pub repo: String,
    pub pr_number: Option<i64>,
    pub title: Option<String>,
    pub unresolved_findings: i64,
    pub unanswered_questions: i64,
    pub verdict: serde_json::Value,
    pub verdict_stale: bool,
    pub pr_head_drift: Option<bool>,
    pub updated_at: i64,
}

/// `score = unanswered_questions*2 + unresolved_findings` (design doc §2
/// row 13, verbatim) — never a quality verdict; every term is named and
/// surfaced on the row itself.
pub(crate) fn inbox_score(row: &InboxRow) -> i64 {
    row.unanswered_questions * 2 + row.unresolved_findings
}

/// Descending score; ties by `updated_at` descending; final tiebreak
/// `review_id` ascending (see the module doc). A stable, TOTAL order over
/// any input, so repeated calls over unchanged state produce byte-identical
/// output regardless of the rows' original (e.g. per-repo scan) order.
pub(crate) fn sort_inbox_rows(rows: &mut [InboxRow]) {
    rows.sort_by(|a, b| {
        inbox_score(b)
            .cmp(&inbox_score(a))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.review_id.cmp(&b.review_id))
    });
}

/// See the module doc's "unanswered_questions — derivation" section.
/// `asker` is the top-level question annotation's own `author`; `replies`
/// need not be pre-sorted (this picks the max by `(created_at, id)` itself,
/// so callers can hand it whatever order `list_review_annotations`
/// returns).
pub(crate) fn thread_is_unanswered(asker: &str, replies: &[&AnnotationRow]) -> bool {
    match replies.iter().max_by_key(|r| (r.created_at, r.id.clone())) {
        Some(last) => last.author == asker,
        None => true,
    }
}

/// The per-repo scan + per-review scoring composition — the reusable HALF
/// of [`list_inbox_route`], factored out (S2-A, kb-code v6.0 "One Inbox")
/// so `crate::unified_inbox`'s `reviews` lane calls this SAME logic rather
/// than a second copy: two independent implementations of "unresolved_
/// findings"/"unanswered_questions"/`pr_head_drift` would drift the moment
/// either route's caller edits only one. Returns UNSORTED, UNTRUNCATED rows
/// — the caller runs [`sort_inbox_rows`] + its own `truncate` (this route's
/// `limit` and the unified inbox's fixed cap are independent policy, not
/// this fn's concern). `pub` (PF-K1, widened from `pub(crate)`) — beyond
/// this crate's OWN two callers, `tests/measure/latency.rs`'s PR Room
/// budget calls this directly, in-process, without a daemon.
///
/// 2026-08-31 incident (store.rs module doc): takes `&Store` (not
/// `&SharedState`, its only prior use) so every async caller wraps the
/// whole repo×review scan in ONE `run_blocking` closure.
///
/// PF-K1 — replaces the old per-review `latest_patchset` / `get_review_pr_
/// binding` / `list_review_findings` / `list_review_annotations` fan-out
/// (each its own round trip PER REVIEW) with one batched pass PER REPO:
/// every review id from that repo's `list_reviews` call is collected up
/// front, then each of the four lookups runs ONCE over the whole set via
/// `store::Store::latest_patchsets` / `get_review_pr_bindings` (shared with
/// `crate::reviews::compose_review_list_rows`) / `list_review_findings_
/// batch` / `list_review_annotations_batch`. Row composition + scoring
/// logic is byte-for-byte the same per-review arithmetic as before, just
/// reading from the batched maps instead of a fresh query each time; row
/// PUSH order is unchanged (still repo_names order, then each repo's own
/// `list_reviews` order), so [`sort_inbox_rows`]'s tiebreak contract is
/// unaffected either way.
pub fn compose_rows(
    store: &store::Store,
    repo_names: &[String],
    state_filter: Option<&str>,
) -> Result<Vec<InboxRow>, ApiError> {
    let mut reviews_by_repo: Vec<Vec<store::ReviewRow>> = Vec::with_capacity(repo_names.len());
    let mut review_ids: Vec<i64> = Vec::new();
    for repo_name in repo_names {
        let reviews = store.list_reviews(repo_name, state_filter)?;
        review_ids.extend(reviews.iter().map(|r| r.id));
        reviews_by_repo.push(reviews);
    }
    if review_ids.is_empty() {
        return Ok(Vec::new());
    }

    let latest_map = store.latest_patchsets(&review_ids)?;
    let binding_map = store.get_review_pr_bindings(&review_ids)?;
    let findings_map = store.list_review_findings_batch(&review_ids, None, false)?;
    let ann_map = store.list_review_annotations_batch(&review_ids, true)?;

    let mut rows = Vec::with_capacity(review_ids.len());
    for reviews in reviews_by_repo {
        for review in reviews {
            let latest_ps = latest_map.get(&review.id).cloned();
            let binding = binding_map.get(&review.id).cloned().unwrap_or_default();

            // "unresolved_findings" — non-superseded, disposition NULL or
            // dispute (design doc §2 row 13).
            let findings = findings_map.get(&review.id).cloned().unwrap_or_default();
            let unresolved_findings = findings
                .iter()
                .filter(|f| {
                    f.disposition.is_none()
                        || f.disposition.as_deref() == Some(store::DISPOSITION_DISPUTE)
                })
                .count() as i64;

            // "unanswered_questions" — see the module doc.
            let ann_rows = ann_map.get(&review.id).cloned().unwrap_or_default();
            let mut by_id: HashMap<String, AnnotationRow> = HashMap::new();
            let mut replies_by_parent: HashMap<String, Vec<AnnotationRow>> = HashMap::new();
            for row in ann_rows {
                match row.parent_id.clone() {
                    Some(pid) => replies_by_parent.entry(pid).or_default().push(row),
                    None => {
                        by_id.insert(row.id.clone(), row);
                    }
                }
            }
            let mut unanswered_questions = 0i64;
            for (aid, ann) in &by_id {
                if ann.intent != crate::annotations::INTENT_QUESTION || ann.resolved {
                    continue;
                }
                let replies: Vec<&AnnotationRow> = replies_by_parent
                    .get(aid)
                    .map(|v| v.iter().collect())
                    .unwrap_or_default();
                if thread_is_unanswered(&ann.author, &replies) {
                    unanswered_questions += 1;
                }
            }

            let (verdict, verdict_stale) =
                crate::reviews::verdict_block(&review, latest_ps.as_ref().map(|p| p.ps_number));
            // pr_head_drift — LOCAL-only (see the module doc).
            let pr_head_drift = binding.pr_number.map(|_| {
                binding.pr_head_sha.as_deref() != latest_ps.as_ref().map(|p| p.tip_sha.as_str())
            });

            rows.push(InboxRow {
                review_id: review.id,
                repo: review.repo.clone(),
                pr_number: binding.pr_number,
                title: review.title.clone(),
                unresolved_findings,
                unanswered_questions,
                verdict,
                verdict_stale,
                pr_head_drift,
                updated_at: review.updated_at,
            });
        }
    }
    Ok(rows)
}

/// One [`InboxRow`] as the `review-inbox/1` wire shape — the SAME mapping
/// both [`list_inbox_route`] and the unified inbox's `reviews` lane emit
/// (S2-A: "verbatim from the existing review_inbox composition"), factored
/// out alongside [`compose_rows`] for the identical reason.
pub(crate) fn inbox_row_to_json(r: &InboxRow) -> serde_json::Value {
    serde_json::json!({
        "review_id": r.review_id,
        "repo": r.repo,
        "pr_number": r.pr_number,
        "title": r.title,
        "unresolved_findings": r.unresolved_findings,
        "unanswered_questions": r.unanswered_questions,
        "verdict": r.verdict,
        "verdict_stale": r.verdict_stale,
        "pr_head_drift": r.pr_head_drift,
        "updated_at": r.updated_at,
    })
}

/// `GET /api/reviews/inbox?repo=&state=open&limit=` (design doc §2 row 13).
/// Bearer. Fans out over every configured repo when `repo` is absent
/// (`state.repos`, single process, sequential — this crate's fan-out is
/// per-request, not multi-corpus federation, so invariant #28's `buffered_
/// join` doesn't apply here); `?repo=` scopes to one (404 on an unknown
/// name, via `find_repo`). `state` defaults to `open` (unlike the base
/// `/reviews` list route, which defaults to unfiltered) — pass
/// `state=all` for the unfiltered view. `limit` caps the OUTPUT (after
/// sorting), default [`INBOX_DEFAULT_LIMIT`], hard-capped at
/// [`INBOX_MAX_LIMIT`].
pub async fn list_inbox_route(
    State(state): State<SharedState>,
    Query(params): Query<InboxParams>,
) -> Result<impl IntoResponse, ApiError> {
    let state_filter: Option<&str> = match params.state.as_deref() {
        None => Some("open"),
        Some("all") => None,
        Some(s @ ("open" | "closed")) => Some(s),
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "state must be open|closed|all, got {other:?}"
            )))
        }
    };
    let limit = params
        .limit
        .unwrap_or(INBOX_DEFAULT_LIMIT)
        .min(INBOX_MAX_LIMIT);

    let repo_names: Vec<String> = match params.repo.as_deref() {
        Some(r) => {
            find_repo(&state, r)?;
            vec![r.to_string()]
        }
        None => state.repos.iter().map(|r| r.name.clone()).collect(),
    };

    let state_filter_owned = state_filter.map(|s| s.to_string());
    let mut rows = state
        .store
        .run_blocking(move |store| compose_rows(store, &repo_names, state_filter_owned.as_deref()))
        .await?;
    sort_inbox_rows(&mut rows);
    rows.truncate(limit);

    let out: Vec<serde_json::Value> = rows.iter().map(inbox_row_to_json).collect();

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "reviews": out,
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(review_id: i64, unanswered: i64, unresolved: i64, updated_at: i64) -> InboxRow {
        InboxRow {
            review_id,
            repo: "r".into(),
            pr_number: None,
            title: None,
            unresolved_findings: unresolved,
            unanswered_questions: unanswered,
            verdict: serde_json::Value::Null,
            verdict_stale: false,
            pr_head_drift: None,
            updated_at,
        }
    }

    #[test]
    fn inbox_score_weights_unanswered_questions_double() {
        assert_eq!(inbox_score(&row(1, 1, 0, 0)), 2);
        assert_eq!(inbox_score(&row(1, 0, 3, 0)), 3);
        assert_eq!(inbox_score(&row(1, 2, 1, 0)), 5);
    }

    #[test]
    fn sort_inbox_rows_orders_by_score_descending() {
        let mut rows = vec![row(1, 0, 1, 100), row(2, 1, 0, 100), row(3, 0, 5, 100)];
        sort_inbox_rows(&mut rows);
        let ids: Vec<i64> = rows.iter().map(|r| r.review_id).collect();
        assert_eq!(ids, vec![3, 2, 1]); // scores 5, 2, 1
    }

    #[test]
    fn sort_inbox_rows_breaks_score_ties_by_updated_at_descending() {
        let mut rows = vec![row(1, 1, 0, 50), row(2, 1, 0, 200), row(3, 1, 0, 100)];
        sort_inbox_rows(&mut rows);
        let ids: Vec<i64> = rows.iter().map(|r| r.review_id).collect();
        assert_eq!(ids, vec![2, 3, 1]); // updated_at 200, 100, 50
    }

    #[test]
    fn sort_inbox_rows_final_tiebreak_is_review_id_ascending_and_is_deterministic() {
        let mut a = vec![row(5, 1, 0, 100), row(2, 1, 0, 100), row(9, 1, 0, 100)];
        let mut b = vec![row(9, 1, 0, 100), row(5, 1, 0, 100), row(2, 1, 0, 100)];
        sort_inbox_rows(&mut a);
        sort_inbox_rows(&mut b);
        let ids_a: Vec<i64> = a.iter().map(|r| r.review_id).collect();
        let ids_b: Vec<i64> = b.iter().map(|r| r.review_id).collect();
        assert_eq!(ids_a, vec![2, 5, 9]);
        assert_eq!(
            ids_a, ids_b,
            "same state must sort byte-identically regardless of input order"
        );
    }

    fn ann(id: &str, author: &str, created_at: i64) -> AnnotationRow {
        AnnotationRow {
            id: id.to_string(),
            repo_id: 1,
            path: String::new(),
            anchor: None,
            anchor_kind: "review".to_string(),
            anchor2: None,
            parent_id: None,
            intent: "question".to_string(),
            body: "q".to_string(),
            author: author.to_string(),
            created_at,
            updated_at: created_at,
            resolved: false,
            review_id: Some(1),
            ps_number: Some(1),
            side: None,
            set_id: None,
            trail_id: None,
        }
    }

    #[test]
    fn thread_is_unanswered_with_zero_replies() {
        assert!(thread_is_unanswered("you", &[]));
    }

    #[test]
    fn thread_is_unanswered_when_the_askers_own_followup_is_the_last_voice() {
        let r1 = ann("r1", "claude", 10);
        let r2 = ann("r2", "you", 20);
        assert!(thread_is_unanswered("you", &[&r1, &r2]));
    }

    #[test]
    fn thread_is_answered_when_someone_else_spoke_most_recently() {
        let r1 = ann("r1", "you", 10);
        let r2 = ann("r2", "claude", 20);
        assert!(!thread_is_unanswered("you", &[&r1, &r2]));
    }

    #[test]
    fn thread_is_unanswered_picks_the_max_by_created_at_regardless_of_input_order() {
        let newest = ann("newest", "you", 30);
        let oldest = ann("oldest", "claude", 5);
        // Handed out of order — the fn must still pick `newest` as "last".
        assert!(thread_is_unanswered("you", &[&newest, &oldest]));
        assert!(!thread_is_unanswered("claude", &[&newest, &oldest]));
    }
}
