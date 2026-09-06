//! PRR-R5 ("The PR Room," kb v0.39 T2, Phase 5) — GitHub-shaped export
//! (`GET /api/reviews/{id}/export/github`, ordinary `auth_bearer` review-
//! read) + publish recording (`POST /api/reviews/{id}/findings/{slug}/
//! published`, `POST /api/reviews/{id}/verdict/published` — S2-B GATED:
//! loopback unconditionally, else `[review] remote_mutations`-gated
//! bearer, default OFF ⇒ 404 for a non-loopback caller, see
//! `crate::review_gate::review_mutations_gate`). Spec of record: `/tmp/
//! design-server.md` §2 rows 14-16 + §3.2 (the position-mapping contract).
//!
//! # Pure computation, zero GitHub calls
//!
//! Mirrors `crate::review_distill`'s posture exactly: every input is either
//! immutable (patchset shas, finding content) or read fresh at request time
//! (findings, the review's verdict, the pinned blob at the target
//! patchset). This route never talks to GitHub — `kb-code review
//! export-github` hands an agent a ready-to-`gh` payload, and the agent's
//! own `gh api .../pulls/comments` / `gh pr review` calls are 100% outside
//! this daemon (design doc §3.2's own framing: "kb-code never calls
//! GitHub's write API — it only computes the payload and, after the fact,
//! records what was published").
//!
//! # Position mapping — the SAME ladder, never a second one
//!
//! Every candidate finding's line/side is resolved via
//! `crate::review_comments::resolve_for_ps_with_content` against the
//! review's LATEST patchset — the identical function `/comments`,
//! `/distill`, and `/findings` all use, so an export can never disagree
//! with what a human sees in the browser for the same finding.
//!
//! # The export-time "orphan" bucket is WIDER than `resolution.orphaned`
//!
//! Design doc Risk #1 is explicit that `multi`/`whole_file` findings are
//! excluded from GitHub line-comments **by default**, alongside any
//! genuinely orphaned `single`/`range` finding — even though
//! `resolve_for_ps_with_content` reports `orphaned:false` for a
//! **present** `whole_file` path (no line claim ever made) and for a
//! `multi` finding (whose anchor only ever pins the FIRST of its cited
//! lines — the location-kind ladder's own "documented approximation").
//! Posting either as a precise single-line GitHub review comment would be
//! dishonest, so [`classify_for_export`] treats "no real line precision"
//! as its own, wider skip reason, independent of `resolution.orphaned`.
//!
//! # Candidate set
//!
//! Starts from every NON-superseded finding on the review
//! (`list_review_findings(review_id, None, include_superseded=false)`),
//! then:
//! 1. `published_state == "published"` findings are ALWAYS excluded — no
//!    override flag exists for this (design doc Risk #5: "the guard is
//!    the export route filters out published findings by default, not a
//!    lock" — the ONLY guard against a double-post is this exclusion, so
//!    it does not get an opt-out).
//! 2. `disposition == "waive"` findings are excluded UNLESS
//!    `?include_waived=true`.
//! 3. `?finding_slugs=f-a,f-b` (comma-separated, matching this crate's own
//!    csv-query convention), when present, narrows the set to exactly
//!    those slugs — applied AFTER (1)/(2), i.e. it selects a subset of the
//!    already-eligible set rather than bypassing the published/waived
//!    guards. A named slug that fails (1)/(2), is superseded, or does not
//!    exist on this review simply does not appear anywhere in the
//!    response (never a 400 — matches this route's "structural fact, not
//!    an error" posture for `event: null` too).
//!
//! # `line_end` — a deliberate, additive field beyond the design doc's
//! literal sketch
//!
//! The design doc's row-14 sketch shows `comments[]` as `{path, line,
//! side, body, finding_slug, orphaned:false}` with no `start_line`/
//! `line_end`. A `range`-kind finding resolves BOTH endpoints via
//! `resolve_for_ps_with_content` (`ResolvedForPs::line`/`line_end`); GitHub
//! itself needs a `start_line` to render a real multi-line review comment,
//! and silently dropping the range's start would misrepresent a
//! multi-line finding as a single-line one at its end — the "wrong line
//! is worse than an honest omission" law this codebase applies everywhere
//! else. `line_end` is `null` for a `single`/`whole_file`-derived comment,
//! set for a resolved `range`. This is ADDITIVE only (every literally-named
//! field is still present, byte-identical in meaning).

