//! V73-K3 — `kbc-claim/1`, the agent-prose CLAIM register (design D18).
//!
//! An agent that reads code produces two very different kinds of output.
//! One is a FACT this daemon can re-derive — a symbol's definition, a
//! caller list, a diff hunk — and the whole crate's discipline is that such
//! a fact carries a trust class minted per request and is never cached.
//! The other is PROSE: "this method is the retry path", "we chose the
//! queue over a cron because …", "the alternative I rejected was …".
//! Prose cannot be re-derived, so it must be STORED; and because it cannot
//! be re-derived it must never be trusted the way a derivation is.
//!
//! `claims` is the one table for all of it (migration V0035). D18 names six
//! renderings — commit-time explain cards, the rejected-alternatives
//! ledger, decision threads, branch stories, trail notes and entity answers
//! — and rules that each is a KIND plus a rendering, never a table of its
//! own. That is invariant 9's posture ("a `reading_sets` row with `kind =
//! 'workspace'` IS a workspace, not a new entity") applied one layer up.
//!
//! # The three rules
//!
//! **(a) Surfaced, never scored.** A claim is rendered beside the fact it
//! is about. It is never a ranking term, never a boost, never a filter
//! default, and never a trust class of its own. `confidence` is the
//! AGENT'S OWN DECLARATION — a number the author chose, printed verbatim —
//! not a probability this daemon computed, and nothing multiplies it into
//! anything. The pin is structural, not a convention:
//! [`tests::no_ranking_module_imports_the_claim_register`] scans this
//! crate's ranking sources and fails by name if any of them so much as
//! names this module. kb root invariant #10's provenance lane
//! ("surfaced-never-scored … structurally unreachable from the scoring
//! types") is the precedent.
//!
//! **(b) The class is computed per request and is not a column.** There is
//! no `trust` column, exactly as `lane_facts` (V0030) and `entity_defs`
//! (V0029) have none. What is STORED is the claim's WITNESS: `blob_sha`,
//! the bytes the author was looking at. [`ladder_state`] turns that into
//! `pinned` / `drifted` / `unanchored` at read time by comparing it with
//! the file's live blob — root invariant #2's "kb-code mints classes,
//! nothing is cached", applied to the LLM's own prose. A drifted claim is
//! GREYED with a caption naming both blobs (D18's own wording), never
//! hidden and never silently re-anchored: this module runs no ladder of its
//! own, because a claim is about a whole subject rather than a line, and
//! guessing which lines it "really" meant is the wrong-`exact` class.
//!
//! **(c) Writes are loopback-only and audited.** A claim is content this
//! daemon did not derive, authored by a process on the operator's own box;
//! `POST /api/claims` therefore rides the `transcripts_api` sub-router's
//! loopback gate (D22's local-canonical ruling, root invariant #4
//! unamended) and the crate-wide `audit_mutations` ledger records it with
//! no per-handler wiring (invariant 1).

use crate::routes::{find_repo, safe_rel_path, ApiError};
use crate::state::SharedState;
use crate::store::{self, StoreBlocking};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "kbc-claim/1";

/// D18's subject vocabulary, closed. A subject NAMES the thing the prose is
/// about; it is not a location and carries no line.
pub const SUBJECT_KINDS: &[&str] = &["path", "sym", "ent", "commit", "hunk", "review", "branch"];

/// D18's kind vocabulary, closed. Every rendering D18 names is one of
/// these plus a presentation — never a table.
pub const KINDS: &[&str] = &[
    "explain",
    "alternative",
    "decision",
    "story",
    "note",
    "answer",
];

/// Ladder states. Deliberately NOT the `exact`/`likely`/`candidate` trust
/// vocabulary: a claim is prose, and dressing prose in the trust
/// vocabulary would let a reader mistake "the author was looking at these
/// bytes" for "this daemon verified this statement".
pub const STATE_PINNED: &str = "pinned";
pub const STATE_DRIFTED: &str = "drifted";
pub const STATE_UNANCHORED: &str = "unanchored";

