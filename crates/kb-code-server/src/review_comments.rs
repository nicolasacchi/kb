//! V4.C1 — review-scoped comments: lazy carry-forward resolution and
//! `GET /api/reviews/{id}/comments`.
//!
//! A review comment is an ordinary `annotations` row with `review_id`
//! set (see `routes::create_annotation`'s review-scope branch). Its
//! `ps_number`/`side` record the patchset and side the anchor was
//! **created** against. Re-resolution against a later (or earlier)
//! target patchset is computed **per request, never persisted** — both
//! inputs are immutable (the stored `Anchor` and the pinned blob at
//! `target_ps.{tip,base}_sha`), so `capture_patchset` and the
//! auto-capture worker stay untouched.
//!
//! # Resolution contract
//!
//! [`resolve_for_ps`] reuses the existing ladder
//! (`annotations::resolve` → kb-core exact → Jaro-Winkler fuzzy →
//! stale; [`annotations::normalize_range`] for ranges). It does **not**
//! invent a matcher. File absent at the target sha → orphaned. A
//! `stale` result → orphaned. When the resolved line's trimmed text
//! does not equal the stored snippet (the `locate_line`-miss fallback
//! inside `resolve` would otherwise pin the original offset onto
//! whatever now lives there), the answer is also orphaned: a wrong
//! line is worse than an honest orphan.

use crate::annotations;
use crate::git::{GitError, GitRepo, DEFAULT_BLOB_SIZE_CAP};
use crate::reviews::{require_review, resolve_ps};
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{AnnotationRow, ReviewPatchsetRow, Store, StoreBlocking};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use kb_core::review::Anchor;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub const SCHEMA: &str = "review-comments/1";

/// Which blob of `target_ps` a comment's `side` names.
pub fn target_sha_for_side<'a>(side: Option<&str>, ps: &'a ReviewPatchsetRow) -> &'a str {
    match side {
        Some("old") => ps.base_sha.as_str(),
        _ => ps.tip_sha.as_str(),
    }
}

/// The patchset + blob this comment was resolved against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedAgainst {
    pub ps: i64,
    pub sha: String,
}

/// The stored (creation-time) position, surfaced only when orphaned so
/// a client can still say "this was line L of ps N".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OriginalAnchor {
    pub ps: i64,
    pub side: String,
    pub line: u32,
    pub snippet: String,
}

/// Outcome of [`resolve_for_ps`]. `line`/`line_end` are `None` when
/// orphaned (a wrong-line number is worse than a hole). `original` is
/// `Some` iff `orphaned`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedForPs {
    pub line: Option<u32>,
    pub line_end: Option<u32>,
    pub orphaned: bool,
    pub resolved_against: ResolvedAgainst,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original: Option<OriginalAnchor>,
    /// PRR-R3 (design doc §2 row 9 / Risk #4) — `"exact"` | `"fuzzy"`,
    /// straight from the SAME `annotations::resolve` call this function
    /// already makes (`annotations::MatchConfidence`, no second resolution
    /// algorithm). `None` when `orphaned` (nothing to grade) or for an
    /// anchor kind with no textual match at all (`whole_file`/`review` —
    /// presence-only or unconditional). A `range`'s confidence is the
    /// WEAKER of its two independently-resolved endpoints — a range is
    /// only as trustworthy as its least-certain edge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<&'static str>,
}

/// `annotations::MatchConfidence` -> the wire string [`ResolvedForPs::
/// confidence`] carries. `pub(crate)` — PRR-R7's `crate::review_github_threads`
/// reuses this exact mapping rather than a second copy (the same "shared fn,
/// not a second matcher" discipline as [`read_blob_text`]/
/// [`line_matches_snippet`]).
pub(crate) fn confidence_str(c: annotations::MatchConfidence) -> &'static str {
    match c {
        annotations::MatchConfidence::Exact => "exact",
        annotations::MatchConfidence::Fuzzy => "fuzzy",
    }
}

/// The weaker of two endpoint confidences for a `range` — Fuzzy wins any
/// mix (a range is only as trustworthy as its least-certain edge).
fn combine_confidence(
    a: annotations::MatchConfidence,
    b: annotations::MatchConfidence,
) -> &'static str {
    use annotations::MatchConfidence::{Exact, Fuzzy};
    match (a, b) {
        (Exact, Exact) => "exact",
        (Fuzzy, _) | (_, Fuzzy) => "fuzzy",
    }
}

