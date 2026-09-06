//! W2.3 — kb-code's OWN lance chunk-vector store (`chunk_vectors`), at
//! `<state>/kb-code/lance/` — a SEPARATE lance dataset from kb's own
//! per-kb `artifacts`/`artifact_chunks` tables. Per the operator-approved
//! architecture: kb-code owns its own lance tables in its own state dir;
//! kb-core is untouched by this store (the ONE kb-core edit this step
//! makes is `embed::ModelInfo::max_length`, unrelated to storage). Ops
//! COPY kb-core's SQ5 chunk-table idioms
//! (`crates/kb-core/src/storage/{schema.rs:392-477,lance.rs:1084-1332}`)
//! rather than importing them — a from-scratch, kb-code-owned schema with
//! no dependency on kb-core's `Doc`/`ChunkDoc` types.
//!
//! # Schema
//!
//! `chunk_id` (`"{blob_hash}#{idx}"`, the merge_insert key) · `blob_hash`
//! (ADR-2 content key — see `crate::store`'s module doc) · `repo`/`path`
//! (ADVISORY: a blob is embedded ONCE under ADR-2's "never re-embed a seen
//! blob," recording whichever repo/path it was FIRST captured from; if the
//! same content later also appears under a second path or a second repo,
//! that path does NOT get its own chunk rows — search results report the
//! recorded repo/path even if the file has since moved or the content also
//! lives elsewhere, matching `store::Store::symbols_for_repo`'s own
//! documented stale-path/shared-blob approximation) · `lang` · `span_start`/
//! `span_end` (1-based inclusive source line numbers, `UInt32` — matches
//! `extract::Symbol`'s convention) · `text` (header + body, embed-ready —
//! see `chunk.rs`) · `embedding` (`FixedSizeList<Float32, DIM>`, nullable
//! on the OUTER field — kb-core invariant #1's lesson: a row whose embed
//! failed must still be representable, even though this Wave never
//! actually writes one with `embedding: None`).

use super::embedding_dim;
use arrow::array::{Array, FixedSizeListArray, Float32Array, StringArray, UInt32Array};
use arrow::buffer::NullBuffer;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use futures::TryStreamExt;
use lancedb::{
    connect,
    index::{vector::IvfPqIndexBuilder, Index},
    query::{ExecutableQuery, QueryBase, Select},
    Connection, Table as LanceTable,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub const TABLE_NAME: &str = "chunk_vectors";

#[derive(Debug, thiserror::Error)]
pub enum ChunkStoreError {
    #[error("kb-code chunk store: {0}")]
    Lance(String),
}

pub type Result<T> = std::result::Result<T, ChunkStoreError>;

fn lance_err(context: &str, e: impl std::fmt::Display) -> ChunkStoreError {
    ChunkStoreError::Lance(format!("{context}: {e}"))
}

/// Escape a value for a lance filter string literal (doubling `'`) — the
/// SAME rule as kb-core's private `escape_literal` (`storage/lance.rs`);
/// this store keeps its own copy rather than importing it (ops are
/// COPIED, not imported — see the module doc).
fn escape_literal(s: &str) -> String {
    s.replace('\'', "''")
}

fn filter_eq(col: &str, val: &str) -> String {
    format!("{col} = '{}'", escape_literal(val))
}

/// One row headed for `chunk_vectors`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkRow {
    pub chunk_id: String,
    pub blob_hash: String,
    pub repo: String,
    pub path: String,
    pub lang: String,
    pub span_start: u32,
    pub span_end: u32,
    pub text: String,
    pub embedding: Option<Vec<f32>>,
}

/// One ranked semantic search hit — `semantic::search`'s wire shape.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ChunkHit {
    pub repo: String,
    pub path: String,
    pub span_start: u32,
    pub span_end: u32,
    pub score: f32,
    pub snippet: String,
}

fn chunk_schema(dim: i32) -> Schema {
    Schema::new(vec![
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("blob_hash", DataType::Utf8, false),
        Field::new("repo", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("lang", DataType::Utf8, false),
        Field::new("span_start", DataType::UInt32, false),
        Field::new("span_end", DataType::UInt32, false),
        Field::new("text", DataType::Utf8, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), dim),
            true,
        ),
    ])
}

