//! Fine-grained comment endpoints (R5). The pre-R5 surface was just
//! `GET`/`POST` of the *whole* `ReviewFile`; every client reconstructed
//! mutations itself (the CLI did untyped `serde_json` surgery, the SPA
//! rebuilt the document + retried on 412). These routes let the CLI and
//! SPA send a small delta and have the daemon own the mutation:
//!
//!   POST   …/review/{id}/comments                 add a comment   → 201
//!   POST   …/review/{id}/comments/{cid}/replies   add a reply     → 201
//!   POST   …/review/{id}/comments/{cid}/resolve   resolve one
//!   POST   …/review/{id}/comments/{cid}/unresolve reopen one
//!   POST   …/review/{id}/resolve-all              bulk resolve
//!   POST   …/review/{id}/unresolve-all            bulk reopen
//!   PATCH  …/review/{id}/comments/{cid}           edit comment body
//!   PATCH  …/review/{id}/comments/{cid}/replies/{rid}  edit reply body
//!   PATCH  …/review/{id}/comments/{cid}/anchor    re-point the anchor (R9)
//!   DELETE …/review/{id}/comments/{cid}           delete a comment
//!   DELETE …/review/{id}/comments/{cid}/replies/{rid}  delete a reply
//!   GET    …/reviews                              list/query comments
//!
//! Concurrency: every mutation runs the load → typed-mutation → save
//! sequence under the per-kb `review_lock` (`review_lock_for(&kb)`), so
//! the targeted change
//! is atomic without the client juggling `If-Match` (the whole-doc POST's
//! optimistic concurrency was needed only because two clients could PUT
//! conflicting *whole documents*; a targeted append/flip can't). Each
//! mutation emits `comments.updated`; `add` additionally records a
//! `history.recorded` row (the only mutation that creates a new comment).
//!
//! Empty-file contract: `add` creates the review file on absence; every
//! other mutation 404s when the file (or the comment/reply) is missing.

use crate::middleware::{error_to_problem_json, Identity};
use crate::routes::is_safe_id;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use kb_core::review::{self, Anchor, Author, Choice, CommentStatus, NewComment, ReviewFile};
use kb_core::types::KbName;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

// --- request bodies --------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AddCommentBody {
    pub body: String,
    pub anchor: Anchor,
    pub author: Author,
    /// Source file the comment is anchored to (a page `src` for multi-page
    /// artifacts). Defaults to the artifact id.
    #[serde(default)]
    pub file: Option<String>,
    #[serde(rename = "fileLabel", default)]
    pub file_label: Option<String>,
    #[serde(default)]
    pub choices: Vec<Choice>,
    /// Y4 — staged attachment ids to adopt onto the new comment (the SPA
    /// stages at compose-time, then passes the ids here). Additive +
    /// `#[serde(default)]` so existing clients are unaffected.
    #[serde(default)]
    pub attachment_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddReplyBody {
    pub author: Author,
    pub body: String,
    #[serde(default)]
    pub choices: Vec<Choice>,
    /// Y4 — staged attachment ids to adopt onto the new reply.
    #[serde(default)]
    pub attachment_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct EditBody {
    pub body: String,
}

/// R9 — body of `PATCH …/comments/{cid}/anchor`: the replacement anchor.
#[derive(Debug, Deserialize)]
pub struct SetAnchorBody {
    pub anchor: Anchor,
}

/// W2.15a — body of `POST …/review/{id}/verdict`.
#[derive(Debug, Deserialize)]
pub struct SetVerdictBody {
    pub state: kb_core::review::VerdictState,
    #[serde(default)]
    pub note: Option<String>,
}

// --- list query + row ------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ReviewsQuery {
    /// Restrict to artifacts in this folder (relative to the kb source
    /// root). Artifacts no longer in lance can't be folder-matched.
    pub folder: Option<String>,
    /// `open` (default) | `resolved` | `all`.
    pub status: Option<String>,
    /// `you` | `claude` — filter by comment author.
    pub author: Option<String>,
    /// v0.34 Y1 — filter by attribution username (beside `author` role).
    pub user: Option<String>,
    /// When `true`, only comments whose anchor is currently stale.
    pub stale: Option<bool>,
    /// Narrow to a single artifact id (the CLI resolves `--path` to an id
    /// first, then passes it here).
    pub artifact_id: Option<String>,
}

/// One row of `GET /reviews` — a strict superset of the pre-R6 `kb comments
/// list --json` keys (additive only: `file`, `file_label`, `stale`, `folder`,
/// `source_relative`), so existing `| jq` consumers keep working. Keys stay
/// snake_case to match the old CLI output verbatim — `created_at`, NOT
/// `createdAt` (the kb-comments/1 wire schema is camelCase, but this list row
/// is the CLI's shape, not the document's).
#[derive(Debug, Serialize)]
struct ReviewRow {
    kb: String,
    artifact_id: String,
    title: String,
    comment_id: String,
    status: CommentStatus,
    author: Author,
    anchor: Anchor,
    file: String,
    file_label: String,
    body: String,
    created_at: DateTime<Utc>,
    stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_relative: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReviewsResponse {
    comments: Vec<ReviewRow>,
}

// --- shared helpers --------------------------------------------------------

/// Validate the kb (exists) + the artifact id (safe shape). Returns the
/// parsed `KbName` or an early problem+json response.
///
/// The `Err` is a fully-rendered `Response<Body>` (the early-return HTTP
/// response) by design, not a lightweight propagating error — boxing it
/// to satisfy `result_large_err` would add an allocation on every
/// validation failure for no benefit.
#[allow(clippy::result_large_err)]
pub(crate) fn validate(state: &KbHandles, kb: &str, id: &str) -> Result<KbName, Response<Body>> {
    let kb_name = KbName::new(kb).map_err(|e| error_to_problem_json(&e))?;
    if !state.kbs.contains_key(&kb_name) {
        return Err(error_to_problem_json(&kb_core::Error::NotFound(format!(
            "kb {kb_name}"
        ))));
    }
    if !is_safe_id(id) {
        return Err(error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "artifact id {id:?} contains illegal characters"
        ))));
    }
    Ok(kb_name)
}

