//! V4.S1 — apply a stored suggestion to the working tree.
//!
//! `POST /api/annotations/{id}/apply` is the daemon's **second** sanctioned
//! working-tree mutation (`crate::checkout` is the first). It splices
//! `annotation_suggestions.replacement` over the resolved `line`/`range`
//! on the live file and stamps the suggestion row `applied=1`.
//!
//! # No store/mirror wiring
//!
//! Deliberately owns NO store/mirror wiring beyond the suggestion-row
//! update (and the optional `resolved=1` stamp). The live-mirror watcher
//! (`crate::mirror`) already treats a plain filesystem write as an
//! ordinary idle reconcile — the same principle `crate::checkout`'s
//! module doc records for `git checkout`/`git switch`. This handler
//! writes the file and walks away; it does not call `IndexSink`, does
//! not bump the index generation, and does not emit `mirror.updated`.
//! The already-armed watcher picks the change up on its own.
//!
//! # `applied` records the ACT, not the file
//!
//! `annotation_suggestions.applied` is set only when this route actually
//! splices. If the working-tree range already equals `replacement`, the
//! handler returns `{already_applied: true, changed: false}` **without**
//! writing, **without** emitting SSE, and **without** flipping `applied`.
//! A file that happens to contain the replacement (operator edit, a
//! previous apply that crashed after the write, a coincident edit) is
//! not evidence that this daemon applied the suggestion. `applied` is
//! the audit of the act.
//!
//! # Exact-match guard
//!
//! The resolved range's current text must byte-equal `suggestion.original`
//! (joined the same way C2 captured it: `line..=line_end` with `\n`).
//! Drift is a structured 409; the tree is left untouched. Dirty state
//! elsewhere in the tree is irrelevant — the exact-match guard **is**
//! the policy.
//!
//! Anchor resolution reuses the existing ladder (`annotations::resolve`
//! → kb-core exact → Jaro-Winkler → stale) plus
//! `review_comments::line_matches_snippet` (a wrong line is worse than
//! an honest refusal). There is no third resolver.
//!
//! # Trailing newline
//!
//! The splice is by lines. The file's trailing-newline state
//! (`content.ends_with('\n')`) is preserved exactly, even when the
//! replacement has a different line count.
//!
//! # Sequential store updates
//!
//! Mark-applied and the optional resolve (`body.resolve`) are sequential
//! `Store` calls, not one transaction. Mark-applied is a single-row
//! UPDATE; resolve is the existing `update_annotation` COALESCE. A crash
//! between them can leave a written file + applied suggestion with the
//! annotation still open — retry hits the already-equal path (no second
//! write) and the operator can PATCH-resolve. Keeping it sequential
//! avoids growing `PreparedAnnotationOp` for a one-shot mutation.

use crate::annotations;
use crate::git::GitRepo;
use crate::review_comments::line_matches_snippet;
use crate::routes::{
    emit_annotation_changed, find_repo_by_id, safe_rel_path, validate_suggestion_target, ApiError,
};
use crate::state::SharedState;
use crate::store::{AnnotationRow, Store, StoreBlocking};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use kb_core::review::Anchor;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// `POST /api/annotations/{id}/apply` body. `resolve` defaults false.
#[derive(Debug, Default, Deserialize)]
pub struct ApplySuggestionBody {
    #[serde(default)]
    pub resolve: bool,
}

/// Successful splice (the file changed).
#[derive(Debug, Serialize)]
struct ApplyChangedBody {
    applied: bool,
    changed: bool,
    path: String,
    line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_end: Option<u32>,
}

/// Working-tree range already equals `replacement` — no write, no SSE.
#[derive(Debug, Serialize)]
struct AlreadyAppliedBody {
    already_applied: bool,
    changed: bool,
    applied: bool,
}

/// Exact-match miss (or untrusted resolve). Tree left untouched.
#[derive(Debug, Serialize)]
struct DriftBody {
    error: String,
    expected: String,
    found: String,
    resolved_line: u32,
}

/// Resolved working-tree span plus its current text.
/// `trusted` is false when the ladder went stale or
/// `line_matches_snippet` refused the line — we still surface the
/// best-effort range so the already-equal-replacement check can run
/// (a previous apply deletes the original snippet).
struct ResolvedWorkingRange {
    start: u32,
    end: u32,
    text: String,
    trusted: bool,
}

/// Inputs to the line-oriented splice. Named (not a tuple) so the
/// trailing-newline / different-line-count contract stays readable.
struct LineSplice<'a> {
    start: u32,
    end: u32,
    replacement: &'a str,
}

