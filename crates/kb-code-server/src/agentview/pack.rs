//! `GET /api/pack?repo=&paths=&budget=` + `kb-code pack` (W5.1) — a context
//! PACK for a caller-given set of files: summaries and pointers over an
//! unbounded dump, per file:
//!
//! 1. **map outline** — that file's own outline entry ([`super::map::
//!    build_map`] scoped to exactly `path`, `budget = usize::MAX` so it is
//!    never itself truncated — see that module's doc).
//! 2. **provenance summary** — the file-grade "why" (`provenance::why::
//!    file_why`'s top sessions by line coverage), truncated to
//!    [`PACK_PROVENANCE_LIMIT`].
//! 3. **open annotations** (V71-X1; recon `cli-agent-surface.md` open
//!    question 3 — "should `pack` finally populate `annotations`") — every
//!    OPEN, TOP-LEVEL, WORKING-TREE annotation (`review_id IS NULL`,
//!    excluded the same way `unified_inbox`'s own annotations lane
//!    excludes them — a review-scoped comment is the Review Room's own
//!    surface, `GET /api/reviews/{id}/comments`, and showing it again here
//!    would mix two different attention systems) anchored to one of the
//!    REQUESTED paths, resolved through the SAME [`crate::routes::
//!    annotation_view`] every other annotation-reading route uses (never a
//!    second resolver). Each row carries `first_line`/`line_count` — a
//!    `range` annotation's resolved `line`/`line_end`, or a single line for
//!    every other anchor kind (`symbol`/`line`/`diff`) — so an agent can
//!    cite a position without a second round trip.
//!
//! 4. **recent story entries** — `provenance::story::build_story`'s
//!    (whole-file, `symbol_range: None`) timeline, most-recent-first,
//!    truncated to [`PACK_STORY_LIMIT`].
//!
//! Sections 1, 2 and 4 are ALWAYS included in full — they're summaries,
//! cheap relative to a raw file dump — never gated by `budget`. Steps 3
//! and 5 ARE budget-rationed, against two carved-out shares of the SAME
//! approx-token `budget`:
//!
//! - **step 3's sub-budget** is a hard CEILING of
//!   [`ANNOTATIONS_BUDGET_FRACTION`] (15%) of `budget` — never more, so a
//!   pack full of chatty open threads can never crowd out the file content
//!   that is this route's main payload. An annotation is included WHOLE OR
//!   NOT AT ALL (never head-truncated the way file content is — a
//!   half-quoted review comment reads worse than an honestly omitted one),
//!   visited smallest-body-first (same bias [`fill_content_budget`] uses
//!   for files) so the ceiling favors including MORE short threads over
//!   one long one. Whatever of the 15% ceiling annotations do NOT spend
//!   rolls back into step 5's share — the reservation is a ceiling on
//!   annotations, never a tax on content.
//! - **step 5, FILE CONTENT**, gets `budget` minus whatever step 3 ACTUALLY
//!   used (not minus the ceiling) — every requested file's CURRENT
//!   working-tree text, visited smallest-file-first (so the budget favors
//!   including MORE whole files over one large one), greedily filled
//!   against the `chars/4` approximate token budget ([`super::
//!   approx_tokens`]): a file that fits whole is `Full`; the first file
//!   that doesn't is head-`Truncated` to whatever budget remains (then the
//!   budget is spent — every later file in visit order is `OmittedBudget`);
//!   a non-UTF8 file is `Binary` (never counted against the budget). The
//!   final `files` list preserves the CALLER's own path order — the
//!   smallest-first visit order only decides which files make the cut,
//!   never the response's display order.
//!
//! Every requested path is pre-validated against `store::Store::
//! list_files` BEFORE any section is built (a single cheap lookup) — an
//! unknown path 404s the WHOLE request rather than silently dropping it or
//! running expensive blame/story work over a path that was never indexed.
//!
//! # `?set=` (Phase E3)
//!
//! `?set=<id>` is an ALTERNATIVE to `?paths=` — `400` if BOTH or NEITHER is
//! given. It resolves to a `reading_sets` id's own ordered, deduped paths
//! (`store::Store::reading_set_spans`); the set must belong to the SAME
//! `?repo=` the request already resolved, else `404` (never a silent
//! cross-repo mix-up). A span carrying a line range or a `note` annotates
//! its matching [`PackFileOut`] via the OPTIONAL `lines`/`span_note`
//! fields, populated ONLY on the `set=` path — every `paths=` request's
//! JSON is byte-identical to before this existed (both fields are
//! `#[serde(skip_serializing_if = "Option::is_none")]`). That's why this
//! stays `pack/1` rather than bumping to `pack/2`: the addition is
//! observationally inert for every existing caller, the historical
//! definition of additive. [`PackOut::set_id`] is the same story — `None`
//! (omitted) unless the request came in via `set=`.