/// Like `is_safe_id` but for a comment/reply id path segment — same
/// charset (`c_`/`r_` + hex always pass), returns a 400 response on
/// rejection. `Err` is a rendered `Response<Body>` by design (see
/// `validate` above) — `result_large_err` is intentional here.
#[allow(clippy::result_large_err)]
pub(crate) fn check_subid(id: &str, what: &str) -> Result<(), Response<Body>> {
    if is_safe_id(id) {
        Ok(())
    } else {
        Err(error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "{what} {id:?} contains illegal characters"
        ))))
    }
}

/// Emit `comments.updated` for `(kb, id)` carrying the current open/total
/// counts — the SPA's `useReview` refetches on this; the CLI watch loop
/// surfaces it. W2.15a — additive `verdict` field (the current
/// `ReviewFile.verdict`, or `null`) so a fleet inbox/badge can react to a
/// verdict change without an extra round-trip; the SPA already refetches
/// the whole file on this event, so this is a cheap piggyback, not new
/// wiring (invariant #23 — no new SSE event type, no new bridge handler).
pub(crate) fn emit_updated(state: &KbHandles, kb_name: &KbName, id: &str, file: &ReviewFile) {
    emit_updated_by(state, kb_name, id, file, None);
}

/// `comments.updated` with optional triggering actor `user` (v0.34 Y1 —
/// additive; counts payload unchanged when `user` is None).
pub(crate) fn emit_updated_by(
    state: &KbHandles,
    kb_name: &KbName,
    id: &str,
    file: &ReviewFile,
    user: Option<&str>,
) {
    if let Some(ctx) = state.kbs.get(kb_name) {
        let mut payload = json!({
            "kb": kb_name.as_str(),
            "artifact_id": id,
            "open_count": file.open_count(),
            "total_count": file.comments.len(),
            "verdict": file.verdict,
        });
        if let Some(u) = user {
            payload["user"] = json!(u);
        }
        ctx.bus.emit("comments.updated", payload);
    }
}

/// Ownership for comment/reply body EDIT + DELETE (v0.34 Y1 attribution
/// hygiene, not an ACL). A row with `user=None` belongs to the operator.
/// Returns `None` when allowed, or a 403 problem+json response.
fn forbid_if_not_owner(
    identity: &Identity,
    row_user: Option<&str>,
    operator: &str,
) -> Option<Response<Body>> {
    let owner = row_user.unwrap_or(operator);
    if identity.user == owner {
        return None;
    }
    let body = json!({
        "type": "urn:kb:errors:not-owner",
        "title": "Forbidden",
        "status": 403,
        "detail": format!(
            "only the owner ({owner}) may edit or delete this comment/reply; identity is {}",
            identity.user
        ),
    });
    let mut resp = (StatusCode::FORBIDDEN, Json(body)).into_response();
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/problem+json"),
    );
    Some(resp)
}

/// Load → mutate → save → emit, all under `review_lock`, for a mutation
/// that requires an existing review file (everything except `add`). The
/// closure returns the JSON success body; a missing file or missing
/// comment/reply surfaces as the closure's `Error::NotFound` (404) or the
/// load's 404.
async fn with_review_mut<F>(
    state: &KbHandles,
    kb_name: &KbName,
    id: &str,
    success: StatusCode,
    f: F,
) -> Response<Body>
where
    // The closure returns the JSON body PLUS a `changed` flag (G8): when
    // `false` the mutation was a no-op (resolving an already-resolved comment,
    // re-pointing to the identical anchor, resolve-all with nothing to flip),
    // so we skip the atomic rewrite + the `comments.updated` SSE — matching the
    // `if changed { emit }` convention lists/corkboard/notes/history use.
    F: FnOnce(&mut ReviewFile) -> kb_core::Result<(serde_json::Value, bool)>,
{
    let path = state.paths.kb_review_file(kb_name, id);
    let lock = state.review_lock_for(kb_name);
    let guard = lock.lock().await;
    let mut file = match review::load(&path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no comments for {kb_name}/{id}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let (body, changed) = match f(&mut file) {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    if !changed {
        // No-op: don't rewrite the file (which would churn mtime/ETag) or emit.
        drop(guard);
        return (success, Json(body)).into_response();
    }
    if let Err(e) = review::save_atomic(&path, &file, None) {
        return error_to_problem_json(&e);
    }
    drop(guard);
    emit_updated(state, kb_name, id, &file);
    (success, Json(body)).into_response()
}

fn to_value<T: Serialize>(v: &T) -> kb_core::Result<serde_json::Value> {
    serde_json::to_value(v).map_err(|e| kb_core::Error::Serde(e.to_string()))
}

// --- add comment (creates the file on absence) -----------------------------

pub async fn add_comment(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    Path((kb, id)): Path<(String, String)>,
    Json(payload): Json<AddCommentBody>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    let Some(ctx) = state.kbs.get(&kb_name) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("kb {kb_name}")));
    };

    // Title for a freshly-created skeleton comes from storage (the review
    // file's own title is otherwise empty for a CLI-authored first comment).
    let title = ctx
        .storage
        .get_by_id(id.clone())
        .await
        .ok()
        .flatten()
        .map(|d| d.title)
        .unwrap_or_default();

    let user = identity.user.clone();
    let mut spec = NewComment {
        file: payload.file.unwrap_or_else(|| id.clone()),
        file_label: payload.file_label.unwrap_or_else(|| "main".to_string()),
        anchor: payload.anchor,
        author: payload.author,
        body: payload.body,
        choices: payload.choices,
        attachments: Vec::new(),
        // v0.34 Y1 — stamp resolved identity (client-sent `user` ignored).
        user: Some(user.clone()),
    };
    // Attachment limits (read before the lock; only used when adopting).
    let (_, max_per, grace_hours) = crate::routes::attachments::attachment_limits(&state).await;

    let path = state.paths.kb_review_file(&kb_name, &id);
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;
    let mut file = match review::load(&path) {
        Ok(Some(f)) => f,
        Ok(None) => ReviewFile::empty_skeleton(&kb_name, &id, &title),
        Err(e) => return error_to_problem_json(&e),
    };
    // Adopt staged attachments (if any) onto the new comment under the SAME
    // guard as the review mutation — the manifest shares the review_lock.
    let manifest = if payload.attachment_ids.is_empty() {
        None
    } else {
        let mut m =
            kb_core::attachments::load_manifest(&state.paths.kb_attachment_manifest(&kb_name, &id));
        match crate::routes::attachments::adopt_staged(
            &mut m,
            &payload.attachment_ids,
            0,
            max_per,
            Some(&identity.user),
        ) {
            Ok(atts) => spec.attachments = atts,
            Err(e) => return error_to_problem_json(&e),
        }
        Some(m)
    };
    let created = file.add_comment(spec).clone();
    if let Err(e) = review::save_atomic(&path, &file, None) {
        return error_to_problem_json(&e);
    }
    if let Some(m) = manifest {
        // GC + persist the manifest (the new comment now references the
        // adopted blobs, so they're kept; this also flips their `adopted`
        // flag on disk).
        crate::routes::attachments::gc_manifest(
            &state,
            &kb_name,
            &id,
            m,
            &file.referenced_attachment_ids(),
            grace_hours,
        );
    }
    drop(guard);

    emit_updated_by(&state, &kb_name, &id, &file, Some(&user));

    // Record a history row for the new comment + emit `history.recorded`
    // (mirrors the whole-doc POST path; only brand-new comments do this).
    let now_unix = Utc::now().timestamp();
    if let Ok(history_id) = ctx
        .storage
        .history_record_comment(id.clone(), created.id.clone(), now_unix, user.clone())
        .await
    {
        ctx.bus.emit(
            "history.recorded",
            json!({
                "kb": kb_name.as_str(),
                "kind": "comment",
                "id": history_id,
                "artifact_id": id,
                "comment_id": created.id,
                "user": user,
            }),
        );
    }

    let body = match to_value(&created) {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    (StatusCode::CREATED, Json(body)).into_response()
}

