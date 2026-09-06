//! `GET /api/similar?repo=&path=&start=&end=[&limit=]` + `kb-code similar`
//! (W5.2) — nearest semantic-lane chunks to a given span, EXCLUDING
//! whatever the queried span itself already indexes as. Same "400 with a
//! hint" convention as `GET /api/search/semantic`
//! (`routes::search_semantic`) when the semantic lane isn't enabled
//! (daemon-wide, or for this specific repo) — see that route's doc.
//!
//! The span's own text is embedded WITHOUT `semantic::chunk::QUERY_PREFIX`
//! (`semantic::search::embed_raw`, not `embed_query`): a code span compared
//! against indexed CODE chunks is a content-to-content comparison, not the
//! query-to-code asymmetry `QUERY_PREFIX` exists for (see that constant's
//! own doc — it explicitly instructs QUERY text only, never indexed
//! content). Embedding it the SAME way the indexer embeds every other
//! chunk (`semantic::indexer`'s `embed_batch` call, also unprefixed) keeps
//! both sides of the nearest-neighbour comparison in one convention.
//!
//! # Excluding the source span
//!
//! A freshly-embedded span, if it happens to already be indexed as (part
//! of) a chunk, would otherwise trivially rank itself first. A hit is
//! excluded when it shares the queried `(repo, path)` AND its line range
//! overlaps `[start, end]` — chunk boundaries rarely align exactly with an
//! arbitrary caller-given span, so an OVERLAP check (not exact equality)
//! is what actually keeps "the same code, chunked slightly differently"
//! out of the results.

use crate::routes::{find_repo, safe_rel_path, ApiError};
use crate::semantic;
use crate::state::SharedState;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "similar/1";

#[derive(Debug, Deserialize)]
pub struct SimilarParams {
    pub repo: String,
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SimilarOut {
    pub schema: &'static str,
    pub repo: String,
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub hits: Vec<semantic::store::ChunkHit>,
}

/// `GET /api/similar?repo=&path=&start=&end=[&limit=]`.
pub async fn similar_route(
    State(state): State<SharedState>,
    Query(params): Query<SimilarParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();
    if params.start < 1 || params.end < params.start {
        return Err(ApiError::bad_request(
            "start must be >= 1 and end must be >= start",
        ));
    }
    if !state.semantic.repo_enabled(&params.repo) {
        return Err(ApiError::bad_request(format!(
            "semantic search is not enabled for repo {:?} — add it to [semantic] repos \
             (with [semantic] enabled = true) in kb-code.toml to opt in",
            params.repo
        )));
    }
    let (Some(chunk_store), Some(embedder)) = (
        state.semantic_chunk_store.clone(),
        state.semantic_embedder.clone(),
    ) else {
        return Err(ApiError::bad_request(
            "semantic search is disabled for this daemon",
        ));
    };

    let bytes = super::read_working_tree_file(repo, &path)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| ApiError::bad_request(format!("{path}: not valid UTF-8")))?;
    let span_text = extract_lines(&text, params.start, params.end)?;
    if span_text.trim().is_empty() {
        return Err(ApiError::bad_request("the requested span is empty"));
    }

    let limit = params
        .limit
        .unwrap_or(semantic::search::DEFAULT_LIMIT)
        .clamp(1, semantic::search::MAX_LIMIT);

    let query_vec = semantic::search::embed_raw(&embedder, &span_text).await?;
    let hits =
        semantic::search::search(&chunk_store, &query_vec, Some(&params.repo), limit).await?;

    let hits: Vec<_> = hits
        .into_iter()
        .filter(|h| {
            !(h.repo == params.repo
                && h.path == path
                && ranges_overlap(h.span_start, h.span_end, params.start, params.end))
        })
        .take(limit as usize)
        .collect();

    let out = SimilarOut {
        schema: SCHEMA,
        repo: repo.name.clone(),
        path,
        start: params.start,
        end: params.end,
        hits,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

fn ranges_overlap(a_start: u32, a_end: u32, b_start: u32, b_end: u32) -> bool {
    a_start <= b_end && b_start <= a_end
}

/// 1-based inclusive line slice of `text` — `end` is clamped to the file's
/// own line count (a caller-given `end` past EOF is not an error, unlike
/// `start` past EOF, which is: there is no span to embed at all).
fn extract_lines(text: &str, start: u32, end: u32) -> Result<String, ApiError> {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len() as u32;
    if start > total {
        return Err(ApiError::bad_request(format!(
            "start {start} is past the file's {total} line(s)"
        )));
    }
    let end = end.min(total);
    Ok(lines[(start - 1) as usize..end as usize].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_overlap_detects_any_shared_line() {
        assert!(ranges_overlap(1, 5, 5, 10));
        assert!(ranges_overlap(5, 10, 1, 5));
        assert!(ranges_overlap(1, 10, 3, 4));
        assert!(!ranges_overlap(1, 5, 6, 10));
        assert!(!ranges_overlap(6, 10, 1, 5));
    }

    #[test]
    fn extract_lines_slices_1_based_inclusive() {
        let text = "a\nb\nc\nd\n";
        assert_eq!(extract_lines(text, 2, 3).unwrap(), "b\nc");
        assert_eq!(extract_lines(text, 1, 4).unwrap(), "a\nb\nc\nd");
    }

    #[test]
    fn extract_lines_clamps_end_past_eof_but_rejects_start_past_eof() {
        let text = "a\nb\n";
        assert_eq!(extract_lines(text, 1, 100).unwrap(), "a\nb");
        assert!(extract_lines(text, 100, 200).is_err());
    }
}