fn rows_to_batches(rows: &[ChunkRow], dim: i32) -> Result<Vec<RecordBatch>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let schema = Arc::new(chunk_schema(dim));
    let chunk_ids: StringArray = rows.iter().map(|r| Some(r.chunk_id.as_str())).collect();
    let blob_hashes: StringArray = rows.iter().map(|r| Some(r.blob_hash.as_str())).collect();
    let repos: StringArray = rows.iter().map(|r| Some(r.repo.as_str())).collect();
    let paths: StringArray = rows.iter().map(|r| Some(r.path.as_str())).collect();
    let langs: StringArray = rows.iter().map(|r| Some(r.lang.as_str())).collect();
    let span_starts = UInt32Array::from_iter_values(rows.iter().map(|r| r.span_start));
    let span_ends = UInt32Array::from_iter_values(rows.iter().map(|r| r.span_end));
    let texts: StringArray = rows.iter().map(|r| Some(r.text.as_str())).collect();

    let dim_usize = dim as usize;
    let mut values: Vec<f32> = Vec::with_capacity(rows.len() * dim_usize);
    let mut validity: Vec<bool> = Vec::with_capacity(rows.len());
    for r in rows {
        match &r.embedding {
            Some(v) => {
                if v.len() != dim_usize {
                    return Err(ChunkStoreError::Lance(format!(
                        "chunk {}: embedding dim {} != configured dim {}",
                        r.chunk_id,
                        v.len(),
                        dim_usize
                    )));
                }
                values.extend_from_slice(v);
                validity.push(true);
            }
            None => {
                values.extend(std::iter::repeat_n(0.0_f32, dim_usize));
                validity.push(false);
            }
        }
    }
    let inner = Float32Array::from(values);
    let item_field = Arc::new(Field::new("item", DataType::Float32, true));
    let nulls = NullBuffer::from(validity);
    let embeddings = FixedSizeListArray::new(item_field, dim, Arc::new(inner), Some(nulls));

    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(chunk_ids),
            Arc::new(blob_hashes),
            Arc::new(repos),
            Arc::new(paths),
            Arc::new(langs),
            Arc::new(span_starts),
            Arc::new(span_ends),
            Arc::new(texts),
            Arc::new(embeddings),
        ],
    )
    .map_err(|e| lance_err("rows_to_batches: RecordBatch::try_new", e))?;
    Ok(vec![batch])
}

/// kb-code's own lance chunk-vector table handle.
pub struct ChunkStore {
    _conn: Connection,
    table: LanceTable,
    dim: i32,
    /// Lazy IVF-PQ build gate — same dirty-flag shape as kb-core's
    /// `vector_needs_build` (`storage/lance.rs`): set on every write,
    /// cleared once a rebuild (or a "too small to index yet" no-op) runs.
    vector_needs_build: AtomicBool,
    vector_build_count: AtomicU64,
}

impl ChunkStore {
    /// Open (or create) the `chunk_vectors` dataset at `lance_dir`
    /// (`<state>/kb-code/lance/`). Dim is fixed at `embedding_dim()`
    /// (jina-embeddings-v2-base-code, 768) — kb-code's semantic lane has no
    /// per-repo model choice (unlike kb-core's per-kb dim, invariant #4).
    pub async fn open(lance_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(lance_dir).map_err(|e| lance_err("create_dir_all", e))?;
        let conn = connect(&lance_dir.to_string_lossy())
            .execute()
            .await
            .map_err(|e| lance_err("connect", e))?;
        let dim = embedding_dim();
        let names = conn
            .table_names()
            .execute()
            .await
            .map_err(|e| lance_err("table_names", e))?;
        let table = if names.iter().any(|n| n == TABLE_NAME) {
            conn.open_table(TABLE_NAME)
                .execute()
                .await
                .map_err(|e| lance_err("open_table", e))?
        } else {
            conn.create_empty_table(TABLE_NAME, Arc::new(chunk_schema(dim)))
                .execute()
                .await
                .map_err(|e| lance_err("create_empty_table", e))?
        };
        Ok(Self {
            _conn: conn,
            table,
            dim,
            vector_needs_build: AtomicBool::new(true),
            vector_build_count: AtomicU64::new(0),
        })
    }