// --- add reply -------------------------------------------------------------

pub async fn add_reply(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    Path((kb, id, cid)): Path<(String, String, String)>,
    Json(payload): Json<AddReplyBody>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if let Err(resp) = check_subid(&cid, "comment id") {
        return resp;
    }
    let (_, max_per, grace_hours) = crate::routes::attachments::attachment_limits(&state).await;

    let path = state.paths.kb_review_file(&kb_name, &id);
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;
    let mut file = match review::load(&path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no comments for {kb_name}/{id}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    // Resolve any staged attachments to adopt onto the reply (manifest
    // shares the review_lock).
    let (manifest, atts) = if payload.attachment_ids.is_empty() {
        (None, Vec::new())
    } else {
        let mut m =
            kb_core::attachments::load_manifest(&state.paths.kb_attachment_manifest(&kb_name, &id));
        let atts = match crate::routes::attachments::adopt_staged(
            &mut m,
            &payload.attachment_ids,
            0,
            max_per,
            Some(&identity.user),
        ) {
            Ok(a) => a,
            Err(e) => return error_to_problem_json(&e),
        };
        (Some(m), atts)
    };
    // Create the reply, then attach the adopted set to it.
    let user = identity.user.clone();
    let rid = match file.add_reply(
        &cid,
        payload.author,
        payload.body,
        payload.choices,
        Some(user.clone()),
    ) {
        Ok(r) => r.id.clone(),
        Err(e) => return error_to_problem_json(&e),
    };
    for att in atts {
        if let Err(e) = file.add_reply_attachment(&cid, &rid, att) {
            return error_to_problem_json(&e);
        }
    }
    if let Err(e) = review::save_atomic(&path, &file, None) {
        return error_to_problem_json(&e);
    }
    if let Some(m) = manifest {
        crate::routes::attachments::gc_manifest(
            &state,
            &kb_name,
            &id,
            m,
            &file.referenced_attachment_ids(),
            grace_hours,
        );
    }
    // Response = the freshly-saved reply (carries its attachments).
    let reply_json = match file
        .comments
        .iter()
        .find(|c| c.id == cid)
        .and_then(|c| c.replies.iter().find(|r| r.id == rid))
    {
        Some(r) => match to_value(r) {
            Ok(v) => v,
            Err(e) => return error_to_problem_json(&e),
        },
        None => json!({ "id": rid }),
    };
    drop(guard);
    emit_updated_by(&state, &kb_name, &id, &file, Some(&user));
    (StatusCode::CREATED, Json(reply_json)).into_response()
}

// --- resolve / unresolve (single) ------------------------------------------

pub async fn resolve(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id, cid)): Path<(String, String, String)>,
) -> Response<Body> {
    set_status(state, kb, id, cid, CommentStatus::Resolved).await
}

pub async fn unresolve(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id, cid)): Path<(String, String, String)>,
) -> Response<Body> {
    set_status(state, kb, id, cid, CommentStatus::Open).await
}

async fn set_status(
    state: Arc<KbHandles>,
    kb: String,
    id: String,
    cid: String,
    status: CommentStatus,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if let Err(resp) = check_subid(&cid, "comment id") {
        return resp;
    }
    with_review_mut(&state, &kb_name, &id, StatusCode::OK, |file| {
        let changed = file.set_comment_status(&cid, status)?;
        Ok((
            json!({ "ok": true, "open_count": file.open_count() }),
            changed,
        ))
    })
    .await
}