use super::{approx_tokens, map, read_working_tree_file};
use crate::config::RepoEntry;
use crate::provenance::{story, why};
use crate::routes::{annotation_view, find_repo, safe_rel_path, ApiError};
use crate::state::SharedState;
use crate::store::StoreBlocking;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub const SCHEMA: &str = "pack/1";
/// Default `?budget=` (approx tokens, content only) when omitted.
pub const DEFAULT_BUDGET: usize = 4_000;
/// Defensive ceiling on an operator-supplied `?budget=` — same precedent as
/// `map::MAX_BUDGET`.
pub const MAX_BUDGET: usize = 200_000;
/// How many of a file's file-grade "why" sessions the provenance summary
/// section keeps — a pointer, not the full `GET /api/why` payload.
const PACK_PROVENANCE_LIMIT: usize = 3;
/// How many of a file's story entries the recent-story section keeps,
/// most-recent-first. A CT-E2 attention-gap beat (a run of session-less
/// commits, already collapsed by `build_story` itself) counts as ONE entry
/// against this limit — which is exactly the point: a pre-kb-capture
/// history costs one slot, not all five.
const PACK_STORY_LIMIT: usize = 5;
/// V71-X1 — the open-annotations sub-budget's ceiling, as a fraction of the
/// request's overall `budget`. See the module doc's step 3 for the
/// "ceiling, not a tax" rationing rule.
const ANNOTATIONS_BUDGET_FRACTION: f64 = 0.15;

#[derive(Debug, Deserialize)]
pub struct PackParams {
    pub repo: String,
    /// Comma-separated repo-relative paths — axum's stock `Query`
    /// extractor has no built-in repeated-key-to-`Vec` support (that needs
    /// `serde_qs`, not used elsewhere in this crate), so this mirrors
    /// `search::grammar`'s own "one string, parsed here" convention rather
    /// than growing a new dependency for one param. Mutually exclusive with
    /// `set` — see the module doc.
    #[serde(default)]
    pub paths: Option<String>,
    /// Phase E3 — resolve to a `reading_sets` id's own ordered paths
    /// instead of a caller-given list. Mutually exclusive with `paths`.
    #[serde(default)]
    pub set: Option<String>,
    pub budget: Option<usize>,
}