    pub fn dim(&self) -> i32 {
        self.dim
    }

    /// Replace ALL chunks for `blob_hash`: delete-then-merge_insert (SQ5's
    /// `upsert_chunks` idiom — `lance.rs:1091-1114`) rather than a bare
    /// merge_insert. A salt bump (a tree-sitter grammar/query version
    /// change) can change the chunk COUNT for unchanged content — a bare
    /// merge_insert would leave the OLD chunk count's trailing rows (e.g.
    /// old `#3`/`#4` when the new chunking only emits `#0`-`#2`) orphaned
    /// in the table; deleting first makes every write authoritative for
    /// its blob. A no-op call (`rows` empty) still performs the delete —
    /// used by the indexer when a re-chunk legitimately produces zero
    /// chunks (e.g. now-blank content).
    pub async fn upsert_chunks_for_blob(&self, blob_hash: &str, rows: &[ChunkRow]) -> Result<()> {
        self.table
            .delete(&filter_eq("blob_hash", blob_hash))
            .await
            .map_err(|e| lance_err("delete (pre-upsert)", e))?;
        if !rows.is_empty() {
            let batches = rows_to_batches(rows, self.dim)?;
            let reader = arrow::record_batch::RecordBatchIterator::new(
                batches.into_iter().map(Ok),
                Arc::new(chunk_schema(self.dim)),
            );
            let mut merge = self.table.merge_insert(&["chunk_id"]);
            merge
                .when_matched_update_all(None)
                .when_not_matched_insert_all();
            merge
                .execute(Box::new(reader))
                .await
                .map_err(|e| lance_err("merge_insert", e))?;
        }
        self.vector_needs_build.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Drop every chunk row for `blob_hash` — the orphan-sweep primitive
    /// (`semantic::indexer`'s ref-count pass: "no current file anywhere
    /// still points at this blob_hash").
    pub async fn delete_chunks_for_blob(&self, blob_hash: &str) -> Result<()> {
        self.table
            .delete(&filter_eq("blob_hash", blob_hash))
            .await
            .map_err(|e| lance_err("delete", e))?;
        self.vector_needs_build.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub async fn count_rows(&self) -> Result<u64> {
        self.table
            .count_rows(None)
            .await
            .map(|n| n as u64)
            .map_err(|e| lance_err("count_rows", e))
    }

    /// Lazy IVF-PQ index build behind the dirty flag — same small-table
    /// tolerance as kb-core's `ensure_chunk_vector_index` (`lance.rs:1155-
    /// 1185`): an error whose message names an emptiness/too-small
    /// condition just clears the flag rather than failing the caller (lance
    /// brute-force-scans a too-small/unindexed table, still correct — only
    /// slower).
    pub async fn ensure_vector_index(&self) -> Result<()> {
        if !self.vector_needs_build.load(Ordering::Relaxed) {
            return Ok(());
        }
        let result = self
            .table
            .create_index(&["embedding"], Index::IvfPq(IvfPqIndexBuilder::default()))
            .execute()
            .await;
        if let Err(e) = result {
            let lower = e.to_string().to_lowercase();
            if lower.contains("already exists")
                || lower.contains("not enough")
                || lower.contains("empty")
                || lower.contains("no rows")
                || lower.contains("zero")
                || lower.contains("at least")
            {
                self.vector_needs_build.store(false, Ordering::Relaxed);
                return Ok(());
            }
            return Err(lance_err("create_index", e));
        }
        self.vector_build_count.fetch_add(1, Ordering::Relaxed);
        self.vector_needs_build.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// Number of real IVF-PQ rebuilds since open — observability, mirrors
    /// kb-core's `chunk_vector_rebuild_count`.
    pub fn vector_rebuild_count(&self) -> u64 {
        self.vector_build_count.load(Ordering::Relaxed)
    }

    /// Test-only: insert a whole batch of rows in ONE lance commit, no
    /// per-blob delete first — used by the 10k-row validation checkpoint to
    /// build a realistically-sized table without paying thousands of
    /// separate dataset commits (production writes are per-blob and small;
    /// see `upsert_chunks_for_blob`). Callers must ensure `rows` doesn't
    /// contain a `chunk_id` already present from a PRIOR call in the same
    /// test if they want plain insert semantics — `merge_insert` still
    /// updates-in-place on a repeat id, it just skips the pre-delete.
    #[cfg(test)]
    async fn bulk_insert_for_test(&self, rows: &[ChunkRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let batches = rows_to_batches(rows, self.dim)?;
        let reader = arrow::record_batch::RecordBatchIterator::new(
            batches.into_iter().map(Ok),
            Arc::new(chunk_schema(self.dim)),
        );
        let mut merge = self.table.merge_insert(&["chunk_id"]);
        merge
            .when_matched_update_all(None)
            .when_not_matched_insert_all();
        merge
            .execute(Box::new(reader))
            .await
            .map_err(|e| lance_err("bulk merge_insert", e))?;
        self.vector_needs_build.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Vector search: over-fetch `over_fetch` chunks, then max-pool to the
    /// best-scoring chunk per `(repo, path)` — the recorded, advisory
    /// repo/path (see the module doc) — returning the top `limit` hits,
    /// score-descending, tie-broken by `(repo, path)` for determinism.
    /// `repo_filter` restricts the lance-side scan to one recorded repo
    /// name via an `only_if` predicate applied BEFORE the over-fetch
    /// limit, so a narrow repo's results aren't starved by a large one
    /// sharing the same over-fetch budget. `score = 1 / (1 + distance)`,
    /// matching kb-core's `chunk_vector_query` convention. Empty table (or
    /// a repo filter matching nothing) → empty `Vec`, never an error.
    pub async fn search(
        &self,
        query_vec: &[f32],
        over_fetch: u32,
        limit: u32,
        repo_filter: Option<&str>,
    ) -> Result<Vec<ChunkHit>> {
        if self.count_rows().await? == 0 {
            return Ok(Vec::new());
        }
        let mut q = self
            .table
            .query()
            .nearest_to(query_vec.to_vec())
            .map_err(|e| lance_err("nearest_to", e))?;
        if let Some(repo) = repo_filter {
            q = q.only_if(filter_eq("repo", repo));
        }
        let stream = q
            .select(Select::columns(&[
                "repo",
                "path",
                "span_start",
                "span_end",
                "text",
            ]))
            .limit(over_fetch as usize)
            .execute()
            .await
            .map_err(|e| lance_err("query execute", e))?;
        let batches: Vec<_> = stream
            .try_collect()
            .await
            .map_err(|e| lance_err("stream collect", e))?;

        // Max-pool: keep the best-scoring (min distance) chunk per
        // (repo, path).
        let mut best: HashMap<(String, String), ChunkHit> = HashMap::new();
        for batch in &batches {
            let (Some(repos), Some(paths), Some(starts), Some(ends), Some(texts)) = (
                batch
                    .column_by_name("repo")
                    .and_then(|c| c.as_any().downcast_ref::<StringArray>()),
                batch
                    .column_by_name("path")
                    .and_then(|c| c.as_any().downcast_ref::<StringArray>()),
                batch
                    .column_by_name("span_start")
                    .and_then(|c| c.as_any().downcast_ref::<UInt32Array>()),
                batch
                    .column_by_name("span_end")
                    .and_then(|c| c.as_any().downcast_ref::<UInt32Array>()),
                batch
                    .column_by_name("text")
                    .and_then(|c| c.as_any().downcast_ref::<StringArray>()),
            ) else {
                return Err(ChunkStoreError::Lance(
                    "chunk store search: expected column missing or wrong type".into(),
                ));
            };
            let dists = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
            for i in 0..batch.num_rows() {
                let repo = repos.value(i).to_string();
                let path = paths.value(i).to_string();
                let dist = dists.map(|d| d.value(i)).unwrap_or(0.0);
                let score = 1.0 / (1.0 + dist);
                let key = (repo.clone(), path.clone());
                let better = match best.get(&key) {
                    Some(existing) => score > existing.score,
                    None => true,
                };
                if better {
                    best.insert(
                        key,
                        ChunkHit {
                            repo,
                            path,
                            span_start: starts.value(i),
                            span_end: ends.value(i),
                            score,
                            snippet: texts.value(i).to_string(),
                        },
                    );
                }
            }
        }
        let mut hits: Vec<ChunkHit> = best.into_values().collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.repo.cmp(&b.repo))
                .then_with(|| a.path.cmp(&b.path))
        });
        hits.truncate(limit as usize);
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        chunk_id: &str,
        blob_hash: &str,
        repo: &str,
        path: &str,
        embedding: Vec<f32>,
    ) -> ChunkRow {
        ChunkRow {
            chunk_id: chunk_id.to_string(),
            blob_hash: blob_hash.to_string(),
            repo: repo.to_string(),
            path: path.to_string(),
            lang: "rust".to_string(),
            span_start: 1,
            span_end: 3,
            text: format!("{path} | fn | fn f() {{}}"),
            embedding: Some(embedding),
        }
    }

    /// Deterministic pseudo-random unit-ish vector, distinct per seed —
    /// good enough for nearest-neighbour ordering tests without a real
    /// embedder (no ONNX/network dependency in this crate's test suite).
    fn vec_for_seed(seed: u64, dim: usize) -> Vec<f32> {
        let mut state = seed.wrapping_mul(2654435761).wrapping_add(1);
        (0..dim)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                ((state >> 33) as f32 / u32::MAX as f32) - 0.5
            })
            .collect()
    }