// --- resolve-all / unresolve-all -------------------------------------------

pub async fn resolve_all(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    set_all_status(state, kb, id, CommentStatus::Resolved).await
}

pub async fn unresolve_all(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    set_all_status(state, kb, id, CommentStatus::Open).await
}

async fn set_all_status(
    state: Arc<KbHandles>,
    kb: String,
    id: String,
    status: CommentStatus,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    with_review_mut(&state, &kb_name, &id, StatusCode::OK, |file| {
        let flipped = file.set_all_status(status);
        Ok((
            json!({ "flipped": flipped, "open_count": file.open_count() }),
            flipped > 0,
        ))
    })
    .await
}

// --- atomic apply batch (v0.19, borrowed from redline) ---------------------

#[derive(Debug, Deserialize)]
pub struct ApplyBatchBody {
    pub ops: Vec<kb_core::review::BatchOp>,
}

/// Cap on ops per batch — generous for an agent's edit-then-reanchor-then-
/// resolve sequence, low enough to bound one request's work.
const MAX_BATCH_OPS: usize = 512;

/// `POST …/review/{id}/apply` — apply an ordered batch of comment mutations
/// atomically under one `review_lock` acquisition, with a single
/// `save_atomic` + one `comments.updated` SSE for the whole batch. All-or-
/// nothing: if any op fails (e.g. a `NotFound` comment id) nothing is
/// written. Creates the review file on absence when the first op is an
/// `add_comment` (mirrors the single `add` route). History rows are recorded
/// for every comment the batch creates, exactly as `add_comment` does.
pub async fn apply_batch(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    Path((kb, id)): Path<(String, String)>,
    Json(payload): Json<ApplyBatchBody>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if payload.ops.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "apply requires at least one op".to_string(),
        ));
    }
    if payload.ops.len() > MAX_BATCH_OPS {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "batch of {} ops exceeds the {MAX_BATCH_OPS} limit",
            payload.ops.len()
        )));
    }
    let Some(ctx) = state.kbs.get(&kb_name) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("kb {kb_name}")));
    };
    // Title for a fresh skeleton (a batch may open with `add_comment` on an
    // artifact that has no review file yet).
    let title = ctx
        .storage
        .get_by_id(id.clone())
        .await
        .ok()
        .flatten()
        .map(|d| d.title)
        .unwrap_or_default();

    let path = state.paths.kb_review_file(&kb_name, &id);
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;
    let mut file = match review::load(&path) {
        Ok(Some(f)) => f,
        Ok(None) => ReviewFile::empty_skeleton(&kb_name, &id, &title),
        Err(e) => return error_to_problem_json(&e),
    };
    // v0.34 Y1: the owner-only policy (body EDIT + DELETE) must hold through
    // the batch path too — gate each owner-gated op against the loaded rows
    // BEFORE applying (all-or-nothing: one forbidden op rejects the whole
    // batch, matching apply_ops' atomicity). A missing target falls through
    // to apply_ops' own NotFound. Resolve/reply/anchor ops stay open to all.
    let operator = state.operator_user().to_string();
    for op in &payload.ops {
        let row_user: Option<Option<&str>> = match op {
            review::BatchOp::EditComment { comment_id, .. }
            | review::BatchOp::DeleteComment { comment_id } => file
                .comments
                .iter()
                .find(|c| &c.id == comment_id)
                .map(|c| c.user.as_deref()),
            review::BatchOp::EditReply {
                comment_id,
                reply_id,
                ..
            }
            | review::BatchOp::DeleteReply {
                comment_id,
                reply_id,
            } => file
                .comments
                .iter()
                .find(|c| &c.id == comment_id)
                .and_then(|c| c.replies.iter().find(|r| &r.id == reply_id))
                .map(|r| r.user.as_deref()),
            _ => None,
        };
        if let Some(row_user) = row_user {
            if let Some(resp) = forbid_if_not_owner(&identity, row_user, &operator) {
                return resp;
            }
        }
    }
    let report = match file.apply_ops(&payload.ops, &id) {
        Ok(r) => r,
        // Atomic: apply_ops mutated only its working clone, so nothing is
        // persisted and the on-disk file is untouched.
        Err(e) => return error_to_problem_json(&e),
    };
    let summary = json!({
        "applied": report.applied,
        "created_comment_ids": report.created_comment_ids,
        "created_reply_ids": report.created_reply_ids,
        "open_count": file.open_count(),
        "total_count": file.comments.len(),
    });
    if !report.mutated {
        // Batch of pure no-ops (e.g. resolving already-resolved comments):
        // skip the rewrite + SSE (G8).
        drop(guard);
        return (StatusCode::OK, Json(summary)).into_response();
    }
    if let Err(e) = review::save_atomic(&path, &file, None) {
        return error_to_problem_json(&e);
    }
    drop(guard);
    emit_updated(&state, &kb_name, &id, &file);
    // History rows for any comments the batch created (mirrors `add_comment`).
    // Note: apply_ops itself does not yet stamp `user` on NewComment inside
    // the batch ops (ops schema is pre-Y); history attribution uses the
    // request Identity. Follow-up for Z: thread user into apply_ops adds.
    let now_unix = Utc::now().timestamp();
    let hist_user = identity.user.clone();
    for cid in &report.created_comment_ids {
        if let Ok(history_id) = ctx
            .storage
            .history_record_comment(id.clone(), cid.clone(), now_unix, hist_user.clone())
            .await
        {
            ctx.bus.emit(
                "history.recorded",
                json!({
                    "kb": kb_name.as_str(),
                    "kind": "comment",
                    "id": history_id,
                    "artifact_id": id,
                    "comment_id": cid,
                    "user": hist_user,
                }),
            );
        }
    }
    (StatusCode::OK, Json(summary)).into_response()
}

// --- portable import (v0.19, borrowed from redline) ------------------------

