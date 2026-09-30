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
//!   POST   …/review/{id}/comments/{cid}/keep      queue a proposal → 201
//!   POST   …/comments/{id}/keep                   keep comment as memory
//!   GET    …/reviews                              list/query comments
//!
//! v0.40 TN1/TN2 adds one route and two request fields:
//!   PATCH  …/review/{id}/comments/{cid}/meta      set tags and/or private
//!
//! `add` accepts `tags` and `private`; `/meta` is the only way to change
//! them afterwards. "Private" means a NOTE: a comment no agent may see,
//! through any surface, ever. The default rule this file implements is
//! one sentence — every read, list and COUNT here shows only
//! `!c.private` rows and takes no opt-in parameter; the only ways a note
//! is ever visible are the `?visibility=all` operator reads in
//! `routes::review` / `list_reviews`, the `GET /api/review-notes` index,
//! and the two lossless transports (`embed_into_html` / `…/import`).
//! `keep` and `keep_memory` refuse a note with 409, because a proposal and
//! a memory artifact are both agent-readable and promoting a note is the
//! worst promotion in the system.
//! The single-comment mutations — `resolve`, `unresolve`, `set_anchor`,
//! `add_reply` — refuse a note with the same 409 `keep` uses. The batch
//! path already skipped notes in `resolve-all`/`unresolve-all`; these are
//! its single-row twins and were the gap, because a note id is NOT secret
//! (`/api/anchors/stale` answers it fleet-wide, and the indexer walks every
//! open comment unfiltered), so "the caller cannot read the note" was never
//! a barrier. The guard lives in `kb_core::review::reject_private_note` so
//! both the HTTP routes and `BatchOp::{Resolve,Unresolve,SetAnchor,
//! AddReply}` get it from one place. `PATCH …/meta` is deliberately NOT
//! guarded: un-privating is how the operator re-opens a note to edit it.
//!
//! `keep` copies one open or resolved comment into the proposal queue
//! (`source: "comment"`). It does not approve, does not ingest a memory,
//! and does not rewrite or delete the comment. `keep_memory` is the other
//! half: one memory in this kb's memory corpus, or the global memory
//! corpus when this kb is not memory-scoped. A second click with the same
//! comment id does not insert another memory.
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
use std::path::{Path as FsPath, PathBuf};
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
    /// v0.40 TN1 — comment-scoped labels. `#[serde(default)]` so an
    /// agent's existing `kb comments add` payload is byte-for-byte
    /// unaffected, and normalised through the ONE normalizer
    /// (`kb_core::review::normalize_comment_tags`) at the write edge — the
    /// batch path (`BatchOp::AddComment`) runs the same function, so the
    /// route and the batch can never diverge.
    #[serde(default)]
    pub tags: Vec<String>,
    /// v0.40 TN2 — create the comment as a PRIVATE NOTE: invisible to every
    /// agent-facing surface, by design. NOT an access control — `kb comments
    /// add --note` sends `private: true` from the CLI, and any caller that
    /// can reach the daemon can POST the same body. The flag is a statement
    /// of the operator's INTENT ("this is for me, not for the agent"), and
    /// the note hides itself from agents rather than being kept from them;
    /// there is nothing to authenticate here, only a rendering rule.
    ///
    /// Absent is indistinguishable from `false` on disk
    /// (`skip_serializing_if = "is_false"`), so a legacy sidecar re-saves
    /// byte-identically and an existing client's payload is unaffected.
    #[serde(default)]
    pub private: bool,
}

/// v0.40 TN1/TN2 — body of `PATCH …/review/{id}/comments/{cid}/meta`.
/// Tri-state on purpose: `None` means "don't touch", which is what lets the
/// handler tell a real change (save + `comments.updated`) from a no-op
/// (neither, per G8). A body setting NEITHER field is a 400, not a silent
/// no-op — a filter the caller believed it applied is a lie.
#[derive(Debug, Deserialize)]
pub struct CommentMetaBody {
    /// Replaces the WHOLE tag set (normalised: slugified, deduped, sorted).
    /// `Some([])` clears the tags; `None` leaves them alone.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// `Some(false)` un-privates the comment, which makes the whole thread
    /// readable by every agent-facing surface again.
    #[serde(default)]
    pub private: Option<bool>,
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
    /// v0.40 TN2 — `public` (DEFAULT) | `all`. Absent means public, never
    /// "everything": a note the operator hid must not surface because the
    /// reader forgot the param. `all` is the operator opt-in the SPA sends;
    /// the CLI never does, so every CLI read inherits the public default
    /// (the CLI is operator-run but agent-reachable). An unrecognised value
    /// is a 400 — `Visibility::from_query` returns `None` and we refuse
    /// rather than guess, mirroring `ExportFormat::from_query`.
    pub visibility: Option<String>,
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
    /// v0.40 TN1 — comment-scoped labels (slug-normalised + sorted on
    /// disk). Always emitted as an array, never omitted, so a `| jq`
    /// consumer can read `.tags` without a null check. NOT the artifact's
    /// `kb-tags`: a different namespace that is deliberately never mirrored.
    tags: Vec<String>,
    /// v0.40 TN2 — this row is a private note. Only ever `true` on a read
    /// that passed `?visibility=all`; the default public read cannot
    /// produce one.
    private: bool,
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
            "total_count": public_count(file),
            "verdict": file.verdict,
        });
        if let Some(u) = user {
            payload["user"] = json!(u);
        }
        ctx.bus.emit("comments.updated", payload);
    }
}

