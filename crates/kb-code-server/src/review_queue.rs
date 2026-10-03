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
//!      after `disposition_at`;
//!   3. `follow-up` — a finding the human AGREED with (or marked fix-later)
//!      whose thread is still open and which has neither a stored
//!      suggestion nor a later patchset hunk by the author near its lines
//!      (`touched_in`, rebase-aware, `rebased` not counted);
//!   4. `pr-drift` — an open PR-bound review whose stored PR head differs
//!      from its latest patchset tip (the local half of `review status`; no
//!      network);
//!   5. `flag` — an open `flag-for-agent` thread whose latest voice is not
//!      an agent.
//!
//! A thread appears at most ONCE: lanes are claimed in order by
//! `annotation_id`, so a disputed finding whose thread is also an open
//! question is a `question` row only.
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
//!   * `follow-up` findings are matched on stored suggestions and on
//!     `touched_in` author hunks; a `whole_file`/line-less finding has no
//!     lines to match, so it stays queued until resolved or superseded.
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
use std::collections::{HashMap, HashSet};

pub const QUEUE_SCHEMA: &str = "kbc-agent-queue/1";
pub const LANE_QUESTION: &str = "question";
pub const LANE_DISPUTE: &str = "dispute";
pub const LANE_FOLLOW_UP: &str = "follow-up";
pub const LANE_PR_DRIFT: &str = "pr-drift";
pub const LANE_FLAG: &str = "flag";

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
    thread_awaits_agent(
        intent,
        crate::annotations::INTENT_QUESTION,
        resolved,
        opener,
        replies,
        finding_origin,
    )
}