#[derive(Debug, Deserialize)]
pub struct ImportQuery {
    /// Overwrite an existing non-empty review file. Off by default so an
    /// import can't silently clobber live comments.
    #[serde(default)]
    pub force: bool,
}

/// `POST …/review/{id}/import?force=` — write a whole `kb-comments/1`
/// document to the sidecar, preserving ids / statuses / replies / timestamps
/// (the inverse of `kb comments export --embed`). Refuses to overwrite an
/// existing non-empty review unless `force=true`, so it can't clobber live
/// state — this is a restore/move operation, deliberately distinct from the
/// R8-retired interactive whole-doc POST (which was dropped for concurrent-
/// edit races, not for restore). The body's `artifact` ref is re-pinned to
/// the import target so an artifact's comments can move to a new id/kb.
pub async fn import(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Query(q): Query<ImportQuery>,
    Json(mut incoming): Json<ReviewFile>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if incoming.schema != review::SCHEMA {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "import schema {:?} not supported (expected {})",
            incoming.schema,
            review::SCHEMA
        )));
    }
    incoming.artifact.id = id.clone();
    incoming.artifact.kb = kb_name.as_str().to_string();

    let path = state.paths.kb_review_file(&kb_name, &id);
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;
    match review::load(&path) {
        Ok(Some(existing)) if !existing.comments.is_empty() && !q.force => {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "{kb_name}/{id} already has {} comment(s); pass force=true to overwrite",
                existing.comments.len()
            )));
        }
        Ok(_) => {}
        Err(e) => return error_to_problem_json(&e),
    }
    if let Err(e) = review::save_atomic(&path, &incoming, None) {
        return error_to_problem_json(&e);
    }
    drop(guard);
    emit_updated(&state, &kb_name, &id, &incoming);
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "imported": incoming.comments.len(),
            "open_count": incoming.open_count(),
        })),
    )
        .into_response()
}

// --- edit comment / reply body ---------------------------------------------

