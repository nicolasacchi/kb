//! W2.3 — env-gated end-to-end test for the semantic lane: spawns a REAL
//! `kb-embedder` subprocess (`jina-embeddings-v2-base-code` — ~320 MB,
//! downloaded to the shared XDG model cache on first use), chunks+embeds a
//! tiny fixture repo (two functions), and queries it in natural language.
//!
//! NOT run by default — `just ci-code` never sets `KB_CODE_SEMANTIC_E2E`;
//! this is a genuinely heavy, network-touching integration test (a real
//! model download + a real ONNX inference process), run manually:
//!
//! ```text
//! KB_CODE_SEMANTIC_E2E=1 cargo test -p kb-code-server --test measure semantic_e2e -- --ignored --nocapture
//! ```
//!
//! Uses the real `~/.cache/kb/models` cache (matching production — same
//! convention as `kb_core::embed`'s own `#[ignore]`-gated live model tests);
//! override with `KB_CACHE_DIR` if a different cache root is desired.

use kb_code_server::ingest;
use kb_code_server::semantic::{indexer, search, ChunkStore};
use kb_code_server::store::Store;
use kb_core::embed::Embedder;
use std::sync::{Arc, Mutex};

#[tokio::test]
#[ignore = "measure lane — run with --ignored or KB_CODE_MEASURE=1"]
async fn semantic_e2e_embeds_and_queries_a_tiny_fixture_repo() {
    if std::env::var("KB_CODE_SEMANTIC_E2E").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping semantic_e2e_embeds_and_queries_a_tiny_fixture_repo: set \
             KB_CODE_SEMANTIC_E2E=1 to run (downloads jina-embeddings-v2-base-code, ~320 MB, \
             and spawns a real kb-embedder subprocess)"
        );
        return;
    }

    let repo_dir = tempfile::tempdir().unwrap();
    let fixture_src = b"\
/// Adds two integers.
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// Multiplies two integers together.
pub fn multiply(a: i32, b: i32) -> i32 {
    a * b
}
";
    std::fs::write(repo_dir.path().join("math.rs"), fixture_src).unwrap();

    let sqlite_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&sqlite_dir.path().join("index.db")).unwrap());
    let repo_id = store
        .upsert_repo("fixture", repo_dir.path().to_str().unwrap())
        .unwrap();
    let blob_hash = ingest::git_blob_hash(fixture_src);
    ingest::index_file(
        &store,
        repo_id,
        "math.rs",
        fixture_src,
        &blob_hash,
        true,
        false,
    )
    .unwrap();

    let lance_dir = tempfile::tempdir().unwrap();
    let chunk_store = Arc::new(
        ChunkStore::open(&lance_dir.path().join("lance"))
            .await
            .expect("open chunk store"),
    );

    let cache_dir = kb_core::embed::models_cache_dir(
        &std::env::var("KB_CACHE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(std::env::var("HOME").expect("HOME set")).join(".cache/kb")
            }),
    );
    let embedder = Arc::new(Mutex::new(
        Embedder::spawn_ipc("jina-embeddings-v2-base-code", cache_dir, 19)
            .expect("spawn real kb-embedder subprocess — is it built? `just build-embedder`"),
    ));

    let stats = indexer::reindex_repo_incremental(
        &store,
        &chunk_store,
        &embedder,
        "fixture",
        repo_id,
        repo_dir.path(),
    )
    .await
    .expect("reindex_repo_incremental");
    eprintln!("[semantic_e2e] reindex stats: {stats:?}");
    assert_eq!(
        stats.embedded_blobs, 1,
        "expected exactly one blob embedded: {stats:?}"
    );
    assert!(stats.embedded_chunks >= 1);

    let (q, limit) =
        search::validate_query("a function that multiplies two numbers", None).unwrap();
    let query_vec = search::embed_query(&embedder, &q)
        .await
        .expect("embed_query");
    let hits = search::search(&chunk_store, &query_vec, None, limit)
        .await
        .expect("search");
    eprintln!("[semantic_e2e] hits: {hits:#?}");
    assert!(!hits.is_empty(), "expected at least one semantic hit");
    assert!(
        hits[0].snippet.contains("multiply"),
        "expected the multiply function to rank first for a \"multiplies two numbers\" \
         query: {hits:#?}"
    );
}
