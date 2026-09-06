//! PRR-F ("The PR Room," kb v0.39 T2 frontier unit, design-ui.md §12.2
//! "Reviewer X-ray") — `GET /api/reviews/{id}/impact?path=`: per-changed-
//! file blast-radius chips ("N callers · M in this diff").
//!
//! # Why this is a NEW small aggregate, not a loop over `/api/impact/analysis`
//!
//! `crate::impact_analysis::impact_analysis_at` (V3.1-H2) answers "what is
//! the blast radius of ONE symbol" — full transitive BFS (depth ≤ 3) +
//! type-implementor edges + reverse imports + blame/session provenance.
//! That is the right shape for a symbol-focused inspector panel; it is the
//! WRONG shape for "lazily fetch a chip when a reviewer expands a diff
//! file header" (design-ui.md §12.2's own feasibility note): a file can
//! carry many changed symbols, and none of the transitive/provenance work
//! is needed for a one-line chip. This module instead composes the SAME
//! underlying primitive impact_analysis_at itself calls for its direct
//! buckets — [`hierarchy::callers_at`] — directly, once per CHANGED,
//! CALLABLE symbol in the requested file, and reports only two counts:
//! callers whose call site lives in a path that is ALSO part of this
//! review's change set ("in this diff" — the reviewer's own blind spot,
//! per the design doc) vs. everywhere else.
//!
//! # Scope, honestly
//!
//! Two limits are load-bearing, not oversights, and both are surfaced on
//! the wire rather than silently degrading to zero:
//!
//! - **Which symbols count as "changed".** A symbol is in scope iff its
//!   declaration span `[line_start, line_end]` intersects at least one
//!   `+` line of the file's own diff (parsed from [`crate::diff::diff_file`]
//!   hunk headers — [`changed_new_lines`]). This is the SAME "no textual
//!   match ⇒ no claim" posture the rest of this crate's diff-anchored
//!   surfaces use (`review_comments`/`review_findings`): a symbol whose
//!   body didn't textually change never shows up, even if its containing
//!   file did.
//! - **Which languages have a caller graph at all.** Call-site extraction
//!   ([`hierarchy::supports_hierarchy`]) covers exactly FOUR proof
//!   languages (rust / typescript / tsx / python) — design-ui.md §12.2's
//!   own feasibility note: "4 langs today; Ruby needs T1 SCIP." A file in
//!   any other language degrades to `lang_supported: false` with an empty
//!   symbol list and a named `note` — never a fabricated `0 callers`
//!   (`UNSUPPORTED_LANG_NOTE`).
//!
//! Only EXACT/LIKELY caller sites are counted (the SAME cap `impact_
//! analysis_at`'s own `direct_exact`/`direct_likely` buckets apply) —
//! `candidate`-class sites are the fuzzy repo-wide name-match tail; folding
//! them into "blast radius" would inflate an inherently untrustworthy
//! count, the opposite of this crate's "a wrong exact is a release
//! blocker" law.

use crate::hierarchy::{self, is_callable_kind_pub};
use crate::resolve::{CLASS_EXACT, CLASS_LIKELY};
use crate::reviews::{files_changed, require_review, resolve_ps};
use crate::routes::{read_repo_file, safe_rel_path, ApiError};
use crate::state::SharedState;
use crate::store::StoreBlocking;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::collections::HashSet;

pub const SCHEMA: &str = "review-impact-file/1";

/// Attached to every SUPPORTED-language response — names the EXACT/LIKELY-
/// only counting rule (see the module doc's last paragraph) so a consumer
/// never mistakes "callers_total" for an unfiltered repo-wide grep count.
pub const SUPPORTED_LANG_NOTE: &str = "callers_total/callers_in_diff/callers_out_of_diff count \
     EXACT and LIKELY call sites only (hierarchy::callers_at's own trust classes) — CANDIDATE \
     (fuzzy repo-wide name-match) sites are excluded, the same cap /api/impact/analysis's own \
     direct_exact/direct_likely buckets apply.";