pub async fn edit_comment(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    Path((kb, id, cid)): Path<(String, String, String)>,
    Json(payload): Json<EditBody>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if let Err(resp) = check_subid(&cid, "comment id") {
        return resp;
    }
    let operator = state.operator_user().to_string();
    let path = state.paths.kb_review_file(&kb_name, &id);
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;
    let mut file = match review::load(&path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no comments for {kb_name}/{id}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let row_user = match file.comments.iter().find(|c| c.id == cid) {
        Some(c) => c.user.clone(),
        None => return error_to_problem_json(&kb_core::Error::NotFound(format!("comment {cid}"))),
    };
    if let Some(resp) = forbid_if_not_owner(&identity, row_user.as_deref(), &operator) {
        return resp;
    }
    if let Err(e) = file.edit_comment_body(&cid, payload.body) {
        return error_to_problem_json(&e);
    }
    if let Err(e) = review::save_atomic(&path, &file, None) {
        return error_to_problem_json(&e);
    }
    drop(guard);
    emit_updated_by(&state, &kb_name, &id, &file, Some(&identity.user));
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

pub async fn edit_reply(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    Path((kb, id, cid, rid)): Path<(String, String, String, String)>,
    Json(payload): Json<EditBody>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if let Err(resp) = check_subid(&cid, "comment id") {
        return resp;
    }
    if let Err(resp) = check_subid(&rid, "reply id") {
        return resp;
    }
    let operator = state.operator_user().to_string();
    let path = state.paths.kb_review_file(&kb_name, &id);
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;
    let mut file = match review::load(&path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no comments for {kb_name}/{id}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let row_user = match file
        .comments
        .iter()
        .find(|c| c.id == cid)
        .and_then(|c| c.replies.iter().find(|r| r.id == rid))
    {
        Some(r) => r.user.clone(),
        None => return error_to_problem_json(&kb_core::Error::NotFound(format!("reply {rid}"))),
    };
    if let Some(resp) = forbid_if_not_owner(&identity, row_user.as_deref(), &operator) {
        return resp;
    }
    if let Err(e) = file.edit_reply_body(&cid, &rid, payload.body) {
        return error_to_problem_json(&e);
    }
    if let Err(e) = review::save_atomic(&path, &file, None) {
        return error_to_problem_json(&e);
    }
    drop(guard);
    emit_updated_by(&state, &kb_name, &id, &file, Some(&identity.user));
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

// --- re-point a comment's anchor (R9) --------------------------------------

/// `PATCH …/review/{id}/comments/{cid}/anchor` — replace a comment's
/// anchor with an explicit, ground-truth one (Claude re-points after it
/// moves/renames the anchored element during an unrelated edit). Modeled
/// on `delete_comment` rather than `with_review_mut` because it also
/// prunes the stale-anchor sidecar: the old anchor's stale flag is
/// meaningless once re-pointed, so clearing it now gives instant
/// correctness for `GET /reviews?stale=true` + `/api/anchors/stale`. The
/// indexer re-derives staleness against the *new* anchor on the next
/// reindex (imminent — Claude reanchors right after editing the HTML), so
/// no resolver logic is duplicated here.
pub async fn set_anchor(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id, cid)): Path<(String, String, String)>,
    Json(payload): Json<SetAnchorBody>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if let Err(resp) = check_subid(&cid, "comment id") {
        return resp;
    }

    let path = state.paths.kb_review_file(&kb_name, &id);
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;
    let mut file = match review::load(&path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no comments for {kb_name}/{id}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let changed = match file.set_comment_anchor(&cid, payload.anchor) {
        Ok(c) => c,
        Err(e) => return error_to_problem_json(&e),
    };
    if !changed {
        // No-op re-point (same anchor) — skip the rewrite, sidecar prune, and
        // `comments.updated` emit (G8).
        drop(guard);
        return (StatusCode::OK, Json(json!({ "ok": true }))).into_response();
    }
    if let Err(e) = review::save_atomic(&path, &file, None) {
        return error_to_problem_json(&e);
    }
    // Prune any stale-anchor sidecar entry for this comment — the old
    // anchor's flag no longer applies once re-pointed. Done UNDER the
    // per-kb guard (before drop) so two same-kb comment mutations can't
    // race a load→remove→save on the shared sidecar and lose an update.
    // (The indexer is a separate, unsynchronized sidecar writer that
    // self-heals its in-memory map + re-evaluates the new anchor on the
    // next reindex; that cross-process write is out of this lock's scope.)
    let review_dir = state.paths.kb_review_dir(&kb_name);
    let sidecar = kb_core::anchors::sidecar_path(&review_dir);
    let mut stale = kb_core::anchors::load(&sidecar);
    if stale.remove(&(id.clone(), cid.clone())).is_some() {
        if let Err(e) = kb_core::anchors::save(&sidecar, &stale) {
            tracing::warn!(kb = %kb_name, error = %e, "failed to prune anchor-stale sidecar on reanchor");
        }
    }
    drop(guard);

    emit_updated(&state, &kb_name, &id, &file);
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

// --- delete comment / reply ------------------------------------------------

pub async fn delete_comment(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    Path((kb, id, cid)): Path<(String, String, String)>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if let Err(resp) = check_subid(&cid, "comment id") {
        return resp;
    }

    let operator = state.operator_user().to_string();
    let path = state.paths.kb_review_file(&kb_name, &id);
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;
    let mut file = match review::load(&path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no comments for {kb_name}/{id}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let row_user = match file.comments.iter().find(|c| c.id == cid) {
        Some(c) => c.user.clone(),
        None => return error_to_problem_json(&kb_core::Error::NotFound(format!("comment {cid}"))),
    };
    if let Some(resp) = forbid_if_not_owner(&identity, row_user.as_deref(), &operator) {
        return resp;
    }
    let removed = match file.delete_comment(&cid) {
        Ok(c) => c,
        Err(e) => return error_to_problem_json(&e),
    };
    if let Err(e) = review::save_atomic(&path, &file, None) {
        return error_to_problem_json(&e);
    }
    // Prune the on-disk stale-anchor sidecar entry for the deleted comment
    // (immediacy for `GET /api/anchors/stale`). Done UNDER the per-kb guard
    // (before drop) so concurrent same-kb comment mutations can't race a
    // load→remove→save on the shared sidecar. (The indexer is a separate,
    // unsynchronized sidecar writer that self-heals the in-memory map on
    // the next reindex — see indexer.rs orphaned-key prune — so a later
    // reindex can't re-persist this key.)
    let review_dir = state.paths.kb_review_dir(&kb_name);
    let sidecar = kb_core::anchors::sidecar_path(&review_dir);
    let mut stale = kb_core::anchors::load(&sidecar);
    if stale.remove(&(id.clone(), cid.clone())).is_some() {
        if let Err(e) = kb_core::anchors::save(&sidecar, &stale) {
            tracing::warn!(kb = %kb_name, error = %e, "failed to prune anchor-stale sidecar on delete");
        }
    }
    // Y4 — GC the deleted comment's (and its replies') attachments: now
    // orphaned in the manifest, reaped under this same guard. Skipped when
    // the comment carried none (the common case), so no manifest I/O.
    if !removed.attachments.is_empty() || removed.replies.iter().any(|r| !r.attachments.is_empty())
    {
        let (_, _, grace_hours) = crate::routes::attachments::attachment_limits(&state).await;
        let manifest =
            kb_core::attachments::load_manifest(&state.paths.kb_attachment_manifest(&kb_name, &id));
        crate::routes::attachments::gc_manifest(
            &state,
            &kb_name,
            &id,
            manifest,
            &file.referenced_attachment_ids(),
            grace_hours,
        );
    }
    drop(guard);

    emit_updated_by(&state, &kb_name, &id, &file, Some(&identity.user));
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "open_count": file.open_count() })),
    )
        .into_response()
}

pub async fn delete_reply(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    Path((kb, id, cid, rid)): Path<(String, String, String, String)>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if let Err(resp) = check_subid(&cid, "comment id") {
        return resp;
    }
    if let Err(resp) = check_subid(&rid, "reply id") {
        return resp;
    }

    let operator = state.operator_user().to_string();
    let path = state.paths.kb_review_file(&kb_name, &id);
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;
    let mut file = match review::load(&path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no comments for {kb_name}/{id}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let row_user = match file
        .comments
        .iter()
        .find(|c| c.id == cid)
        .and_then(|c| c.replies.iter().find(|r| r.id == rid))
    {
        Some(r) => r.user.clone(),
        None => return error_to_problem_json(&kb_core::Error::NotFound(format!("reply {rid}"))),
    };
    if let Some(resp) = forbid_if_not_owner(&identity, row_user.as_deref(), &operator) {
        return resp;
    }
    let removed = match file.delete_reply(&cid, &rid) {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    if let Err(e) = review::save_atomic(&path, &file, None) {
        return error_to_problem_json(&e);
    }
    // Y4 — GC the deleted reply's attachments: now orphaned in the manifest,
    // reaped under this same guard. Skipped when the reply carried none (the
    // common case), so no manifest I/O. (No stale-anchor sidecar prune here, as
    // in delete_comment — anchors are keyed per-comment and the parent comment
    // is untouched by a reply delete.)
    if !removed.attachments.is_empty() {
        let (_, _, grace_hours) = crate::routes::attachments::attachment_limits(&state).await;
        let manifest =
            kb_core::attachments::load_manifest(&state.paths.kb_attachment_manifest(&kb_name, &id));
        crate::routes::attachments::gc_manifest(
            &state,
            &kb_name,
            &id,
            manifest,
            &file.referenced_attachment_ids(),
            grace_hours,
        );
    }
    drop(guard);

    emit_updated_by(&state, &kb_name, &id, &file, Some(&identity.user));
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

// --- verdict (W2.15a) -------------------------------------------------------
//
// Distinct from a single comment's open/resolved status: the review-PASS
// verdict lives at `ReviewFile.verdict` (kb_core::review). `set_verdict`
// mirrors `add_comment`'s create-on-absence branch — a verdict on an
// otherwise commentless artifact still needs a review file to hold it, so
// (unlike every other mutation here) it doesn't 404 on a missing file.
// `clear_verdict` mirrors `delete_comment` instead (404 on a missing file —
// there's nothing to clear if the artifact never had a review file at all).
// Both mirror the `status-approved` / `status-changes-requested` verdict
// onto the artifact's own kb-tags as a DISPLAY SHORTCUT (spec item 5):
// scoped strictly to `kb-tags` (invariant #12), via the same byte-preserving
// `kb_core::meta_edit` / `kb_core::markdown::set_frontmatter_field` machinery
// `PATCH …/artifacts/{id}/meta` uses — best-effort, so a source-write hiccup
// never fails the verdict-of-record write (already committed above).

pub async fn set_verdict(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    Path((kb, id)): Path<(String, String)>,
    Json(payload): Json<SetVerdictBody>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    let Some(ctx) = state.kbs.get(&kb_name) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("kb {kb_name}")));
    };
    // Title for a freshly-created skeleton (mirrors add_comment).
    let title = ctx
        .storage
        .get_by_id(id.clone())
        .await
        .ok()
        .flatten()
        .map(|d| d.title)
        .unwrap_or_default();

    let user = identity.user.clone();
    let path = state.paths.kb_review_file(&kb_name, &id);
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;
    let mut file = match review::load(&path) {
        Ok(Some(f)) => f,
        Ok(None) => ReviewFile::empty_skeleton(&kb_name, &id, &title),
        Err(e) => return error_to_problem_json(&e),
    };
    let changed = file.set_verdict(payload.state, payload.note, Some(user.clone()));
    if !changed {
        // No-op (G8): identical state+note already set — skip the rewrite,
        // the SSE, and the tag-shortcut write.
        drop(guard);
        return (
            StatusCode::OK,
            Json(json!({ "ok": true, "verdict": file.verdict })),
        )
            .into_response();
    }
    if let Err(e) = review::save_atomic(&path, &file, None) {
        return error_to_problem_json(&e);
    }
    drop(guard);
    emit_updated_by(&state, &kb_name, &id, &file, Some(&user));
    apply_status_tag_shortcut(&state, &kb_name, &id, Some(payload.state)).await;
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "verdict": file.verdict })),
    )
        .into_response()
}

