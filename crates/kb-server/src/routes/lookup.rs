//! `GET /api/kb/{kb}/lookup?q=<input>` — resolve a user-supplied
//! identifier (12-hex artifact id, source-relative path, or unique
//! filename/suffix) to a single artifact's metadata.
//!
//! Drives the `kb find` CLI subcommand and underlies the `--path` flag
//! on `kb comments {list,export,resolve,add}`. The point is to let
//! Claude Code (and humans) talk about artifacts by filename instead
//! of memorising 12-hex hashes.
//!
//! Resolution ladder, first hit wins:
//!   1. `q` looks like a 12-hex id AND `get_by_id` returns Some → Exact
//!   2. `q` treated as source-relative path AND `get_by_source_path`
//!      (against `source_root.join(q)`) returns Some → Exact
//!   3. F3b — moves-table fallback (old_id OR old_rel → new id chain;
//!      newest-wins). Resolved doc under the new id → Exact
//!   4. Filename / suffix match against `list_docs(200)`:
//!        - 1 hit  → UniqueSuffix
//!        - 2..=10 → Ambiguous { candidates }
//!        - 11+    → Ambiguous { candidates: first 10, truncated: true }
//!        - 0      → NotFound (always returns 200, callers branch on `kind`)
//!
//! No write surface; pure read. Inherits the api tree's bearer auth
//! and origin allowlist.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::paths::{doc_folder, doc_rel_path};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const LIST_CAP: u32 = 200;
const CANDIDATES_CAP: usize = 10;
const Q_MAX_LEN: usize = 256;

