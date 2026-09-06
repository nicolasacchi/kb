//! W2.3 — the semantic lane's background indexer worker: chunks + embeds
//! NEW blob_hashes for ENABLED repos (never re-embeds a seen blob — ADR-2's
//! win, applied here via `store::Store::has_chunks`/`mark_chunked`), and
//! prunes `chunk_vectors` rows for blobs no longer referenced by ANY
//! current file in ANY repo (a global ref-count — see `store::Store::
//! orphaned_chunk_blobs`'s doc: blob_hash sharing is global under ADR-2, so
//! the ref-count is too).
//!
//! # Trigger
//!
//! Fed by the SAME `mirror.updated` events the search-lane caches key off
//! (`bus.subscribe()`), filtered to repos in the `[semantic] repos`
//! allowlist — the "subscribe pattern like the search caches" option the
//! W2.3 plan named, chosen over a store-generation poll because nobody
//! polls "did semantic content change" on every request the way a search
//! handler polls `Store::generation()` (there's no per-request call site to
//! attach that poll to) — a background subscriber is the only way new
//! content gets embedded without an explicit reindex verb. A
//! `bus.subscribe()` receiver only sees events emitted AFTER it subscribes
//! (`kb_core::events::EventBus`'s broadcast semantics), so
//! [`SemanticIndexer::spawn`] runs one full [`reindex_repo_incremental`]
//! pass per enabled repo up front (covers content the boot-time HEAD walk /
//! initial reconcile already wrote to the store before the subscriber was
//! live).
//!
//! A periodic [`FALLBACK_INTERVAL`] full sweep backstops a dropped/lagged
//! broadcast event (`tokio::sync::broadcast`'s bounded-channel semantics
//! can silently skip envelopes under a burst — `EventBus`'s own doc) —
//! mirrors `mirror`'s own periodic reconcile philosophy ("catch anything
//! notify missed"). Re-scanning an already-fully-embedded repo is cheap:
//! every blob is an sqlite `has_chunks` hit, no lance write, no embed call.
//!
//! # Batching
//!
//! Each ready blob's chunks are embedded in ONE `Embedder::embed_batch`
//! call (already internally chunked at `embed::MAX_BATCH_SIZE`), then
//! written in ONE `ChunkStore::upsert_chunks_for_blob` lance commit —
//! bounded per blob, not per file-list-sized burst.

use crate::config::RepoEntry;
use crate::lang::{self, LangInfo};
use crate::semantic::chunk;
use crate::semantic::store::{ChunkRow, ChunkStore, ChunkStoreError};
use crate::store::{FileRow, Store, StoreBlocking, StoreError};
use kb_core::embed::Embedder;
use kb_core::events::EventBus;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum IndexerError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    ChunkStore(#[from] ChunkStoreError),
}

pub type Result<T> = std::result::Result<T, IndexerError>;

/// Periodic full-sweep backstop — see the module doc. Long enough that it
/// never competes meaningfully with the event-driven path under normal
/// operation (every real edit already triggers a pass via `mirror.updated`);
/// short enough that a dropped event's content isn't invisible to semantic
/// search for more than a few minutes.
pub const FALLBACK_INTERVAL: Duration = Duration::from_secs(300);

/// Aggregate counts from one [`reindex_repo_incremental`] pass —
/// test/observability hook, mirrors `ingest::WalkStats`'s shape.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReindexStats {
    pub embedded_blobs: usize,
    pub embedded_chunks: usize,
    pub skipped_cache_hit: usize,
    pub orphans_pruned: usize,
}

/// Owns the background worker task. Held purely for RAII in `AppState`
/// (mirrors `mirror::MirrorWatcher`'s own convention) — dropping this does
/// NOT stop the task (a tokio task keeps running once spawned regardless of
/// whether its handle is held; see `sink::spawn`'s identical doc note), only
/// daemon shutdown actually ends it.
pub struct SemanticIndexer {
    #[allow(dead_code)]
    handle: tokio::task::JoinHandle<()>,
}