/// v0.40 TN2 — the number of comments in `file` an AGENT may see.
///
/// Counts lie in one direction only: every count any agent can reach —
/// this SSE payload included, the batch/delete/import response bodies — is
/// public-only. Undercounting is a cosmetic lie; overcounting would PROVE a
/// hidden note exists, which is exactly the leak the flag prevents. The
/// operator's exact count comes from the review file the SPA refetches on
/// this same event, never from here.
///
/// Reads through `Comment::is_private`, the ONE visibility predicate, so no
/// route hand-rolls a second spelling that can disagree with it.
fn public_count(file: &ReviewFile) -> usize {
    file.comments.iter().filter(|c| !c.is_private()).count()
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
    // v0.40 TN1 — normalise BEFORE the lock, through the same function
    // `BatchOp::AddComment::apply_to` runs, so the HTTP route and the batch
    // path cannot store two different tag sets for the same input. An
    // over-limit / over-long request is a 400 here, never a silent
    // truncation.
    let tags = match review::normalize_comment_tags(&payload.tags) {
        Ok(t) => t,
        Err(e) => return error_to_problem_json(&e),
    };
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
        // v0.40 TN1/TN2.
        tags,
        private: payload.private,
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
    //
    // v0.40 TN2 — a PRIVATE note gets NO ledger row, deliberately. The
    // ledger is read by four agent-facing surfaces (`daycard`'s comments
    // lane, `timeline`'s id list, and the `kind='comment'` counts in
    // `timeline` + `history`), and it carries no visibility bit — so a row
    // written here would render a note's existence (and later its title)
    // to every agent. Skipping the write is the single decision that keeps
    // those four surfaces consistent for free; post-filtering them would
    // need a second `.review/` walk per surface AND would leave counts
    // that disagree with the id list. Stated consequence: the ledger is an
    // event log, not a derived index, so un-privating a note does not
    // backfill its history row.
    if !created.is_private() {
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
    // Unresolve (and any future non-resolve status) must not touch the
    // stale-anchor sidecar. The shared path already holds the review lock
    // for the file rewrite; there is no sidecar write to sequence with it.
    if status != CommentStatus::Resolved {
        return with_review_mut(&state, &kb_name, &id, StatusCode::OK, |file| {
            let changed = file.set_comment_status(&cid, status)?;
            Ok((
                json!({ "ok": true, "open_count": file.open_count() }),
                changed,
            ))
        })
        .await;
    }

    // Resolve — same lock scope as `set_anchor` / `delete_comment`: the
    // review-file rewrite and the stale-anchor sidecar prune share the
    // per-kb guard so two same-kb mutations can't race a load→remove→save
    // on the sidecar. A resolved comment's stale flag is meaningless
    // (ux-13 — the queue was mostly already-resolved rows).
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
    let changed = match file.set_comment_status(&cid, status) {
        Ok(c) => c,
        Err(e) => return error_to_problem_json(&e),
    };
    if changed {
        if let Err(e) = review::save_atomic(&path, &file, None) {
            return error_to_problem_json(&e);
        }
    }
    // Prune even on a no-op re-resolve: the comment may already be
    // Resolved and still own a sidecar row. Non-resolve never reaches here.
    let review_dir = state.paths.kb_review_dir(&kb_name);
    let sidecar = kb_core::anchors::sidecar_path(&review_dir);
    if let Err(e) = kb_core::anchors::prune_if_resolved(&sidecar, &id, &cid, true) {
        tracing::warn!(kb = %kb_name, error = %e, "failed to prune anchor-stale sidecar on resolve");
    }
    drop(guard);
    if changed {
        emit_updated(&state, &kb_name, &id, &file);
    }
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "open_count": file.open_count() })),
    )
        .into_response()
}

// --- keep (queue a proposal; do not approve or delete) ---------------------

/// `POST …/review/{id}/comments/{cid}/keep` — copy one comment into the
/// proposal queue as `kb-proposal/1` with `source: "comment"`. Open and
/// resolved comments are both eligible. Does not approve, does not ingest
/// a memory, and does not delete or rewrite the comment. v0.40 TN2: a
/// PRIVATE note is refused with 409 (see `keep_comment_as_proposal`).
///
/// Wire in `router.rs` (not edited here), beside `resolve`:
/// `.route("/kb/{kb}/review/{id}/comments/{cid}/keep", post(routes::comments::keep))`.
pub async fn keep(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id, cid)): Path<(String, String, String)>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if let Err(resp) = check_subid(&cid, "comment id") {
        return resp;
    }
    let review_path = state.paths.kb_review_file(&kb_name, &id);
    let proposals_dir = state.paths.kb_proposals_dir(&kb_name);

    // Review lock, then the proposal lock. Keep reads the comment and
    // writes only the queue — never the reverse order, and never the
    // review file. `set_status`'s stale-anchor prune is not this path.
    let review_lock = state.review_lock_for(&kb_name);
    let proposal_lock = state.proposal_lock_for(&kb_name);
    let review_guard = review_lock.lock().await;
    let proposal_guard = proposal_lock.lock().await;
    let proposal = match keep_comment_as_proposal(&review_path, &proposals_dir, &id, &cid) {
        Ok(p) => p,
        Err(e) => return error_to_problem_json(&e),
    };
    drop(proposal_guard);
    drop(review_guard);

    if let Some(ctx) = state.kbs.get(&kb_name) {
        ctx.bus.emit(
            "proposal.created",
            json!({
                "kb": kb_name.as_str(),
                "id": proposal.id.clone(),
                "title": proposal.title.clone(),
            }),
        );
    }
    (StatusCode::CREATED, Json(proposal)).into_response()
}