/// Bound on how many changed, callable symbols get a caller-graph lookup
/// per request — one [`hierarchy::callers_at`] call apiece, each itself
/// bounded by [`hierarchy::CALLERS_GROUP_CAP`]. A file with more changed
/// callables than this reports the first `MAX_CHANGED_SYMBOLS` (declaration
/// order, i.e. `line_start` ascending) and sets `symbols_truncated: true` —
/// never silently drops the excess without saying so.
pub const MAX_CHANGED_SYMBOLS: usize = 20;

pub const UNSUPPORTED_LANG_NOTE: &str = "caller-graph extraction (kb-code's hierarchy/1) covers \
     rust, typescript, tsx, and python only (hierarchy::supports_hierarchy) — every other \
     language, or a file kb-code can't detect a language for at all, degrades to \
     lang_supported: false with no chips, never a fabricated zero.";

#[derive(Debug, Deserialize)]
pub struct ReviewImpactParams {
    pub path: String,
}

/// One changed, callable symbol's caller counts within THIS review's
/// change set. `callers_total = callers_in_diff + callers_out_of_diff`
/// (both EXACT/LIKELY only — see the module doc).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedSymbolImpact {
    pub name: String,
    pub kind: String,
    pub line: u32,
    pub col: u32,
    pub callers_total: usize,
    pub callers_in_diff: usize,
    pub callers_out_of_diff: usize,
    /// `true` when [`hierarchy::callers_at`] itself truncated this
    /// symbol's caller groups at [`hierarchy::CALLERS_GROUP_CAP`].
    pub truncated: bool,
}

/// The new-file line numbers a hunk's `+` lines land on — a minimal,
/// self-contained unified-diff hunk-header parser (this module's own; see
/// [`crate::diff`]'s own doc for why this crate keeps one parser per
/// consumer rather than a single shared hunk type). Only `+` lines are
/// counted: a `-` line has no new-side line number, and a context (` `)
/// line by definition didn't change. Malformed/unparseable hunk headers are
/// skipped (the line count simply doesn't advance for that hunk) rather
/// than panicking or guessing — same "honest miss over a guessed line"
/// posture `review_github_threads::last_diff_hunk_line` documents for the
/// sibling GitHub-hunk parser.
pub(crate) fn changed_new_lines(diff_text: &str) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    let mut new_line: u32 = 0;
    let mut in_hunk = false;
    for line in diff_text.lines() {
        if let Some(rest) = line.strip_prefix("@@ ") {
            match parse_hunk_new_start(rest) {
                Some(start) => {
                    new_line = start;
                    in_hunk = true;
                }
                None => in_hunk = false,
            }
            continue;
        }
        if !in_hunk {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => {
                out.insert(new_line);
                new_line = new_line.saturating_add(1);
            }
            Some(b'-') => { /* old-side only — no new-line number */ }
            Some(b' ') => new_line = new_line.saturating_add(1),
            _ => {
                /* blank context line ("" after trimming the newline) or an
                unrecognised marker — treat as context, same as a
                space-prefixed line, rather than losing hunk sync. */
                new_line = new_line.saturating_add(1);
            }
        }
    }
    out
}

/// Parse the new-side start out of a hunk header's REMAINDER (everything
/// after the leading `"@@ "` this fn's only caller already stripped), e.g.
/// `"-12,7 +15,9 @@ fn foo() {"` → `Some(15)`. `None` on anything that
/// doesn't contain a well-formed `+<digits>` run — an honest "not a hunk
/// header after all" rather than a guessed line 0.
fn parse_hunk_new_start(rest: &str) -> Option<u32> {
    let plus_at = rest.find('+')?;
    let after = &rest[plus_at + 1..];
    let end = after.find([',', ' ']).unwrap_or(after.len());
    after[..end].parse::<u32>().ok()
}

