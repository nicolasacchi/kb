//! v0.44 F9 — `GET /api/reviews/agent-queue`: what is waiting FOR THE AGENT,
//! computed on each read.
//!
//! The Room works out "awaiting-agent" in the browser only
//! (`web-code/src/lib/questionState.ts`: "a pure client, no new server
//! state"), and `review inbox` is ordered for the HUMAN (it counts a
//! question as unanswered when the last voice is the ASKER, whoever that
//! is). A `/loop` over reviews therefore had no server-side answer to "what
//! should I do next?". This is that answer, in two lanes with named terms
//! and a fixed order:
//!
//!   1. `question` — an open `question` thread whose latest voice is not an
//!      agent (the Rust port of `questionChipState == "awaiting-agent"`;
//!      ONE golden, `grammar/question-state.golden.json`, is read by this
//!      module's test and by `questionState.golden.test.ts`);
//!   2. `dispute` — a finding the human DISPUTED (and which is neither
//!      superseded nor resolved) with no agent reply on its thread at or
//!      after `disposition_at`.
//!
//! Rows sort by lane, then oldest-waiting first, then `(review_id,
//! annotation_id)` — a total order, so repeated reads over unchanged state
//! are byte-identical. Each row carries the `next` argv to run.
//!
//! # What this is not
//!
//!   * Not a score and not a verdict: lanes are named, the order is fixed,
//!     nothing is weighted.
//!   * Not a dispatcher. The daemon spawns no non-git process (crate
//!     invariant 10); dispatch stays with the agent layer (`/loop` plus
//!     `kb-review-work`).
//!   * "No agent reply" is a HEURISTIC: the agent may have answered
//!     somewhere else (a different thread, GitHub). It is a work queue, not
//!     proof of neglect. Agent-ness is a NAME convention
//!     ([`crate::review_timeline::AGENT_AUTHOR_NAMES`]); kb-code
//!     authenticates nobody, so a reply under the human's own name keeps a
//!     row on the queue (that is why the CLI's author ladder defaults
//!     replies to `claude`, never `you`).
//!   * Lanes 3-5 of the design (agree/fix-later findings with no suggestion,
//!     PR head drift, flag-for-agent annotations) are not built; see the
//!     PR that introduced this module.
//!
//! The annotation load is the inbox's: ONE batched query per repo
//! (`list_review_annotations_batch`, `list_review_findings_batch`), never a
//! comment-tree fetch per review.

use crate::review_inbox::is_agent_author;
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{self, AnnotationRow, ReviewFindingRow, StoreBlocking};
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const QUEUE_SCHEMA: &str = "kbc-agent-queue/1";
pub const LANE_QUESTION: &str = "question";
pub const LANE_DISPUTE: &str = "dispute";

/// One voice on a thread (opener or reply).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Voice<'a> {
    pub author: &'a str,
    pub at: i64,
}

/// Port of `questionState.ts`'s `latestVoice` + `voiceIsAgent`: the latest
/// voice is the opener unless a reply is at least as recent (`>=`, so a
/// later entry wins a same-second tie); a finding's own opener is judged by
/// its `origin`, everything else by author name.
fn latest_is_agent(opener: Voice<'_>, replies: &[Voice<'_>], finding_origin: Option<&str>) -> bool {
    let mut latest = opener;
    let mut is_opener = true;
    for r in replies {
        if r.at >= latest.at {
            latest = *r;
            is_opener = false;
        }
    }
    match (is_opener, finding_origin) {
        (true, Some(origin)) => origin == "import",
        _ => is_agent_author(latest.author),
    }
}