/// `POST /api/annotations/{id}/apply` — LOOPBACK-ONLY (see `router.rs`).
/// Body `{resolve?: bool}`. See the module doc for the exact-match
/// guard, the `applied`-is-an-act rule, and the no-mirror-wiring rule.
pub async fn apply_suggestion_route(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: Option<Json<ApplySuggestionBody>>,
) -> Result<Response, ApiError> {
    let resolve_after = body.map(|Json(b)| b.resolve).unwrap_or(false);
    let state_bg = state.clone();
    // 2026-08-31 incident (store.rs module doc): this handler has no
    // `.await` anywhere — row/suggestion fetch, the working-tree read +
    // splice, and the store stamps are all synchronous — so the whole
    // body runs as ONE closure on the blocking pool.
    state
        .store
        .run_blocking(move |store| apply_suggestion(store, &state_bg, id, resolve_after))
        .await
}

/// The sync body of [`apply_suggestion_route`] — see that fn's doc for why
/// it runs as a single `run_blocking` closure (2026-08-31 incident,
/// store.rs module doc).
fn apply_suggestion(
    store: &Store,
    state: &SharedState,
    id: String,
    resolve_after: bool,
) -> Result<Response, ApiError> {
    let row = store
        .get_annotation(&id)?
        .ok_or_else(|| ApiError::not_found(format!("annotation {id:?}")))?;
    // Kind/top-level check before the suggestion lookup so a reply is
    // 400 (must not reach the splice) even when it has no suggestion
    // row — "annotation with no suggestion" is the 404 on a valid
    // top-level line|range target.
    validate_suggestion_target(&row)?;
    let suggestion = store
        .get_annotation_suggestion(&id)?
        .ok_or_else(|| ApiError::not_found(format!("no suggestion on annotation {id:?}")))?;

    let repo = find_repo_by_id(state, row.repo_id)?;
    let rel = safe_rel_path(&row.path)?.to_string();
    // V70-A2 (SEC-13) — the apply lane READS and then WRITES this path, so
    // containment matters twice over: a symlinked path would let a stored
    // annotation splice bytes outside the repo.
    let abs = crate::security::paths::contained_abs_path(&repo.path, &rel)?;

    let content = match std::fs::read_to_string(&abs) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(conflict_simple(format!(
                "{rel}: not found in the working tree"
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            return Ok(conflict_simple(format!("{rel}: not valid UTF-8")));
        }
        Err(e) => {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("read {rel}: {e}"),
            ));
        }
    };

    let resolved = match resolve_working_range(&row, &content) {
        Ok(r) => r,
        Err(AnchorResolveError::Corrupt(msg)) => {
            return Err(ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, msg));
        }
    };

    if resolved.text == suggestion.replacement {
        // File already matches replacement — no write, no SSE, do not
        // mark applied (`applied` is the act, not the file).
        return Ok((
            [(header::CACHE_CONTROL, "no-store")],
            Json(AlreadyAppliedBody {
                already_applied: true,
                changed: false,
                applied: suggestion.applied,
            }),
        )
            .into_response());
    }

    if !resolved.trusted || resolved.text != suggestion.original {
        let error = if resolved.trusted {
            "working-tree range no longer matches suggestion.original"
        } else {
            "could not resolve suggestion anchor onto the working tree"
        };
        return Ok(drift_response(
            error,
            &suggestion.original,
            &resolved.text,
            resolved.start,
        ));
    }

    let next = splice_lines(
        &content,
        &LineSplice {
            start: resolved.start,
            end: resolved.end,
            replacement: &suggestion.replacement,
        },
    );
    write_working_tree_atomically(&abs, &next)?;

    let now = chrono::Utc::now().timestamp();
    let head_sha = current_head_sha(&repo.path);
    let marked = store.mark_annotation_suggestion_applied(&id, now, &head_sha)?;
    if !marked {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("suggestion for {id:?} vanished immediately after the write"),
        ));
    }

    // Sequential, not one tx — see the module doc. Resolve uses the
    // existing `update_annotation` COALESCE so we do not grow a second
    // resolve helper (and so we emit `annotation.changed` exactly once
    // ourselves if asked; there is no resolve helper that already emits).
    if resolve_after {
        let _ = store.update_annotation(&id, None, Some(true), None, now)?;
    }

    emit_suggestion_applied(&state.bus, &repo.name, &rel, &id, row.review_id);
    if resolve_after {
        emit_annotation_changed(&state.bus, &repo.name, &rel, row.review_id);
    }

    let line_end = if resolved.start == resolved.end {
        None
    } else {
        Some(resolved.end)
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(ApplyChangedBody {
            applied: true,
            changed: true,
            path: rel,
            line: resolved.start,
            line_end,
        }),
    )
        .into_response())
}