/// `GET /api/reviews/{id}/impact?path=` (design-ui.md §12.2). Bearer, same
/// ordinary review-read gate as `/map`/`/reading-order`/`/findings`. `400`
/// when `path` isn't part of the review's LATEST patchset change set (same
/// "no target to resolve against" rule the rest of the review-read surface
/// enforces).
pub async fn review_impact_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<ReviewImpactParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, repo_id) = require_review(&state, id).await?;
    let target_ps = state
        .store
        .run_blocking(move |store| resolve_ps(store, id, None))
        .await?;
    let path = safe_rel_path(&params.path)?.to_string();

    let root = repo.path.clone();
    let base = target_ps.base_sha.clone();
    let tip = target_ps.tip_sha.clone();
    let tip_for_task = tip.clone();
    let path_for_task = path.clone();
    let (files, diff_text) = tokio::task::spawn_blocking(move || {
        let files = files_changed(&root, &base, &tip_for_task)?;
        // V70-A2 (SEC-17) — two patchset shas this daemon resolved itself,
        // never caller text, so `Revspec::trusted` is the honest
        // constructor for the type `diff_file` now takes.
        let diff_text = crate::diff::diff_file(
            &root,
            &crate::git::Revspec::trusted(base.clone()),
            Some(&crate::git::Revspec::trusted(tip_for_task.clone())),
            &path_for_task,
        )?;
        Ok::<_, ApiError>((files, diff_text))
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    let changed_paths: HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    if !changed_paths.contains(path.as_str()) {
        return Err(ApiError::bad_request(format!(
            "path {path:?} is not part of review {id}'s latest patchset change set"
        )));
    }

    let read = read_repo_file(repo, &path, Some(tip.as_str()))?;
    let lang = crate::lang::detect(&path, Some(&read.bytes));
    let Some(lang) = lang.filter(|l| hierarchy::supports_hierarchy(l.id)) else {
        return Ok((
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "schema": SCHEMA,
                "review_id": id,
                "repo": review.repo,
                "ps_number": target_ps.ps_number,
                "path": path,
                "lang_supported": false,
                "changed_symbols": [],
                "callers_total": 0,
                "callers_in_diff": 0,
                "callers_out_of_diff": 0,
                "symbols_truncated": false,
                "note": UNSUPPORTED_LANG_NOTE,
            })),
        ));
    };

    let changed_lines = changed_new_lines(&diff_text);
    // 2026-08-31 incident (store.rs module doc): the whole
    // symbols_for_blob + per-symbol callers_at fan-out is pure store work
    // (plus CPU-only filtering/sorting/aggregation between calls) — one
    // blocking-pool trip instead of up to MAX_CHANGED_SYMBOLS+1 round trips.
    let repo_owned = repo.clone();
    let path_c = path.clone();
    let tip_c = tip.clone();
    let blob_hash = read.blob_hash.clone();
    let salt = lang.symbol_salt;
    let changed_paths_owned: HashSet<String> =
        changed_paths.iter().map(|s| (*s).to_string()).collect();
    let (changed_symbols, callers_total, callers_in_diff, callers_out_of_diff, symbols_truncated) =
        state
            .store
            .run_blocking(move |store| -> Result<_, ApiError> {
                let all_symbols = store.symbols_for_blob(&blob_hash, salt)?;
                let mut candidates: Vec<_> = all_symbols
                    .into_iter()
                    .filter(|s| is_callable_kind_pub(&s.kind))
                    .filter(|s| {
                        changed_lines
                            .range(s.line_start..=s.line_end.max(s.line_start))
                            .next()
                            .is_some()
                    })
                    .collect();
                candidates.sort_by(|a, b| {
                    a.line_start
                        .cmp(&b.line_start)
                        .then(a.col_start.cmp(&b.col_start))
                });
                let symbols_truncated = candidates.len() > MAX_CHANGED_SYMBOLS;
                candidates.truncate(MAX_CHANGED_SYMBOLS);

                let mut changed_symbols = Vec::with_capacity(candidates.len());
                let (mut callers_total, mut callers_in_diff, mut callers_out_of_diff) =
                    (0usize, 0usize, 0usize);
                // PF-K1 — ONE blob-read cache shared across every
                // `callers_at` call below (up to `MAX_CHANGED_SYMBOLS`, 20,
                // per request): `is_dynamic_call`'s per-call-site git blob
                // read is otherwise uncached both within and across these
                // calls (see `hierarchy::BlobReadCache`'s own doc).
                let mut blob_cache = hierarchy::BlobReadCache::new();
                for sym in &candidates {
                    let callers = hierarchy::callers_at_with_cache(
                        store,
                        &repo_owned,
                        repo_id,
                        &path_c,
                        sym.line_start,
                        sym.col_start,
                        Some(tip_c.as_str()),
                        &mut blob_cache,
                    )?;
                    let (mut total, mut in_diff, mut out_diff) = (0usize, 0usize, 0usize);
                    for g in &callers.callers {
                        let g_in_diff = changed_paths_owned.contains(&g.path);
                        for site in &g.sites {
                            if site.class != CLASS_EXACT && site.class != CLASS_LIKELY {
                                continue;
                            }
                            total += 1;
                            if g_in_diff {
                                in_diff += 1;
                            } else {
                                out_diff += 1;
                            }
                        }
                    }
                    callers_total += total;
                    callers_in_diff += in_diff;
                    callers_out_of_diff += out_diff;
                    changed_symbols.push(ChangedSymbolImpact {
                        name: sym.name.clone(),
                        kind: sym.kind.clone(),
                        line: sym.line_start,
                        col: sym.col_start,
                        callers_total: total,
                        callers_in_diff: in_diff,
                        callers_out_of_diff: out_diff,
                        truncated: callers.truncated,
                    });
                }
                Ok((
                    changed_symbols,
                    callers_total,
                    callers_in_diff,
                    callers_out_of_diff,
                    symbols_truncated,
                ))
            })
            .await?;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "review_id": id,
            "repo": review.repo,
            "ps_number": target_ps.ps_number,
            "path": path,
            "lang_supported": true,
            "changed_symbols": changed_symbols,
            "callers_total": callers_total,
            "callers_in_diff": callers_in_diff,
            "callers_out_of_diff": callers_out_of_diff,
            "symbols_truncated": symbols_truncated,
            "note": SUPPORTED_LANG_NOTE,
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_new_lines_counts_only_plus_lines_on_the_new_side() {
        let diff = "diff --git a/f.ts b/f.ts\n\
index 111..222 100644\n\
--- a/f.ts\n\
+++ b/f.ts\n\
@@ -1,3 +1,4 @@\n\
 context\n\
-removed\n\
+added one\n\
+added two\n\
 trailing context\n";
        let lines = changed_new_lines(diff);
        // new-side layout: 1=context,2=added one,3=added two,4=trailing context
        assert_eq!(lines, BTreeSet::from([2, 3]));
    }

    #[test]
    fn changed_new_lines_handles_multiple_hunks_independently() {
        let diff = "@@ -1,1 +1,2 @@\n\
 ctx\n\
+first hunk add\n\
@@ -10,1 +11,2 @@\n\
 ctx\n\
+second hunk add\n";
        let lines = changed_new_lines(diff);
        assert_eq!(lines, BTreeSet::from([2, 12]));
    }

    #[test]
    fn changed_new_lines_is_empty_for_a_textless_diff() {
        assert!(changed_new_lines("").is_empty());
        assert!(changed_new_lines("Binary files a/x and b/x differ\n").is_empty());
    }

    #[test]
    fn parse_hunk_new_start_reads_the_plus_side() {
        assert_eq!(parse_hunk_new_start("-12,7 +15,9 @@"), Some(15));
        assert_eq!(parse_hunk_new_start("-0,0 +1,5 @@ fn foo() {"), Some(1));
        assert_eq!(parse_hunk_new_start("not a hunk header"), None);
    }
}
