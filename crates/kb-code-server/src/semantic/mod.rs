//! W2.3 — the SEMANTIC search lane: kb-code's own lance chunk-vector store
//! (`store`, `<state>/kb-code/lance/`), a cAST-style tree-sitter chunker
//! (`chunk`), a background embedding worker (`indexer`), and the search API
//! (`search`) behind `GET /api/search/semantic` (`routes.rs`).
//!
//! **Decided architecture** (operator-approved, see the W2.3 plan): kb-code
//! owns its OWN lance tables in its own state dir — kb-core is untouched by
//! this module except ONE edit (`kb_core::embed::ModelInfo::max_length`,
//! plumbed for `jina-embeddings-v2-base-code`'s 8192-token window). The
//! chunk-store ops are COPIED from kb-core's SQ5 `artifact_chunks` idioms
//! (`crates/kb-core/src/storage/{schema.rs,lance.rs}`), not imported — see
//! `store`'s module doc for the exact lines they're copied from.
//!
//! **Off by default, per-repo staged rollout**: `[semantic] enabled = false`
//! plus `repos = []` (`config::SemanticSection`) — a cold-fleet embed is
//! hours of wall-clock work, so a repo only gets chunked+embedded once it is
//! BOTH `enabled = true` AND named in `repos`. The flag gates both the
//! background indexer (`indexer::SemanticIndexer`) and the search route
//! (`routes::search_semantic` 400s for a disabled repo, with a hint).
//!
//! **No reranker yet, no RRF fusion** — deliberately out of scope for this
//! step (opt-in reranking and multi-lane fusion are W2.4's job).

pub mod chunk;
pub mod indexer;
pub mod search;
pub mod store;

pub use chunk::{Chunk, ChunkError, QUERY_PREFIX};
pub use indexer::SemanticIndexer;
pub use search::SemanticSearchError;
pub use store::{ChunkHit, ChunkRow, ChunkStore, ChunkStoreError};

/// The ONLY embedding model the semantic lane drives (W2.3 scope — no
/// per-repo model choice yet). Registered in `kb_core::embed::
/// SUPPORTED_MODELS` with `dim = 768` and (the one kb-core edit this step
/// makes) `max_length = Some(8192)`.
pub const MODEL_NAME: &str = "jina-embeddings-v2-base-code";

/// Embedding width for [`MODEL_NAME`] — resolved from the kb-core registry
/// rather than hardcoded a second time, so a future registry edit that
/// changes the dim can't silently desync the lance schema width. Panics if
/// the model isn't registered (a build-time invariant, not a runtime
/// condition — `MODEL_NAME` is a compile-time constant this crate controls).
pub fn embedding_dim() -> i32 {
    kb_core::embed::model_info(MODEL_NAME)
        .unwrap_or_else(|| {
            panic!("{MODEL_NAME} must be registered in kb_core::embed::SUPPORTED_MODELS")
        })
        .dim as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_dim_matches_the_registry() {
        assert_eq!(embedding_dim(), 768);
    }
}