    #[tokio::test]
    async fn open_creates_an_empty_table_at_the_registered_dim() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChunkStore::open(&tmp.path().join("lance")).await.unwrap();
        assert_eq!(store.dim(), embedding_dim());
        assert_eq!(store.count_rows().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn open_is_idempotent_on_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let lance_dir = tmp.path().join("lance");
        let store = ChunkStore::open(&lance_dir).await.unwrap();
        let dim = store.dim() as usize;
        store
            .upsert_chunks_for_blob(
                "hashA",
                &[row("hashA#0", "hashA", "r", "a.rs", vec_for_seed(1, dim))],
            )
            .await
            .unwrap();
        drop(store);

        let reopened = ChunkStore::open(&lance_dir).await.unwrap();
        assert_eq!(reopened.count_rows().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn upsert_then_delete_by_blob_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChunkStore::open(&tmp.path().join("lance")).await.unwrap();
        let dim = store.dim() as usize;
        store
            .upsert_chunks_for_blob(
                "hashA",
                &[
                    row("hashA#0", "hashA", "r", "a.rs", vec_for_seed(1, dim)),
                    row("hashA#1", "hashA", "r", "a.rs", vec_for_seed(2, dim)),
                ],
            )
            .await
            .unwrap();
        store
            .upsert_chunks_for_blob(
                "hashB",
                &[row("hashB#0", "hashB", "r", "b.rs", vec_for_seed(3, dim))],
            )
            .await
            .unwrap();
        assert_eq!(store.count_rows().await.unwrap(), 3);

        store.delete_chunks_for_blob("hashA").await.unwrap();
        assert_eq!(store.count_rows().await.unwrap(), 1);

        // Deleting an already-gone blob is a no-op, not an error.
        store.delete_chunks_for_blob("hashA").await.unwrap();
        assert_eq!(store.count_rows().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn re_upsert_with_fewer_chunks_drops_the_stale_trailing_rows() {
        // Simulates a salt bump that re-chunks the same blob into FEWER
        // pieces — the delete-before-merge_insert idiom must leave no
        // orphaned old-numbered chunk_ids behind.
        let tmp = tempfile::tempdir().unwrap();
        let store = ChunkStore::open(&tmp.path().join("lance")).await.unwrap();
        let dim = store.dim() as usize;
        store
            .upsert_chunks_for_blob(
                "hashA",
                &[
                    row("hashA#0", "hashA", "r", "a.rs", vec_for_seed(1, dim)),
                    row("hashA#1", "hashA", "r", "a.rs", vec_for_seed(2, dim)),
                    row("hashA#2", "hashA", "r", "a.rs", vec_for_seed(3, dim)),
                ],
            )
            .await
            .unwrap();
        assert_eq!(store.count_rows().await.unwrap(), 3);

        store
            .upsert_chunks_for_blob(
                "hashA",
                &[row("hashA#0", "hashA", "r", "a.rs", vec_for_seed(9, dim))],
            )
            .await
            .unwrap();
        assert_eq!(
            store.count_rows().await.unwrap(),
            1,
            "old #1/#2 must be gone, not left orphaned"
        );
    }

    #[tokio::test]
    async fn search_ranks_the_planted_near_duplicate_first() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChunkStore::open(&tmp.path().join("lance")).await.unwrap();
        let dim = store.dim() as usize;
        let target = vec_for_seed(42, dim);
        // A near-duplicate of `target` (tiny perturbation) plus a handful
        // of unrelated vectors — the near-duplicate must rank first.
        let mut near_dup = target.clone();
        near_dup[0] += 0.001;
        let rows = vec![
            row("dup#0", "dup", "r", "near.rs", near_dup),
            row(
                "noise0#0",
                "noise0",
                "r",
                "noise0.rs",
                vec_for_seed(100, dim),
            ),
            row(
                "noise1#0",
                "noise1",
                "r",
                "noise1.rs",
                vec_for_seed(200, dim),
            ),
            row(
                "noise2#0",
                "noise2",
                "r",
                "noise2.rs",
                vec_for_seed(300, dim),
            ),
        ];
        for r in rows {
            store
                .upsert_chunks_for_blob(&r.blob_hash, std::slice::from_ref(&r))
                .await
                .unwrap();
        }
        let hits = store.search(&target, 16, 5, None).await.unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].path, "near.rs", "got: {hits:#?}");
    }

