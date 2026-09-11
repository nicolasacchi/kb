//! S2-A ("One Inbox," kb-code v6.0, design doc `/tmp/design-s2.md` § S2-A)
//! — `GET /api/inbox`: a federated attention queue composing
//! kb-code's own review/annotation lanes with a live, degrade-honest
//! pull from kb, plus V76-R3b's worktrees lane. Never a merged cross-lane score (surfaced-never-scored,
//! same law `review_inbox`'s own doc names for ITS score): the lanes have
//! incommensurable units, so each keeps its own source ordering and the
//! response is a plain three-way JSON split, `unified-inbox/1`.
//!
//! # Reviews lane
//!
//! Verbatim reuse of [`crate::review_inbox`]'s own composition
//! ([`review_inbox::compose_rows`] + [`review_inbox::sort_inbox_rows`] +
//! [`review_inbox::inbox_row_to_json`], all widened to `pub(crate)` for
//! this — see that module's doc for the score formula) over EVERY
//! configured repo, `state=open`, capped at [`LANE_CAP`]. Two independent
//! scoring/sorting implementations would drift the moment either route's
//! caller edited only one; this route calls the SAME functions `GET
//! /api/reviews/inbox` does, never a second copy.
//!
//! # Annotations lane
//!
//! Every OPEN, TOP-LEVEL, WORKING-TREE (`review_id IS NULL`) annotation
//! across every configured repo whose `intent` is `question` or
//! `flag-for-agent`, newest `updated_at` first, capped at [`LANE_CAP`].
//! `review_id IS NULL` is load-bearing: a review-scoped open question is
//! ALREADY counted by the reviews lane's own `unanswered_questions` term
//! (`review_inbox`'s module doc) — showing it again here would double-
//! count the same attention item across two lanes. [`store::Store::
//! list_open_working_tree_annotations`] enforces the filter at the SQL
//! layer, not as a post-hoc scan, so this lane can never accidentally
//! regress into re-showing a review comment.
//!
//! # kb lane
//!
//! Two concurrent (`tokio::join!`) federated pulls — [`KbClient::desk`]
//! and [`KbClient::open_comments`] — relayed through in the SAME "kb-code
//! does not re-model kb's wire shapes" posture those methods themselves
//! document. Degradation is HONEST and never a 500: any kb-side failure
//! collapses the WHOLE kb lane to `{available:false, reason, desk:null,
//! comments:null}` (the design doc's own pinned shape — there is no
//! partial success shown to the caller even when only one of the two
//! pulls failed, since a client rendering "half a kb lane" has no honest
//! story to tell about the missing half); the OTHER two lanes render
//! regardless (`kb-code lanes always render` — the design doc's own
//! words). `reason` is a closed vocabulary — see [`kb_error_reason`].
//! When BOTH pulls fail with different errors, the `desk` pull's reason
//! wins (an arbitrary but DETERMINISTIC tiebreak, documented here since
//! the design doc names the vocabulary but not a tiebreak).
//!
//! Each of `desk`/`comments` is independently truncated to [`LANE_CAP`]
//! items AFTER the fetch (kb's own `desk` route has no `limit` param at
//! all, and `comments` is over-fetched by one so a same-size response
//! can be told apart from a truncated one), each carrying its own
//! `"truncated": bool` — see [`compose_kb_lane`].

use crate::annotations;
use crate::join::kb_client::KbClientError;
use crate::review_inbox;
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{AnnotationRow, StoreBlocking};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use kb_core::review::Anchor;

pub const SCHEMA: &str = "unified-inbox/1";

/// Per-lane output cap — pinned by the design doc ("capped 50") for all
/// three lanes (reviews / annotations / EACH of kb's two sub-objects).
pub const LANE_CAP: usize = 50;

/// How many chars of an annotation's body ride along as its `excerpt` —
/// mirrors kb's own `routes/inbox.rs::EXCERPT_CHARS` (same number, same
/// "first N chars of the thread-root body" contract), independently named
/// here since this crate has no dependency on kb-server's route module.
const EXCERPT_CHARS: usize = 200;

/// The `intent` set the annotations lane surfaces — see the module doc.
/// A `const` array (not a `Vec`) so `&INTENTS` is a `&[&str]` with no
/// per-request allocation.
const ANNOTATION_INTENTS: [&str; 2] = [
    annotations::INTENT_QUESTION,
    annotations::INTENT_FLAG_FOR_AGENT,
];