/// Re-resolve `row` against `target_ps`'s pinned blob for `row.side`.
/// Opens a `GitRepo` on `repo_root` and reads the blob once. Prefer
/// [`resolve_for_ps_with_content`] when a caller already has the bytes
/// (the comments route caches one read per `(path, sha)`).
pub fn resolve_for_ps(
    repo_root: &Path,
    row: &AnnotationRow,
    target_ps: &ReviewPatchsetRow,
) -> ResolvedForPs {
    let sha = target_sha_for_side(row.side.as_deref(), target_ps).to_string();
    let content = read_blob_text(repo_root, &row.path, &sha);
    resolve_for_ps_with_content(row, target_ps, &sha, content.as_deref())
}

/// Same as [`resolve_for_ps`] but takes already-read file text
/// (`None` = file absent / not valid UTF-8 at that sha → orphaned).
pub fn resolve_for_ps_with_content(
    row: &AnnotationRow,
    target_ps: &ReviewPatchsetRow,
    sha: &str,
    content: Option<&str>,
) -> ResolvedForPs {
    let against = ResolvedAgainst {
        ps: target_ps.ps_number,
        sha: sha.to_string(),
    };
    let original = original_from_row(row);
    let orphaned = |orig: Option<OriginalAnchor>| ResolvedForPs {
        line: None,
        line_end: None,
        orphaned: true,
        resolved_against: against.clone(),
        original: orig,
        confidence: None,
    };

    // PRR-R3 (design arbitration #6) — a review-scoped, PATH-LESS "general
    // question" annotation has no anchor to resolve against AT ALL and is
    // always resolved by construction (there is nothing to go stale) —
    // dispatched before the `content`/`anchor` checks below, which all
    // assume a real file.
    if row.anchor_kind == annotations::ANCHOR_KIND_REVIEW {
        return ResolvedForPs {
            line: None,
            line_end: None,
            orphaned: false,
            resolved_against: against,
            original: None,
            confidence: None,
        };
    }

    let Some(text) = content else {
        return orphaned(original);
    };

    // A reply has no anchor of its own — callers should not resolve
    // one, but fail closed rather than invent a line.
    if row.parent_id.is_some() || row.anchor.is_none() {
        return orphaned(original);
    }

    // PRR-R3 — a `whole_file` finding's `anchor` column holds the bare
    // PATH (not JSON, see `annotations::ANCHOR_KIND_WHOLE_FILE`'s doc), so
    // it must never reach the `serde_json::from_str::<Anchor>` parse below
    // (that would always fail and mislabel a present file as orphaned).
    // Resolved iff the path exists at `target_ps`'s pinned blob for this
    // row's side — `content` being `Some` above already proved that; no
    // line claim is ever made, so there is no textual confidence to grade.
    if row.anchor_kind == annotations::ANCHOR_KIND_WHOLE_FILE {
        return ResolvedForPs {
            line: None,
            line_end: None,
            orphaned: false,
            resolved_against: against,
            original: None,
            confidence: None,
        };
    }

    let Ok(anchor) = serde_json::from_str::<Anchor>(row.anchor.as_deref().unwrap_or("")) else {
        return orphaned(original);
    };

    if row.anchor_kind == annotations::ANCHOR_KIND_RANGE {
        let Some(end_json) = row.anchor2.as_deref() else {
            return orphaned(original);
        };
        let Ok(end_anchor) = serde_json::from_str::<Anchor>(end_json) else {
            return orphaned(original);
        };
        let start = annotations::resolve(text, &anchor);
        let end = annotations::resolve(text, &end_anchor);
        if start.stale
            || end.stale
            || !line_matches_snippet(text, start.line, snippet_of(&anchor))
            || !line_matches_snippet(text, end.line, snippet_of(&end_anchor))
        {
            return orphaned(original);
        }
        let (lo, hi, _) = annotations::normalize_range(start, end);
        return ResolvedForPs {
            line: Some(lo),
            line_end: Some(hi),
            orphaned: false,
            resolved_against: against,
            original: None,
            confidence: Some(combine_confidence(start.confidence, end.confidence)),
        };
    }

    let resolved = annotations::resolve(text, &anchor);
    if resolved.stale || !line_matches_snippet(text, resolved.line, snippet_of(&anchor)) {
        return orphaned(original);
    }
    ResolvedForPs {
        line: Some(resolved.line),
        line_end: None,
        confidence: Some(confidence_str(resolved.confidence)),
        orphaned: false,
        resolved_against: against,
        original: None,
    }
}