    #[tokio::test]
    async fn search_max_pools_per_repo_path_and_respects_repo_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChunkStore::open(&tmp.path().join("lance")).await.unwrap();
        let dim = store.dim() as usize;
        let q = vec_for_seed(7, dim);
        // Two chunks in the SAME (repo, path): only the better one should
        // survive max-pooling.
        let mut better = q.clone();
        better[0] += 0.0001;
        let worse = vec_for_seed(999, dim);
        store
            .upsert_chunks_for_blob(
                "hashA",
                &[
                    ChunkRow {
                        span_start: 1,
                        span_end: 3,
                        ..row("hashA#0", "hashA", "r1", "a.rs", worse)
                    },
                    ChunkRow {
                        span_start: 4,
                        span_end: 6,
                        ..row("hashA#1", "hashA", "r1", "a.rs", better)
                    },
                ],
            )
            .await
            .unwrap();
        store
            .upsert_chunks_for_blob(
                "hashB",
                &[row(
                    "hashB#0",
                    "hashB",
                    "r2",
                    "b.rs",
                    vec_for_seed(500, dim),
                )],
            )
            .await
            .unwrap();

        let all = store.search(&q, 16, 10, None).await.unwrap();
        let r1_hits: Vec<_> = all.iter().filter(|h| h.repo == "r1").collect();
        assert_eq!(
            r1_hits.len(),
            1,
            "max-pool must collapse to one hit per (repo,path): {all:#?}"
        );
        assert_eq!(
            r1_hits[0].span_start, 4,
            "the BETTER-scoring chunk must survive pooling"
        );