/// The shared turn-taking rule: an unresolved thread of `want` intent whose
/// latest voice is not an agent. Lane 1 asks it of `question`, lane 5 of
/// `flag-for-agent`. Pure.
pub fn thread_awaits_agent(
    intent: &str,
    want: &str,
    resolved: bool,
    opener: Voice<'_>,
    replies: &[Voice<'_>],
    finding_origin: Option<&str>,
) -> bool {
    if resolved || intent != want {
        return false;
    }
    !latest_is_agent(opener, replies, finding_origin)
}

/// Findings the human agreed with (or deferred as fix-later) that are still
/// open and carry no stored suggestion: the candidates for the `follow-up`
/// lane before the author-hunk check. Pure.
pub fn followup_candidates<'a>(
    annotations: &'a [AnnotationRow],
    findings: &'a [ReviewFindingRow],
    suggested: &HashSet<String>,
) -> Vec<(&'a ReviewFindingRow, &'a AnnotationRow)> {
    let threads: HashMap<&str, &AnnotationRow> = annotations
        .iter()
        .filter(|a| a.parent_id.is_none())
        .map(|a| (a.id.as_str(), a))
        .collect();
    findings
        .iter()
        .filter(|f| {
            !f.superseded
                && matches!(
                    f.disposition.as_deref(),
                    Some(store::DISPOSITION_AGREE) | Some(store::DISPOSITION_FIX_LATER)
                )
                && !suggested.contains(&f.annotation_id)
        })
        .filter_map(|f| {
            let t = threads.get(f.annotation_id.as_str())?;
            (!t.resolved).then_some((f, *t))
        })
        .collect()
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

/// Facts about one review that its annotation/finding rows cannot give.
#[derive(Debug, Clone, Default)]
pub struct ReviewFacts {
    /// The review is open (lane 4 never fires on a closed one).
    pub open: bool,
    /// Annotation ids carrying a stored suggestion.
    pub suggested: HashSet<String>,
    /// Finding annotation ids with an author hunk near their lines in a
    /// later patchset (`touched_in`, not `rebased`).
    pub authored: HashSet<String>,
    /// The stored PR head the review was last synced against.
    pub pr_head_sha: Option<String>,
    /// The latest patchset's tip and capture time.
    pub latest_tip: Option<String>,
    pub latest_captured_at: Option<i64>,
}

/// One review's queue rows from its already-loaded rows. Pure. A thread is
/// claimed by the first lane that wants it (lane order), so no
/// `annotation_id` appears twice.
pub fn rows_for_review(
    review_id: i64,
    repo: &str,
    pr_number: Option<i64>,
    annotations: &[AnnotationRow],
    findings: &[ReviewFindingRow],
    facts: &ReviewFacts,
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
    let mut claimed: HashSet<String> = HashSet::new();
    // Deterministic claim order inside a lane: HashMap iteration is not.
    let mut thread_ids: Vec<&&str> = threads.keys().collect();
    thread_ids.sort();
    for aid in thread_ids {
        let t = threads[*aid];
        let replies = voices_of(&replies_by_parent, aid);
        let opener = Voice {
            author: t.author.as_str(),
            at: t.created_at,
        };
        let origin = finding_by_ann.get(*aid).map(|f| f.origin.as_str());
        if awaiting_agent(&t.intent, t.resolved, opener, &replies, origin) {
            // Waiting since the last human voice (the opener or the latest
            // reply), which is when the agent's turn began.
            let since = replies
                .iter()
                .map(|r| r.at)
                .chain(std::iter::once(opener.at))
                .max()
                .unwrap_or(opener.at);
            claimed.insert(t.id.clone());
            out.push(QueueRow {
                lane: LANE_QUESTION.to_string(),
                lane_order: 1,
                review_id,
                repo: repo.to_string(),
                pr_number,
                annotation_id: t.id.clone(),
                path: t.path.clone(),
                ps_number: t.ps_number,
                finding_slug: finding_by_ann.get(*aid).map(|f| f.slug.clone()),
                waiting_since: since,
                from: t.author.clone(),
                next: vec![argv(&["kb-code", "review", "comments", &id_s, "--json"])],
            });
        }
    }
    for f in findings {
        if f.superseded || claimed.contains(&f.annotation_id) {
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
            claimed.insert(f.annotation_id.clone());
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
    // Lane 3 — agreed/fix-later, still open, nothing done about it yet.
    for (f, t) in followup_candidates(annotations, findings, &facts.suggested) {
        if claimed.contains(&f.annotation_id) || facts.authored.contains(&f.annotation_id) {
            continue;
        }
        claimed.insert(f.annotation_id.clone());
        out.push(QueueRow {
            lane: LANE_FOLLOW_UP.to_string(),
            lane_order: 3,
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
    // Lane 4 — the PR moved under the review (local comparison only).
    if let (true, Some(_), Some(head), Some(tip)) = (
        facts.open,
        pr_number,
        facts.pr_head_sha.as_deref(),
        facts.latest_tip.as_deref(),
    ) {
        if head != tip {
            out.push(QueueRow {
                lane: LANE_PR_DRIFT.to_string(),
                lane_order: 4,
                review_id,
                repo: repo.to_string(),
                pr_number,
                annotation_id: String::new(),
                path: String::new(),
                ps_number: None,
                finding_slug: None,
                waiting_since: facts.latest_captured_at.unwrap_or(0),
                from: String::new(),
                next: vec![
                    argv(&["kb-code", "review", "status", &id_s, "--json"]),
                    argv(&["kb-code", "review", "sync", "--repo", repo, "--json"]),
                ],
            });
        }
    }
    // Lane 5 — flag-for-agent threads nobody on the agent side answered.
    let mut flag_ids: Vec<&&str> = threads.keys().collect();
    flag_ids.sort();
    for aid in flag_ids {
        let t = threads[*aid];
        if claimed.contains(&t.id) {
            continue;
        }
        let replies = voices_of(&replies_by_parent, aid);
        let opener = Voice {
            author: t.author.as_str(),
            at: t.created_at,
        };
        if thread_awaits_agent(
            &t.intent,
            crate::annotations::INTENT_FLAG_FOR_AGENT,
            t.resolved,
            opener,
            &replies,
            None,
        ) {
            let since = replies
                .iter()
                .map(|r| r.at)
                .chain(std::iter::once(opener.at))
                .max()
                .unwrap_or(opener.at);
            claimed.insert(t.id.clone());
            out.push(QueueRow {
                lane: LANE_FLAG.to_string(),
                lane_order: 5,
                review_id,
                repo: repo.to_string(),
                pr_number,
                annotation_id: t.id.clone(),
                path: t.path.clone(),
                ps_number: t.ps_number,
                finding_slug: None,
                waiting_since: since,
                from: t.author.clone(),
                next: vec![argv(&["kb-code", "review", "comments", &id_s, "--json"])],
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

/// One review's loaded inputs. The store half of the composition; the git
/// half (`authored`) is filled by [`attach_authored`].
#[derive(Debug, Clone)]
pub struct ReviewInput {
    pub review_id: i64,
    pub repo: String,
    pub pr_number: Option<i64>,
    pub annotations: Vec<AnnotationRow>,
    pub findings: Vec<ReviewFindingRow>,
    pub facts: ReviewFacts,
    /// Loaded only when the review has `follow-up` candidates.
    pub patchsets: Vec<store::ReviewPatchsetRow>,
}

/// The batched store loads, once per repo (the inbox's loads plus one
/// suggestions join and one latest-patchset read). BLOCKING (run inside
/// `run_blocking`).
pub fn load_inputs(
    store: &store::Store,
    repo_names: &[String],
    state_filter: Option<&str>,
) -> Result<Vec<ReviewInput>, ApiError> {
    let mut reviews = Vec::new();
    for repo_name in repo_names {
        reviews.extend(store.list_reviews(repo_name, state_filter)?);
    }
    if reviews.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = reviews.iter().map(|r| r.id).collect();
    let binding_map = store.get_review_pr_bindings(&ids)?;
    let mut findings_map = store.list_review_findings_batch(&ids, None, false)?;
    let mut ann_map = store.list_review_annotations_batch(&ids, true)?;
    let latest = store.latest_patchsets(&ids)?;
    let suggested = store.review_suggestion_annotation_ids(&ids)?;
    let mut out = Vec::new();
    for r in &reviews {
        let binding = binding_map.get(&r.id);
        let annotations = ann_map.remove(&r.id).unwrap_or_default();
        let findings = findings_map.remove(&r.id).unwrap_or_default();
        let facts = ReviewFacts {
            open: r.state == "open",
            suggested: annotations
                .iter()
                .filter(|a| suggested.contains(&a.id))
                .map(|a| a.id.clone())
                .collect(),
            authored: HashSet::new(),
            pr_head_sha: binding.and_then(|b| b.pr_head_sha.clone()),
            latest_tip: latest.get(&r.id).map(|p| p.tip_sha.clone()),
            latest_captured_at: latest.get(&r.id).map(|p| p.captured_at),
        };
        let patchsets = if followup_candidates(&annotations, &findings, &facts.suggested).is_empty()
        {
            Vec::new()
        } else {
            store.list_patchsets(r.id)?
        };
        out.push(ReviewInput {
            review_id: r.id,
            repo: r.repo.clone(),
            pr_number: binding.and_then(|b| b.pr_number),
            annotations,
            findings,
            facts,
            patchsets,
        });
    }
    Ok(out)
}

/// Fill `facts.authored`: which `follow-up` candidates already have an
/// author hunk near their lines in a later patchset. BLOCKING (git).
/// `rebased` entries are not evidence and do not count.
pub fn attach_authored(ctx: &crate::git::roots::GitCtx, input: &mut ReviewInput) {
    let candidates =
        followup_candidates(&input.annotations, &input.findings, &input.facts.suggested);
    let mut queries = Vec::new();
    let mut ann_of: HashMap<i64, String> = HashMap::new();
    for (f, t) in candidates {
        if let Some(q) = crate::review_findings::touched_in_query_for(f, t.ps_number) {
            ann_of.insert(f.id, f.annotation_id.clone());
            queries.push(q);
        }
    }
    if queries.is_empty() {
        return;
    }
    let touched =
        crate::review_finding_touches::compute_touched_in(ctx, &input.patchsets, &queries);
    for (fid, res) in touched {
        let acted = res
            .entries
            .iter()
            .any(|e| e.overlap != crate::review_finding_touches::OVERLAP_REBASED);
        if acted {
            if let Some(aid) = ann_of.get(&fid) {
                input.facts.authored.insert(aid.clone());
            }
        }
    }
}

/// Rows for every loaded review, sorted. Pure.
pub fn compose_rows(inputs: &[ReviewInput]) -> Vec<QueueRow> {
    let mut rows = Vec::new();
    for i in inputs {
        rows.extend(rows_for_review(
            i.review_id,
            &i.repo,
            i.pr_number,
            &i.annotations,
            &i.findings,
            &i.facts,
        ));
    }
    sort_queue(&mut rows);
    rows
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
    let mut inputs = state
        .store
        .run_blocking(move |store| load_inputs(store, &repo_names, state_filter.as_deref()))
        .await?;
    // The git half (lane 3's author-hunk check), only for reviews that have
    // `follow-up` candidates, one repo context per repo.
    let mut ctxs: HashMap<String, crate::git::roots::GitCtx> = HashMap::new();
    for i in inputs.iter().filter(|i| !i.patchsets.is_empty()) {
        if ctxs.contains_key(&i.repo) {
            continue;
        }
        if let Some(entry) = state.repos.iter().find(|r| r.name == i.repo) {
            let ctx = crate::git::roots::GitCtx::resolve_entry(&state.store, entry).await;
            ctxs.insert(i.repo.clone(), ctx);
        }
    }
    let rows = tokio::task::spawn_blocking(move || {
        for i in inputs.iter_mut().filter(|i| !i.patchsets.is_empty()) {
            if let Some(ctx) = ctxs.get(&i.repo) {
                attach_authored(ctx, i);
            }
        }
        compose_rows(&inputs)
    })
    .await
    .map_err(|e| ApiError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
        let rows = rows_for_review(
            1,
            "r",
            None,
            std::slice::from_ref(&q),
            &[],
            &ReviewFacts::default(),
        );
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
        assert!(rows_for_review(
            1,
            "r",
            None,
            &[q.clone(), reply],
            &[],
            &ReviewFacts::default()
        )
        .is_empty());
        // a human follow-up AFTER the agent's reply puts it back, waiting
        // since the follow-up
        let reply = ann("r1", Some("q1"), "claude", 200, "note");
        let again = ann("r2", Some("q1"), "you", 300, "note");
        let rows = rows_for_review(
            1,
            "r",
            None,
            &[q, reply, again],
            &[],
            &ReviewFacts::default(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].waiting_since, 300);
    }

    #[test]
    fn a_question_the_agent_asked_itself_is_not_the_agents_work() {
        // The inbox counts this as "unanswered" (asker spoke last); the
        // queue must not: the agent is the asker.
        let q = ann("q1", None, "claude", 100, "question");
        assert!(rows_for_review(1, "r", None, &[q], &[], &ReviewFacts::default()).is_empty());
    }

    #[test]
    fn a_resolved_or_non_question_thread_is_not_queued() {
        let mut q = ann("q1", None, "you", 100, "question");
        q.resolved = true;
        let note = ann("n1", None, "you", 100, "note");
        assert!(rows_for_review(1, "r", None, &[q, note], &[], &ReviewFacts::default()).is_empty());
    }

    #[test]
    fn a_disputed_finding_is_queued_until_the_agent_replies_after_the_dispute() {
        // The finding's own thread is an `import`-origin agent opener with
        // intent "finding": it is NOT a lane-1 row, only a lane-2 one.
        let opener = ann("f1", None, "claude", 100, "finding");
        let fs = [finding("f-x", "f1", Some("dispute"), 500)];
        let rows = rows_for_review(
            1,
            "r",
            Some(7),
            std::slice::from_ref(&opener),
            &fs,
            &ReviewFacts::default(),
        );
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
        assert!(rows_for_review(
            1,
            "r",
            None,
            &[opener.clone(), reply],
            &fs,
            &ReviewFacts::default()
        )
        .is_empty());
        // an agent reply BEFORE the dispute does not
        let early = ann("r0", Some("f1"), "claude", 400, "note");
        assert_eq!(
            rows_for_review(
                1,
                "r",
                None,
                &[opener.clone(), early],
                &fs,
                &ReviewFacts::default()
            )
            .len(),
            1
        );
        // other dispositions are not this lane
        let agree = [finding("f-x", "f1", Some("agree"), 500)];
        // (it IS a lane-3 `follow-up` row, but not a `dispute` one)
        assert!(
            rows_for_review(1, "r", None, &[opener], &agree, &ReviewFacts::default())
                .iter()
                .all(|r| r.lane != LANE_DISPUTE)
        );
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

    fn facts() -> ReviewFacts {
        ReviewFacts::default()
    }

    fn lanes(rows: &[QueueRow]) -> Vec<(&str, &str)> {
        rows.iter()
            .map(|r| (r.lane.as_str(), r.annotation_id.as_str()))
            .collect()
    }

    /// F9b — a disputed finding whose thread is ALSO an open question used
    /// to be listed twice (lane 1 and lane 2). Fails without the claim set.
    #[test]
    fn a_disputed_question_thread_is_one_row_not_two() {
        let mut opener = ann("q1", None, "you", 100, "question");
        opener.intent = "question".into();
        let mut f = finding("f-q", "q1", Some("dispute"), 500);
        // a human-promoted finding: its opener is the human's voice
        f.origin = "manual".into();
        let rows = rows_for_review(1, "r", None, std::slice::from_ref(&opener), &[f], &facts());
        assert_eq!(lanes(&rows), vec![("question", "q1")], "{rows:?}");
    }

    /// F9b lane 3 — agree / fix-later with nothing done yet.
    #[test]
    fn an_agreed_finding_with_no_suggestion_or_author_hunk_is_a_follow_up() {
        let opener = ann("f1", None, "claude", 100, "finding");
        let fs = [finding("f-a", "f1", Some("agree"), 500)];
        let rows = rows_for_review(1, "r", None, std::slice::from_ref(&opener), &fs, &facts());
        assert_eq!(lanes(&rows), vec![("follow-up", "f1")]);
        assert_eq!((rows[0].lane_order, rows[0].waiting_since), (3, 500));
        // fix-later is the same lane
        let fl = [finding("f-a", "f1", Some("fix-later"), 600)];
        assert_eq!(
            rows_for_review(1, "r", None, std::slice::from_ref(&opener), &fl, &facts()).len(),
            1
        );
        // a stored suggestion, an author hunk, a resolved thread, a waive
        // or a superseded finding each take it off the queue
        let mut with_suggestion = facts();
        with_suggestion.suggested.insert("f1".into());
        assert!(rows_for_review(
            1,
            "r",
            None,
            std::slice::from_ref(&opener),
            &fs,
            &with_suggestion
        )
        .is_empty());
        let mut with_hunk = facts();
        with_hunk.authored.insert("f1".into());
        assert!(
            rows_for_review(1, "r", None, std::slice::from_ref(&opener), &fs, &with_hunk)
                .is_empty()
        );
        let mut resolved = opener.clone();
        resolved.resolved = true;
        assert!(rows_for_review(1, "r", None, &[resolved], &fs, &facts()).is_empty());
        let waive = [finding("f-a", "f1", Some("waive"), 500)];
        assert!(rows_for_review(
            1,
            "r",
            None,
            std::slice::from_ref(&opener),
            &waive,
            &facts()
        )
        .is_empty());
        let mut sup = finding("f-a", "f1", Some("agree"), 500);
        sup.superseded = true;
        assert!(rows_for_review(
            1,
            "r",
            None,
            std::slice::from_ref(&opener),
            &[sup],
            &facts()
        )
        .is_empty());
    }

    /// F9b lane 4 — the stored PR head differs from the latest tip.
    #[test]
    fn pr_head_drift_is_a_review_level_row_only_for_an_open_pr_bound_review() {
        let drift = ReviewFacts {
            open: true,
            pr_head_sha: Some("aaa".into()),
            latest_tip: Some("bbb".into()),
            latest_captured_at: Some(42),
            ..ReviewFacts::default()
        };
        let rows = rows_for_review(3, "r", Some(9), &[], &[], &drift);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            (
                rows[0].lane.as_str(),
                rows[0].lane_order,
                rows[0].waiting_since
            ),
            ("pr-drift", 4, 42)
        );
        assert_eq!(rows[0].annotation_id, "");
        assert_eq!(rows[0].next[0][2], "status");
        // same head: nothing; not PR-bound: nothing; closed: nothing
        let same = ReviewFacts {
            latest_tip: Some("aaa".into()),
            ..drift.clone()
        };
        assert!(rows_for_review(3, "r", Some(9), &[], &[], &same).is_empty());
        assert!(rows_for_review(3, "r", None, &[], &[], &drift).is_empty());
        let closed = ReviewFacts {
            open: false,
            ..drift
        };
        assert!(rows_for_review(3, "r", Some(9), &[], &[], &closed).is_empty());
    }

    /// F9b lane 5 — an open flag-for-agent thread awaiting the agent.
    #[test]
    fn a_flag_for_agent_thread_is_queued_until_an_agent_replies() {
        let flag = ann("g1", None, "you", 100, "flag-for-agent");
        let rows = rows_for_review(1, "r", None, std::slice::from_ref(&flag), &[], &facts());
        assert_eq!(lanes(&rows), vec![("flag", "g1")]);
        assert_eq!(rows[0].lane_order, 5);
        let reply = ann("r1", Some("g1"), "claude", 200, "note");
        assert!(rows_for_review(1, "r", None, &[flag.clone(), reply], &[], &facts()).is_empty());
        let mut resolved = flag;
        resolved.resolved = true;
        assert!(rows_for_review(1, "r", None, &[resolved], &[], &facts()).is_empty());
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