/// `pub(crate)` — PRR-R3's findings-list route (`crate::review_findings`)
/// reuses this exact blob read (same per-`(path, sha)` caching convention
/// as [`build_comment_groups`]) rather than a second copy.
pub(crate) fn read_blob_text(repo_root: &Path, path: &str, sha: &str) -> Option<String> {
    let git = GitRepo::open(repo_root).ok()?;
    match git.read_blob(sha, path, DEFAULT_BLOB_SIZE_CAP) {
        Ok(bytes) => String::from_utf8(bytes).ok(),
        Err(GitError::PathNotFound { .. }) | Err(GitError::NotABlob { .. }) => None,
        Err(_) => None,
    }
}

fn snippet_of(anchor: &Anchor) -> &str {
    match anchor {
        Anchor::Selection { snippet, .. } => snippet.as_str(),
        _ => "",
    }
}

fn original_line_of(anchor: &Anchor) -> u32 {
    match anchor {
        Anchor::Selection { offset, .. } => *offset,
        _ => 1,
    }
}

fn original_from_row(row: &AnnotationRow) -> Option<OriginalAnchor> {
    let ps = row.ps_number?;
    let side = row.side.clone().unwrap_or_else(|| "new".to_string());
    let anchor_json = row.anchor.as_deref()?;
    let Ok(anchor) = serde_json::from_str::<Anchor>(anchor_json) else {
        return None;
    };
    Some(OriginalAnchor {
        ps,
        side,
        line: original_line_of(&anchor),
        snippet: snippet_of(&anchor).to_string(),
    })
}

/// The resolved line must still carry the stored snippet (trimmed). A
/// 200-char-capped snippet is accepted as a prefix of a longer line —
/// that is the construction contract (`anchor_for_line`), not a new
/// matcher. Anything else is treated as uncertain → orphaned.
pub(crate) fn line_matches_snippet(content: &str, line: u32, snippet: &str) -> bool {
    if line == 0 {
        return false;
    }
    let Some(text) = content.lines().nth((line - 1) as usize) else {
        return false;
    };
    let trimmed = text.trim();
    trimmed == snippet
        || (snippet.len() == kb_core::review::DEFAULT_CONTEXT_CHARS && trimmed.starts_with(snippet))
}

// --- GET /api/reviews/{id}/comments ---------------------------------------

#[derive(Debug, Deserialize)]
pub struct ReviewCommentsParams {
    #[serde(default)]
    pub ps: Option<String>,
    /// Include resolved threads. Default excludes them.
    #[serde(default)]
    pub all: bool,
}

/// Wire shape of a suggestion as this route surfaces it (read-only;
/// V4.C2 owns apply). `null` on the comment when no row exists.
#[derive(Debug, Serialize)]
struct SuggestionView {
    replacement: String,
    original: String,
    applied: bool,
    applied_at: Option<i64>,
}

/// `GET /api/reviews/{id}/comments?ps=latest|N&all=true` — review-scoped
/// annotations grouped by path, threads nested (parent + replies). Each
/// top-level comment carries a [`ResolvedForPs`] block for the TARGET
/// patchset (default latest) and a read-only suggestion block.
/// Resolution is computed lazily; blob reads are cached per `(path, sha)`
/// for the request.
pub async fn review_comments(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<ReviewCommentsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, repo_id) = require_review(&state, id).await?;
    // 2026-08-31 incident (store.rs module doc): ps-resolve + annotations
    // list + the per-comment suggestion lookups inside `build_comment_
    // groups` are all synchronous store work — one blocking-pool trip.
    let ps_param = params.ps.clone();
    let all = params.all;
    let repo_root = repo.path.clone();
    let (target_ps, groups_out) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let target_ps = resolve_ps(store, id, ps_param.as_deref())?;
            let rows = store.list_review_annotations(id, all)?;
            let ctx = crate::prose_refs::RefCtx {
                repo_id,
                review_id: Some(id),
            };
            let groups_out = build_comment_groups(store, &repo_root, &target_ps, rows, &ctx)?;
            Ok((target_ps, groups_out))
        })
        .await?;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "review_id": id,
            "repo": review.repo,
            "ps": target_ps.ps_number,
            "groups": groups_out,
        })),
    ))
}