        let scoped = store.search(&q, 16, 10, Some("r2")).await.unwrap();
        assert!(scoped.iter().all(|h| h.repo == "r2"), "got: {scoped:#?}");
        assert!(!scoped.iter().any(|h| h.repo == "r1"));
    }

    #[tokio::test]
    async fn search_on_empty_table_is_empty_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChunkStore::open(&tmp.path().join("lance")).await.unwrap();
        let dim = store.dim() as usize;
        let hits = store
            .search(&vec_for_seed(1, dim), 16, 10, None)
            .await
            .unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn ensure_vector_index_is_idempotent_and_counts_real_rebuilds() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChunkStore::open(&tmp.path().join("lance")).await.unwrap();
        let dim = store.dim() as usize;
        store
            .upsert_chunks_for_blob(
                "hashA",
                &[row("hashA#0", "hashA", "r", "a.rs", vec_for_seed(1, dim))],
            )
            .await
            .unwrap();
        // A too-small table (well under lance's IVF-PQ minimum row count)
        // must not error — `ensure_vector_index` tolerates it exactly like
        // kb-core's own `ensure_chunk_vector_index`.
        store.ensure_vector_index().await.unwrap();
        let before = store.vector_rebuild_count();
        // Calling again with nothing dirtied must be a true no-op (no
        // second rebuild attempt).
        store.ensure_vector_index().await.unwrap();
        assert_eq!(store.vector_rebuild_count(), before);
    }

    // --- VALIDATION CHECKPOINT (W2.3 plan step 2) --------------------------
    //
    // ~10k synthetic chunks: upsert, delete-by-blob, and top-k query
    // correctness (a planted near-duplicate ranks first), with wall-clock
    // numbers printed via `--nocapture`. This is the in-test store-decision
    // checkpoint the plan calls for — a catastrophic failure here (a panic,
    // a multi-minute stall, or a wrong top-1) is the only thing that reopens
    // "kb-code owns its own lance tables," per the plan's framing.
    #[tokio::test]
    async fn validation_checkpoint_10k_chunks_upsert_delete_and_query() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ChunkStore::open(&tmp.path().join("lance")).await.unwrap();
        let dim = store.dim() as usize;

        const N: usize = 10_000;
        // Build in a handful of bulk commits (1k rows each) rather than N
        // separate dataset commits — matches the real indexer's shape
        // (batched per reindex tick), not "one lance write per chunk."
        // `upsert_chunks_for_blob`'s per-call delete-then-insert path is
        // exercised separately by the round-trip/re-upsert tests above;
        // this checkpoint is about the TABLE-AT-SCALE numbers.
        let t0 = std::time::Instant::now();
        for batch_start in (0..N).step_by(1_000) {
            let mut rows = Vec::with_capacity(1_000);
            for i in batch_start..(batch_start + 1_000).min(N) {
                let blob = format!("blob{i}");
                rows.push(row(
                    &format!("{blob}#0"),
                    &blob,
                    "synthetic-repo",
                    &format!("src/file_{i}.rs"),
                    vec_for_seed(i as u64, dim),
                ));
            }
            store.bulk_insert_for_test(&rows).await.unwrap();
        }
        let upsert_elapsed = t0.elapsed();
        assert_eq!(store.count_rows().await.unwrap(), N as u64);

        let t1 = std::time::Instant::now();
        store.ensure_vector_index().await.unwrap();
        let index_elapsed = t1.elapsed();

        // Plant a near-duplicate of a known row and confirm it ranks first.
        let target_idx = 4242;
        let mut near_dup = vec_for_seed(target_idx as u64, dim);
        near_dup[0] += 0.001;
        let t2 = std::time::Instant::now();
        let hits = store.search(&near_dup, 40, 5, None).await.unwrap();
        let query_elapsed = t2.elapsed();
        assert!(!hits.is_empty());
        assert_eq!(
            hits[0].path,
            format!("src/file_{target_idx}.rs"),
            "planted near-duplicate must rank first: {hits:#?}"
        );

        // Real per-blob delete-by-blob at table-at-scale: a bounded sample
        // (200 blobs), each its own `delete_chunks_for_blob` call — the
        // real orphan-sweep shape (one call per orphaned blob), timed at
        // the 10k-row table size rather than run 10k times (the write-side
        // loop above already proves the bulk-commit path; this proves the
        // per-blob DELETE predicate's cost against a 10k-row table).
        const DELETES: usize = 200;
        let t3 = std::time::Instant::now();
        for i in (0..N).step_by(N / DELETES) {
            store
                .delete_chunks_for_blob(&format!("blob{i}"))
                .await
                .unwrap();
        }
        let delete_elapsed = t3.elapsed();
        let remaining = store.count_rows().await.unwrap();
        assert!(
            remaining < N as u64,
            "delete-by-blob must have removed rows"
        );

        eprintln!(
            "[W2.3 checkpoint] N={N} bulk_upsert={upsert_elapsed:?} index_build={index_elapsed:?} \
             query={query_elapsed:?} delete_x{DELETES}={delete_elapsed:?} remaining={remaining}"
        );
    }
}