fn emit_suggestion_applied(
    bus: &kb_core::events::EventBus,
    repo: &str,
    path: &str,
    annotation_id: &str,
    review_id: Option<i64>,
) {
    let mut body = serde_json::json!({
        "repo": repo,
        "path": path,
        "annotation_id": annotation_id,
    });
    if let Some(id) = review_id {
        body["review_id"] = serde_json::json!(id);
    }
    bus.emit("suggestion.applied", body);
}

fn drift_response(error: &str, expected: &str, found: &str, resolved_line: u32) -> Response {
    (
        StatusCode::CONFLICT,
        [(header::CACHE_CONTROL, "no-store")],
        Json(DriftBody {
            error: error.to_string(),
            expected: expected.to_string(),
            found: found.to_string(),
            resolved_line,
        }),
    )
        .into_response()
}

fn conflict_simple(error: impl Into<String>) -> Response {
    (
        StatusCode::CONFLICT,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "error": error.into() })),
    )
        .into_response()
}

enum AnchorResolveError {
    Corrupt(String),
}

fn resolve_working_range(
    row: &AnnotationRow,
    content: &str,
) -> Result<ResolvedWorkingRange, AnchorResolveError> {
    let raw = row.anchor.as_deref().ok_or_else(|| {
        AnchorResolveError::Corrupt(format!("annotation {} is missing its start anchor", row.id))
    })?;
    let start_anchor: Anchor = serde_json::from_str(raw).map_err(|e| {
        AnchorResolveError::Corrupt(format!(
            "annotation {} has a corrupt start anchor: {e}",
            row.id
        ))
    })?;
    let start = annotations::resolve(content, &start_anchor);

    if row.anchor_kind == annotations::ANCHOR_KIND_RANGE {
        let end_raw = row.anchor2.as_deref().ok_or_else(|| {
            AnchorResolveError::Corrupt(format!(
                "range annotation {} is missing its end anchor",
                row.id
            ))
        })?;
        let end_anchor: Anchor = serde_json::from_str(end_raw).map_err(|e| {
            AnchorResolveError::Corrupt(format!(
                "range annotation {} has a corrupt end anchor: {e}",
                row.id
            ))
        })?;
        let end = annotations::resolve(content, &end_anchor);
        let trusted = !start.stale
            && !end.stale
            && line_matches_snippet(content, start.line, snippet_of(&start_anchor))
            && line_matches_snippet(content, end.line, snippet_of(&end_anchor));
        // A fuzzy-wrong line is worse than the stored offset (same
        // "orphan over wrong line" rule as `resolve_for_ps`). The
        // already-equal-replacement check needs the original slot.
        let (lo, hi) = if trusted {
            let (lo, hi, _) = annotations::normalize_range(start, end);
            (lo, hi)
        } else {
            let a = offset_of(&start_anchor);
            let b = offset_of(&end_anchor);
            if a <= b {
                (a, b)
            } else {
                (b, a)
            }
        };
        let text = range_text(content, lo, hi).unwrap_or_default();
        return Ok(ResolvedWorkingRange {
            start: lo,
            end: hi,
            text,
            trusted,
        });
    }

    let trusted =
        !start.stale && line_matches_snippet(content, start.line, snippet_of(&start_anchor));
    let line = if trusted {
        start.line
    } else {
        offset_of(&start_anchor)
    };
    let text = range_text(content, line, line).unwrap_or_default();
    Ok(ResolvedWorkingRange {
        start: line,
        end: line,
        text,
        trusted,
    })
}

fn offset_of(anchor: &Anchor) -> u32 {
    match anchor {
        Anchor::Selection { offset, .. } => *offset,
        _ => 1,
    }
}

fn snippet_of(anchor: &Anchor) -> &str {
    match anchor {
        Anchor::Selection { snippet, .. } => snippet.as_str(),
        _ => "",
    }
}

/// Same join as C2's `original_from_content` (`line..=line_end` with `\n`).
fn range_text(content: &str, start: u32, end: u32) -> Option<String> {
    if start < 1 || end < start {
        return None;
    }
    let lines: Vec<&str> = content.lines().collect();
    let lo = (start - 1) as usize;
    let hi = end as usize;
    if hi > lines.len() {
        return None;
    }
    Some(lines[lo..hi].join("\n"))
}