impl SemanticIndexer {
    /// Spawn the worker. `enabled_repo_names` is `config::SemanticSection::
    /// repos` (already filtered to `enabled == true` by the caller — see
    /// `SemanticSection::repo_enabled`) — every OTHER configured repo is
    /// invisible to this worker, never scanned, never embedded.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        store: Arc<Store>,
        chunk_store: Arc<ChunkStore>,
        embedder: Arc<Mutex<Embedder>>,
        repos: Vec<RepoEntry>,
        repo_ids: HashMap<String, i64>,
        enabled_repo_names: Vec<String>,
        bus: Arc<EventBus>,
    ) -> Self {
        let handle = tokio::spawn(run(
            store,
            chunk_store,
            embedder,
            repos,
            repo_ids,
            enabled_repo_names,
            bus,
        ));
        Self { handle }
    }
}

/// Run one [`reindex_repo_incremental`] pass over EVERY `enabled` repo —
/// the up-front pass at worker startup and the periodic [`FALLBACK_INTERVAL`]
/// backstop both call this (see the module doc for why each exists).
async fn sweep_all(
    enabled: &[RepoEntry],
    repo_ids: &HashMap<String, i64>,
    store: &Arc<Store>,
    chunk_store: &ChunkStore,
    embedder: &Arc<Mutex<Embedder>>,
) {
    for repo in enabled {
        let Some(&repo_id) = repo_ids.get(&repo.name) else {
            continue;
        };
        match reindex_repo_incremental(
            store,
            chunk_store,
            embedder,
            &repo.name,
            repo_id,
            &repo.path,
        )
        .await
        {
            Ok(stats) => tracing::info!(
                repo = %repo.name, ?stats,
                "kb-code semantic: reindex pass complete",
            ),
            Err(e) => tracing::warn!(
                repo = %repo.name, error = %e,
                "kb-code semantic: reindex pass failed",
            ),
        }
    }
}