/// Group already-fetched annotation rows (parents + replies, per
/// [`Store::list_review_annotations`]) into `{path, comments: [...]}`
/// blocks, each parent carrying its lazily-resolved position against
/// `target_ps` plus a read-only suggestion block and nested replies.
///
/// Shared by [`review_comments`] and (CT-E7) `crate::review_distill` —
/// the two callers stay identical by construction rather than by two
/// queries agreeing after the fact.
///
/// `ctx` (V76-B3, kbc-prose/1) scopes the additive `body_refs` each comment
/// and reply carries: the repo for path/symbol resolution, the review for
/// `f-<slug>` mentions.
pub(crate) fn build_comment_groups(
    store: &Store,
    repo_root: &Path,
    target_ps: &ReviewPatchsetRow,
    rows: Vec<AnnotationRow>,
    ctx: &crate::prose_refs::RefCtx,
) -> Result<Vec<serde_json::Value>, ApiError> {
    let mut replies: HashMap<String, Vec<AnnotationRow>> = HashMap::new();
    let mut parents: Vec<AnnotationRow> = Vec::new();
    for row in rows {
        match row.parent_id.clone() {
            Some(pid) => replies.entry(pid).or_default().push(row),
            None => parents.push(row),
        }
    }

    let mut cache: HashMap<(String, String), Option<String>> = HashMap::new();
    let mut groups: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();

    for parent in parents {
        let sha = target_sha_for_side(parent.side.as_deref(), target_ps).to_string();
        // PRR-R3 — a review-scoped, path-less "general question" has no
        // file to read at all (`path == ""`, always resolved regardless of
        // `content` — see `resolve_for_ps_with_content`'s own dispatch);
        // skip the git blob read entirely rather than spawn a lookup that
        // can only ever miss.
        let resolution = if parent.anchor_kind == annotations::ANCHOR_KIND_REVIEW {
            resolve_for_ps_with_content(&parent, target_ps, &sha, None)
        } else {
            let key = (parent.path.clone(), sha.clone());
            if !cache.contains_key(&key) {
                let text = read_blob_text(repo_root, &parent.path, &sha);
                cache.insert(key.clone(), text);
            }
            let content = cache.get(&key).and_then(|c| c.as_deref());
            resolve_for_ps_with_content(&parent, target_ps, &sha, content)
        };

        let suggestion = store
            .get_annotation_suggestion(&parent.id)?
            .map(|s| SuggestionView {
                replacement: s.replacement,
                original: s.original,
                applied: s.applied,
                applied_at: s.applied_at,
            });

        let thread_replies: Vec<serde_json::Value> = replies
            .remove(&parent.id)
            .unwrap_or_default()
            .into_iter()
            .map(|r| reply_json(store, ctx, r))
            .collect::<Result<_, ApiError>>()?;

        groups
            .entry(parent.path.clone())
            .or_default()
            .push(serde_json::json!({
                "id": parent.id,
                "path": parent.path,
                "intent": parent.intent,
                "body": parent.body,
                "body_refs": crate::prose_refs::field_refs(store, ctx, &parent.body)?,
                "author": parent.author,
                "created_at": parent.created_at,
                "updated_at": parent.updated_at,
                "resolved": parent.resolved,
                "anchor_kind": parent.anchor_kind,
                "side": parent.side,
                "ps_number": parent.ps_number,
                "resolution": resolution,
                "suggestion": suggestion,
                "replies": thread_replies,
            }));
    }

    Ok(groups
        .into_iter()
        .map(|(path, comments)| serde_json::json!({ "path": path, "comments": comments }))
        .collect())
}

fn reply_json(
    store: &Store,
    ctx: &crate::prose_refs::RefCtx,
    row: AnnotationRow,
) -> Result<serde_json::Value, ApiError> {
    Ok(serde_json::json!({
        "id": row.id,
        "parent_id": row.parent_id,
        "path": row.path,
        "intent": row.intent,
        "body": row.body,
        "body_refs": crate::prose_refs::field_refs(store, ctx, &row.body)?,
        "author": row.author,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
        "resolved": row.resolved,
    }))
}