/// Replace `start..=end` (1-based, inclusive) with `replacement`'s lines.
/// Preserves whether `content` ended with `\n`.
fn splice_lines(content: &str, splice: &LineSplice<'_>) -> String {
    let had_trailing_nl = content.ends_with('\n');
    let lines: Vec<&str> = content.lines().collect();
    let start_idx = (splice.start.saturating_sub(1) as usize).min(lines.len());
    let end_idx = (splice.end as usize).clamp(start_idx, lines.len());
    let new_lines: Vec<&str> = splice.replacement.lines().collect();

    let mut out = Vec::with_capacity(lines.len() - (end_idx - start_idx) + new_lines.len());
    out.extend_from_slice(&lines[..start_idx]);
    out.extend(new_lines);
    out.extend_from_slice(&lines[end_idx..]);

    let mut result = out.join("\n");
    if had_trailing_nl {
        result.push('\n');
    }
    result
}

/// Temp file in the same directory + rename. Copies the destination's
/// permissions onto the temp file so a 0644 source does not become 0600.
/// `tempfile` is a *dev*-dependency in this crate, so this is std-only.
fn write_working_tree_atomically(path: &Path, contents: &str) -> Result<(), ApiError> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let tmp_path = dir.join(format!(
        ".kb-apply-{}-{}.tmp",
        std::process::id(),
        annotations::short_random_hex()
    ));
    let write_err = |e: std::io::Error, what: &str| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{what} {}: {e}", path.display()),
        )
    };
    if let Err(e) = std::fs::write(&tmp_path, contents) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(write_err(e, "write temp for"));
    }
    if let Ok(meta) = std::fs::metadata(path) {
        if let Err(e) = std::fs::set_permissions(&tmp_path, meta.permissions()) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(write_err(e, "preserve permissions for"));
        }
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(write_err(e, "rename onto"));
    }
    Ok(())
}

fn current_head_sha(repo_root: &Path) -> String {
    GitRepo::open(repo_root)
        .ok()
        .and_then(|g| g.head_info().ok())
        .and_then(|h| h.sha)
        .unwrap_or_default()
}

// --- PRR-R10 — multi-file suggestion batch apply --------------------------
//
// `POST /api/annotations/apply-batch` (LOOPBACK-ONLY, same family as the
// single apply above) generalizes the single-suggestion splice to N
// annotation ids across (potentially) many files, in TWO PHASES:
//
// 1. VERIFY ALL — the exact same per-id checks the single apply performs
//    (suggestion exists, not a reply / valid anchor kind / not `side=old`
//    via `validate_suggestion_target`, not already applied, the anchor
//    re-resolves TRUSTED, and the resolved range byte-equals
//    `suggestion.original`), PLUS same-file overlap detection across the
//    batch itself. ANY single failure aborts the whole batch with a 409
//    carrying a verdict for EVERY requested id (`{id, ok, error?}`) —
//    nothing is written to the working tree or the store; the only I/O is
//    the same read-only content fetch the single apply already does.
//
// 2. APPLY ALL — only reached when every verdict is `ok`. Suggestions are
//    grouped by `(repo_id, path)`; within one file, splices land in
//    DESCENDING start-line order over the SAME content snapshot the verify
//    phase already read (so line numbers computed during verify stay valid
//    — no re-read, no re-resolve), then the whole file is written ONCE via
//    `write_working_tree_atomically` (the same temp-file+rename primitive
//    the single apply uses). The store is stamped (`applied` + optional
//    `resolved`) and `suggestion.applied` (+ `annotation.changed` when
//    `resolve_threads`) is emitted PER ITEM only AFTER every file in the
//    batch has been durably written — a mid-batch IO failure can then
//    never leave the store believing something is applied when the disk
//    doesn't agree with it. On such a failure the handler restores every
//    already-written file back to its pre-batch bytes (best effort — a
//    restore-write can itself fail) and returns an honest
//    `{applied, restored, failed}` record: `restored` lists exactly the
//    files that DID roll back; `applied` reports any file whose restore
//    ITSELF failed (its new bytes are still on disk despite the aborted
//    batch — a caller must not assume `restored` covers everything that
//    was written).
//
// Reuses (never duplicates) `resolve_working_range`, `splice_lines`,
// `write_working_tree_atomically`, `AnchorResolveError`, and
// `current_head_sha` from the single-apply machinery above.

/// `apply-batch`'s hard cap. Each id here can trigger a full-file rewrite
/// (not a cheap row op like `routes::MAX_ANNOTATION_BATCH_OPS`'s 100 ops),
/// so the ceiling is lower.
const MAX_APPLY_BATCH_IDS: usize = 50;