/// Read one comment and enqueue a proposal through
/// [`super::proposals::enqueue_proposal`]. Does not approve, does not
/// write a memory artifact, and does not mutate the review file.
///
/// v0.40 TN2 — refuses a PRIVATE note with `Error::Conflict` (409). A
/// proposal is a work item the agent is handed on the next session, so
/// `keep` on a note is the single worst promotion in the system: it would
/// turn "the agent must never see this" into "the agent's next task".
/// The guard sits between the read and `enqueue_proposal`, so the only
/// thing that happened before the 409 is a `review::load` — no
/// `.proposals/<id>.json` is written and no proposal row is emitted.
fn keep_comment_as_proposal(
    review_path: &std::path::Path,
    proposals_dir: &std::path::Path,
    artifact_id: &str,
    comment_id: &str,
) -> kb_core::Result<super::proposals::Proposal> {
    let file = match review::load(review_path)? {
        Some(f) => f,
        None => {
            return Err(kb_core::Error::NotFound(format!(
                "no comments for artifact {artifact_id}"
            )))
        }
    };
    let comment = file
        .comments
        .iter()
        .find(|c| c.id == comment_id)
        .ok_or_else(|| {
            kb_core::Error::NotFound(format!("no comment {comment_id} on artifact {artifact_id}"))
        })?;
    if comment.is_private() {
        return Err(kb_core::Error::Conflict(format!(
            "comment {comment_id} is a private note; un-private it before keeping it as a proposal"
        )));
    }
    let title = super::proposals::truncate_proposal_title(&comment.body);
    if title.is_empty() {
        return Err(kb_core::Error::BadRequest(
            "proposal title must not be empty".into(),
        ));
    }
    let body = format!(
        "{}\n\nProvenance: artifact {artifact_id}, comment {comment_id}",
        comment.body
    );
    super::proposals::enqueue_proposal(
        proposals_dir,
        super::proposals::EnqueueProposal {
            title,
            body,
            category: super::proposals::default_category(),
            tags: Vec::new(),
            global: true,
            linked_kbs: Vec::new(),
            salience: None,
            supersedes: None,
            session_id: None,
            source: super::proposals::ProposalSource::Comment,
            note: None,
        },
    )
}

// --- keep as memory (idempotent on comment id) -----------------------------

/// One comment lifted into a memory corpus. `already` is true when a prior
/// click with this comment id already inserted the memory.
#[derive(Debug, Serialize)]
struct KeepMemoryResponse {
    id: String,
    path: String,
    already: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct KeepRecord {
    id: String,
    path: String,
    comment_id: String,
}

#[derive(Debug)]
struct FoundComment {
    comment_id: String,
    artifact_id: String,
    title: String,
    body: String,
    anchor: Anchor,
    author: Author,
}

struct KeepInsert {
    title: String,
    text: String,
    summary: String,
    artifact_id: String,
    anchor: Anchor,
    author: Author,
}

#[derive(Debug)]
enum KeepError {
    Core(kb_core::Error),
    Http(Box<Response<Body>>),
}

/// `POST /api/kb/{kb}/comments/{id}/keep` — write one memory for this
/// comment. `{id}` is the comment id. The memory text is the comment body,
/// the title is `Keep: <artifact title or source path>`, and the citation
/// includes the comment id. A second click with the same comment id does
/// not insert another memory. 404 when the comment is missing.
///
/// The write is [`crate::routes::artifacts::ingest`] — the same memory
/// insert `kb remember` and proposal approval use. This kb is the corpus
/// when it is memory-scoped; otherwise the memory lands in the global
/// memory corpus (the one named `memory` when several global corpora
/// exist). Does not rewrite or delete the comment, and does not queue a
/// proposal (`keep` owns that).
///
/// Mounted in `router.rs` as
/// `.route("/kb/{kb}/comments/{id}/keep", post(routes::comments::keep_memory))`.
pub async fn keep_memory(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let kb_name = match KbName::new(&kb) {
        Ok(k) => k,
        Err(e) => return error_to_problem_json(&e),
    };
    if !state.kbs.contains_key(&kb_name) {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("kb {kb_name}")));
    }
    if let Err(resp) = check_subid(&id, "comment id") {
        return resp;
    }

    let source_is_memory = state
        .kbs
        .get(&kb_name)
        .is_some_and(|c| c.memory_scope.is_some());
    let target_name = if source_is_memory {
        kb_name.clone()
    } else {
        match global_memory_name(&state) {
            Ok(n) => n,
            Err(e) => return error_to_problem_json(&e),
        }
    };
    let Some(memory_root) = state.kbs.get(&target_name).map(|c| c.source_path.clone()) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!(
            "memory corpus {target_name}"
        )));
    };

    let review_dir = state.paths.kb_review_dir(&kb_name);
    let source_root = state
        .kbs
        .get(&kb_name)
        .map(|c| c.source_path.clone())
        .unwrap_or_default();
    let source_storage = state.kbs.get(&kb_name).map(|c| c.storage.clone());

    let lock = state.review_lock_for(&kb_name);
    let _guard = lock.lock().await;

    let found = match find_comment(&review_dir, &id) {
        Ok(f) => f,
        Err(e) => return error_to_problem_json(&e),
    };
    let source_rel = if found.title.trim().is_empty() {
        match source_storage {
            Some(storage) => storage
                .get_by_id(found.artifact_id.clone())
                .await
                .ok()
                .flatten()
                .map(|d| kb_core::paths::doc_rel_path(&d.path, &source_root))
                .unwrap_or_default(),
            None => String::new(),
        }
    } else {
        String::new()
    };

    let state_for_insert = Arc::clone(&state);
    let target_for_insert = target_name.clone();
    let source_kb = kb_name.as_str().to_string();
    let outcome = insert_keep_once(&review_dir, &memory_root, &found, &source_rel, |spec| {
        let Some(target_ctx) = state_for_insert.kbs.get(&target_for_insert) else {
            return Err(KeepError::Core(kb_core::Error::NotFound(format!(
                "memory corpus {target_for_insert}"
            ))));
        };
        match crate::routes::artifacts::ingest(
            &state_for_insert,
            &target_for_insert,
            target_ctx,
            keep_ingest_body(&spec, &source_kb),
        ) {
            Ok(written) => Ok((written.id, written.path)),
            Err(resp) => Err(KeepError::Http(Box::new(resp))),
        }
    });

    match outcome {
        Ok(body) => {
            let status = if body.already {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            (status, Json(body)).into_response()
        }
        Err(KeepError::Core(e)) => error_to_problem_json(&e),
        Err(KeepError::Http(resp)) => *resp,
    }
}