/// Phase E3 — one resolved span's optional annotations, keyed by path (the
/// FIRST span for a given path wins when a set references it more than
/// once — see [`pack_route`]).
#[derive(Debug, Clone, Default)]
struct SpanMeta {
    lines: Option<PackSpanLines>,
    note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PackContent {
    Full { text: String },
    Truncated { text: String },
    Binary,
    OmittedBudget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PackSpanLines {
    pub start: u32,
    pub end: u32,
}

/// One open, working-tree annotation row in a [`PackFileOut`] (V71-X1) —
/// [`crate::routes::AnnotationView`]'s own fields, minus what a pack
/// reader doesn't need (`repo`/`path`, implied by which file this row
/// lives under; the raw `anchor` JSON, superseded here by `first_line`/
/// `line_count`), plus those two derived fields. See the module doc's
/// step 3 for what `first_line`/`line_count` mean for each anchor kind.
#[derive(Debug, Clone, Serialize)]
pub struct PackAnnotationOut {
    pub id: String,
    pub anchor_kind: String,
    pub intent: String,
    pub body: String,
    pub author: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// `true` when the anchored content has drifted since this annotation
    /// was created — same meaning as [`crate::routes::AnnotationView::
    /// stale`].
    pub stale: bool,
    /// 1-based. For a `range` annotation, the resolved START line; for
    /// every other anchor kind, the annotation's own single resolved line.
    pub first_line: u32,
    /// `1` for every anchor kind except `range`, where it is
    /// `line_end - first_line + 1`.
    pub line_count: u32,
    /// `symbol`-kind only — see [`crate::routes::AnnotationView::symbol`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackFileOut {
    pub path: String,
    pub outline: String,
    pub symbols: Vec<map::MapSymbolOut>,
    pub provenance: Vec<why::FileSessionOut>,
    pub annotations: Vec<PackAnnotationOut>,
    pub story: Vec<story::StoryEntry>,
    pub content: PackContent,
    /// Phase E3 — the matching `reading_sets` span's own line range, ONLY
    /// when this pack was resolved via `?set=` AND that span carried one.
    /// `None` (omitted) for every `?paths=` request — see the module doc's
    /// additive-versioning call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<PackSpanLines>,
    /// Phase E3 — the matching span's own `note` (e.g. a from-session
    /// commit subject), when set. Same additive posture as `lines`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackOut {
    pub schema: &'static str,
    pub repo: String,
    pub paths: Vec<String>,
    pub budget: usize,
    pub content_tokens_used: usize,
    /// V71-X1 — the ceiling step 3 (open annotations) could spend this
    /// request: `(budget as f64 * ANNOTATIONS_BUDGET_FRACTION).floor()`.
    /// Always present (not gated by whether any annotation exists) so a
    /// caller can see the ceiling even on a pack with nothing to show.
    pub annotations_budget: usize,
    /// V71-X1 — approx tokens ACTUALLY spent on included annotation
    /// bodies, always `<= annotations_budget`. `content_tokens_used`'s
    /// effective budget is `budget - annotations_tokens_used` (the
    /// unspent share of the ceiling rolls back to content — see the
    /// module doc's step 3).
    pub annotations_tokens_used: usize,
    /// `true` when at least one file's content was head-truncated or
    /// omitted entirely for budget reasons, OR at least one open
    /// annotation was dropped for exceeding its 15% ceiling — never set by
    /// a summary section (those are never rationed; see the module doc).
    pub truncated: bool,
    pub files: Vec<PackFileOut>,
    /// Phase E3 — the reading set this pack was resolved from, when
    /// requested via `?set=` (vs. `?paths=`). `None` (omitted) for the
    /// `paths=` form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_id: Option<String>,
}

/// `GET /api/pack?repo=&paths=&budget=` OR `GET /api/pack?repo=&set=&budget=`
/// — see the module doc's `?set=` section for the resolution + annotation
/// rules. `400` if both/neither of `paths`/`set` is given; `404` an unknown
/// `set` id, or one belonging to a DIFFERENT repo than `?repo=`.
pub async fn pack_route(
    State(state): State<SharedState>,
    Query(params): Query<PackParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;

    let (paths, span_meta, set_id) = match (&params.paths, &params.set) {
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "pass exactly one of paths= or set=, not both",
            ))
        }
        (None, None) => return Err(ApiError::bad_request("pass paths= or set=")),
        (Some(raw), None) => {
            let paths: Vec<String> = raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if paths.is_empty() {
                return Err(ApiError::bad_request("paths must not be empty"));
            }
            for p in &paths {
                safe_rel_path(p)?;
            }
            (paths, HashMap::new(), None)
        }
        (None, Some(set_id)) => {
            // One round trip covers the set lookup, the repo-scope check,
            // and (on success) the span read — all sequential store/CPU
            // work with no async in between (store.rs's 2026-08-31 incident
            // note).
            let set_id_c = set_id.clone();
            let repo_name = repo.name.clone();
            let (paths, meta) = state
                .store
                .run_blocking(move |store| -> Result<_, ApiError> {
                    let row = store
                        .get_reading_set(&set_id_c)?
                        .ok_or_else(|| ApiError::not_found(format!("set {set_id_c:?}")))?;
                    if row.repo_id != repo_id {
                        return Err(ApiError::not_found(format!(
                            "set {set_id_c:?}: not found in repo {repo_name:?}"
                        )));
                    }
                    let spans = store.reading_set_spans(&set_id_c)?;
                    let mut paths: Vec<String> = Vec::new();
                    let mut meta: HashMap<String, SpanMeta> = HashMap::new();
                    for span in spans {
                        if !paths.contains(&span.path) {
                            paths.push(span.path.clone());
                        }
                        // First span for a given path wins (see `SpanMeta`'s doc).
                        meta.entry(span.path.clone()).or_insert_with(|| SpanMeta {
                            lines: match (span.line_start, span.line_end) {
                                (Some(s), Some(e)) => Some(PackSpanLines {
                                    start: s as u32,
                                    end: e as u32,
                                }),
                                _ => None,
                            },
                            note: span.note.clone(),
                        });
                    }
                    if paths.is_empty() {
                        return Err(ApiError::bad_request(format!(
                            "set {set_id_c:?} has no spans"
                        )));
                    }
                    Ok((paths, meta))
                })
                .await?;
            (paths, meta, Some(set_id.clone()))
        }
    };

    // V70-A2 (the critique's MISSING #5) — the CONFIGURED denylist on the
    // one route that hands file CONTENT to an agent, which then writes it
    // into an indexed transcript. Refuses the whole pack rather than
    // silently dropping the offending file: a pack that quietly omits what
    // the caller asked for is a pack the caller reasons wrongly about.
    // (`read_working_tree_file` enforces the built-in floor regardless —
    // see `security::secrets`' two-level split.)
    for p in &paths {
        state.secret_policy.check(p)?;
    }

    let budget = params.budget.unwrap_or(DEFAULT_BUDGET).clamp(1, MAX_BUDGET);

    let mut out = build_pack(&state, repo, repo_id, &paths, budget).await?;
    for f in &mut out.files {
        if let Some(meta) = span_meta.get(&f.path) {
            f.lines = meta.lines;
            f.span_note = meta.note.clone();
        }
    }
    out.set_id = set_id;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

