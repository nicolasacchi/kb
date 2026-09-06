//! W2.3 — semantic search: embed the query (with the asymmetric
//! [`super::QUERY_PREFIX`]), lance nearest-neighbour over-fetch, max-pool
//! per `(repo, path)` (`store::ChunkStore::search`) — the logic behind
//! `GET /api/search/semantic` (`routes::search_semantic`).
//!
//! Split into two steps: [`embed_query`] is an async fn that internally
//! wraps the genuinely BLOCKING embed call (the `kb_core::embed_ipc`
//! stdin/stdout round-trip — see `Embedder::embed_one`'s doc) in
//! `tokio::task::spawn_blocking` itself — mirrors kb-server's own
//! `embed_cache::embed_query` convention exactly (a blocking Mutex-held
//! call on a tokio worker thread starves whatever else that thread was
//! about to run, so the wrap lives INSIDE the helper, not left to every
//! caller to remember); [`search`] is a plain async fn over the
//! (already-embedded) query vector, awaiting the lance query directly.
//! `routes::search_semantic` composes the two with two `.await`s, no
//! caller-side `spawn_blocking` of its own.
//!
//! Deliberately NO reranker and NO RRF fusion — see the module-level
//! (`semantic/mod.rs`) doc; both are out of this step's scope.

use super::chunk::QUERY_PREFIX;
use super::store::{ChunkHit, ChunkStore, ChunkStoreError};
use kb_core::embed::Embedder;
use std::sync::{Arc, Mutex};

pub const DEFAULT_LIMIT: u32 = 10;
pub const MAX_LIMIT: u32 = 50;

/// Over-fetch multiplier for the lance nearest-neighbour scan — see
/// `store::ChunkStore::search`'s doc: enough slack that max-pooling per
/// `(repo, path)` still has several distinct candidates to choose from
/// before truncating to the caller's `limit`, without over-fetching so much
/// that a big repo's chunk count dominates the scan cost.
pub const OVER_FETCH_MULTIPLIER: u32 = 4;

#[derive(Debug, thiserror::Error)]
pub enum SemanticSearchError {
    #[error("q must not be empty")]
    EmptyQuery,
    #[error(transparent)]
    Store(#[from] ChunkStoreError),
    #[error("embed query: {0}")]
    Embed(String),
}

pub type Result<T> = std::result::Result<T, SemanticSearchError>;

/// Normalize + validate `q`, clamp `limit` — shared by callers that need
/// the same "is this query even worth embedding" gate before paying for the
/// (blocking) embed call. Returns the trimmed query and clamped limit.
pub fn validate_query(q: &str, limit: Option<u32>) -> Result<(String, u32)> {
    let trimmed = q.trim();
    if trimmed.is_empty() {
        return Err(SemanticSearchError::EmptyQuery);
    }
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    Ok((trimmed.to_string(), limit))
}

/// Embed `text` VERBATIM (no prefix) via the shared per-daemon embedder.
/// Internally runs the blocking IPC round-trip on `spawn_blocking` (see the
/// module doc) — safe to `.await` directly from any async context. The
/// primitive both [`embed_query`] (prefixed) and `agentview::similar`
/// (unprefixed — a code span compared against indexed CODE chunks is a
/// content-to-content comparison, not the query-to-code asymmetry
/// [`QUERY_PREFIX`] exists for) build on.
pub async fn embed_raw(embedder: &Arc<Mutex<Embedder>>, text: &str) -> Result<Vec<f32>> {
    let embedder = embedder.clone();
    let owned = text.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let mut guard = embedder.lock().unwrap_or_else(|e| e.into_inner());
        guard.embed_one(&owned)
    })
    .await;
    match result {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(SemanticSearchError::Embed(e.to_string())),
        Err(e) => Err(SemanticSearchError::Embed(format!(
            "embed task panicked: {e}"
        ))),
    }
}

/// Embed `q` with the asymmetric [`QUERY_PREFIX`] via [`embed_raw`]. `q`
/// should already be validated (non-empty) via [`validate_query`].
pub async fn embed_query(embedder: &Arc<Mutex<Embedder>>, q: &str) -> Result<Vec<f32>> {
    let prefixed = format!("{QUERY_PREFIX}{q}");
    embed_raw(embedder, &prefixed).await
}

/// Search `chunk_store` with an already-embedded query vector (see
/// [`embed_query`]), scoped to `repo_filter` if given, capped at `limit`
/// (already clamped by [`validate_query`]). Best-effort `ensure_vector_index`
/// first (an index-build failure never fails the search — lance brute-force
/// scans a too-small/unbuilt table, same tolerance as kb-core's own search
/// route).
pub async fn search(
    chunk_store: &ChunkStore,
    query_vec: &[f32],
    repo_filter: Option<&str>,
    limit: u32,
) -> Result<Vec<ChunkHit>> {
    let _ = chunk_store.ensure_vector_index().await;
    let over_fetch = limit.saturating_mul(OVER_FETCH_MULTIPLIER);
    let hits = chunk_store
        .search(query_vec, over_fetch, limit, repo_filter)
        .await?;
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_query_rejects_empty_and_whitespace_only() {
        assert!(matches!(
            validate_query("", None),
            Err(SemanticSearchError::EmptyQuery)
        ));
        assert!(matches!(
            validate_query("   ", None),
            Err(SemanticSearchError::EmptyQuery)
        ));
    }

    #[test]
    fn validate_query_trims_and_clamps_limit() {
        let (q, limit) = validate_query("  parse tree walker  ", None).unwrap();
        assert_eq!(q, "parse tree walker");
        assert_eq!(limit, DEFAULT_LIMIT);

        let (_, clamped_low) = validate_query("q", Some(0)).unwrap();
        assert_eq!(clamped_low, 1);
        let (_, clamped_high) = validate_query("q", Some(9_999)).unwrap();
        assert_eq!(clamped_high, MAX_LIMIT);
    }

    #[tokio::test]
    async fn search_on_an_empty_store_is_empty_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChunkStore::open(&tmp.path().join("lance")).await.unwrap();
        let dim = store.dim() as usize;
        let hits = search(&store, &vec![0.0; dim], None, 10).await.unwrap();
        assert!(hits.is_empty());
    }
}