/// `GET /api/inbox` (S2-A) — see the module doc for the three lanes.
/// Ordinary `auth_bearer` read, same sensitivity class as `GET
/// /api/reviews/inbox`/`GET /api/annotations/open` (both of which this
/// route draws from) — nothing here is more sensitive than either already
/// exposed alone.
pub async fn unified_inbox_route(
    State(state): State<SharedState>,
) -> Result<impl IntoResponse, ApiError> {
    let repo_names: Vec<String> = state.repos.iter().map(|r| r.name.clone()).collect();

    // 2026-08-31 incident (store.rs module doc): reviews-lane compose +
    // annotations-lane compose are both pure store-backed reads — each
    // gets its own blocking-pool trip (the kb-lane's async KbClient pulls
    // below stay OUTSIDE any closure, per the brief).
    let repo_names_c = repo_names.clone();
    let mut review_rows = state
        .store
        .run_blocking(move |store| review_inbox::compose_rows(store, &repo_names_c, Some("open")))
        .await?;
    review_inbox::sort_inbox_rows(&mut review_rows);
    review_rows.truncate(LANE_CAP);
    let reviews: Vec<serde_json::Value> = review_rows
        .iter()
        .map(review_inbox::inbox_row_to_json)
        .collect();

    // --- annotations lane. -----------------------------------------------
    let state_bg = state.clone();
    let annotations = state
        .store
        .run_blocking(move |_store| compose_annotations_lane(&state_bg))
        .await?;

    // --- kb lane — two concurrent federated pulls, honest degrade. -------
    let kb = compose_kb_lane(&state).await;

    // --- worktrees lane (V76-R3b). Surfaced-never-scored; degrades
    // honestly when the workspace table is empty. -----------------------
    let state_wt = state.clone();
    let worktrees = state
        .store
        .run_blocking(move |_store| crate::worktrees::compose_inbox_lane(&state_wt))
        .await;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "reviews": reviews,
            "annotations": annotations,
            "kb": kb,
            "worktrees": worktrees,
        })),
    ))
}

/// The annotations lane — see the module doc. Fetches up to `LANE_CAP + 1`
/// rows PER REPO (sufficient to guarantee the true global top-[`LANE_CAP`]
/// is never missed once every repo's candidates are merged and re-sorted —
/// no single repo can contribute more than [`LANE_CAP`] rows to a
/// [`LANE_CAP`]-sized final result), then merges, re-sorts by `updated_at`
/// descending (tiebreak `id` ascending, a total order so repeated calls
/// over unchanged state are byte-identical), and truncates once more to
/// the final cap.
fn compose_annotations_lane(state: &SharedState) -> Result<Vec<serde_json::Value>, ApiError> {
    let mut rows: Vec<(String, AnnotationRow, i64)> = Vec::new();
    for repo in &state.repos {
        let repo_id = *state.repo_ids.get(&repo.name).ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("repo {:?} is configured but has no store id", repo.name),
            )
        })?;
        let repo_rows = state.store.list_open_working_tree_annotations(
            repo_id,
            &ANNOTATION_INTENTS,
            LANE_CAP + 1,
        )?;
        for (row, reply_count) in repo_rows {
            rows.push((repo.name.clone(), row, reply_count));
        }
    }
    rows.sort_by(|(_, a, _), (_, b, _)| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    rows.truncate(LANE_CAP);

    Ok(rows
        .into_iter()
        .map(|(repo, row, reply_count)| annotation_row_to_json(&repo, &row, reply_count))
        .collect())
}

/// The `line?` field of one annotations-lane row — the anchor's own
/// recorded `Selection.offset` when `anchor` parses as one (the `line` /
/// `range` / `symbol` / `diff` kinds all store a `Selection` in this
/// column — see `crate::annotations`'s module doc), else `None` (the
/// `whole_file` kind stores a bare path string, and `review` stores
/// nothing — neither is a valid `Anchor` JSON object, so the parse simply
/// fails and this honestly returns `None` rather than guessing). Never
/// RE-RESOLVED against the current working tree (unlike `routes::
/// annotation_view`) — this is an attention-queue listing, not a
/// precision-position read, and re-resolving every open annotation's
/// anchor against a freshly-read file on every `/api/inbox` call would add
/// per-row disk IO this lane doesn't need.
fn annotation_line(row: &AnnotationRow) -> Option<u32> {
    let raw = row.anchor.as_deref()?;
    match serde_json::from_str::<Anchor>(raw) {
        Ok(Anchor::Selection { offset, .. }) => Some(offset),
        _ => None,
    }
}

/// One annotations-lane row, in the design doc's pinned shape: `{id, repo,
/// path, line?, intent, author, excerpt (200 chars), reply_count,
/// updated_at}`.
fn annotation_row_to_json(repo: &str, row: &AnnotationRow, reply_count: i64) -> serde_json::Value {
    let excerpt: String = row.body.trim().chars().take(EXCERPT_CHARS).collect();
    serde_json::json!({
        "id": row.id,
        "repo": repo,
        "path": row.path,
        "line": annotation_line(row),
        "intent": row.intent,
        "author": row.author,
        "excerpt": excerpt,
        "reply_count": reply_count,
        "updated_at": row.updated_at,
    })
}

/// Map a [`KbClientError`] onto the design doc's closed `reason`
/// vocabulary — `"disabled" | "unreachable" | "sibling_mismatch"`.
/// `BadStatus`/`Parse` (and every other variant — `ClientBuild`/
/// `InvalidPath`, neither of which [`crate::join::kb_client::KbClient::
/// desk`]/`open_comments` can actually produce today, but the match stays
/// total rather than assuming that never changes) all degrade to the SAME
/// `"unreachable"` reason, per the design doc's own mapping: "kb answered,
/// but not usefully" and "kb never answered" are the same actionable fact
/// to a caller of this route.
fn kb_error_reason(err: &KbClientError) -> &'static str {
    match err {
        KbClientError::Disabled => "disabled",
        KbClientError::SiblingMismatch(_) => "sibling_mismatch",
        KbClientError::Unreachable(_, _)
        | KbClientError::BadStatus(_)
        | KbClientError::Parse(_)
        | KbClientError::ClientBuild(_)
        | KbClientError::InvalidPath(_) => "unreachable",
    }
}