async fn run(
    store: Arc<Store>,
    chunk_store: Arc<ChunkStore>,
    embedder: Arc<Mutex<Embedder>>,
    repos: Vec<RepoEntry>,
    repo_ids: HashMap<String, i64>,
    enabled_repo_names: Vec<String>,
    bus: Arc<EventBus>,
) {
    let enabled: Vec<RepoEntry> = repos
        .into_iter()
        .filter(|r| enabled_repo_names.iter().any(|n| n == &r.name))
        .collect();

    // Up-front pass — see the module doc: covers content already in the
    // store before this subscriber was live.
    sweep_all(&enabled, &repo_ids, &store, &chunk_store, &embedder).await;

    let mut rx = bus.subscribe();
    let mut fallback = tokio::time::interval(FALLBACK_INTERVAL);
    // The first tick fires immediately; we already just swept above.
    fallback.tick().await;

    loop {
        tokio::select! {
            _ = fallback.tick() => {
                sweep_all(&enabled, &repo_ids, &store, &chunk_store, &embedder).await;
            }
            recv = rx.recv() => {
                match recv {
                    Ok(env) if env.type_ == "mirror.updated" => {
                        let Some(repo_name) = env.payload.get("repo").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        let Some(repo) = enabled.iter().find(|r| r.name == repo_name) else {
                            continue; // not an enabled repo — ignore
                        };
                        let Some(&repo_id) = repo_ids.get(repo_name) else {
                            continue;
                        };
                        match reindex_repo_incremental(&store, &chunk_store, &embedder, repo_name, repo_id, &repo.path).await {
                            Ok(stats) if stats.embedded_blobs > 0 || stats.orphans_pruned > 0 => {
                                tracing::info!(repo = repo_name, ?stats, "kb-code semantic: incremental pass complete");
                            }
                            Ok(_) => {}
                            Err(e) => tracing::warn!(repo = repo_name, error = %e, "kb-code semantic: incremental pass failed"),
                        }
                    }
                    Ok(_) => {} // some other event type — nothing to do
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Dropped events under a burst — the periodic fallback
                        // sweep (above) is the correctness backstop; nothing to
                        // do here beyond continuing to drain.
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    tracing::info!("kb-code semantic indexer exiting (bus closed — normal at shutdown)");
}

/// Whether `file`'s blob is a candidate for a fresh chunk+embed pass — a
/// PURE sqlite lookup (no lance, no embedder), so the "which blobs need
/// work" decision is unit-testable without a live `kb-embedder` subprocess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlobStatus {
    /// Has a supported grammar and isn't cached yet — needs chunk+embed.
    NeedsEmbedding(LangInfo),
    /// Has a supported grammar but `Store::has_chunks` already says yes.
    CacheHit,
    /// A tier marker (too-large/binary/unknown/lfs) — no grammar, nothing
    /// to chunk, ever.
    NoGrammar,
}

fn classify_blob(store: &Store, file: &FileRow) -> Result<BlobStatus> {
    let Some(lang_info) = lang::for_id(&file.lang) else {
        return Ok(BlobStatus::NoGrammar);
    };
    if store.has_chunks(&file.blob_hash, lang_info.salt)? {
        return Ok(BlobStatus::CacheHit);
    }
    Ok(BlobStatus::NeedsEmbedding(lang_info))
}

/// One repo's incremental pass: chunk+embed every NEW blob_hash reachable
/// from `repo_id`'s CURRENT `files` rows (an sqlite `has_chunks` cache hit
/// skips everything already done), then prune orphaned chunk_vectors rows
/// (global ref-count — see the module doc). Reads WORKING-TREE bytes off
/// disk (`repo_root.join(path)`), matching `sink.rs`'s own live-content
/// model (unlike `ingest::index_repo_working_tree`'s git-blob reads) — the
/// `files` table already reflects live edits by the time this runs.
pub async fn reindex_repo_incremental(
    store: &Arc<Store>,
    chunk_store: &ChunkStore,
    embedder: &Arc<Mutex<Embedder>>,
    repo_name: &str,
    repo_id: i64,
    repo_root: &Path,
) -> Result<ReindexStats> {
    let mut stats = ReindexStats::default();

    // 2026-08-31 incident (store.rs module doc): `Store` calls reachable
    // from async context (this is a background `tokio::spawn`ed task) run
    // on the blocking pool via `run_blocking`, never inline. The read AND
    // the pure in-memory dedup pass share ONE round trip.
    //
    // Dedup by blob_hash — ADR-2: one embed pass per unique CONTENT, not
    // per path. `list_files` is path-ordered, so the alphabetically-first
    // path sharing a blob_hash is the deterministic "representative"
    // (repo, path) recorded on that blob's chunk rows.
    let representative_files: Vec<FileRow> = store
        .run_blocking(move |store| {
            let files = store.list_files(repo_id)?;
            let mut representative: HashMap<String, FileRow> = HashMap::new();
            for f in files {
                representative.entry(f.blob_hash.clone()).or_insert(f);
            }
            Ok::<_, StoreError>(representative.into_values().collect())
        })
        .await?;

    for file in representative_files {
        let blob_hash = file.blob_hash.clone();
        // Store-only segment (a pure `has_chunks` lookup) — the fs read/
        // chunk/embed work below is either non-store or already its own
        // `spawn_blocking` (the embed IPC call), so it stays a separate
        // round trip rather than being folded into this one (rule: wrap
        // each store SEGMENT when async work interleaves).
        let file_for_classify = file.clone();
        let lang_info = match store
            .run_blocking(move |store| classify_blob(store, &file_for_classify))
            .await?
        {
            BlobStatus::NoGrammar => continue,
            BlobStatus::CacheHit => {
                stats.skipped_cache_hit += 1;
                continue;
            }
            BlobStatus::NeedsEmbedding(li) => li,
        };

        let abs = repo_root.join(&file.path);
        let bytes = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(
                    repo = %repo_name, path = %file.path, error = %e,
                    "kb-code semantic: read failed (raced away?) — retried next pass",
                );
                continue;
            }
        };
        let chunks = match chunk::chunk_file(lang_info.id, &file.path, &bytes) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    repo = %repo_name, path = %file.path, error = %e,
                    "kb-code semantic: chunking failed — skipping this blob",
                );
                continue;
            }
        };
        if chunks.is_empty() {
            // Legitimately nothing to embed (blank/whitespace-only content)
            // — mark it so we don't re-check this blob every pass.
            let blob_hash_c = blob_hash.clone();
            store
                .run_blocking(move |store| store.mark_chunked(&blob_hash_c, lang_info.salt, 0))
                .await?;
            continue;
        }

        // `embed_batch` is a genuinely blocking IPC round-trip to the
        // kb-embedder subprocess (holds the shared `std::sync::Mutex` for
        // its duration) — ALWAYS run off the async worker thread via
        // `spawn_blocking`, even from this dedicated background task; same
        // rule `kb_core::indexer::prepare_doc`/`embed_cache::embed_query`
        // apply (a blocking Mutex-held call on a tokio worker thread
        // starves whatever else that thread was about to run, not just
        // "the async runtime" in the abstract).
        let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let embedder_for_task = embedder.clone();
        let embed_result = tokio::task::spawn_blocking(move || {
            let mut guard = embedder_for_task.lock().unwrap_or_else(|e| e.into_inner());
            guard.embed_batch(&texts)
        })
        .await;
        let embeddings = match embed_result {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tracing::warn!(
                    repo = %repo_name, path = %file.path, error = %e,
                    "kb-code semantic: embed_batch failed — will retry next pass",
                );
                continue;
            }
            Err(e) => {
                tracing::warn!(
                    repo = %repo_name, path = %file.path, error = %e,
                    "kb-code semantic: embed task panicked — will retry next pass",
                );
                continue;
            }
        };
        if embeddings.len() != chunks.len() {
            tracing::warn!(
                repo = %repo_name, path = %file.path,
                got = embeddings.len(), want = chunks.len(),
                "kb-code semantic: embedder returned a mismatched vector count — skipping this blob",
            );
            continue;
        }

        let rows: Vec<ChunkRow> = chunks
            .iter()
            .zip(embeddings)
            .map(|(c, emb)| ChunkRow {
                chunk_id: format!("{blob_hash}#{}", c.idx),
                blob_hash: blob_hash.clone(),
                repo: repo_name.to_string(),
                path: file.path.clone(),
                lang: lang_info.id.to_string(),
                span_start: c.line_start,
                span_end: c.line_end,
                text: c.text.clone(),
                embedding: Some(emb),
            })
            .collect();
        let row_count = rows.len();
        chunk_store
            .upsert_chunks_for_blob(&blob_hash, &rows)
            .await?;
        let blob_hash_c = blob_hash.clone();
        store
            .run_blocking(move |store| store.mark_chunked(&blob_hash_c, lang_info.salt, row_count))
            .await?;
        stats.embedded_blobs += 1;
        stats.embedded_chunks += row_count;
    }

    // Orphan sweep — global, repo-agnostic (see `Store::orphaned_chunk_blobs`'s
    // doc). Runs on EVERY call regardless of which repo triggered it: cheap
    // (one indexed sqlite query), and correctness doesn't depend on which
    // repo happened to fire the pass. One `run_blocking` round trip covers
    // the read + set-building; each delete/clear pair below stays its own
    // round trip since the lance delete is async work between them.
    let orphan_blobs: BTreeSet<String> = store
        .run_blocking(|store| {
            let mut set = BTreeSet::new();
            for (blob_hash, _salt) in store.orphaned_chunk_blobs()? {
                set.insert(blob_hash);
            }
            Ok::<_, StoreError>(set)
        })
        .await?;
    for blob_hash in orphan_blobs {
        chunk_store.delete_chunks_for_blob(&blob_hash).await?;
        let blob_hash_c = blob_hash.clone();
        store
            .run_blocking(move |store| store.clear_chunk_status_for_blob(&blob_hash_c))
            .await?;
        stats.orphans_pruned += 1;
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::store::ChunkStore;

    fn make_store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        (tmp, store)
    }

    async fn make_chunk_store() -> (tempfile::TempDir, ChunkStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChunkStore::open(&tmp.path().join("lance")).await.unwrap();
        (tmp, store)
    }

    // `Embedder` has no trait seam (see `kb_core::embed`'s doc: the IPC
    // backend needs a real `kb-embedder` subprocess) — every test below
    // exercises `classify_blob`/the orphan-sweep pair directly (the
    // store-side bookkeeping `reindex_repo_incremental` runs BEFORE and
    // AFTER any embed call) rather than the full embed round trip. The
    // embed call itself is covered by `kb_core::embed_ipc`'s own tests plus
    // the env-gated `KB_CODE_SEMANTIC_E2E` end-to-end test.

    #[test]
    fn classify_blob_treats_tier_markers_as_no_grammar() {
        let (_tmp, store) = make_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        for (path, tier) in [
            ("README.md", "unknown"),
            ("big.bin", "too-large"),
            ("photo.png", "binary"),
            ("model.psd", "lfs"),
        ] {
            store
                .upsert_file(repo_id, path, "hash-tier", tier, 10)
                .unwrap();
            let file = store.get_file(repo_id, path).unwrap().unwrap();
            assert_eq!(
                classify_blob(&store, &file).unwrap(),
                BlobStatus::NoGrammar,
                "{tier} must never be a chunk candidate"
            );
        }
    }

    #[test]
    fn classify_blob_distinguishes_cache_hit_from_needs_embedding() {
        let (_tmp, store) = make_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();
        let file = store.get_file(repo_id, "a.rs").unwrap().unwrap();

        let salt = lang::for_id("rust").unwrap().salt;
        match classify_blob(&store, &file).unwrap() {
            BlobStatus::NeedsEmbedding(li) => assert_eq!(li.salt, salt),
            other => panic!("expected NeedsEmbedding, got {other:?}"),
        }

        store.mark_chunked("hashA", salt, 3).unwrap();
        assert_eq!(
            classify_blob(&store, &file).unwrap(),
            BlobStatus::CacheHit,
            "a marked-chunked blob must never be re-offered for embedding"
        );
    }

    #[tokio::test]
    async fn orphan_sweep_deletes_lance_rows_and_clears_sqlite_status_together() {
        let (_s, store) = make_store();
        let (_c, chunk_store) = make_chunk_store().await;
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();

        // Seed a blob that IS still referenced, and one that is NOT.
        store
            .upsert_file(repo_id, "live.rs", "hashLive", "rust", 10)
            .unwrap();
        let salt = lang::for_id("rust").unwrap().salt;
        store.mark_chunked("hashLive", salt, 1).unwrap();
        store.mark_chunked("hashGone", salt, 1).unwrap();
        let dim = chunk_store.dim() as usize;
        chunk_store
            .upsert_chunks_for_blob(
                "hashLive",
                &[ChunkRow {
                    chunk_id: "hashLive#0".into(),
                    blob_hash: "hashLive".into(),
                    repo: "r".into(),
                    path: "live.rs".into(),
                    lang: "rust".into(),
                    span_start: 1,
                    span_end: 1,
                    text: "live.rs | module | fn a(){}".into(),
                    embedding: Some(vec![0.1; dim]),
                }],
            )
            .await
            .unwrap();
        chunk_store
            .upsert_chunks_for_blob(
                "hashGone",
                &[ChunkRow {
                    chunk_id: "hashGone#0".into(),
                    blob_hash: "hashGone".into(),
                    repo: "r".into(),
                    path: "gone.rs".into(),
                    lang: "rust".into(),
                    span_start: 1,
                    span_end: 1,
                    text: "gone.rs | module | fn b(){}".into(),
                    embedding: Some(vec![0.2; dim]),
                }],
            )
            .await
            .unwrap();
        assert_eq!(chunk_store.count_rows().await.unwrap(), 2);

        // Sweep via the store-level helpers directly (the same calls
        // `reindex_repo_incremental`'s tail makes) — proves the pairing.
        let orphans = store.orphaned_chunk_blobs().unwrap();
        assert_eq!(orphans, vec![("hashGone".to_string(), salt.to_string())]);
        for (blob_hash, _salt) in orphans {
            chunk_store
                .delete_chunks_for_blob(&blob_hash)
                .await
                .unwrap();
            store.clear_chunk_status_for_blob(&blob_hash).unwrap();
        }

        assert_eq!(chunk_store.count_rows().await.unwrap(), 1);
        assert!(store.has_chunks("hashLive", salt).unwrap());
        assert!(!store.has_chunks("hashGone", salt).unwrap());
    }
}