/// Global curated-memory write target. Prefers the corpus named `memory`
/// so a `sessions` corpus (also `memory_scope = "global"`) is not the
/// landing place for a kept comment. Mirrors the CLI write-target rule.
fn global_memory_name(state: &KbHandles) -> kb_core::Result<KbName> {
    let mut named_memory: Option<KbName> = None;
    let mut curated: Vec<KbName> = Vec::new();
    for (name, ctx) in &state.kbs {
        if ctx.memory_scope.as_deref() != Some("global") {
            continue;
        }
        if name.as_str() == "memory" {
            named_memory = Some(name.clone());
        }
        if name.as_str() != "sessions" {
            curated.push(name.clone());
        }
    }
    if let Some(name) = named_memory {
        return Ok(name);
    }
    match curated.len() {
        1 => Ok(curated.remove(0)),
        0 => Err(kb_core::Error::NotFound("no global memory corpus".into())),
        _ => Err(kb_core::Error::BadRequest(
            "multiple global memory corpora; cannot choose a keep target".into(),
        )),
    }
}

fn find_comment(review_dir: &FsPath, comment_id: &str) -> kb_core::Result<FoundComment> {
    let entries = match std::fs::read_dir(review_dir) {
        Ok(entries) => entries,
        Err(_) => return Err(kb_core::Error::NotFound(format!("no comment {comment_id}"))),
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        files.push(path);
    }
    files.sort();
    for path in files {
        let Some(file) = review::load(&path).ok().flatten() else {
            continue;
        };
        if let Some(comment) = file.comments.iter().find(|c| c.id == comment_id) {
            // v0.40 TN2 — a PRIVATE note is refused here, the single read
            // both keep-memory paths funnel through. A memory artifact is
            // agent-readable FOREVER (`kb recall` surfaces it on every
            // future prompt), so promoting a note into one is irreversible
            // in the worst way. Because this returns before
            // `insert_keep_once` runs, no keep record is written and the
            // ingest closure is never called.
            if comment.is_private() {
                return Err(kb_core::Error::Conflict(format!(
                    "comment {comment_id} is a private note; un-private it before keeping it as a memory"
                )));
            }
            return Ok(FoundComment {
                comment_id: comment.id.clone(),
                artifact_id: file.artifact.id.clone(),
                title: file.artifact.title.clone(),
                body: comment.body.clone(),
                anchor: comment.anchor.clone(),
                author: comment.author,
            });
        }
    }
    Err(kb_core::Error::NotFound(format!("no comment {comment_id}")))
}

/// Artifact title when it has one, otherwise the source path. Never empty
/// — ingest rejects an empty title.
fn keep_label(artifact_title: &str, source_path: &str) -> String {
    let title = artifact_title.trim();
    if !title.is_empty() {
        return title.lines().next().unwrap_or(title).trim().to_string();
    }
    let path = source_path.trim();
    if !path.is_empty() {
        return path.to_string();
    }
    "artifact".to_string()
}

fn keep_text(body: &str, comment_id: &str) -> String {
    let citation = format!("Citation: comment {comment_id}");
    let body = body.trim_end();
    if body.is_empty() {
        citation
    } else {
        format!("{body}\n\n{citation}")
    }
}

fn keep_record_path(review_dir: &FsPath, comment_id: &str) -> PathBuf {
    review_dir.join("keeps").join(format!("{comment_id}.json"))
}

fn read_keep_record(path: &FsPath) -> Option<KeepRecord> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_keep_record(path: &FsPath, rec: &KeepRecord) -> kb_core::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| kb_core::Error::Storage(format!("create keep record dir: {e}")))?;
    }
    let bytes = serde_json::to_vec(rec).map_err(|e| kb_core::Error::Serde(e.to_string()))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)
        .map_err(|e| kb_core::Error::Storage(format!("write keep record: {e}")))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| kb_core::Error::Storage(format!("rename keep record: {e}")))?;
    Ok(())
}

fn keep_ingest_body(spec: &KeepInsert, source_kb: &str) -> crate::routes::artifacts::IngestBody {
    crate::routes::artifacts::IngestBody {
        title: spec.title.clone(),
        body: Some(spec.text.clone()),
        body_html: None,
        category: "memory-user".to_string(),
        tags: Vec::new(),
        salience: None,
        decay: None,
        supersedes: None,
        summary: Some(spec.summary.clone()),
        session_id: None,
        global: None,
        linked_kbs: Vec::new(),
        author: Some(spec.author),
        source_kb: Some(source_kb.to_string()),
        source_artifact: Some(spec.artifact_id.clone()),
        source_anchor: Some(spec.anchor.clone()),
        source: None,
        memory_type: None,
        outcome: None,
    }
}