/// `POST /api/annotations/apply-batch` body.
#[derive(Debug, Deserialize)]
pub struct ApplyBatchBody {
    pub annotation_ids: Vec<String>,
    #[serde(default)]
    pub resolve_threads: bool,
}

/// One id's verify-phase outcome. `ok: true` never carries `error`;
/// `ok: false` always does — the addendum's `{id, ok|error}` shorthand,
/// made self-describing with an explicit flag rather than requiring a
/// caller to probe for key presence.
#[derive(Debug, Serialize)]
struct BatchVerdict {
    id: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<BatchVerdictError>,
}

impl BatchVerdict {
    fn ok(id: &str) -> Self {
        Self {
            id: id.to_string(),
            ok: true,
            error: None,
        }
    }

    fn err(id: &str, kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            id: id.to_string(),
            ok: false,
            error: Some(BatchVerdictError {
                kind,
                detail: detail.into(),
            }),
        }
    }
}

/// Machine-checkable failure kind + a human detail string. `kind`
/// vocabulary: `not_found` · `invalid_target` · `no_suggestion` ·
/// `already_applied` · `file_not_found` · `not_utf8` · `corrupt_anchor` ·
/// `unresolvable_anchor` · `drift` · `overlap`.
#[derive(Debug, Serialize)]
struct BatchVerdictError {
    kind: &'static str,
    detail: String,
}

/// A single successfully-verified suggestion, carrying everything the
/// apply phase needs without re-reading the store or the working tree.
struct VerifiedItem {
    id: String,
    repo_id: i64,
    repo_root: std::path::PathBuf,
    repo_name: String,
    review_id: Option<i64>,
    abs: std::path::PathBuf,
    rel: String,
    start: u32,
    end: u32,
    replacement: String,
}

enum VerifyOneOutcome {
    Ok(VerifiedItem),
    Err(BatchVerdictError),
}

/// One id's verify pass. Returns `Err(ApiError)` only for genuinely
/// server-side faults (an annotation referencing an unconfigured repo, a
/// corrupt stored path) — the SAME faults the single apply route
/// propagates as a bare error rather than a structured conflict. Every
/// USER-facing failure mode comes back as `Ok(VerifyOneOutcome::Err(_))`
/// so the caller can fold it into that id's verdict instead of aborting
/// the whole HTTP response before every id has a verdict.
fn verify_one(
    store: &Store,
    state: &SharedState,
    id: &str,
    content_cache: &mut HashMap<(i64, String), String>,
) -> Result<VerifyOneOutcome, ApiError> {
    let row = match store.get_annotation(id)? {
        Some(r) => r,
        None => {
            return Ok(VerifyOneOutcome::Err(BatchVerdictError {
                kind: "not_found",
                detail: format!("annotation {id:?} does not exist"),
            }));
        }
    };
    if let Err(e) = validate_suggestion_target(&row) {
        return Ok(VerifyOneOutcome::Err(BatchVerdictError {
            kind: "invalid_target",
            detail: e.message().to_string(),
        }));
    }
    let suggestion = match store.get_annotation_suggestion(id)? {
        Some(s) => s,
        None => {
            return Ok(VerifyOneOutcome::Err(BatchVerdictError {
                kind: "no_suggestion",
                detail: format!("annotation {id:?} carries no suggestion"),
            }));
        }
    };
    if suggestion.applied {
        return Ok(VerifyOneOutcome::Err(BatchVerdictError {
            kind: "already_applied",
            detail: format!("suggestion on {id:?} is already applied"),
        }));
    }

    let repo = find_repo_by_id(state, row.repo_id)?;
    let rel = safe_rel_path(&row.path)?.to_string();
    // V70-A2 (SEC-13) — same containment as the single-apply path above.
    let abs = crate::security::paths::contained_abs_path(&repo.path, &rel)?;
    let cache_key = (row.repo_id, rel.clone());
    let content = if let Some(c) = content_cache.get(&cache_key) {
        c.clone()
    } else {
        let c = match std::fs::read_to_string(&abs) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(VerifyOneOutcome::Err(BatchVerdictError {
                    kind: "file_not_found",
                    detail: format!("{rel}: not found in the working tree"),
                }));
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Ok(VerifyOneOutcome::Err(BatchVerdictError {
                    kind: "not_utf8",
                    detail: format!("{rel}: not valid UTF-8"),
                }));
            }
            Err(e) => {
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("read {rel}: {e}"),
                ));
            }
        };
        content_cache.insert(cache_key, c.clone());
        c
    };

    let resolved = match resolve_working_range(&row, &content) {
        Ok(r) => r,
        Err(AnchorResolveError::Corrupt(msg)) => {
            return Ok(VerifyOneOutcome::Err(BatchVerdictError {
                kind: "corrupt_anchor",
                detail: msg,
            }));
        }
    };
    if !resolved.trusted {
        return Ok(VerifyOneOutcome::Err(BatchVerdictError {
            kind: "unresolvable_anchor",
            detail: "could not resolve suggestion anchor onto the working tree".to_string(),
        }));
    }
    if resolved.text != suggestion.original {
        return Ok(VerifyOneOutcome::Err(BatchVerdictError {
            kind: "drift",
            detail: format!(
                "working-tree range no longer matches suggestion.original (expected {:?}, found {:?})",
                suggestion.original, resolved.text
            ),
        }));
    }

    Ok(VerifyOneOutcome::Ok(VerifiedItem {
        id: id.to_string(),
        repo_id: row.repo_id,
        repo_root: repo.path.clone(),
        repo_name: repo.name.clone(),
        review_id: row.review_id,
        abs,
        rel,
        start: resolved.start,
        end: resolved.end,
        replacement: suggestion.replacement.clone(),
    }))
}