pub async fn clear_verdict(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };

    let path = state.paths.kb_review_file(&kb_name, &id);
    let lock = state.review_lock_for(&kb_name);
    let guard = lock.lock().await;
    let mut file = match review::load(&path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no comments for {kb_name}/{id}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let changed = file.clear_verdict();
    if !changed {
        drop(guard);
        return (StatusCode::OK, Json(json!({ "ok": true }))).into_response();
    }
    if let Err(e) = review::save_atomic(&path, &file, None) {
        return error_to_problem_json(&e);
    }
    drop(guard);
    emit_updated(&state, &kb_name, &id, &file);
    apply_status_tag_shortcut(&state, &kb_name, &id, None).await;
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

/// kb-tags slugs the display shortcut writes/removes. Plain slugs (the
/// slugifier — `kb_core::parser::slugify_tag` — turns `:` into `-`, so
/// these ARE the on-disk form every tag goes through; there is no
/// `status:approved` literal anywhere on disk).
const STATUS_TAG_PREFIX: &str = "status-";
const STATUS_TAG_APPROVED: &str = "status-approved";
const STATUS_TAG_CHANGES: &str = "status-changes-requested";

fn status_tag_for(state: kb_core::review::VerdictState) -> Option<&'static str> {
    match state {
        kb_core::review::VerdictState::Approve => Some(STATUS_TAG_APPROVED),
        kb_core::review::VerdictState::RequestChanges => Some(STATUS_TAG_CHANGES),
        // `Comment` is a working note, not a pass/fail signal — no tag.
        kb_core::review::VerdictState::Comment => None,
    }
}

/// W2.15a spec item 5 — mirror a verdict change onto the artifact's own
/// kb-tags: `verdict_state = Some(_)` sets the matching `status-*` tag
/// (replacing any prior one; `Comment` state removes it); `None` (clear)
/// just removes it. Best-effort: `state.kbs`/storage/source misses are
/// logged and swallowed — the verdict of record already landed in the
/// review JSON before this runs, so a tag-write failure must never
/// surface as an error on the verdict request.
async fn apply_status_tag_shortcut(
    state: &KbHandles,
    kb_name: &KbName,
    id: &str,
    verdict_state: Option<kb_core::review::VerdictState>,
) {
    let Some(ctx) = state.kbs.get(kb_name) else {
        return;
    };
    let doc = match ctx.storage.get_by_id(id.to_string()).await {
        Ok(Some(d)) => d,
        _ => return,
    };
    let desired = verdict_state.and_then(status_tag_for);
    let mut tags: Vec<String> = doc
        .tags
        .iter()
        .filter(|t| !t.starts_with(STATUS_TAG_PREFIX))
        .cloned()
        .collect();
    if let Some(t) = desired {
        tags.push(t.to_string());
    }
    if tags == doc.tags {
        return;
    }
    let src = match std::fs::read_to_string(&doc.path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(kb = %kb_name, id, error = %e, "verdict status-tag shortcut: failed to read source");
            return;
        }
    };
    let is_md = ctx.ext_map.is_markdown(std::path::Path::new(&doc.path));
    let new_src = if is_md {
        let joined = tags.join(", ");
        let val = (!tags.is_empty()).then_some(joined.as_str());
        kb_core::markdown::set_frontmatter_field(&src, "kb-tags", val)
    } else {
        kb_core::meta_edit::set_tags(&src, &tags)
    };
    if new_src != src {
        if let Err(e) =
            kb_core::fsx::write_atomic(std::path::Path::new(&doc.path), new_src.as_bytes())
        {
            tracing::warn!(kb = %kb_name, id, error = %e, "verdict status-tag shortcut: failed to write source");
        }
    }
}