/// Insert at most one memory for `found.comment_id`. The closure is the
/// existing ingest path; it is not called when a keep record for this
/// comment id already exists.
#[allow(clippy::result_large_err)]
fn insert_keep_once<F>(
    review_dir: &FsPath,
    memory_root: &FsPath,
    found: &FoundComment,
    source_path: &str,
    insert: F,
) -> Result<KeepMemoryResponse, KeepError>
where
    F: FnOnce(KeepInsert) -> Result<(String, String), KeepError>,
{
    let record_path = keep_record_path(review_dir, &found.comment_id);
    if let Some(existing) = read_keep_record(&record_path) {
        return Ok(KeepMemoryResponse {
            id: existing.id,
            path: existing.path,
            already: true,
        });
    }
    let spec = KeepInsert {
        title: format!("Keep: {}", keep_label(&found.title, source_path)),
        text: keep_text(&found.body, &found.comment_id),
        summary: format!("Citation: comment {}", found.comment_id),
        artifact_id: found.artifact_id.clone(),
        anchor: found.anchor.clone(),
        author: found.author,
    };
    let (id, path) = insert(spec)?;
    if let Err(e) = write_keep_record(
        &record_path,
        &KeepRecord {
            id: id.clone(),
            path: path.clone(),
            comment_id: found.comment_id.clone(),
        },
    ) {
        let _ = std::fs::remove_file(memory_root.join(&path));
        return Err(KeepError::Core(e));
    }
    Ok(KeepMemoryResponse {
        id,
        path,
        already: false,
    })
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
            | review::BatchOp::DeleteComment { comment_id }
            // `SetMeta` mutates the comment's OWN row, exactly like
            // EditComment, and carries the same disclosure: `private: false`
            // hands the whole thread back to every agent-facing surface —
            // which is why `set_comment_meta` (the PATCH twin) is gated by
            // `forbid_if_not_owner` for it. Without this arm `SetMeta` fell
            // through `_ => None`, `row_user` was None, the owner check never
            // ran, and a non-owner could un-private someone else's note via
            // POST …/apply while the byte-identical PATCH returned 403.
            | review::BatchOp::SetMeta { comment_id, .. } => file
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
    // v0.40 TN2 — a note gets NO history-ledger row, so a `set_meta` that
    // turns an ALREADY-PUBLIC comment into a note has to un-write the row
    // that comment wrote at creation. `apply_ops` reports ids, not
    // transitions, so the visibility bit is snapshotted off the file BEFORE
    // the ops run (the predicate is a real transition, not "is it private
    // now") and compared against the post-save file below — the same
    // before/after read the `PATCH …/meta` route does.
    let was_public: std::collections::HashSet<String> = payload
        .ops
        .iter()
        .filter_map(|op| match op {
            review::BatchOp::SetMeta { comment_id, .. } => Some(comment_id),
            _ => None,
        })
        .filter(|cid| {
            file.comments
                .iter()
                .any(|c| &c.id == *cid && !c.is_private())
        })
        .cloned()
        .collect();
    let report = match file.apply_ops(&payload.ops, &id) {
        Ok(r) => r,
        // Atomic: apply_ops mutated only its working clone, so nothing is
        // persisted and the on-disk file is untouched.
        Err(e) => return error_to_problem_json(&e),
    };
    // v0.40 TN2 — the counts an agent sees are public-only, on the same rule
    // as the `comments.updated` payload.
    let summary = json!({
        "applied": report.applied,
        "created_comment_ids": report.created_comment_ids,
        "created_reply_ids": report.created_reply_ids,
        "open_count": file.open_count(),
        "total_count": public_count(&file),
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
    // v0.40 TN2 — a PRIVATE note created by this batch gets NO ledger row,
    // exactly as in `add_comment`. `apply_ops` reports ids, not comments, so
    // the visibility bit is read back off the (already-mutated, already-
    // saved) `file` — a missing id is left to record, since every created
    // id is present in the file it was created in.
    let now_unix = Utc::now().timestamp();
    let hist_user = identity.user.clone();
    for cid in &report.created_comment_ids {
        if file.comments.iter().any(|c| &c.id == cid && c.is_private()) {
            continue;
        }
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

    // The mirror of the delete in `set_comment_meta`: the comment is now a
    // note, so the row its public self wrote at creation must go, or the four
    // unfiltered ledger readers (daycard activity lane, calendar per-day
    // count, `kind='comment'` list, timeline) keep proving it exists. Ids the
    // batch CREATED are excluded: `report.created_comment_ids` is where those
    // are enumerated, and a comment born in this batch was never public, so it
    // wrote no row to remove.
    for cid in &was_public {
        if report.created_comment_ids.contains(cid) {
            continue;
        }
        if !file.comments.iter().any(|c| &c.id == cid && c.is_private()) {
            continue;
        }
        // Ordering + failure semantics copied from the PATCH route: the
        // delete runs after the review lock is dropped, and a failure does NOT
        // fail the request. The sidecar file is the source of truth and now
        // says `private: true`, so a 500 would report a mutation as failed
        // when it landed, and the client's retry would find no transition left
        // — turning a transient storage error into a permanent, silent leak. A
        // surviving row over-counts, the leaking direction, so it is logged at
        // `warn` with the artifact + comment id rather than swallowed.
        if let Err(e) = ctx
            .storage
            .history_forget_comment(id.clone(), cid.clone())
            .await
        {
            tracing::warn!(
                kb = kb_name.as_str(),
                artifact = id.as_str(),
                comment = cid.as_str(),
                error = %e,
                "comment is now a private note but its history row could not be deleted"
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
        // v0.40 TN2 (KB-TN-LEAK-002) — the refusal keys off the PUBLIC count,
        // not `!comments.is_empty()`. Both halves had to move together: with
        // the private-inclusive guard, a sidecar holding ONLY notes still
        // answers 400 where an empty one answers 200, and the status code
        // alone is a per-artifact existence oracle — worse than the count
        // alone, since the message cannot even be read without arithmetic.
        // A notes-only sidecar must NOT refuse: the refusal is itself proof
        // that a hidden comment exists, and an operator restoring a backup
        // over their own notes is a legitimate case this now allows.
        //
        // The WRITE stays lossless either way — private notes in the payload
        // are saved verbatim (see the success body below). Only the decision
        // and the number are public-only.
        Ok(Some(existing)) if !q.force => {
            let n = public_count(&existing);
            if n > 0 {
                return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                    "{kb_name}/{id} already has {n} comment(s); pass force=true to overwrite"
                )));
            }
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
            // v0.40 TN2 — public-only, on the same rule as every other
            // agent-visible count. The IMPORT ITSELF is lossless: private
            // notes in the payload are written verbatim, because this route
            // is a restore/move transport and silently dropping operator
            // data is a worse failure than the leak it would prevent.
            "imported": public_count(&incoming),
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

// --- comment meta: tags + private flag (v0.40 TN1/TN2) ----------------------

/// `PATCH …/review/{id}/comments/{cid}/meta` — set a comment's labels and/or
/// its private-note flag. Modelled on `with_review_mut` (NOT `set_anchor`)
/// because there is no sidecar to sequence: the mutation is the whole
/// contract, so the shared load → mutate → save path handles the 404s and,
/// crucially, the G8 no-op branch.
///
/// G8 is load-bearing here, and is why this is not just another PATCH: the
/// SPA's tag chips and the 🔒 toggle are both idempotent controls, and a
/// handler that rewrote the file + re-emitted `comments.updated` on every
/// click would churn the mtime (and therefore the ETag) and re-notify every
/// open tab for nothing. `set_comment_meta` returns `Ok(false)` when neither
/// field actually moved, and `with_review_mut` then skips BOTH the
/// `save_atomic` and the SSE.
///
/// The response carries the EFFECTIVE post-normalisation values, copying
/// `patch_meta`'s "Effective values" shape in `routes::artifacts.rs`. This
/// is not a convenience: `["A","a"]` is stored as `["a"]`, and a client
/// that re-derived the normalisation itself would drift from the server the
/// moment the rules changed. The server's answer is the only one.
///
/// Ownership: gated by the same `forbid_if_not_owner` policy as body EDIT and
/// DELETE, because `private: false` is a disclosure — it hands the whole
/// thread back to every agent-facing surface. It is expressed as
/// `Error::Forbidden` (still a 403 problem+json) because the check runs
/// inside the `with_review_mut` closure, where the shared helper's
/// pre-rendered `Response` cannot be threaded out through
/// `kb_core::Result<…>`.
pub async fn set_comment_meta(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    Path((kb, id, cid)): Path<(String, String, String)>,
    Json(payload): Json<CommentMetaBody>,
) -> Response<Body> {
    let kb_name = match validate(&state, &kb, &id) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    if let Err(resp) = check_subid(&cid, "comment id") {
        return resp;
    }
    if payload.tags.is_none() && payload.private.is_none() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "comment meta patch must set at least one of: tags, private".to_string(),
        ));
    }

    let operator = state.operator_user().to_string();
    let req_tags = payload.tags;
    let req_private = payload.private;
    // v0.40 TN2 — set when this PATCH turns an ALREADY-PUBLIC comment into a
    // note, which is the one transition that has to un-write history. Lifted
    // out of the mutation closure (which is sync) so the ledger delete can
    // happen below, against the actor. `Arc<AtomicBool>` rather than a
    // `Cell`: the flag is read after an `.await`, and a `Cell` borrowed into
    // the closure would make this handler's future `!Send`, which axum
    // rejects at the router.
    let became_note = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let became_note_flag = Arc::clone(&became_note);
    // `cid` moves into the mutation closure below; the ledger delete runs
    // after it, so it gets its own copy.
    let cid_row = cid.clone();
    let resp = with_review_mut(&state, &kb_name, &id, StatusCode::OK, move |file| {
        // Same ownership rule as `forbid_if_not_owner`: a row with no
        // stamped `user` belongs to the operator. A MISSING comment falls
        // through to `set_comment_meta`, which raises the canonical 404 —
        // this handler never invents a second spelling of "no such comment".
        let row_user = file
            .comments
            .iter()
            .find(|c| c.id == cid_row)
            .map(|c| c.user.clone());
        if let Some(row_user) = row_user {
            let owner = row_user.as_deref().unwrap_or(operator.as_str());
            if identity.user != owner {
                return Err(kb_core::Error::Forbidden(format!(
                    "only the owner ({owner}) may retag or re-privatise this comment; identity is {}",
                    identity.user
                )));
            }
        }
        // Read the visibility bit off the loaded file BEFORE the mutation, so
        // the predicate is a real transition and not "is it private now".
        let was_public = file
            .comments
            .iter()
            .any(|c| c.id == cid_row && !c.is_private());
        let changed = file.set_comment_meta(&cid_row, req_tags.as_deref(), req_private)?;
        if was_public
            && file
                .comments
                .iter()
                .any(|c| c.id == cid_row && c.is_private())
        {
            became_note_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // Effective values — what is on disk NOW, after normalisation.
        // Read back off the file rather than recomputed, so the response
        // cannot disagree with the bytes that were just written.
        let (tags, private) = match file.comments.iter().find(|c| c.id == cid_row) {
            Some(c) => (c.tags.clone(), c.private),
            None => (Vec::new(), false),
        };
        Ok((
            json!({ "ok": true, "changed": changed, "tags": tags, "private": private }),
            changed,
        ))
    })
    .await;

    // v0.40 TN2 — a note gets NO history-ledger row (the same rule
    // `add_comment` applies to a note born private, and for the same
    // reason: `history` carries no visibility bit, so the four surfaces that
    // read it unfiltered — daycard's `pivot_activity` comments lane, the
    // history calendar's per-day count, the `kind='comment'` list and the
    // timeline — would keep proving the note exists, and one overcounted
    // agent-facing number is the whole leak). So the row a public comment
    // wrote at creation time is deleted HERE, on the flip.
    //
    // Deliberately NOT mirrored in the other direction: un-privating does
    // NOT backfill a row. The ledger is an event log ("a comment was
    // authored at T"), not a derived index, and the operator's own note
    // list is built from the sidecar's `created_at` — so a backfill would
    // invent an event that never happened rather than restore one.
    //
    // Ordering: the delete runs AFTER `with_review_mut` has saved and
    // released the review lock, so the review lock is never held across an
    // actor round-trip. A failed delete does NOT fail the PATCH: the sidecar
    // file is the source of truth and it now says `private: true`, so a 500
    // would report a mutation as failed when it in fact landed, and the
    // client's retry would find no transition left to act on — turning a
    // transient storage error into a permanent, silent leak. A surviving row
    // over-counts, which is the leaking direction, so the error is logged at
    // `warn` (loudly, with the artifact + comment id) rather than swallowed.
    //
    // v0.40 TN2 — deliberately NO `gc_manifest` here, unlike
    // `delete_comment`. The flip does not orphan anything: the note still
    // references its own attachments, so `gc_plan` keeps every one of them
    // and a GC call would be a no-op. What the flip DOES have to stop is the
    // BLOB being reachable, and that is enforced where reachability is
    // decided — `attachments::serve` refuses a blob whose owning comment is
    // private, so the URL published in the comment body, in `?cm=on`, in
    // `kb comments export` and in any share bundle stops resolving. Making
    // the blob unreferenced instead would mean deleting the operator's own
    // screenshot on a REVERSIBLE toggle (un-privating would not bring it
    // back, and the note body would keep an `attachment:<aid>` ref to a
    // file that is gone) — data loss traded for a gate that now exists.
    if became_note.load(std::sync::atomic::Ordering::Relaxed) {
        if let Some(ctx) = state.kbs.get(&kb_name) {
            if let Err(e) = ctx
                .storage
                .history_forget_comment(id.clone(), cid.clone())
                .await
            {
                tracing::warn!(
                    kb = kb_name.as_str(),
                    artifact = id.as_str(),
                    comment = cid.as_str(),
                    error = %e,
                    "comment is now a private note but its history row could not be deleted"
                );
            }
        }
    }
    resp
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
    // v0.40 TN2 — visibility. An ABSENT `?visibility=` means `Public`, never
    // "everything": this route is the CLI's `kb comments list` backend and
    // the CLI never passes the param, so the default is what every
    // agent-reachable read inherits. `Visibility::from_query` returning
    // `None` is a 400 rather than a guess — same rule as
    // `ExportFormat::from_query` on the export route.
    let visibility = match q.visibility.as_deref() {
        None => review::Visibility::Public,
        Some(raw) => match review::Visibility::from_query(raw) {
            Some(v) => v,
            None => {
                return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                    "visibility {raw:?} not one of public|all"
                )))
            }
        },
    };

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
                // v0.40 TN2 — the visibility gate. `Visibility::includes`
                // IS the predicate, so this row can never disagree with
                // `Comment::is_private`; `Public` (the default) drops every
                // private note here, which is why the CLI and every agent
                // read of this list cannot see one.
                if !visibility.includes(&c) {
                    continue;
                }
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
                // v0.40 TN1/TN2 — additive, per the row's stated contract
                // ("a strict superset of the pre-R6 `kb comments list
                // --json` keys"). `private` can only be `true` when the
                // request passed `?visibility=all`.
                tags: c.tags,
                private: c.private,
            });
        }
    }

    Json(ReviewsResponse { comments: rows }).into_response()
}