/// Soft caps. Each REFUSES, naming the number — never truncates (K1's
/// `size_cap` rule).
pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_EVIDENCE: usize = 64;
pub const MAX_SUBJECT_BYTES: usize = 512;
/// The default page size for `GET /api/claims`, and its hard ceiling.
pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 500;

pub fn new_claim_id() -> String {
    format!("clm_{}", crate::annotations::short_random_hex())
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The repo-relative path a subject IMPLIES, or `None` when the subject
/// kind has no path at all. `hunk:` addresses are `<path>@<ps>#<n>` (K1's
/// `refs::Ref::Hunk` grammar), so the path is the segment before the LAST
/// `@` — the same split K1's own parser makes.
pub fn subject_path(subject_kind: &str, subject: &str) -> Option<String> {
    match subject_kind {
        "path" => Some(subject.to_string()),
        "hunk" => subject.rsplit_once('@').map(|(p, _)| p.to_string()),
        _ => None,
    }
}

/// The Ladder, computed per request (rule (b)).
///
/// * `pinned` — the claim named a blob and it is the file's CURRENT blob.
/// * `drifted` — the claim named a blob and the file has moved on (or the
///   path is gone entirely). The caption names both sides; nothing is
///   re-anchored.
/// * `unanchored` — the claim named no blob, or its subject has no path to
///   compare against (`sym`/`ent`/`commit`/`review`/`branch`). This is the
///   honest answer, NOT a failure: a decision about a branch has no blob.
pub fn ladder_state(
    claim_blob: Option<&str>,
    current_blob: Option<&str>,
) -> (&'static str, String) {
    match (claim_blob, current_blob) {
        (None, _) => (
            STATE_UNANCHORED,
            "this claim pinned no blob — it is about a subject with no single set of bytes, \
             or its author recorded none"
                .to_string(),
        ),
        (Some(claimed), Some(current)) if claimed == current => (
            STATE_PINNED,
            format!(
                "the bytes this claim was written against are still the file's own ({current})"
            ),
        ),
        (Some(claimed), Some(current)) => (
            STATE_DRIFTED,
            format!(
                "written against {claimed}, the file is now {current} — the prose is shown as \
                 authored and is NOT re-anchored"
            ),
        ),
        (Some(claimed), None) => (
            STATE_DRIFTED,
            format!(
                "written against {claimed}; this daemon has no current blob for the subject's \
                 path (deleted, or never indexed) — shown as authored, never removed"
            ),
        ),
    }
}

// --- wire ------------------------------------------------------------------

/// `POST /api/claims` — loopback-only.
#[derive(Debug, Deserialize)]
pub struct ClaimBody {
    /// `"kbc-claim/1"`. Required and checked, so a payload written for a
    /// future shape fails by name instead of half-landing.
    pub schema: String,
    pub repo: String,
    pub subject_kind: String,
    pub subject: String,
    pub kind: String,
    pub body_md: String,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// The bytes the author was looking at. Optional, and its ABSENCE is
    /// meaningful (`unanchored`) rather than a defect — see [`ladder_state`].
    #[serde(default)]
    pub blob_sha: Option<String>,
    /// Soft link to a review. No FK (the `canvas_sets` precedent): a claim
    /// written during a review outlives it.
    #[serde(default)]
    pub review_id: Option<i64>,
}

/// One claim on the wire. `state`/`caption`/`current_blob` are the
/// per-request Ladder (rule (b)) and are never stored.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ClaimOut {
    pub schema: &'static str,
    pub id: String,
    pub repo: String,
    pub subject_kind: String,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_id: Option<i64>,
    pub kind: String,
    pub body_md: String,
    /// The AGENT'S declaration, verbatim (rule (a)). Never a term.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    pub evidence: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_blob: Option<String>,
    pub state: &'static str,
    pub caption: String,
    /// V76-B3 (kbc-prose/1) — the `body_md` prose refs, computed per
    /// request by the ROUTES (`claim_out` itself stays pure and leaves this
    /// empty); additive, never persisted, never a ranking input (rule (a)).
    pub refs: crate::prose_refs::FieldRefs,
    pub created_at: i64,
}