struct FileMeta {
    outline: String,
    symbols: Vec<map::MapSymbolOut>,
    provenance: Vec<why::FileSessionOut>,
    story: Vec<story::StoryEntry>,
}

/// `pub(crate)` so `kb-code pack`'s own tests (and any future direct
/// caller) can exercise this without an HTTP round trip.
pub(crate) async fn build_pack(
    state: &SharedState,
    repo: &RepoEntry,
    repo_id: i64,
    paths: &[String],
    budget: usize,
) -> Result<PackOut, ApiError> {
    // Pre-validate every path is actually indexed BEFORE any blame/story
    // work runs — see the module doc.
    let known: HashSet<String> = state
        .store
        .run_blocking(move |store| {
            store
                .list_files(repo_id)
                .map(|files| files.into_iter().map(|f| f.path).collect::<HashSet<_>>())
        })
        .await?;
    for path in paths {
        if !known.contains(path) {
            return Err(ApiError::not_found(format!(
                "{path}: not indexed in repo {:?}",
                repo.name
            )));
        }
    }

    // V71-X1 — step 3: open, working-tree annotations across every
    // requested path, resolved through the SAME `annotation_view` every
    // other annotation route uses. One blocking closure covers the store
    // read AND every row's resolution (which needs that path's current
    // working-tree text but no further async work) — same "sequential
    // store/CPU work stays in ONE closure" discipline `store.rs`'s
    // 2026-08-31 incident note established for this crate.
    let repo_c = repo.clone();
    let repo_name_c = repo.name.clone();
    let paths_c = paths.to_vec();
    let annotation_candidates: Vec<(String, PackAnnotationOut, usize)> = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let rows = store.list_open_annotations_on_paths(repo_id, &paths_c)?;
            let mut content_cache: HashMap<String, String> = HashMap::new();
            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                // A review-scoped comment is the Review Room's own surface
                // (`GET /api/reviews/{id}/comments`) — see the module
                // doc's step 3 for why this pack excludes it, matching
                // `unified_inbox`'s own annotations-lane exclusion.
                if row.review_id.is_some() {
                    continue;
                }
                let path = row.path.clone();
                let content = match content_cache.get(&path) {
                    Some(c) => c.clone(),
                    None => {
                        // Best-effort: a read failure (vanished file, the
                        // built-in secret floor) degrades to an empty
                        // string, same fallback `routes::list_open_
                        // annotations` already uses for its own per-path
                        // content cache — this only affects live
                        // re-resolution of `line`/`stale`, never whether
                        // the row itself is included.
                        let text = read_working_tree_file(&repo_c, &path)
                            .ok()
                            .and_then(|bytes| String::from_utf8(bytes).ok())
                            .unwrap_or_default();
                        content_cache.insert(path.clone(), text.clone());
                        text
                    }
                };
                let view = annotation_view(store, row, &repo_name_c, &content)?;
                let first_line = view.line;
                let line_count = view
                    .line_end
                    .map(|end| end.saturating_sub(first_line).saturating_add(1))
                    .unwrap_or(1);
                let tokens = approx_tokens(&view.body);
                let out_row = PackAnnotationOut {
                    id: view.id,
                    anchor_kind: view.anchor_kind,
                    intent: view.intent,
                    body: view.body,
                    author: view.author,
                    created_at: view.created_at,
                    updated_at: view.updated_at,
                    stale: view.stale,
                    first_line,
                    line_count,
                    symbol: view.symbol,
                };
                out.push((path, out_row, tokens));
            }
            Ok(out)
        })
        .await?;

    let annotations_budget = ((budget as f64) * ANNOTATIONS_BUDGET_FRACTION).floor() as usize;
    let (mut annotations_by_path, annotations_tokens_used, annotations_truncated) =
        fill_annotations_budget(annotation_candidates, annotations_budget);
    // The unspent share of the annotations ceiling rolls back to content —
    // see the module doc's step 3. `content_tokens_used`'s own budget is
    // never the raw `budget` minus the CEILING, only minus what step 3
    // actually spent.
    let content_budget = budget.saturating_sub(annotations_tokens_used);

    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut metas: Vec<FileMeta> = Vec::with_capacity(paths.len());
    for path in paths {
        // `build_map` is a store read + pure CPU ranking — its own round
        // trip per path, since `why::file_why`/`story::build_story` below
        // are real async work this closure must stay outside of.
        let repo_name = repo.name.clone();
        let path_c = path.clone();
        let map_out = state
            .store
            .run_blocking(move |store| {
                map::build_map(store, repo_id, &repo_name, &path_c, usize::MAX, now_ms)
            })
            .await?;
        let (outline, symbols) = match map_out.files.into_iter().next() {
            Some(f) => (map_out.outline, f.symbols),
            None => (String::new(), Vec::new()),
        };

        let why_out = why::file_why(state, repo, repo_id, path).await?;
        let mut provenance = why_out.sessions;
        provenance.truncate(PACK_PROVENANCE_LIMIT);

        let story_out = story::build_story(state, repo, repo_id, path, None).await?;
        let mut story_entries = story_out.entries;
        story_entries.reverse(); // most-recent-first
        story_entries.truncate(PACK_STORY_LIMIT);

        metas.push(FileMeta {
            outline,
            symbols,
            provenance,
            story: story_entries,
        });
    }

    let contents = fill_content_budget(repo, paths, content_budget);
    let content_tokens_used: usize = contents
        .iter()
        .map(|c| match c {
            PackContent::Full { text } | PackContent::Truncated { text } => approx_tokens(text),
            PackContent::Binary | PackContent::OmittedBudget => 0,
        })
        .sum();
    let truncated = annotations_truncated
        || contents.iter().any(|c| {
            matches!(
                c,
                PackContent::Truncated { .. } | PackContent::OmittedBudget
            )
        });

    let files = paths
        .iter()
        .zip(metas)
        .zip(contents)
        .map(|((path, meta), content)| PackFileOut {
            path: path.clone(),
            outline: meta.outline,
            symbols: meta.symbols,
            provenance: meta.provenance,
            annotations: annotations_by_path.remove(path).unwrap_or_default(),
            story: meta.story,
            content,
            // Populated afterward by `pack_route` when this pack was
            // resolved via `?set=` — `build_pack` itself has no set
            // context (see the module doc).
            lines: None,
            span_note: None,
        })
        .collect();

    Ok(PackOut {
        schema: SCHEMA,
        repo: repo.name.clone(),
        paths: paths.to_vec(),
        budget,
        content_tokens_used,
        annotations_budget,
        annotations_tokens_used,
        truncated,
        files,
        set_id: None,
    })
}