/// The kb lane — see the module doc's "kb lane" section for the
/// all-or-nothing degrade contract and the desk-wins tiebreak.
async fn compose_kb_lane(state: &SharedState) -> serde_json::Value {
    let (desk_result, comments_result) = tokio::join!(
        state.kb_client.desk(),
        state.kb_client.open_comments((LANE_CAP + 1) as u32),
    );
    match (desk_result, comments_result) {
        (Ok(desk), Ok(comments)) => {
            let mut desk_items = desk.items;
            let desk_truncated = desk_items.len() > LANE_CAP;
            desk_items.truncate(LANE_CAP);

            let mut comment_items = comments.items;
            let comments_truncated = comment_items.len() > LANE_CAP;
            comment_items.truncate(LANE_CAP);

            serde_json::json!({
                "available": true,
                "reason": serde_json::Value::Null,
                "desk": {
                    "items": desk_items,
                    "attention": desk.attention,
                    "truncated": desk_truncated,
                },
                "comments": {
                    "items": comment_items,
                    "total_open": comments.total_open,
                    "truncated": comments_truncated,
                },
            })
        }
        (desk_result, comments_result) => {
            // desk's error wins on a double failure — see the module doc.
            let reason = desk_result
                .err()
                .or_else(|| comments_result.err())
                .as_ref()
                .map(kb_error_reason)
                .unwrap_or("unreachable");
            serde_json::json!({
                "available": false,
                "reason": reason,
                "desk": serde_json::Value::Null,
                "comments": serde_json::Value::Null,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ann(id: &str, intent: &str, review_id: Option<i64>, resolved: bool) -> AnnotationRow {
        AnnotationRow {
            id: id.to_string(),
            repo_id: 1,
            path: "a.rs".to_string(),
            anchor: Some(
                r#"{"kind":"selection","css_path":"","offset":9,"snippet":"fn a() {}"}"#
                    .to_string(),
            ),
            anchor_kind: "line".to_string(),
            anchor2: None,
            parent_id: None,
            intent: intent.to_string(),
            body: "why is this here?".to_string(),
            author: "you".to_string(),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
            resolved,
            review_id,
            ps_number: None,
            side: None,
            set_id: None,
            trail_id: None,
        }
    }

    #[test]
    fn annotation_line_reads_the_selection_offset() {
        let row = ann("ann_1", "question", None, false);
        assert_eq!(annotation_line(&row), Some(9));
    }

    #[test]
    fn annotation_line_is_none_for_a_whole_file_bare_path_anchor() {
        let mut row = ann("ann_1", "question", None, false);
        row.anchor_kind = annotations::ANCHOR_KIND_WHOLE_FILE.to_string();
        row.anchor = Some("src/lib.rs".to_string());
        assert_eq!(annotation_line(&row), None);
    }

    #[test]
    fn annotation_line_is_none_when_anchor_is_absent() {
        let mut row = ann("ann_1", "question", None, false);
        row.anchor = None;
        assert_eq!(annotation_line(&row), None);
    }

    #[test]
    fn annotation_row_to_json_has_the_pinned_shape_and_a_200_char_excerpt() {
        let mut row = ann("ann_1", "flag-for-agent", None, false);
        row.body = "x".repeat(300);
        let json = annotation_row_to_json("kb", &row, 4);
        assert_eq!(json["id"], "ann_1");
        assert_eq!(json["repo"], "kb");
        assert_eq!(json["path"], "a.rs");
        assert_eq!(json["line"], 9);
        assert_eq!(json["intent"], "flag-for-agent");
        assert_eq!(json["author"], "you");
        assert_eq!(json["excerpt"].as_str().unwrap().len(), EXCERPT_CHARS);
        assert_eq!(json["reply_count"], 4);
        assert_eq!(json["updated_at"], 1_700_000_000);
    }

    #[test]
    fn kb_error_reason_maps_the_closed_vocabulary() {
        assert_eq!(kb_error_reason(&KbClientError::Disabled), "disabled");
        assert_eq!(
            kb_error_reason(&KbClientError::SiblingMismatch("x".to_string())),
            "sibling_mismatch"
        );
        assert_eq!(
            kb_error_reason(&KbClientError::Unreachable(
                "http://x".to_string(),
                "boom".to_string()
            )),
            "unreachable"
        );
        assert_eq!(
            kb_error_reason(&KbClientError::BadStatus(
                reqwest::StatusCode::INTERNAL_SERVER_ERROR
            )),
            "unreachable"
        );
        assert_eq!(
            kb_error_reason(&KbClientError::Parse("bad json".to_string())),
            "unreachable"
        );
    }
}