/// Same-file (same `(repo_id, rel)`) inclusive-range overlap check across
/// every INDIVIDUALLY-passing item. O(n^2) per path group — the 50-id cap
/// keeps this trivial. Flips BOTH sides of an overlapping pair to an
/// error verdict (never just one), so the response never implies one of
/// the two was fine.
fn detect_overlaps(verified: &[Option<VerifiedItem>], verdicts: &mut [BatchVerdict]) {
    let mut by_path: HashMap<(i64, String), Vec<usize>> = HashMap::new();
    for (i, v) in verified.iter().enumerate() {
        if let Some(item) = v {
            by_path
                .entry((item.repo_id, item.rel.clone()))
                .or_default()
                .push(i);
        }
    }
    for idxs in by_path.into_values() {
        if idxs.len() < 2 {
            continue;
        }
        for a in 0..idxs.len() {
            for b in (a + 1)..idxs.len() {
                let ia = idxs[a];
                let ib = idxs[b];
                let item_a = verified[ia].as_ref().unwrap();
                let item_b = verified[ib].as_ref().unwrap();
                if item_a.start <= item_b.end && item_b.start <= item_a.end {
                    verdicts[ia] = BatchVerdict::err(
                        &item_a.id,
                        "overlap",
                        format!("overlaps {} on {}", item_b.id, item_a.rel),
                    );
                    verdicts[ib] = BatchVerdict::err(
                        &item_b.id,
                        "overlap",
                        format!("overlaps {} on {}", item_a.id, item_b.rel),
                    );
                }
            }
        }
    }
}

/// One applied suggestion in the response — mirrors `ApplyChangedBody`'s
/// `path`/`line`/`line_end` shape so a batch item and a single-apply
/// response describe an applied splice identically.
#[derive(Debug, Serialize, Clone)]
struct AppliedItem {
    id: String,
    path: String,
    line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_end: Option<u32>,
}

/// `POST /api/annotations/apply-batch` — LOOPBACK-ONLY (see `router.rs`).
/// Body `{annotation_ids: [..], resolve_threads?: bool}`. See the module
/// section doc above for the two-phase contract.
pub async fn apply_suggestions_batch_route(
    State(state): State<SharedState>,
    Json(body): Json<ApplyBatchBody>,
) -> Result<Response, ApiError> {
    if body.annotation_ids.is_empty() {
        return Err(ApiError::bad_request(
            "apply-batch requires at least one annotation id",
        ));
    }
    if body.annotation_ids.len() > MAX_APPLY_BATCH_IDS {
        return Err(ApiError::bad_request(format!(
            "batch of {} ids exceeds the {MAX_APPLY_BATCH_IDS} limit",
            body.annotation_ids.len()
        )));
    }

    let state_bg = state.clone();
    // 2026-08-31 incident (store.rs module doc): this handler has no
    // `.await` anywhere — the verify phase's per-id store+fs reads,
    // overlap detection, and the apply phase's per-file writes + store
    // stamps are all synchronous — so the whole two-phase body runs as
    // ONE closure on the blocking pool.
    state
        .store
        .run_blocking(move |store| {
            let mut verdicts: Vec<BatchVerdict> = Vec::with_capacity(body.annotation_ids.len());
            let mut verified: Vec<Option<VerifiedItem>> =
                Vec::with_capacity(body.annotation_ids.len());
            let mut content_cache: HashMap<(i64, String), String> = HashMap::new();

            for id in &body.annotation_ids {
                match verify_one(store, &state_bg, id, &mut content_cache)? {
                    VerifyOneOutcome::Ok(item) => {
                        verdicts.push(BatchVerdict::ok(id));
                        verified.push(Some(item));
                    }
                    VerifyOneOutcome::Err(err) => {
                        verdicts.push(BatchVerdict::err(id, err.kind, err.detail));
                        verified.push(None);
                    }
                }
            }

            detect_overlaps(&verified, &mut verdicts);

            if verdicts.iter().any(|v| !v.ok) {
                return Ok((
                    StatusCode::CONFLICT,
                    [(header::CACHE_CONTROL, "no-store")],
                    Json(serde_json::json!({ "verdicts": verdicts })),
                )
                    .into_response());
            }

            // Every `verified[i]` is `Some` — every verdict was `ok`.
            let items: Vec<VerifiedItem> = verified.into_iter().map(|v| v.unwrap()).collect();
            apply_verified_batch(
                store,
                &state_bg,
                items,
                body.resolve_threads,
                &content_cache,
            )
        })
        .await
}