/// Turn a stored row plus the subject's CURRENT blob into a wire claim.
/// Pure — the only place the Ladder is applied, so the two read routes
/// cannot disagree.
pub fn claim_out(row: &store::ClaimRow, repo: &str, current_blob: Option<&str>) -> ClaimOut {
    let (state, caption) = ladder_state(row.blob_sha.as_deref(), current_blob);
    ClaimOut {
        schema: SCHEMA,
        id: row.id.clone(),
        repo: repo.to_string(),
        subject_kind: row.subject_kind.clone(),
        subject: row.subject.clone(),
        subject_path: row.subject_path.clone(),
        review_id: row.review_id,
        kind: row.kind.clone(),
        body_md: row.body_md.clone(),
        confidence: row.confidence,
        evidence: parse_evidence(&row.evidence_json),
        session_id: row.session_id.clone(),
        model: row.model.clone(),
        blob_sha: row.blob_sha.clone(),
        current_blob: current_blob.map(str::to_string),
        state,
        caption,
        refs: crate::prose_refs::FieldRefs::default(),
        created_at: row.created_at,
    }
}

/// V76-B3 (kbc-prose/1) — resolve a claim's `body_md` refs on the blocking
/// pool and attach them. The ONE place routes fill the field `claim_out`
/// leaves empty.
async fn with_refs(
    state: &SharedState,
    repo_id: i64,
    mut out: ClaimOut,
) -> Result<ClaimOut, ApiError> {
    let body = out.body_md.clone();
    let review_id = out.review_id;
    out.refs = state
        .store
        .run_blocking(move |store| {
            crate::prose_refs::field_refs(
                store,
                &crate::prose_refs::RefCtx {
                    repo_id,
                    review_id,
                    ps_number: None,
                },
                &body,
            )
        })
        .await?;
    Ok(out)
}

/// Stored evidence is a JSON array of ref STRINGS. A row whose JSON does
/// not parse yields an empty list rather than a 500 — the column is
/// written only by this module's own validated route, so a parse failure
/// means hand-editing, and answering "no evidence" is more useful than
/// failing the whole read.
fn parse_evidence(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

// --- validation ------------------------------------------------------------

fn validate(body: &ClaimBody) -> Result<(), ApiError> {
    if body.schema != SCHEMA {
        return Err(ApiError::bad_request(format!(
            "schema must be {SCHEMA:?}, got {:?}",
            body.schema
        )));
    }
    if !SUBJECT_KINDS.contains(&body.subject_kind.as_str()) {
        return Err(ApiError::bad_request(format!(
            "subject_kind must be one of {SUBJECT_KINDS:?}, got {:?}",
            body.subject_kind
        )));
    }
    if !KINDS.contains(&body.kind.as_str()) {
        return Err(ApiError::bad_request(format!(
            "kind must be one of {KINDS:?}, got {:?}",
            body.kind
        )));
    }
    if body.subject.trim().is_empty() {
        return Err(ApiError::bad_request("subject must not be empty"));
    }
    if body.subject.len() > MAX_SUBJECT_BYTES {
        return Err(ApiError::bad_request(format!(
            "subject is {} bytes, over the {MAX_SUBJECT_BYTES}-byte cap",
            body.subject.len()
        )));
    }
    if body.body_md.trim().is_empty() {
        return Err(ApiError::bad_request(
            "body_md must not be empty — a claim with no prose is not a claim",
        ));
    }
    if body.body_md.len() > MAX_BODY_BYTES {
        return Err(ApiError::bad_request(format!(
            "body_md is {} bytes, over the {MAX_BODY_BYTES}-byte cap — refused rather than \
             truncated",
            body.body_md.len()
        )));
    }
    if body.evidence.len() > MAX_EVIDENCE {
        return Err(ApiError::bad_request(format!(
            "{} evidence refs, over the {MAX_EVIDENCE} cap",
            body.evidence.len()
        )));
    }
    if let Some(c) = body.confidence {
        if !(0.0..=1.0).contains(&c) || !c.is_finite() {
            return Err(ApiError::bad_request(format!(
                "confidence must be within 0.0..=1.0, got {c}"
            )));
        }
    }
    // Evidence is K1's ref grammar and nothing else — a malformed ref is
    // refused here rather than stored and reported forever after.
    for e in &body.evidence {
        match crate::review_doc::refs::parse_ref(e) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(ApiError::bad_request(format!(
                    "evidence {e:?} names no kbc scheme — every ref must start with one of {:?}",
                    crate::review_doc::refs::SCHEMES
                )))
            }
            Err(reason) => {
                return Err(ApiError::bad_request(format!(
                    "evidence {e:?} is malformed: {reason}"
                )))
            }
        }
    }
    // A path-shaped subject is a repo-relative path and is validated like
    // every other one this crate accepts.
    if let Some(p) = subject_path(&body.subject_kind, &body.subject) {
        safe_rel_path(&p)?;
    }
    Ok(())
}