use crate::annotations;
use crate::review_comments::{self, ResolvedForPs};
use crate::reviews::require_review;
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{self, ReviewFindingRow, StoreBlocking};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

pub const EXPORT_SCHEMA: &str = "kbc-github-export/1";

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Deliberately self-contained rather than reusing `reviews::
/// emit_review_changed` (module-private to `reviews.rs`, a multi-phase hot
/// file this unit avoids touching) or `review_findings::
/// emit_findings_review_changed` (module-private to that file, same
/// avoid-the-shared-hot-file reasoning R3 already documented for its own
/// copy). Same `review.changed` wire shape.
fn emit_review_changed(
    bus: &kb_core::events::EventBus,
    review_id: i64,
    repo: &str,
    reason: &str,
    finding_slug: Option<&str>,
) {
    let mut body = serde_json::json!({
        "review_id": review_id,
        "repo": repo,
        "reason": reason,
    });
    if let Some(slug) = finding_slug {
        body["finding_slug"] = serde_json::json!(slug);
    }
    bus.emit("review.changed", body);
}

// --- GET /api/reviews/{id}/export/github ----------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct ExportGithubParams {
    /// Comma-separated finding slugs — narrows the candidate set to
    /// exactly these (still subject to the published/waived guards; see
    /// the module doc's "Candidate set" section).
    #[serde(default)]
    pub finding_slugs: Option<String>,
    #[serde(default)]
    pub include_waived: bool,
    #[serde(default)]
    pub include_orphaned_as_general: bool,
}

/// Why one candidate finding did NOT become a `comments[]` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkipReason {
    /// `resolve_for_ps_with_content` reports the anchor orphaned (file
    /// gone, snippet mismatch, stale).
    Orphaned,
    /// `location_kind == "whole_file"` — no line claim was ever made.
    WholeFile,
    /// `location_kind == "multi"` — only the FIRST cited line has real
    /// anchor precision (the location-kind ladder's own documented
    /// approximation); posting it as a precise single-line comment would
    /// mislead.
    MultiLine,
}

impl SkipReason {
    fn wire(self) -> &'static str {
        match self {
            SkipReason::Orphaned => "orphaned",
            SkipReason::WholeFile => "whole_file",
            SkipReason::MultiLine => "multi_line",
        }
    }
}

/// Design doc Risk #1's "no real line precision" bucket — WIDER than
/// `resolution.orphaned` alone (see the module doc). `None` means the
/// finding is safe to export as a real `path`/`line`/`side` comment.
fn classify_for_export(
    finding: &ReviewFindingRow,
    resolution: &ResolvedForPs,
) -> Option<SkipReason> {
    if resolution.orphaned {
        return Some(SkipReason::Orphaned);
    }
    if finding.location_kind == store::LOCATION_KIND_WHOLE_FILE {
        return Some(SkipReason::WholeFile);
    }
    if finding.location_kind == store::LOCATION_KIND_MULTI {
        return Some(SkipReason::MultiLine);
    }
    None
}

/// The finding's first cited line, straight from its OWN stored
/// `location_lines` JSON (not `resolution.original`, which is only `Some`
/// on a genuinely orphaned anchor — see `ResolvedForPs::original`'s own
/// doc) — so a `whole_file`/`multi` skip (`resolution.orphaned == false`)
/// still gets an honest `original.line` in `skipped_orphaned`/
/// `general_comments`. `None` for `whole_file` (no line was ever cited).
fn first_location_line(finding: &ReviewFindingRow) -> Option<i64> {
    finding
        .location_lines
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<i64>>(s).ok())
        .and_then(|v| v.first().copied())
}