/// The open-annotations budget-fill pass — see the module doc's step 3.
/// `candidates` arrives in the store's own `path ASC, created_at ASC, id
/// ASC` order (so a path's rows land in creation order in the output);
/// visits smallest-body-first for the INCLUSION decision only (same bias
/// [`fill_content_budget`] uses), never partially — an annotation is
/// either fully included or not counted against the budget at all.
/// Returns the survivors grouped by path, tokens actually spent, and
/// whether any candidate was dropped for exceeding `budget`.
fn fill_annotations_budget(
    candidates: Vec<(String, PackAnnotationOut, usize)>,
    budget: usize,
) -> (HashMap<String, Vec<PackAnnotationOut>>, usize, bool) {
    let mut visit_order: Vec<usize> = (0..candidates.len()).collect();
    visit_order.sort_by_key(|&i| candidates[i].2);

    let mut included = vec![false; candidates.len()];
    let mut used = 0usize;
    let mut truncated = false;
    for i in visit_order {
        let tokens = candidates[i].2;
        if used + tokens <= budget {
            used += tokens;
            included[i] = true;
        } else {
            truncated = true;
        }
    }

    let mut by_path: HashMap<String, Vec<PackAnnotationOut>> = HashMap::new();
    for (i, (path, out_row, _tokens)) in candidates.into_iter().enumerate() {
        if included[i] {
            by_path.entry(path).or_default().push(out_row);
        }
    }
    (by_path, used, truncated)
}