// --- routes ----------------------------------------------------------------

/// `POST /api/claims` — LOOPBACK-ONLY (rule (c)).
pub async fn create_claim(
    State(state): State<SharedState>,
    Json(body): Json<ClaimBody>,
) -> Result<impl IntoResponse, ApiError> {
    validate(&body)?;
    let (repo, repo_id) = find_repo(&state, &body.repo)?;
    let repo_name = repo.name.clone();
    let row = store::ClaimRow {
        id: new_claim_id(),
        repo_id,
        subject_kind: body.subject_kind.clone(),
        subject: body.subject.clone(),
        subject_path: subject_path(&body.subject_kind, &body.subject),
        review_id: body.review_id,
        kind: body.kind.clone(),
        body_md: body.body_md.clone(),
        confidence: body.confidence,
        evidence_json: serde_json::to_string(&body.evidence).unwrap_or_else(|_| "[]".into()),
        session_id: body.session_id.clone(),
        model: body.model.clone(),
        blob_sha: body.blob_sha.clone(),
        created_at: now_unix(),
    };
    let insert = row.clone();
    state
        .store
        .run_blocking(move |store| store.insert_claim(&insert))
        .await?;
    let current = current_blob_for(&state, repo_id, &row).await;
    let out = with_refs(
        &state,
        repo_id,
        claim_out(&row, &repo_name, current.as_deref()),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(out),
    ))
}

/// The subject's CURRENT blob, or `None` when the subject has no path (or
/// the path is not indexed). One store read; never derived, never cached.
async fn current_blob_for(
    state: &SharedState,
    repo_id: i64,
    row: &store::ClaimRow,
) -> Option<String> {
    let path = row.subject_path.clone()?;
    state
        .store
        .run_blocking(move |store| store.get_file(repo_id, &path))
        .await
        .ok()
        .flatten()
        .map(|f| f.blob_hash)
}

