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
//! 3. **open annotations** — ALWAYS EMPTY in this Wave. kb-code's own
//!    review/annotations sidecar (a sibling Wave-4 track building on
//!    `kb_core::review::Anchor`) had not landed on this branch as of Wave 5
//!    — see the crate's own Wave narrative. The field is still part of
//!    `pack/1`'s versioned shape (forward-compatible: wiring the real data
//!    in later changes only this section's population logic, never the
//!    schema).
//! 4. **recent story entries** — `provenance::story::build_story`'s
//!    (whole-file, `symbol_range: None`) timeline, most-recent-first,
//!    truncated to [`PACK_STORY_LIMIT`].
//!
//! Sections 1-4 are ALWAYS included in full — they're summaries, cheap
//! relative to a raw file dump — never gated by `budget`. Only step 5,
//! FILE CONTENT, is budget-rationed:
//!
//! 5. **content** — every requested file's CURRENT working-tree text,
//!    visited smallest-file-first (so the budget favors including MORE
//!    whole files over one large one), greedily filled against the
//!    `chars/4` approximate token `budget` ([`super::approx_tokens`]):
//!    a file that fits whole is `Full`; the first file that doesn't is
//!    head-`Truncated` to whatever budget remains (then the budget is
//!    spent — every later file in visit order is `OmittedBudget`); a
//!    non-UTF8 file is `Binary` (never counted against the budget). The
//!    final `files` list preserves the CALLER's own path order — the
//!    smallest-first visit order only decides which files make the cut,
//!    never the response's display order.
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
use crate::routes::{find_repo, safe_rel_path, ApiError};
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

#[derive(Debug, Clone, Serialize)]
pub struct PackFileOut {
    pub path: String,
    pub outline: String,
    pub symbols: Vec<map::MapSymbolOut>,
    pub provenance: Vec<why::FileSessionOut>,
    pub annotations: Vec<serde_json::Value>,
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
    /// `true` when at least one file's content was head-truncated or
    /// omitted entirely for budget reasons — never set by a summary
    /// section (those are never rationed; see the module doc).
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

    let contents = fill_content_budget(repo, paths, budget);
    let content_tokens_used: usize = contents
        .iter()
        .map(|c| match c {
            PackContent::Full { text } | PackContent::Truncated { text } => approx_tokens(text),
            PackContent::Binary | PackContent::OmittedBudget => 0,
        })
        .sum();
    let truncated = contents.iter().any(|c| {
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
            annotations: Vec::new(),
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
        truncated,
        files,
        set_id: None,
    })
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