#[cfg(test)]
mod tests {
    use super::{
        find_comment, insert_keep_once, keep_comment_as_proposal, keep_label, keep_text, KeepError,
    };
    use kb_core::paths::KbPaths;
    use kb_core::review::{self, Anchor, Author, CommentStatus, NewComment};
    use kb_core::types::KbName;

    fn files_under(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn keep_writes_a_proposal_file_and_not_a_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let kb = KbName::new("smoke").unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "keep-test");
        let artifact_id = "abc123def456";
        let review_path = paths.kb_review_file(&kb, artifact_id);
        let proposals_dir = paths.kb_proposals_dir(&kb);
        std::fs::create_dir_all(review_path.parent().unwrap()).unwrap();

        let mut file = review::ReviewFile::empty_skeleton(&kb, artifact_id, "Artifact");
        let comment_text = "k".repeat(crate::routes::proposals::TITLE_LIMIT + 40);
        file.add_comment(NewComment {
            file: artifact_id.to_string(),
            file_label: "Artifact".to_string(),
            anchor: Anchor::File,
            author: Author::You,
            body: comment_text.clone(),
            choices: Vec::new(),
            attachments: Vec::new(),
            user: None,
            tags: Vec::new(),
            private: false,
        });
        let cid = file.comments[0].id.clone();
        file.set_comment_status(&cid, CommentStatus::Resolved)
            .unwrap();
        review::save_atomic(&review_path, &file, None).unwrap();
        let review_before = std::fs::read(&review_path).unwrap();
        let files_before = files_under(tmp.path());

