//! Z4 — fleet-wide open-comments inbox. `GET /api/inbox`.
//!
//! Open SPA comments live per-artifact in `<state>/<kb>/.review/<id>.json`
//! files (kb-comments/1, invariant #6) scattered across every corpus, and a
//! reply sits silently until the operator stumbles on the artifact. This
//! route is the ONE place to see every OPEN comment across all kbs: it fans
//! out over `state.kbs`, walks each kb's `.review/` dir via the canonical
//! `review::load` (never parsing the JSON by hand), and returns each open
//! comment (the thread root) with a short excerpt, reply count, and
//! anchor/staleness, newest-updated first.
//!
//! Federated per corpus (invariant #28): the per-kb futures run through
//! `routes::buffered_join` (`state.fanout_cap`, PF-R1 — the operator-
//! configurable `[server] fanout_cap`, default 8, submission order); each
//! returns its partial and a corpus with a missing/broken `.review` dir
//! yields an empty Vec rather than `?`-propagating — one bad corpus never
//! 500s the fleet.

use crate::state::KbHandles;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Default page size — the newest N open comments. Matches the `kb comments
/// inbox` CLI default. Hard cap prevents an oversized `?limit=` blowing out
/// the response.
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 500;
/// How many chars of the comment body ride along as the thread-root excerpt.
const EXCERPT_CHARS: usize = 200;

/// One open comment in the fleet inbox — the thread root plus enough context
/// to triage it and deep-link to the artifact. `total_open` (on the response)
/// is the fleet-wide count BEFORE the `limit` truncation.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct InboxItem {
    pub kb: String,
    pub artifact_id: String,
    /// Source-relative path of the artifact (for the SPA `/a/{kb}/{rel}`
    /// deep-link). `None` when the artifact has left lance (the review file
    /// outlived it) — the row is then non-navigable.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
    /// Current title from lance, else the review file's own recorded title.
    pub title: String,
    pub comment_id: String,
    /// First [`EXCERPT_CHARS`] chars of the thread-root body.
    pub excerpt: String,
    /// `"you" | "claude"`.
    pub author: String,
    /// Number of replies on the thread.
    pub reply_count: u32,
    /// Anchor scope — `file | chapter | section | selection`.
    pub anchor: String,
    /// The anchor is currently flagged stale by the indexer sidecar.
    pub stale: bool,
    /// Comment creation time (unix secs).
    pub created_at: i64,
    /// Last-activity time (unix secs): the max of the comment's own
    /// created/edited time and every reply's created/edited time, so a fresh
    /// reply floats the thread to the top of the inbox.
    pub updated_at: i64,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct InboxResponse {
    pub items: Vec<InboxItem>,
    /// Fleet-wide count of open comments (the un-truncated total), so the SPA
    /// badge can show the true number even when `items` is capped at `limit`.
    pub total_open: u64,
}

#[derive(Debug, Deserialize, Default)]
pub struct InboxParams {
    /// Page cap, clamped to [`MAX_LIMIT`]. Absent → [`DEFAULT_LIMIT`].
    pub limit: Option<u32>,
    /// Restrict to a single corpus (its name). Absent → every configured kb.
    pub kb: Option<String>,
}

/// `GET /api/inbox?limit=&kb=` — every OPEN comment across the fleet, newest
/// activity first.
pub async fn list(
    State(state): State<Arc<KbHandles>>,
    Query(params): Query<InboxParams>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
    let want_kb = params
        .kb
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // Fan out per corpus (#28), submission order, partial-tolerant.
    let mut futs: Vec<super::CorpusFut<'_, Vec<InboxItem>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        if let Some(w) = want_kb {
            if kb_name.as_str() != w {
                continue;
            }
        }
        let review_dir = state.paths.kb_review_dir(kb_name);
        futs.push(Box::pin(async move {
            collect_open(kb_name.as_str(), ctx, &review_dir).await
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut items: Vec<InboxItem> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();

    let total_open = items.len() as u64;
    // Newest activity first; deterministic tiebreak so a same-second batch is
    // stable across requests (and so the test can pin ordering).
    items.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.kb.cmp(&b.kb))
            .then_with(|| a.artifact_id.cmp(&b.artifact_id))
            .then_with(|| a.comment_id.cmp(&b.comment_id))
    });
    items.truncate(limit);

    Json(InboxResponse { items, total_open })
}