/// One file's write, kept around so a later failure in the SAME batch can
/// restore it. `prior` is the exact bytes the verify phase read (nothing
/// re-reads the file between the two phases); `ids` names every
/// suggestion that landed in the file's splice, so the eventual
/// applied/restored report can attribute back to annotation ids.
struct WrittenFile {
    abs: std::path::PathBuf,
    rel: String,
    repo_name: String,
    repo_root: std::path::PathBuf,
    prior: String,
    ids: Vec<AppliedItem>,
    review_ids: Vec<Option<i64>>,
}

/// Uniform `review_id` across a group, mirroring `routes::batch_emit_scope`'s
/// "present iff every review-scoped item names the same review" rule —
/// duplicated here (not imported) since that helper is private to
/// `routes.rs` and this is an 8-line predicate, not splice logic.
fn uniform_review_id(ids: &[Option<i64>]) -> Option<i64> {
    let mut seen: Option<i64> = None;
    for rid in ids.iter().flatten() {
        match seen {
            None => seen = Some(*rid),
            Some(s) if s == *rid => {}
            Some(_) => return None,
        }
    }
    seen
}

/// Phase 2 — apply every already-VERIFIED item. Only reached when every
/// id in the batch passed phase 1, so every splice below is expected to
/// land cleanly; the only failure mode handled here is an IO fault on the
/// write itself (disk full, permissions changed since verify, …).
fn apply_verified_batch(
    store: &Store,
    state: &SharedState,
    items: Vec<VerifiedItem>,
    resolve_threads: bool,
    originals: &HashMap<(i64, String), String>,
) -> Result<Response, ApiError> {
    // Group by (repo_id, rel), preserving first-appearance order — the
    // batch's own id order decides which file is attempted first.
    let mut order: Vec<(i64, String)> = Vec::new();
    let mut groups: HashMap<(i64, String), Vec<VerifiedItem>> = HashMap::new();
    for item in items {
        let key = (item.repo_id, item.rel.clone());
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(item);
    }

    let mut written: Vec<WrittenFile> = Vec::new();

    for key in &order {
        let mut group = groups.remove(key).expect("key came from `order`");
        // DESCENDING start line — later splices first, so an
        // already-applied earlier splice never shifts a later one's
        // stored line number out from under it.
        group.sort_by_key(|it| std::cmp::Reverse(it.start));

        let prior = originals.get(key).cloned().unwrap_or_default();
        let mut content = prior.clone();
        let mut ids = Vec::with_capacity(group.len());
        let mut review_ids = Vec::with_capacity(group.len());
        let abs = group[0].abs.clone();
        let rel = group[0].rel.clone();
        let repo_name = group[0].repo_name.clone();
        let repo_root = group[0].repo_root.clone();
        for it in &group {
            content = splice_lines(
                &content,
                &LineSplice {
                    start: it.start,
                    end: it.end,
                    replacement: &it.replacement,
                },
            );
            let line_end = if it.start == it.end {
                None
            } else {
                Some(it.end)
            };
            ids.push(AppliedItem {
                id: it.id.clone(),
                path: rel.clone(),
                line: it.start,
                line_end,
            });
            review_ids.push(it.review_id);
        }

        if let Err(e) = write_working_tree_atomically(&abs, &content) {
            return Ok(restore_and_report(&written, &rel, &ids, &e));
        }

        written.push(WrittenFile {
            abs,
            rel,
            repo_name,
            repo_root,
            prior,
            ids,
            review_ids,
        });
    }

    // Every file landed on disk. Only NOW does the batch stamp the store
    // and emit SSE — see the module section doc for why this is deferred
    // to the end rather than done per-file as each write lands.
    let now = chrono::Utc::now().timestamp();
    let mut applied_out: Vec<AppliedItem> = Vec::new();
    for wf in &written {
        let head_sha = current_head_sha(&wf.repo_root);
        for (item, review_id) in wf.ids.iter().zip(wf.review_ids.iter()) {
            let marked = store.mark_annotation_suggestion_applied(&item.id, now, &head_sha)?;
            if !marked {
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "suggestion for {:?} vanished immediately after the write",
                        item.id
                    ),
                ));
            }
            if resolve_threads {
                let _ = store.update_annotation(&item.id, None, Some(true), None, now)?;
            }
            emit_suggestion_applied(&state.bus, &wf.repo_name, &wf.rel, &item.id, *review_id);
            applied_out.push(item.clone());
        }
        if resolve_threads {
            let review_id = uniform_review_id(&wf.review_ids);
            emit_annotation_changed(&state.bus, &wf.repo_name, &wf.rel, review_id);
        }
    }

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "applied": applied_out,
            "restored": Vec::<String>::new(),
            "failed": serde_json::Value::Null,
        })),
    )
        .into_response())
}