#[derive(Debug, Deserialize, Default)]
pub struct ClaimsParams {
    /// The repo to read. Required — a claim address is repo-relative and a
    /// fleet-wide read would silently merge two repos' `app/models/order.rb`.
    pub repo: String,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub subject_kind: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub review: Option<i64>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

/// `GET /api/claims?repo=&[subject=|path=|review=]&kind=&limit=&offset=` —
/// bearer. Every row rides the Ladder (rule (b)); `total` is the true
/// pre-paging count, never the page length.
pub async fn list_claims(
    State(state): State<SharedState>,
    Query(params): Query<ClaimsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = repo.name.clone();
    if let Some(k) = params.subject_kind.as_deref() {
        if !SUBJECT_KINDS.contains(&k) {
            return Err(ApiError::bad_request(format!(
                "subject_kind must be one of {SUBJECT_KINDS:?}, got {k:?}"
            )));
        }
    }
    if let Some(k) = params.kind.as_deref() {
        if !KINDS.contains(&k) {
            return Err(ApiError::bad_request(format!(
                "kind must be one of {KINDS:?}, got {k:?}"
            )));
        }
    }
    if let Some(p) = params.path.as_deref() {
        safe_rel_path(p)?;
    }
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);
    let filter = store::ClaimFilter {
        repo_id,
        subject: params.subject.clone(),
        subject_kind: params.subject_kind.clone(),
        subject_path: params.path.clone(),
        review_id: params.review,
        kind: params.kind.clone(),
    };
    let (rows, total, blobs, refs) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let total = store.count_claims(&filter)?;
            let rows = store.list_claims(&filter, limit, offset)?;
            let mut blobs: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for r in &rows {
                if let Some(p) = r.subject_path.as_deref() {
                    if !blobs.contains_key(p) {
                        if let Some(f) = store.get_file(repo_id, p)? {
                            blobs.insert(p.to_string(), f.blob_hash);
                        }
                    }
                }
            }
            // V76-B3 (kbc-prose/1) — one refs pass over the page, inside the
            // SAME store trip as the reads it resolves against.
            let refs = rows
                .iter()
                .map(|r| {
                    crate::prose_refs::field_refs(
                        store,
                        &crate::prose_refs::RefCtx {
                            repo_id,
                            review_id: r.review_id,
                            ps_number: None,
                        },
                        &r.body_md,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((rows, total, blobs, refs))
        })
        .await?;
    let claims: Vec<ClaimOut> = rows
        .iter()
        .zip(refs)
        .map(|(r, refs)| {
            let current = r.subject_path.as_deref().and_then(|p| blobs.get(p));
            let mut out = claim_out(r, &repo_name, current.map(String::as_str));
            out.refs = refs;
            out
        })
        .collect();
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "repo": repo_name,
            "total": total,
            "returned": claims.len(),
            "offset": offset,
            "limit": limit,
            "claims": claims,
        })),
    ))
}