/// Assembly of already-authored fields (title/rationale/recommendation)
/// plus a structural `f-slug` footer — never new prose (design doc §3.2:
/// "it authors no new prose, only assembles").
fn compose_comment_body(f: &ReviewFindingRow) -> String {
    let mut out = format!("**{}**\n\n{}", f.title, f.rationale);
    if let Some(rec) = f.recommendation.as_deref() {
        if !rec.trim().is_empty() {
            out.push_str("\n\n**Recommendation:** ");
            out.push_str(rec);
        }
    }
    out.push_str(&format!("\n\n---\n_finding: {}_", f.slug));
    out
}

/// Same assembly as [`compose_comment_body`], plus an honest
/// "was originally at `path[:line]`" preamble — a general (no path/line)
/// PR comment loses its anchor, so the body is the only place left to say
/// where it used to point.
fn compose_general_body(f: &ReviewFindingRow, original_line: Option<i64>) -> String {
    let loc = match original_line {
        Some(l) => format!("{}:{l}", f.location_path),
        None => f.location_path.clone(),
    };
    let mut out = format!("**{}** — originally at `{loc}`\n\n{}", f.title, f.rationale);
    if let Some(rec) = f.recommendation.as_deref() {
        if !rec.trim().is_empty() {
            out.push_str("\n\n**Recommendation:** ");
            out.push_str(rec);
        }
    }
    out.push_str(&format!("\n\n---\n_finding: {}_", f.slug));
    out
}