/// Best-effort rollback of every FILE already written in this batch
/// attempt, called the moment one file's write fails. Nothing here
/// touches the store (nothing was stamped yet — see `apply_verified_batch`'s
/// doc), so a caller re-driving the same batch after fixing the
/// underlying IO fault sees a clean slate, not a half-stamped one.
fn restore_and_report(
    written: &[WrittenFile],
    failed_rel: &str,
    failed_ids: &[AppliedItem],
    write_err: &ApiError,
) -> Response {
    let mut restored: Vec<String> = Vec::new();
    let mut still_applied: Vec<AppliedItem> = Vec::new();
    for wf in written {
        match write_working_tree_atomically(&wf.abs, &wf.prior) {
            Ok(()) => restored.push(wf.rel.clone()),
            Err(_) => still_applied.extend(wf.ids.iter().cloned()),
        }
    }
    let failed_id = failed_ids.first().map(|i| i.id.clone()).unwrap_or_default();
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "applied": still_applied,
            "restored": restored,
            "failed": {
                "id": failed_id,
                "path": failed_rel,
                "error": write_err.message(),
            },
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn splice(content: &str, start: u32, end: u32, replacement: &str) -> String {
        splice_lines(
            content,
            &LineSplice {
                start,
                end,
                replacement,
            },
        )
    }

    #[test]
    fn splice_preserves_trailing_newline() {
        let src = "fn a() {}\nfn b() {}\nfn c() {}\n";
        assert_eq!(
            splice(src, 2, 2, "fn REPL() {}"),
            "fn a() {}\nfn REPL() {}\nfn c() {}\n"
        );
    }

    #[test]
    fn splice_preserves_missing_trailing_newline() {
        let src = "fn a() {}\nfn b() {}\nfn c() {}";
        assert_eq!(
            splice(src, 2, 2, "fn REPL() {}"),
            "fn a() {}\nfn REPL() {}\nfn c() {}"
        );
    }

    #[test]
    fn splice_replacement_may_change_line_count() {
        let src = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n";
        assert_eq!(
            splice(src, 2, 3, "fn x() {}"),
            "fn a() {}\nfn x() {}\nfn d() {}\n"
        );
        assert_eq!(
            splice(src, 2, 3, "fn x() {}\nfn y() {}\nfn z() {}"),
            "fn a() {}\nfn x() {}\nfn y() {}\nfn z() {}\nfn d() {}\n"
        );
    }

    #[test]
    fn splice_empty_replacement_deletes_the_range() {
        let src = "fn a() {}\nfn b() {}\nfn c() {}\n";
        assert_eq!(splice(src, 2, 2, ""), "fn a() {}\nfn c() {}\n");
    }

    #[test]
    fn range_text_matches_c2_join() {
        let src = "fn a() {}\nfn b() {}\nfn c() {}\n";
        assert_eq!(range_text(src, 2, 2).as_deref(), Some("fn b() {}"));
        assert_eq!(
            range_text(src, 2, 3).as_deref(),
            Some("fn b() {}\nfn c() {}")
        );
        assert_eq!(range_text(src, 2, 9), None);
    }
}