/// `GET /api/claims/{id}` — bearer. One claim, on the Ladder.
pub async fn get_claim(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    let wanted = id.clone();
    let row = state
        .store
        .run_blocking(move |store| store.get_claim(&wanted))
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no such claim: {id}")))?;
    let repo = crate::routes::find_repo_by_id(&state, row.repo_id)?;
    let repo_name = repo.name.clone();
    let current = current_blob_for(&state, row.repo_id, &row).await;
    let out = with_refs(
        &state,
        row.repo_id,
        claim_out(&row, &repo_name, current.as_deref()),
    )
    .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

// --- route contracts (invariant 15) ---------------------------------------

use crate::entities::RouteContract;

fn claims_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [("repo", "r"), ("kind", "note")] {
        if k != omit {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<ClaimsParams>(serde_json::Value::Object(map)).is_ok()
}

pub const CLAIMS_ROUTE: RouteContract = RouteContract {
    path: "/api/claims",
    handler: "claims::list_claims",
    required_params: &["repo"],
    params_accept_without: claims_accept_without,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vocabularies_are_closed_and_disjoint_from_the_trust_words() {
        assert_eq!(SUBJECT_KINDS.len(), 7);
        assert_eq!(KINDS.len(), 6);
        for w in ["exact", "likely", "candidate"] {
            assert!(
                !KINDS.contains(&w)
                    && ![STATE_PINNED, STATE_DRIFTED, STATE_UNANCHORED].contains(&w),
                "{w:?} is the TRUST vocabulary — a claim is prose and never wears it"
            );
        }
    }

    #[test]
    fn subject_path_is_only_minted_for_the_two_kinds_that_have_one() {
        assert_eq!(
            subject_path("path", "app/models/order.rb"),
            Some("app/models/order.rb".into())
        );
        assert_eq!(
            subject_path("hunk", "app/models/order.rb@2#3"),
            Some("app/models/order.rb".into())
        );
        for k in ["sym", "ent", "commit", "review", "branch"] {
            assert_eq!(subject_path(k, "whatever"), None, "{k}");
        }
    }

    #[test]
    fn the_ladder_greys_a_moved_blob_and_never_re_anchors() {
        let (state, caption) = ladder_state(Some("aaa"), Some("aaa"));
        assert_eq!(state, STATE_PINNED);
        assert!(caption.contains("aaa"));

        let (state, caption) = ladder_state(Some("aaa"), Some("bbb"));
        assert_eq!(state, STATE_DRIFTED);
        assert!(caption.contains("aaa") && caption.contains("bbb"));
        assert!(caption.contains("NOT re-anchored"));

        let (state, _) = ladder_state(Some("aaa"), None);
        assert_eq!(state, STATE_DRIFTED);

        let (state, caption) = ladder_state(None, Some("bbb"));
        assert_eq!(state, STATE_UNANCHORED);
        assert!(caption.contains("pinned no blob"));
    }

    /// Rule (a), STRUCTURALLY. Every module in this crate that RANKS
    /// anything is listed here; if one of them ever names `claims`, this
    /// fails by file. A convention ("remember not to score claims") would
    /// not survive a year; a source scan does. Same shape (and the same
    /// stated limits — it proves the expression does not occur, not a
    /// dataflow property) as `git_argv_lint` and
    /// `every_declared_filter_key_has_a_consumer`.
    #[test]
    fn no_ranking_module_imports_the_claim_register() {
        const RANKERS: &[(&str, &str)] = &[
            ("search/matcher.rs", include_str!("search/matcher.rs")),
            ("search/unified.rs", include_str!("search/unified.rs")),
            ("search/results.rs", include_str!("search/results.rs")),
            ("review_inbox.rs", include_str!("review_inbox.rs")),
            ("review_analytics.rs", include_str!("review_analytics.rs")),
            ("unified_inbox.rs", include_str!("unified_inbox.rs")),
            ("resolve.rs", include_str!("resolve.rs")),
            ("usages2.rs", include_str!("usages2.rs")),
        ];
        for (name, src) in RANKERS {
            assert!(
                !src.contains("claims::") && !src.contains("crate::claims"),
                "{name} names the claim register — kbc-claim/1 is SURFACED, NEVER SCORED \
                 (V0035's own header): a claim may be rendered beside a ranked row, never \
                 folded into its score"
            );
        }
    }

    #[test]
    fn the_route_contract_requires_repo() {
        assert!(claims_accept_without(""));
        assert!(!claims_accept_without("repo"));
    }

    /// V73-K5 (gap 7, bonus) — "nothing formally links a question to its
    /// answer" is closed WITHOUT a new claims column: an `answer`-kind
    /// claim names the review question it answers via the SAME `evidence[]`
    /// mechanism every other claim already uses, now that `question:<n>`
    /// is a real kbc scheme (`review_doc::refs::SCHEMES`). No change to
    /// `validate` was needed — this test pins that it already Just Works.
    #[test]
    fn an_answer_kind_claim_can_cite_the_question_it_answers() {
        let body = ClaimBody {
            schema: SCHEMA.to_string(),
            repo: "acme-app".to_string(),
            subject_kind: "review".to_string(),
            subject: "7".to_string(),
            kind: "answer".to_string(),
            body_md: "yes, see the migration guard added in f-3".to_string(),
            confidence: Some(0.9),
            evidence: vec!["question:2".to_string(), "finding:f-3".to_string()],
            session_id: None,
            model: None,
            blob_sha: None,
            review_id: Some(7),
        };
        assert!(validate(&body).is_ok());
    }

    #[test]
    fn a_ci_ref_is_also_valid_claim_evidence() {
        let body = ClaimBody {
            schema: SCHEMA.to_string(),
            repo: "acme-app".to_string(),
            subject_kind: "review".to_string(),
            subject: "7".to_string(),
            kind: "explain".to_string(),
            body_md: "the flaky check is unrelated to this change".to_string(),
            confidence: None,
            evidence: vec!["ci:build (ubuntu-latest)".to_string()],
            session_id: None,
            model: None,
            blob_sha: None,
            review_id: None,
        };
        assert!(validate(&body).is_ok());
    }
}