/// The content-fill pass — see the module doc's step 5. Returns one
/// [`PackContent`] per `paths` entry, in `paths`' OWN order (the
/// smallest-first visit order is purely an internal fill strategy).
fn fill_content_budget(repo: &RepoEntry, paths: &[String], budget: usize) -> Vec<PackContent> {
    let mut order: Vec<usize> = (0..paths.len()).collect();
    let sizes: Vec<u64> = paths
        .iter()
        .map(|p| {
            std::fs::metadata(repo.path.join(p))
                .map(|m| m.len())
                .unwrap_or(u64::MAX)
        })
        .collect();
    order.sort_by_key(|&i| sizes[i]);

    let mut contents: Vec<Option<PackContent>> = vec![None; paths.len()];
    let mut remaining_tokens = budget;
    let mut budget_exhausted = false;
    for i in order {
        if budget_exhausted {
            contents[i] = Some(PackContent::OmittedBudget);
            continue;
        }
        let text = match read_working_tree_file(repo, &paths[i]) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(_) => {
                    contents[i] = Some(PackContent::Binary);
                    continue;
                }
            },
            // A path already validated against `list_files` but unreadable
            // NOW (a race with a concurrent delete) degrades to omitted —
            // never fails the whole pack over a single vanished file.
            Err(_) => {
                contents[i] = Some(PackContent::OmittedBudget);
                continue;
            }
        };
        let tokens = approx_tokens(&text);
        if tokens <= remaining_tokens {
            remaining_tokens -= tokens;
            contents[i] = Some(PackContent::Full { text });
        } else if remaining_tokens > 0 {
            let char_budget = remaining_tokens * 4;
            let truncated_text: String = text.chars().take(char_budget).collect();
            contents[i] = Some(PackContent::Truncated {
                text: truncated_text,
            });
            budget_exhausted = true;
        } else {
            contents[i] = Some(PackContent::OmittedBudget);
            budget_exhausted = true;
        }
    }
    contents
        .into_iter()
        .map(|c| c.unwrap_or(PackContent::OmittedBudget))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ann(id: &str, body: &str) -> PackAnnotationOut {
        PackAnnotationOut {
            id: id.to_string(),
            anchor_kind: "line".to_string(),
            intent: "flag-for-agent".to_string(),
            body: body.to_string(),
            author: "you".to_string(),
            created_at: 0,
            updated_at: 0,
            stale: false,
            first_line: 1,
            line_count: 1,
            symbol: None,
        }
    }

    #[test]
    fn fill_annotations_budget_never_exceeds_its_ceiling() {
        // "aaaa" -> 1 token, "bbbbbbbb" (8 chars) -> 2 tokens.
        let candidates = vec![
            ("a.rs".to_string(), ann("1", "aaaa"), 1),
            ("b.rs".to_string(), ann("2", "bbbbbbbb"), 2),
        ];
        let (by_path, used, truncated) = fill_annotations_budget(candidates, 1);
        assert_eq!(
            used, 1,
            "only the 1-token candidate fits under a budget of 1"
        );
        assert!(truncated, "the 2-token candidate must have been dropped");
        assert_eq!(by_path.get("a.rs").map(|v| v.len()), Some(1));
        assert!(by_path.get("b.rs").is_none());
    }

    #[test]
    fn fill_annotations_budget_includes_whole_or_not_at_all_never_partial() {
        let candidates = vec![("a.rs".to_string(), ann("1", &"x".repeat(40)), 10)];
        // Budget of 5 can't fit a 10-token candidate at all — unlike file
        // content, an annotation is never head-truncated to fit.
        let (by_path, used, truncated) = fill_annotations_budget(candidates, 5);
        assert_eq!(used, 0);
        assert!(truncated);
        assert!(by_path.is_empty());
    }

    #[test]
    fn fill_annotations_budget_unspent_ceiling_reports_as_unused() {
        let candidates = vec![("a.rs".to_string(), ann("1", "aaaa"), 1)];
        let (by_path, used, truncated) = fill_annotations_budget(candidates, 100);
        assert_eq!(
            used, 1,
            "only what was actually spent, never the ceiling itself"
        );
        assert!(!truncated);
        assert_eq!(by_path.get("a.rs").map(|v| v.len()), Some(1));
    }

    #[test]
    fn fill_annotations_budget_groups_by_path_preserving_arrival_order() {
        // Arrival order mirrors the store's own `path ASC, created_at ASC`
        // — two rows on the SAME path must come out in that order.
        let candidates = vec![
            ("a.rs".to_string(), ann("1", "x"), 1),
            ("a.rs".to_string(), ann("2", "y"), 1),
        ];
        let (by_path, _used, _truncated) = fill_annotations_budget(candidates, 100);
        let rows = by_path.get("a.rs").expect("a.rs has rows");
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["1", "2"]
        );
    }

    #[test]
    fn fill_content_budget_visits_smallest_first_but_preserves_caller_order() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("big.txt"), "x".repeat(400)).unwrap();
        std::fs::write(tmp.path().join("small.txt"), "y".repeat(40)).unwrap();
        let repo = RepoEntry {
            name: "r".to_string(),
            path: tmp.path().to_path_buf(),
        };
        // Caller order: big first, small second. Budget only fits ONE of
        // the two whole (small.txt is 10 tokens, big.txt is 100 tokens).
        let paths = vec!["big.txt".to_string(), "small.txt".to_string()];
        let contents = fill_content_budget(&repo, &paths, 10);

        assert_eq!(contents.len(), 2);
        // Response order matches `paths` (big.txt first) even though
        // small.txt was visited FIRST internally (smallest-first fill).
        assert!(
            matches!(contents[1], PackContent::Full { .. }),
            "small.txt (paths[1]) must be the one that fit whole: {:?}",
            contents[1]
        );
        assert!(
            matches!(
                contents[0],
                PackContent::Truncated { .. } | PackContent::OmittedBudget
            ),
            "big.txt (paths[0]) must not have fit whole: {:?}",
            contents[0]
        );
    }

    #[test]
    fn fill_content_budget_marks_non_utf8_files_binary_without_spending_budget() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("bin.dat"), [0xff_u8, 0xfe, 0x00, 0xff]).unwrap();
        std::fs::write(tmp.path().join("text.txt"), "hello").unwrap();
        let repo = RepoEntry {
            name: "r".to_string(),
            path: tmp.path().to_path_buf(),
        };
        let paths = vec!["bin.dat".to_string(), "text.txt".to_string()];
        let contents = fill_content_budget(&repo, &paths, 1_000);
        assert_eq!(contents[0], PackContent::Binary);
        assert!(matches!(contents[1], PackContent::Full { .. }));
    }

    #[test]
    fn fill_content_budget_head_truncates_the_first_file_that_overflows() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "a".repeat(100)).unwrap();
        let repo = RepoEntry {
            name: "r".to_string(),
            path: tmp.path().to_path_buf(),
        };
        let paths = vec!["a.txt".to_string()];
        // 100 chars = 25 tokens; budget of 5 tokens = 20 chars head.
        let contents = fill_content_budget(&repo, &paths, 5);
        match &contents[0] {
            PackContent::Truncated { text } => assert_eq!(text.len(), 20),
            other => panic!("expected Truncated, got {other:?}"),
        }
    }
}