        let proposal =
            keep_comment_as_proposal(&review_path, &proposals_dir, artifact_id, &cid).unwrap();

        assert_eq!(proposal.schema, crate::routes::proposals::SCHEMA);
        assert_eq!(
            proposal.source,
            crate::routes::proposals::ProposalSource::Comment
        );
        assert_eq!(
            proposal.title,
            "k".repeat(crate::routes::proposals::TITLE_LIMIT)
        );
        assert!(proposal.body.starts_with(&comment_text));
        assert!(proposal.body.contains(artifact_id));
        assert!(proposal.body.contains(&cid));

        let on_disk = paths.kb_proposal_file(&kb, &proposal.id);
        assert!(on_disk.is_file(), "keep must write a proposal file");
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&on_disk).unwrap()).unwrap();
        assert_eq!(raw["schema"], "kb-proposal/1");
        assert_eq!(raw["source"], "comment");
        assert_eq!(raw["id"], proposal.id);

        // Comment remains. Keep must not rewrite or delete the review file.
        assert_eq!(std::fs::read(&review_path).unwrap(), review_before);
        let reloaded = review::load(&review_path).unwrap().unwrap();
        assert_eq!(reloaded.comments.len(), 1);
        assert_eq!(reloaded.comments[0].id, cid);
        assert_eq!(reloaded.comments[0].body, comment_text);
        assert_eq!(reloaded.comments[0].status, CommentStatus::Resolved);

        // The only new file is the queued proposal. Approve would have
        // deleted it and written an HTML memory; neither happens here.
        let files_after = files_under(tmp.path());
        let new_files: Vec<_> = files_after
            .iter()
            .filter(|p| !files_before.iter().any(|b| b == *p))
            .cloned()
            .collect();
        assert_eq!(
            new_files.len(),
            1,
            "keep wrote unexpected files: {new_files:?}"
        );
        assert_eq!(new_files[0], on_disk);
        assert!(
            files_after
                .iter()
                .all(|p| p.extension().and_then(|s| s.to_str()) != Some("html")),
            "keep must not write a memory artifact"
        );
    }

    fn review_with_comment(root: &std::path::Path, title: &str, body: &str) -> (KbPaths, String) {
        let kb = KbName::new("smoke").unwrap();
        let paths = KbPaths::rooted_at(root, "keep-memory");
        let artifact_id = "abc123def456";
        let review_path = paths.kb_review_file(&kb, artifact_id);
        std::fs::create_dir_all(review_path.parent().unwrap()).unwrap();
        let mut file = review::ReviewFile::empty_skeleton(&kb, artifact_id, title);
        file.add_comment(NewComment {
            file: artifact_id.to_string(),
            file_label: "Artifact".to_string(),
            anchor: Anchor::File,
            author: Author::You,
            body: body.to_string(),
            choices: Vec::new(),
            attachments: Vec::new(),
            user: None,
            tags: Vec::new(),
            private: false,
        });
        let cid = file.comments[0].id.clone();
        review::save_atomic(&review_path, &file, None).unwrap();
        (paths, cid)
    }

    #[test]
    fn keep_memory_missing_comment_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, _cid) = review_with_comment(tmp.path(), "Tour", "kept text");
        let kb = KbName::new("smoke").unwrap();
        let review_dir = paths.kb_review_dir(&kb);
        let err = find_comment(&review_dir, "c_missing").unwrap_err();
        assert!(
            matches!(err, kb_core::Error::NotFound(_)),
            "missing comment must be NotFound, got {err:?}"
        );
    }

    #[test]
    fn keep_label_prefers_title_then_source_path() {
        assert_eq!(keep_label("Tour", "notes/a.html"), "Tour");
        assert_eq!(keep_label("  ", "notes/a.html"), "notes/a.html");
        assert_eq!(keep_label("", ""), "artifact");
    }

    #[test]
    fn keep_text_cites_the_comment_id() {
        let text = keep_text("hello", "c_abc");
        assert!(text.contains("hello"));
        assert!(text.contains("Citation: comment c_abc"));
    }

    #[test]
    fn keep_memory_inserts_once_and_a_second_click_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, cid) = review_with_comment(tmp.path(), "Tour", "the comment body");
        let kb = KbName::new("smoke").unwrap();
        let review_dir = paths.kb_review_dir(&kb);
        let review_path = paths.kb_review_file(&kb, "abc123def456");
        let review_before = std::fs::read(&review_path).unwrap();
        let found = find_comment(&review_dir, &cid).unwrap();
        let memory_root = tmp.path().join("memory");
        std::fs::create_dir_all(&memory_root).unwrap();
        let calls = std::cell::Cell::new(0);

        let first = insert_keep_once(&review_dir, &memory_root, &found, "", |spec| {
            calls.set(calls.get() + 1);
            assert_eq!(spec.title, "Keep: Tour");
            assert!(spec.text.contains("the comment body"));
            assert!(spec.text.contains(&format!("Citation: comment {cid}")));
            assert!(spec.summary.contains(&cid));
            let rel = format!("kept-{cid}.html");
            std::fs::write(memory_root.join(&rel), &spec.text).unwrap();
            Ok::<_, KeepError>((format!("mem-{cid}"), rel))
        })
        .unwrap();
        assert!(!first.already);
        assert_eq!(calls.get(), 1);

        let second = insert_keep_once(&review_dir, &memory_root, &found, "", |_spec| {
            calls.set(calls.get() + 1);
            Err(KeepError::Core(kb_core::Error::Storage(
                "second keep must not insert".into(),
            )))
        })
        .unwrap();
        assert!(second.already);
        assert_eq!(second.id, first.id);
        assert_eq!(calls.get(), 1, "second click inserted another memory");

        let html: Vec<_> = files_under(&memory_root)
            .into_iter()
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("html"))
            .collect();
        assert_eq!(html.len(), 1, "expected one memory file, got {html:?}");
        let written = std::fs::read_to_string(&html[0]).unwrap();
        assert!(written.contains("the comment body"));
        assert!(written.contains(&format!("Citation: comment {cid}")));

        // The comment itself is untouched. The keep record is a sidecar.
        assert_eq!(std::fs::read(&review_path).unwrap(), review_before);
    }

    #[test]
    fn keep_memory_title_falls_back_to_source_path() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, cid) = review_with_comment(tmp.path(), "", "body");
        let kb = KbName::new("smoke").unwrap();
        let review_dir = paths.kb_review_dir(&kb);
        let found = find_comment(&review_dir, &cid).unwrap();
        let memory_root = tmp.path().join("memory");
        std::fs::create_dir_all(&memory_root).unwrap();
        let written = insert_keep_once(
            &review_dir,
            &memory_root,
            &found,
            "notes/tour.html",
            |spec| {
                assert_eq!(spec.title, "Keep: notes/tour.html");
                Ok::<_, KeepError>(("mem".into(), "kept.html".into()))
            },
        )
        .unwrap();
        assert!(!written.already);
        assert_eq!(written.id, "mem");
    }
}