#[derive(Debug, Deserialize)]
pub struct LookupParams {
    pub q: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct LookupHit {
    pub id: String,
    /// Absolute on-disk path (what the indexer stored).
    pub path: String,
    /// Path relative to the kb's source root, forward-slash separated.
    /// Empty string is impossible for an indexed doc; defensive None
    /// path returns an empty string when the canonicalisation in
    /// `doc_rel_path` fails.
    pub source_relative: String,
    pub folder: String,
    pub title: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum LookupResult {
    Exact(LookupHit),
    UniqueSuffix(LookupHit),
    Ambiguous {
        candidates: Vec<LookupHit>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    NotFound,
}

pub async fn lookup(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(params): Query<LookupParams>,
) -> Response<Body> {
    let q = params.q.trim();
    if q.is_empty() {
        return (StatusCode::BAD_REQUEST, "q must be non-empty").into_response();
    }
    if q.len() > Q_MAX_LEN {
        return (StatusCode::BAD_REQUEST, "q too long").into_response();
    }

    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let source_root = ctx.source_path.clone();

    // 1. 12-hex id passthrough.
    if is_id_shape(q) {
        match ctx.storage.get_by_id(q.to_string()).await {
            Ok(Some(row)) => {
                return Json(LookupResult::Exact(hit_from_row(&row, &source_root))).into_response();
            }
            Ok(None) => { /* fall through */ }
            Err(e) => return error_to_problem_json(&e),
        }
    }

    // 2. Treat q as source-relative path; compose absolute and look up.
    //    The indexer stored absolute paths; the storage helper does
    //    `path = '<absolute>'` filtering. We try both raw and
    //    canonicalised forms so a kb mounted under a symlink still
    //    resolves.
    let q_norm = q.trim_start_matches('/').replace('\\', "/");
    let absolute = source_root.join(&q_norm);
    let abs_str = absolute.to_string_lossy().to_string();
    match ctx.storage.get_by_source_path(abs_str.clone()).await {
        Ok(Some(row)) => {
            return Json(LookupResult::Exact(hit_from_row(&row, &source_root))).into_response();
        }
        Ok(None) => { /* fall through */ }
        Err(e) => return error_to_problem_json(&e),
    }
    // Second try: canonicalised absolute (resolves symlinks).
    if let Ok(canon) = absolute.canonicalize() {
        let canon_str = canon.to_string_lossy().to_string();
        if canon_str != abs_str {
            match ctx.storage.get_by_source_path(canon_str).await {
                Ok(Some(row)) => {
                    return Json(LookupResult::Exact(hit_from_row(&row, &source_root)))
                        .into_response();
                }
                Ok(None) => { /* fall through */ }
                Err(e) => return error_to_problem_json(&e),
            }
        }
    }

    // 3. F3b — moves-table fallback (old id and/or old source-rel). Chain
    // resolution is newest-wins; return the live doc under the new id as
    // Exact so existing CLI consumers (`kb find`, --path resolvers) keep
    // working without learning a new kind.
    match kb_core::relocate::moves_lookup(&ctx.storage, q).await {
        Ok(Some((new_id, _new_rel))) => {
            match ctx.storage.get_by_id(new_id).await {
                Ok(Some(row)) => {
                    return Json(LookupResult::Exact(hit_from_row(&row, &source_root)))
                        .into_response();
                }
                Ok(None) => { /* fall through */ }
                Err(e) => return error_to_problem_json(&e),
            }
        }
        Ok(None) => { /* fall through */ }
        Err(e) => return error_to_problem_json(&e),
    }
    // Also try the normalised source-rel form when `q` was path-shaped
    // but the id-shape branch above already skipped moves.
    if !is_id_shape(q) && q_norm != q {
        match kb_core::relocate::moves_lookup(&ctx.storage, &q_norm).await {
            Ok(Some((new_id, _new_rel))) => {
                match ctx.storage.get_by_id(new_id).await {
                    Ok(Some(row)) => {
                        return Json(LookupResult::Exact(hit_from_row(&row, &source_root)))
                            .into_response();
                    }
                    Ok(None) => { /* fall through */ }
                    Err(e) => return error_to_problem_json(&e),
                }
            }
            Ok(None) => { /* fall through */ }
            Err(e) => return error_to_problem_json(&e),
        }
    }

    // 4. Suffix match against the full doc list. Cheap — 200-row cap.
    let rows = match ctx.storage.list_docs(LIST_CAP).await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };

    let mut candidates: Vec<LookupHit> = rows
        .iter()
        .filter(|row| path_matches_suffix(&row.path, &q_norm))
        .map(|row| hit_from_row(row, &source_root))
        .collect();

    let result = match candidates.len() {
        0 => LookupResult::NotFound,
        1 => LookupResult::UniqueSuffix(candidates.pop().expect("len==1")),
        n => {
            let truncated = n > CANDIDATES_CAP;
            candidates.truncate(CANDIDATES_CAP);
            LookupResult::Ambiguous {
                candidates,
                truncated,
            }
        }
    };
    Json(result).into_response()
}

fn is_id_shape(q: &str) -> bool {
    q.len() == 12
        && q.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Does `row_path` end with `q` such that `q` is either:
/// - the entire path, or
/// - everything from after a `/` to the end.
///
/// Avoids matching `subatlas.html` for `q=atlas.html`. If `q` itself
/// contains a `/`, must match a path-segment-aligned suffix.
fn path_matches_suffix(row_path: &str, q: &str) -> bool {
    if q.is_empty() {
        return false;
    }
    if row_path == q {
        return true;
    }
    // Allow either / or \ on the segment-boundary side so this works on
    // Windows paths if they ever appear (kb is Linux-only today; cheap
    // defense). The indexer normalises to `/` for stored paths via
    // `doc_rel_path`, but `row.path` is the absolute on-disk form.
    let needle = format!("/{q}");
    let alt = format!("\\{q}");
    row_path.ends_with(&needle) || row_path.ends_with(&alt)
}

fn hit_from_row(
    row: &kb_core::storage::lance::DocSummary,
    source_root: &std::path::Path,
) -> LookupHit {
    LookupHit {
        id: row.id.clone(),
        path: row.path.clone(),
        source_relative: doc_rel_path(&row.path, source_root),
        folder: doc_folder(&row.path, source_root),
        title: row.title.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_id_shape_accepts_12_lowercase_hex() {
        assert!(is_id_shape("abc123def456"));
        assert!(is_id_shape("000000000000"));
        assert!(is_id_shape("0a1b2c3d4e5f"));
    }

    #[test]
    fn is_id_shape_rejects_other_shapes() {
        assert!(!is_id_shape("abc123")); // too short
        assert!(!is_id_shape("abc123def4567")); // too long
        assert!(!is_id_shape("ABC123def456")); // uppercase
        assert!(!is_id_shape("atlas.html")); // has dot
        assert!(!is_id_shape("notes/foo")); // has slash
    }

    #[test]
    fn path_matches_suffix_basename() {
        assert!(path_matches_suffix("/x/y/atlas.html", "atlas.html"));
        assert!(path_matches_suffix("/x/atlas.html", "atlas.html"));
        assert!(!path_matches_suffix("/x/subatlas.html", "atlas.html"));
        assert!(!path_matches_suffix("/x/atlas.html.bak", "atlas.html"));
    }

    #[test]
    fn path_matches_suffix_multi_segment() {
        assert!(path_matches_suffix("/x/pm/01.html", "pm/01.html"));
        assert!(!path_matches_suffix("/x/notpm/01.html", "pm/01.html"));
        // Whole-path match
        assert!(path_matches_suffix("/x/pm/01.html", "/x/pm/01.html"));
    }

    #[test]
    fn path_matches_suffix_rejects_empty() {
        assert!(!path_matches_suffix("/x/y", ""));
    }
}