/// `GET /api/reviews/{id}/export/github?finding_slugs=&include_waived=
/// &include_orphaned_as_general=` (design doc §2 row 14 / §3.2). Bearer —
/// no more sensitive than the rest of the review-read surface. `200`
/// always (never `400`): an unset verdict is a structural fact
/// (`event:null, event_reason:"no_verdict_set"`), not an error.
pub async fn export_github_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<ExportGithubParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, _repo_id) = require_review(&state, id).await?;

    // --- candidate set ---------------------------------------------------
    let explicit_slugs: Option<HashSet<String>> = params.finding_slugs.as_deref().map(|s| {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    });
    let include_waived = params.include_waived;
    // 2026-08-31 incident (store.rs module doc): the four sequential reads
    // below (patchset, pr binding, findings, annotations) are contiguous
    // store work — one blocking-pool trip; the resolution loop after stays
    // outside since it touches only the git blob cache, not the store.
    let (latest_ps, binding, candidates, by_id) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let latest_ps = store
                .latest_patchset(id)?
                .ok_or_else(|| ApiError::not_found(format!("review {id} has no patchsets")))?;
            let binding = store.get_review_pr_binding(id)?.unwrap_or_default();
            let candidates: Vec<ReviewFindingRow> = store
                .list_review_findings(id, None, false)? // non-superseded only
                .into_iter()
                .filter(|f| f.published_state != "published") // never overridable (Risk #5)
                .filter(|f| include_waived || f.disposition.as_deref() != Some("waive"))
                .filter(|f| {
                    explicit_slugs
                        .as_ref()
                        .map(|set| set.contains(&f.slug))
                        .unwrap_or(true)
                })
                .collect();

            // --- resolve each candidate against latest_ps, same ladder as
            // /comments/ /findings ---
            let ann_rows = store.list_review_annotations(id, true)?;
            let by_id: HashMap<String, store::AnnotationRow> = ann_rows
                .into_iter()
                .filter(|r| r.parent_id.is_none())
                .map(|r| (r.id.clone(), r))
                .collect();
            Ok((latest_ps, binding, candidates, by_id))
        })
        .await?;

    // --- verdict -> GitHub review event (design doc §3.2's own table) ---
    let (event, event_reason): (Option<&'static str>, Option<&'static str>) =
        match review.verdict.as_deref() {
            Some("approve") => (Some("APPROVE"), None),
            Some("request-changes") => (Some("REQUEST_CHANGES"), None),
            Some("comment") => (Some("COMMENT"), None),
            _ => (None, Some("no_verdict_set")),
        };

    let mut blob_cache: HashMap<(String, String), Option<String>> = HashMap::new();
    let mut comments: Vec<serde_json::Value> = Vec::new();
    let mut general_comments: Vec<serde_json::Value> = Vec::new();
    let mut skipped_orphaned: Vec<serde_json::Value> = Vec::new();

    for f in &candidates {
        // A finding's annotation is a 1:1 FK — `None` here should be
        // unreachable in practice; degrade to an honest skip (never
        // panic/500) mirroring `review_findings::compose_finding_view`'s
        // own precedent for this should-never-happen gap.
        let Some(ann) = by_id.get(&f.annotation_id) else {
            skipped_orphaned.push(serde_json::json!({
                "finding_slug": f.slug,
                "reason": "orphaned",
                "original": { "ps": latest_ps.ps_number, "line": first_location_line(f) },
            }));
            continue;
        };

        let sha = review_comments::target_sha_for_side(ann.side.as_deref(), &latest_ps).to_string();
        let content = if ann.anchor_kind == annotations::ANCHOR_KIND_REVIEW {
            None
        } else {
            let key = (ann.path.clone(), sha.clone());
            if !blob_cache.contains_key(&key) {
                blob_cache.insert(
                    key.clone(),
                    review_comments::read_blob_text(&repo.path, &ann.path, &sha),
                );
            }
            blob_cache.get(&key).and_then(|c| c.as_deref())
        };
        let resolution =
            review_comments::resolve_for_ps_with_content(ann, &latest_ps, &sha, content);

        if let Some(reason) = classify_for_export(f, &resolution) {
            let original_ps = ann.ps_number.unwrap_or(latest_ps.ps_number);
            let original_line = first_location_line(f);
            if params.include_orphaned_as_general {
                general_comments.push(serde_json::json!({
                    "body": compose_general_body(f, original_line),
                    "finding_slug": f.slug,
                    "reason": reason.wire(),
                }));
            } else {
                skipped_orphaned.push(serde_json::json!({
                    "finding_slug": f.slug,
                    "reason": reason.wire(),
                    "original": { "ps": original_ps, "line": original_line },
                }));
            }
            continue;
        }

        let side = if ann.side.as_deref() == Some("old") {
            "LEFT"
        } else {
            "RIGHT"
        };
        comments.push(serde_json::json!({
            "path": f.location_path,
            "line": resolution.line,
            "line_end": resolution.line_end,
            "side": side,
            "body": compose_comment_body(f),
            "finding_slug": f.slug,
            "orphaned": false,
        }));
    }

    // Design doc §3.2 point 4 — checked against the STORED snapshot only,
    // no live GitHub call. No known `pr_head_sha` (never bound / never
    // fetched) means there is nothing to have drifted FROM, so `false`,
    // never a guessed staleness.
    let stale_export = binding
        .pr_head_sha
        .as_deref()
        .map(|h| h != latest_ps.tip_sha)
        .unwrap_or(false);

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": EXPORT_SCHEMA,
            "review_id": id,
            "repo": review.repo,
            "ps_number": latest_ps.ps_number,
            "event": event,
            "event_reason": event_reason,
            "body": review.verdict_note,
            "commit_id": latest_ps.tip_sha,
            "comments": comments,
            "general_comments": general_comments,
            "skipped_orphaned": skipped_orphaned,
            "stale_export": stale_export,
        })),
    ))
}

// --- POST /api/reviews/{id}/findings/{slug}/published ---------------------

/// Design doc §2 row 15's `{github_comment_id?, github_comment_url?,
/// published_at?}`. `github_comment_id` is accepted but NOT persisted — R1
/// shipped `Store::set_finding_published(review_id, slug, published_url,
/// published_at)` (and the V0024 schema backing it) with no
/// `published_comment_id` column, mirroring the SAME "accepted, echoed
/// nowhere durable" posture `FindingsImportBody::import_note` already
/// documents for the exact same reason (one owner writes the columns that
/// exist; this unit doesn't grow R1's shipped schema for a field the spec
/// lists as optional advisory metadata). A documented deviation, not an
/// oversight — see this unit's own report.
#[derive(Debug, Deserialize, Default)]
pub struct PublishFindingBody {
    #[serde(default)]
    #[allow(dead_code)]
    pub github_comment_id: Option<serde_json::Value>,
    #[serde(default)]
    pub github_comment_url: Option<String>,
    #[serde(default)]
    pub published_at: Option<i64>,
}