// --- GET /reviews — list/query ---------------------------------------------

/// One review file surviving the blocking-walk filters:
/// `(artifact_id, fallback_title, comments-with-staleness)`.
type ReviewGroup = (String, String, Vec<(review::Comment, bool)>);

pub async fn list_reviews(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(q): Query<ReviewsQuery>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    // status filter — default to open (matches the CLI `list` default;
    // `--all` passes status=all).
    let want_status = match q.status.as_deref() {
        None | Some("open") => Some(CommentStatus::Open),
        Some("resolved") => Some(CommentStatus::Resolved),
        Some("all") => None,
        Some(other) => {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "status {other:?} not one of open|resolved|all"
            )))
        }
    };
    let want_author = match q.author.as_deref() {
        None => None,
        Some("you") => Some(Author::You),
        Some("claude") => Some(Author::Claude),
        Some(other) => {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "author {other:?} not one of you|claude"
            )))
        }
    };
    // v0.34 Y1 — optional attribution filter (beside author role).
    let want_user = q
        .user
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(kb_core::identity::normalize_username);
    let want_stale = q.stale.unwrap_or(false);

    let review_dir = state.paths.kb_review_dir(&kb_name);

    // Cost shape (mirrors `inbox::collect_open`): the sidecar load + dir walk
    // + per-file JSON parses are synchronous IO, so they run in ONE
    // `spawn_blocking` off the async worker. The status/author/stale filters
    // apply there too, so a review file whose comments are all filtered out
    // never reaches lance. The surviving artifacts' live title/folder/
    // source-rel then resolve in a SINGLE batched `get_by_ids` — the old
    // one-`get_by_id`-per-review-file loop paid the lance manifest-checkout
    // overhead once per file (60 files on a version-accreted dataset →
    // minutes, not seconds).
    let artifact_filter = q.artifact_id.clone();
    let groups: Vec<ReviewGroup> = tokio::task::spawn_blocking(move || {
        let stale_map = kb_core::anchors::load(&kb_core::anchors::sidecar_path(&review_dir));

        let entries = match std::fs::read_dir(&review_dir) {
            Ok(e) => e,
            // No `.review/` dir yet → no comments.
            Err(_) => return Vec::new(),
        };
        // Deterministic order: collect + sort the artifact ids first.
        let mut artifact_files: Vec<(String, std::path::PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Some(want) = &artifact_filter {
                if stem != want {
                    continue;
                }
            }
            artifact_files.push((stem.to_string(), p));
        }
        artifact_files.sort_by(|a, b| a.0.cmp(&b.0));

        let mut groups: Vec<ReviewGroup> = Vec::new();
        for (artifact_id, path) in artifact_files {
            let file = match review::load(&path) {
                Ok(Some(f)) => f,
                _ => continue,
            };
            let mut kept: Vec<(review::Comment, bool)> = Vec::new();
            for c in file.comments {
                if let Some(s) = want_status {
                    if c.status != s {
                        continue;
                    }
                }
                if let Some(a) = want_author {
                    if c.author != a {
                        continue;
                    }
                }
                if let Some(ref u) = want_user {
                    // Missing user on legacy rows matches operator only when
                    // filtering for that name would require backfill; here we
                    // require an exact stamped user match (None never matches).
                    if c.user.as_deref() != Some(u.as_str()) {
                        continue;
                    }
                }
                let is_stale = stale_map.contains_key(&(artifact_id.clone(), c.id.clone()));
                if want_stale && !is_stale {
                    continue;
                }
                kept.push((c, is_stale));
            }
            if !kept.is_empty() {
                groups.push((artifact_id, file.artifact.title, kept));
            }
        }
        groups
    })
    .await
    .unwrap_or_default();

    // ONE batched lance query joins storage for every surviving artifact's
    // current title + folder + source path. A missing row (the artifact left
    // lance — sidecar/review outlived the file) falls back to the review
    // file's own title with `None` folder/source_relative, as before.
    let ids: Vec<String> = groups.iter().map(|(id, _, _)| id.clone()).collect();
    let resolved: std::collections::HashMap<String, kb_core::storage::lance::DocSummary> =
        match ctx.storage.get_by_ids(ids).await {
            Ok(docs) => docs.into_iter().map(|d| (d.id.clone(), d)).collect(),
            Err(_) => std::collections::HashMap::new(),
        };

    let mut rows: Vec<ReviewRow> = Vec::new();
    for (artifact_id, fallback_title, kept) in groups {
        let (title, folder, source_relative) = match resolved.get(&artifact_id) {
            Some(doc) => (
                doc.title.clone(),
                Some(kb_core::paths::doc_folder(&doc.path, &ctx.source_path)),
                Some(kb_core::paths::doc_rel_path(&doc.path, &ctx.source_path)),
            ),
            None => (fallback_title, None, None),
        };
        if let Some(want_folder) = &q.folder {
            if folder.as_deref() != Some(want_folder.as_str()) {
                continue;
            }
        }
        for (c, is_stale) in kept {
            rows.push(ReviewRow {
                kb: kb_name.as_str().to_string(),
                artifact_id: artifact_id.clone(),
                title: title.clone(),
                comment_id: c.id,
                status: c.status,
                author: c.author,
                anchor: c.anchor,
                file: c.file,
                file_label: c.file_label,
                body: c.body,
                created_at: c.created_at,
                stale: is_stale,
                folder: folder.clone(),
                source_relative: source_relative.clone(),
            });
        }
    }

    Json(ReviewsResponse { comments: rows }).into_response()
}