/// Walk one corpus's `.review/` dir and collect its OPEN comments. Best-effort
/// per invariant #28: a missing/unreadable dir or a malformed review file is
/// skipped, never propagated — this future must NOT `?`-return an error.
///
/// Cost shape: the dir walk + per-file JSON parse + sidecar load are all
/// synchronous IO, so they run in ONE `spawn_blocking` per kb (off the async
/// worker). A review file with zero OPEN comments is dropped there and never
/// reaches lance — only artifacts that actually contribute an item are looked
/// up. Their live title/source-rel are then resolved in a SINGLE batched
/// `get_by_ids` query rather than one storage round-trip per file.
///
/// `pub(crate)`: this is THE open-comments collector — the resurface route
/// (`routes::resurface`) rides it too (aggregating rows per artifact) instead
/// of growing a second `.review/` walk (itm-resurface-comments-walk-perf).
pub(crate) async fn collect_open(
    kb: &str,
    ctx: &crate::state::KbContext,
    review_dir: &std::path::Path,
) -> Vec<InboxItem> {
    let kb_owned = kb.to_string();
    let review_dir = review_dir.to_path_buf();

    // Blocking IO off the tokio worker. Returns, per artifact carrying >=1 OPEN
    // comment, its id and the fully-built InboxItems with a placeholder title
    // (the review file's own recorded title) + `source_relative: None`; the
    // live title/source-rel are patched in below.
    let groups: Vec<(String, Vec<InboxItem>)> = tokio::task::spawn_blocking(move || {
        let stale_map = kb_core::anchors::load(&kb_core::anchors::sidecar_path(&review_dir));

        // Missing dir (fresh kb) or unreadable dir → no comments, no 500.
        let entries = match std::fs::read_dir(&review_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        // Collect + sort the artifact ids for a deterministic within-kb order
        // before the cross-kb sort (mirrors `comments::list_reviews`).
        let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            files.push((stem.to_string(), path));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));

        let mut groups: Vec<(String, Vec<InboxItem>)> = Vec::new();
        for (artifact_id, path) in files {
            // Malformed / non-kb-comments JSON (e.g. the sidecar itself, or a
            // partial write) → skip, never 500.
            let file = match kb_core::review::load(&path) {
                Ok(Some(f)) => f,
                _ => continue,
            };
            let mut items: Vec<InboxItem> = Vec::new();
            for c in &file.comments {
                if c.status != kb_core::review::CommentStatus::Open {
                    continue;
                }
                let created = c.created_at.timestamp();
                let mut updated =
                    created.max(c.edited_at.map(|t| t.timestamp()).unwrap_or(created));
                for r in &c.replies {
                    updated = updated.max(r.created_at.timestamp());
                    if let Some(edited) = r.edited_at {
                        updated = updated.max(edited.timestamp());
                    }
                }
                items.push(InboxItem {
                    kb: kb_owned.clone(),
                    artifact_id: artifact_id.clone(),
                    source_relative: None,
                    title: file.artifact.title.clone(),
                    comment_id: c.id.clone(),
                    excerpt: c.body.chars().take(EXCERPT_CHARS).collect(),
                    author: match c.author {
                        kb_core::review::Author::You => "you",
                        kb_core::review::Author::Claude => "claude",
                    }
                    .to_string(),
                    reply_count: c.replies.len() as u32,
                    anchor: c.anchor.scope_name().to_string(),
                    stale: stale_map.contains_key(&(artifact_id.clone(), c.id.clone())),
                    created_at: created,
                    updated_at: updated,
                });
            }
            // Zero OPEN comments → this file contributes nothing; skip the lance
            // lookup entirely (the old code paid an unconditional get_by_id here).
            if !items.is_empty() {
                groups.push((artifact_id, items));
            }
        }
        groups
    })
    .await
    .unwrap_or_default();

    if groups.is_empty() {
        return Vec::new();
    }

    // ONE batched lance query resolves the live title + source-rel for every
    // surviving (open-comment-bearing) artifact, replacing the old per-file
    // get_by_id round-trip. Missing rows keep the review-file fallback title
    // and a `None` source_relative (invariant #6 semantics preserved).
    let ids: Vec<String> = groups.iter().map(|(id, _)| id.clone()).collect();
    let resolved: std::collections::HashMap<String, kb_core::storage::lance::DocSummary> =
        match ctx.storage.get_by_ids(ids).await {
            Ok(docs) => docs.into_iter().map(|d| (d.id.clone(), d)).collect(),
            Err(_) => std::collections::HashMap::new(),
        };

    let mut out: Vec<InboxItem> = Vec::new();
    for (artifact_id, mut items) in groups {
        if let Some(doc) = resolved.get(&artifact_id) {
            let rel = kb_core::paths::doc_rel_path(&doc.path, &ctx.source_path);
            for it in &mut items {
                it.title = doc.title.clone();
                it.source_relative = Some(rel.clone());
            }
        }
        out.extend(items);
    }
    out
}