/// `POST /api/reviews/{id}/findings/{slug}/published` (design doc §2 row
/// 15) — S2-B GATED (`router.rs`'s `review_remote` sub-router: loopback
/// unconditionally, else `[review] remote_mutations`, default OFF — see
/// [`crate::review_gate::review_mutations_gate`]). Advisory-only (Risk #5): recorded AFTER the
/// agent's own `gh` call succeeds, never verified. Idempotent-by-
/// overwrite — publishing the SAME slug twice just updates
/// `published_at`/`published_url` (no distinct "already published" error),
/// which is what makes a re-export naturally exclude it on the next call
/// (the `published_state == "published"` guard in
/// [`export_github_route`]). `404` for an unknown `(review, slug)`. Emits
/// `review.changed{reason:"finding_published", finding_slug}`.
pub async fn publish_finding_route(
    State(state): State<SharedState>,
    AxumPath((id, slug)): AxumPath<(i64, String)>,
    Json(body): Json<PublishFindingBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, _repo_id) = require_review(&state, id).await?;
    let at = body.published_at.unwrap_or_else(now_unix);
    let slug_c = slug.clone();
    let comment_url = body.github_comment_url.clone();
    let existed = state
        .store
        .run_blocking(move |store| {
            store.set_finding_published(id, &slug_c, comment_url.as_deref(), at)
        })
        .await?;
    if !existed {
        return Err(ApiError::not_found(format!(
            "no finding {slug:?} on review {id}"
        )));
    }
    emit_review_changed(
        &state.bus,
        id,
        &review.repo,
        "finding_published",
        Some(&slug),
    );

    let slug_c2 = slug.clone();
    let (target_ps, row) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let target_ps = store
                .latest_patchset(id)?
                .ok_or_else(|| ApiError::not_found(format!("review {id} has no patchsets")))?;
            let row = store.get_review_finding(id, &slug_c2)?.ok_or_else(|| {
                crate::routes::ApiError::new(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "finding vanished immediately after publish recording",
                )
            })?;
            Ok((target_ps, row))
        })
        .await?;
    let repo_root = repo.path.clone();
    let view = state
        .store
        .run_blocking(move |store| {
            crate::review_findings::compose_finding_view(store, &repo_root, &target_ps, &row)
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(view)))
}

// --- POST /api/reviews/{id}/verdict/published ------------------------------

/// Design doc §2 row 16's `{github_review_id?, github_review_url?,
/// published_at?}`. `github_review_id` is accepted but NOT persisted — same
/// reasoning as [`PublishFindingBody::github_comment_id`]; `reviews.
/// verdict_published_at`/`verdict_published_url` (V0024) has no id column
/// either.
#[derive(Debug, Deserialize, Default)]
pub struct PublishVerdictBody {
    #[serde(default)]
    #[allow(dead_code)]
    pub github_review_id: Option<serde_json::Value>,
    #[serde(default)]
    pub github_review_url: Option<String>,
    #[serde(default)]
    pub published_at: Option<i64>,
}

/// `POST /api/reviews/{id}/verdict/published` (design doc §2 row 16) —
/// S2-B GATED, same admission table as [`publish_finding_route`] above.
/// Same advisory/idempotent-by-overwrite posture as
/// [`publish_finding_route`]. `404` for an unknown review. Emits
/// `review.changed{reason:"verdict_published"}`.
pub async fn publish_verdict_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<PublishVerdictBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, _repo, _repo_id) = require_review(&state, id).await?;
    let at = body.published_at.unwrap_or_else(now_unix);
    let review_url = body.github_review_url.clone();
    let existed = state
        .store
        .run_blocking(move |store| {
            store.set_review_verdict_published(id, review_url.as_deref(), at)
        })
        .await?;
    if !existed {
        return Err(ApiError::not_found(format!("no such review: {id}")));
    }
    emit_review_changed(&state.bus, id, &review.repo, "verdict_published", None);

    let (published_at, published_url) = state
        .store
        .run_blocking(move |store| store.get_review_verdict_published(id))
        .await?
        .unwrap_or((None, None));
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "id": id,
            "verdict_published_at": published_at,
            "verdict_published_url": published_url,
        })),
    ))
}