/// `questionChipState(..) == "awaiting-agent"`: unresolved, `question`
/// intent, latest voice not an agent. (An agent latest voice is
/// "awaiting-you" in the Room; resolved is nothing.) Pure.
pub fn awaiting_agent(
    intent: &str,
    resolved: bool,
    opener: Voice<'_>,
    replies: &[Voice<'_>],
    finding_origin: Option<&str>,
) -> bool {
    if resolved || intent != crate::annotations::INTENT_QUESTION {
        return false;
    }
    !latest_is_agent(opener, replies, finding_origin)
}

/// A disputed finding the agent has not answered: disposition `dispute`,
/// not superseded, thread not resolved, and no agent reply at or after
/// `disposition_at` (a same-second reply counts as an answer: the human
/// disputes first, so a reply can only be later, and counting it avoids a
/// row that can never clear). Pure.
pub fn dispute_unanswered(
    disposition: Option<&str>,
    superseded: bool,
    disposition_at: Option<i64>,
    thread_resolved: bool,
    replies: &[Voice<'_>],
) -> bool {
    if disposition != Some(store::DISPOSITION_DISPUTE) || superseded || thread_resolved {
        return false;
    }
    let since = disposition_at.unwrap_or(0);
    !replies
        .iter()
        .any(|r| r.at >= since && is_agent_author(r.author))
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueueRow {
    // `question` | `dispute`
    pub lane: String,
    // 1 for `question`, 2 for `dispute` (the fixed lane order)
    pub lane_order: u32,
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub review_id: i64,
    pub repo: String,
    #[cfg_attr(feature = "ts-export", ts(type = "number | null"))]
    pub pr_number: Option<i64>,
    pub annotation_id: String,
    pub path: String,
    #[cfg_attr(feature = "ts-export", ts(type = "number | null"))]
    pub ps_number: Option<i64>,
    pub finding_slug: Option<String>,
    // when the thread/dispute started waiting (unix seconds)
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub waiting_since: i64,
    // whose turn-taking this is waiting on: the human who asked/disputed
    pub from: String,
    // argv the agent runs next
    pub next: Vec<Vec<String>>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentQueue {
    pub schema: String,
    pub rows: Vec<QueueRow>,
    pub count: usize,
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn sort_replies(replies: &mut [&AnnotationRow]) {
    replies.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
}

fn voices_of<'a>(
    replies_by_parent: &HashMap<&'a str, Vec<&'a AnnotationRow>>,
    id: &str,
) -> Vec<Voice<'a>> {
    replies_by_parent
        .get(id)
        .map(|v| {
            v.iter()
                .map(|r| Voice {
                    author: r.author.as_str(),
                    at: r.created_at,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One review's queue rows from its already-loaded rows. Pure.
pub fn rows_for_review(
    review_id: i64,
    repo: &str,
    pr_number: Option<i64>,
    annotations: &[AnnotationRow],
    findings: &[ReviewFindingRow],
) -> Vec<QueueRow> {
    let mut threads: HashMap<&str, &AnnotationRow> = HashMap::new();
    let mut replies_by_parent: HashMap<&str, Vec<&AnnotationRow>> = HashMap::new();
    for a in annotations {
        match a.parent_id.as_deref() {
            Some(pid) => replies_by_parent.entry(pid).or_default().push(a),
            None => {
                threads.insert(a.id.as_str(), a);
            }
        }
    }
    for v in replies_by_parent.values_mut() {
        sort_replies(v);
    }
    let finding_by_ann: HashMap<&str, &ReviewFindingRow> = findings
        .iter()
        .filter(|f| !f.superseded)
        .map(|f| (f.annotation_id.as_str(), f))
        .collect();
    let id_s = review_id.to_string();

    let mut out: Vec<QueueRow> = Vec::new();
    for (aid, t) in &threads {
        let replies = voices_of(&replies_by_parent, aid);
        let opener = Voice {
            author: t.author.as_str(),
            at: t.created_at,
        };
        let origin = finding_by_ann.get(aid).map(|f| f.origin.as_str());
        if awaiting_agent(&t.intent, t.resolved, opener, &replies, origin) {
            // Waiting since the last human voice (the opener or the latest
            // reply), which is when the agent's turn began.
            let since = replies
                .iter()
                .map(|r| r.at)
                .chain(std::iter::once(opener.at))
                .max()
                .unwrap_or(opener.at);
            out.push(QueueRow {
                lane: LANE_QUESTION.to_string(),
                lane_order: 1,
                review_id,
                repo: repo.to_string(),
                pr_number,
                annotation_id: t.id.clone(),
                path: t.path.clone(),
                ps_number: t.ps_number,
                finding_slug: finding_by_ann.get(aid).map(|f| f.slug.clone()),
                waiting_since: since,
                from: t.author.clone(),
                next: vec![argv(&["kb-code", "review", "comments", &id_s, "--json"])],
            });
        }
    }
    for f in findings {
        if f.superseded {
            continue;
        }
        let Some(t) = threads.get(f.annotation_id.as_str()) else {
            continue;
        };
        let replies = voices_of(&replies_by_parent, &f.annotation_id);
        if dispute_unanswered(
            f.disposition.as_deref(),
            f.superseded,
            f.disposition_at,
            t.resolved,
            &replies,
        ) {
            out.push(QueueRow {
                lane: LANE_DISPUTE.to_string(),
                lane_order: 2,
                review_id,
                repo: repo.to_string(),
                pr_number,
                annotation_id: f.annotation_id.clone(),
                path: f.location_path.clone(),
                ps_number: t.ps_number,
                finding_slug: Some(f.slug.clone()),
                waiting_since: f.disposition_at.unwrap_or(t.created_at),
                from: f.disposition_by.clone().unwrap_or_default(),
                next: vec![argv(&[
                    "kb-code", "review", "findings", "list", &id_s, "--json",
                ])],
            });
        }
    }
    out
}

/// Fixed lane order, then oldest-waiting first, then a total tiebreak.
pub fn sort_queue(rows: &mut [QueueRow]) {
    rows.sort_by(|a, b| {
        (a.lane_order, a.waiting_since, a.review_id, &a.annotation_id).cmp(&(
            b.lane_order,
            b.waiting_since,
            b.review_id,
            &b.annotation_id,
        ))
    });
}

/// The batched composition: the inbox's loads, once per repo. BLOCKING
/// (run inside `run_blocking`).
pub fn compose_queue(
    store: &store::Store,
    repo_names: &[String],
    state_filter: Option<&str>,
) -> Result<Vec<QueueRow>, ApiError> {
    let mut reviews = Vec::new();
    for repo_name in repo_names {
        reviews.extend(store.list_reviews(repo_name, state_filter)?);
    }
    if reviews.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = reviews.iter().map(|r| r.id).collect();
    let binding_map = store.get_review_pr_bindings(&ids)?;
    let findings_map = store.list_review_findings_batch(&ids, None, false)?;
    let ann_map = store.list_review_annotations_batch(&ids, true)?;
    let mut rows = Vec::new();
    for r in &reviews {
        let pr = binding_map.get(&r.id).and_then(|b| b.pr_number);
        let anns = ann_map.get(&r.id).map(Vec::as_slice).unwrap_or(&[]);
        let fs = findings_map.get(&r.id).map(Vec::as_slice).unwrap_or(&[]);
        rows.extend(rows_for_review(r.id, &r.repo, pr, anns, fs));
    }
    sort_queue(&mut rows);
    Ok(rows)
}

#[derive(Debug, Default, Deserialize)]
pub struct QueueParams {
    /// One configured repo; absent = every repo.
    #[serde(default)]
    pub repo: Option<String>,
    /// `open` (default) | `closed` | `all`.
    #[serde(default)]
    pub state: Option<String>,
}

/// `GET /api/reviews/agent-queue?repo=&state=open`
pub async fn agent_queue_route(
    State(state): State<SharedState>,
    Query(params): Query<QueueParams>,
) -> Result<Response, ApiError> {
    let state_filter: Option<String> = match params.state.as_deref() {
        None => Some("open".to_string()),
        Some("all") => None,
        Some(s @ ("open" | "closed")) => Some(s.to_string()),
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "state must be open|closed|all, got {other:?}"
            )))
        }
    };
    let repo_names: Vec<String> = match params.repo.as_deref() {
        Some(r) => {
            find_repo(&state, r)?;
            vec![r.to_string()]
        }
        None => state.repos.iter().map(|r| r.name.clone()).collect(),
    };
    let rows = state
        .store
        .run_blocking(move |store| compose_queue(store, &repo_names, state_filter.as_deref()))
        .await?;
    let body = AgentQueue {
        schema: QUEUE_SCHEMA.to_string(),
        count: rows.len(),
        rows,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response())
}

// --- route contract (invariant 15) --------------------------------------------

use crate::entities::RouteContract;

fn queue_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [("repo", "r"), ("state", "open")] {
        if k != omit {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<QueueParams>(serde_json::Value::Object(map)).is_ok()
}

pub const AGENT_QUEUE_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/agent-queue",
    handler: "review_queue::agent_queue_route",
    required_params: &[],
    params_accept_without: queue_accept_without,
};

pub const V044_F9_QUEUE_ROUTES: &[RouteContract] = &[AGENT_QUEUE_ROUTE];

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("../grammar/question-state.golden.json");

    #[derive(Deserialize)]
    struct GoldenVoice {
        author: String,
        #[serde(rename = "createdAt")]
        created_at: i64,
    }

    #[derive(Deserialize)]
    struct GoldenCase {
        name: String,
        intent: String,
        resolved: bool,
        opener: GoldenVoice,
        replies: Vec<GoldenVoice>,
        #[serde(default, rename = "findingOrigin")]
        finding_origin: Option<String>,
        // "awaiting-agent" | "awaiting-you" | null
        chip: Option<String>,
    }

    #[derive(Deserialize)]
    struct GoldenFile {
        schema: String,
        cases: Vec<GoldenCase>,
    }

    /// The shared fixture. `web-code/src/lib/questionState.golden.test.ts`
    /// reads the same bytes and asserts the same chip against the TS
    /// implementation: one corpus, two implementations.
    #[test]
    fn golden_corpus_matches_the_rust_port_of_the_question_state() {
        let g: GoldenFile = serde_json::from_str(GOLDEN).expect("golden parses");
        assert_eq!(g.schema, "kbc-question-state/1");
        assert!(g.cases.len() >= 8);
        for c in &g.cases {
            let opener = Voice {
                author: &c.opener.author,
                at: c.opener.created_at,
            };
            let replies: Vec<Voice<'_>> = c
                .replies
                .iter()
                .map(|r| Voice {
                    author: &r.author,
                    at: r.created_at,
                })
                .collect();
            let want = c.chip.as_deref() == Some("awaiting-agent");
            assert_eq!(
                awaiting_agent(
                    &c.intent,
                    c.resolved,
                    opener,
                    &replies,
                    c.finding_origin.as_deref()
                ),
                want,
                "{}",
                c.name
            );
        }
    }

    fn ann(id: &str, parent: Option<&str>, author: &str, at: i64, intent: &str) -> AnnotationRow {
        AnnotationRow {
            id: id.to_string(),
            repo_id: 1,
            path: "a.rb".to_string(),
            anchor: None,
            anchor_kind: "line".to_string(),
            anchor2: None,
            parent_id: parent.map(str::to_string),
            intent: intent.to_string(),
            body: "b".to_string(),
            author: author.to_string(),
            created_at: at,
            updated_at: at,
            resolved: false,
            review_id: Some(1),
            ps_number: Some(1),
            side: None,
            set_id: None,
            trail_id: None,
        }
    }

    fn finding(
        slug: &str,
        annotation_id: &str,
        disposition: Option<&str>,
        at: i64,
    ) -> ReviewFindingRow {
        ReviewFindingRow {
            id: 1,
            review_id: 1,
            annotation_id: annotation_id.to_string(),
            slug: slug.to_string(),
            severity: "concern".to_string(),
            category: "style".to_string(),
            location_kind: "single".to_string(),
            location_path: "a.rb".to_string(),
            location_lines: None,
            location_removed: false,
            title: "t".to_string(),
            rationale: "r".to_string(),
            recommendation: None,
            evidence_lang: None,
            evidence_source: None,
            origin: "import".to_string(),
            author: None,
            disposition: disposition.map(str::to_string),
            disposition_note: None,
            disposition_by: Some("you".to_string()),
            disposition_at: Some(at),
            content_updated_at: None,
            published_state: "unpublished".to_string(),
            published_at: None,
            published_url: None,
            superseded: false,
            superseded_at: None,
            superseded_reason: None,
            import_batch_id: "b".to_string(),
            created_at: 1,
            updated_at: 1,
            act: "issue".to_string(),
            blocking: false,
            cites_json: None,
            fingerprint: None,
            superseded_by: None,
        }
    }

    #[test]
    fn a_human_question_with_no_reply_is_queued_and_an_agent_reply_clears_it() {
        let q = ann("q1", None, "you", 100, "question");
        let rows = rows_for_review(1, "r", None, std::slice::from_ref(&q), &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (
                rows[0].lane.as_str(),
                rows[0].lane_order,
                rows[0].waiting_since
            ),
            ("question", 1, 100)
        );
        let reply = ann("r1", Some("q1"), "claude", 200, "note");
        assert!(rows_for_review(1, "r", None, &[q.clone(), reply], &[]).is_empty());
        // a human follow-up AFTER the agent's reply puts it back, waiting
        // since the follow-up
        let reply = ann("r1", Some("q1"), "claude", 200, "note");
        let again = ann("r2", Some("q1"), "you", 300, "note");
        let rows = rows_for_review(1, "r", None, &[q, reply, again], &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].waiting_since, 300);
    }

    #[test]
    fn a_question_the_agent_asked_itself_is_not_the_agents_work() {
        // The inbox counts this as "unanswered" (asker spoke last); the
        // queue must not: the agent is the asker.
        let q = ann("q1", None, "claude", 100, "question");
        assert!(rows_for_review(1, "r", None, &[q], &[]).is_empty());
    }

    #[test]
    fn a_resolved_or_non_question_thread_is_not_queued() {
        let mut q = ann("q1", None, "you", 100, "question");
        q.resolved = true;
        let note = ann("n1", None, "you", 100, "note");
        assert!(rows_for_review(1, "r", None, &[q, note], &[]).is_empty());
    }

    #[test]
    fn a_disputed_finding_is_queued_until_the_agent_replies_after_the_dispute() {
        // The finding's own thread is an `import`-origin agent opener with
        // intent "finding": it is NOT a lane-1 row, only a lane-2 one.
        let opener = ann("f1", None, "claude", 100, "finding");
        let fs = [finding("f-x", "f1", Some("dispute"), 500)];
        let rows = rows_for_review(1, "r", Some(7), std::slice::from_ref(&opener), &fs);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            (
                rows[0].lane.as_str(),
                rows[0].lane_order,
                rows[0].waiting_since
            ),
            ("dispute", 2, 500)
        );
        assert_eq!(rows[0].finding_slug.as_deref(), Some("f-x"));
        assert_eq!(rows[0].pr_number, Some(7));
        assert_eq!(rows[0].next[0][..3], ["kb-code", "review", "findings"]);
        // an agent reply after the dispute clears it
        let reply = ann("r1", Some("f1"), "claude", 600, "note");
        assert!(rows_for_review(1, "r", None, &[opener.clone(), reply], &fs).is_empty());
        // an agent reply BEFORE the dispute does not
        let early = ann("r0", Some("f1"), "claude", 400, "note");
        assert_eq!(
            rows_for_review(1, "r", None, &[opener.clone(), early], &fs).len(),
            1
        );
        // other dispositions are not this lane
        let agree = [finding("f-x", "f1", Some("agree"), 500)];
        assert!(rows_for_review(1, "r", None, &[opener], &agree).is_empty());
    }

    #[test]
    fn dispute_lane_needs_an_unanswered_dispute() {
        let d = Some(store::DISPOSITION_DISPUTE);
        let human = Voice {
            author: "you",
            at: 500,
        };
        let agent_before = Voice {
            author: "claude",
            at: 400,
        };
        let agent_after = Voice {
            author: "claude",
            at: 600,
        };
        assert!(dispute_unanswered(d, false, Some(500), false, &[]));
        assert!(dispute_unanswered(
            d,
            false,
            Some(500),
            false,
            &[agent_before, human]
        ));
        assert!(!dispute_unanswered(
            d,
            false,
            Some(500),
            false,
            &[agent_after]
        ));
        assert!(!dispute_unanswered(d, true, Some(500), false, &[]));
        assert!(!dispute_unanswered(d, false, Some(500), true, &[]));
        assert!(!dispute_unanswered(
            Some("agree"),
            false,
            Some(500),
            false,
            &[]
        ));
        assert!(!dispute_unanswered(None, false, None, false, &[]));
    }

    #[test]
    fn queue_order_is_lane_then_oldest_then_ids() {
        let row = |lane_order: u32, since: i64, review: i64, ann: &str| QueueRow {
            lane: String::new(),
            lane_order,
            review_id: review,
            repo: String::new(),
            pr_number: None,
            annotation_id: ann.to_string(),
            path: String::new(),
            ps_number: None,
            finding_slug: None,
            waiting_since: since,
            from: String::new(),
            next: vec![],
        };
        let mut rows = vec![
            row(2, 10, 1, "a"),
            row(1, 50, 2, "b"),
            row(1, 20, 3, "c"),
            row(1, 20, 1, "z"),
            row(1, 20, 1, "y"),
        ];
        sort_queue(&mut rows);
        let ids: Vec<&str> = rows.iter().map(|r| r.annotation_id.as_str()).collect();
        assert_eq!(ids, vec!["y", "z", "c", "b", "a"]);
    }
}
