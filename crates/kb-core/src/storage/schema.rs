//! Lance Arrow schema + `Doc` struct + `docs_to_batches`. Topic 01 §B
//! lists the canonical 25 fields. The embedding column is `FixedSizeList<
//! Float32, N>` with **nullable: true on the outer field** so v0.0.1 can
//! write rows without an embedding (a one-line gotcha the spike-lance
//! findings flagged).
//!
//! v0.6 / "embedding bake-off" milestone: `N` is per-kb. The schema +
//! batch builders take `dim: i32` so a daemon can host a 384-dim kb
//! (bge-small) and a 768-dim or 1024-dim kb (bge-base / bge-large) at
//! the same time. The dim is resolved at `Storage::open` time — either
//! read from an existing dataset's `embedding` field or derived from
//! the kb's configured `embedding_model` for a fresh dataset.

#[cfg(test)]
use arrow::array::Array;
use arrow::array::{
    BooleanArray, FixedSizeListArray, Float32Array, Int64Array, StringArray, UInt32Array,
};
use arrow::buffer::NullBuffer;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;

use crate::{Error, Result};

/// One indexed artifact row. Mirrors `kb_core::parser::Fields` plus the
/// path/mtime/size_kb metadata the indexer adds.
#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub id: String,
    pub path: String,
    pub title: String,
    pub body: String,
    pub headings: String,
    pub code: String,
    pub prompt: Option<String>,
    pub body_text_excerpt: String,
    pub embedding: Option<Vec<f32>>,
    pub kb_category: Option<String>,
    pub prompt_size_bytes: u32,
    pub size_kb: u32,
    pub js_loc: String,
    pub css_loc: String,
    pub svg_count: u32,
    pub has_svg: bool,
    pub has_form: bool,
    pub has_canvas: bool,
    pub has_animation: bool,
    pub has_details: bool,
    pub has_script: bool,
    pub has_drag: bool,
    pub has_math: bool,
    pub mtime_unix: i64,
    pub indexed_at_unix: i64,
    /// v0.15 — filesystem birth time (btime) of the source file, captured
    /// at index time. `None` on filesystems without btime support (some
    /// network mounts) and on rows indexed before the v15 add_columns
    /// migration.
    pub created_unix: Option<i64>,
    /// v0.6 I1 — gallery glyph + summary counters. All four columns
    /// are added via lance `add_columns` on daemon open; the indexer
    /// re-populates them on the next parse pass.
    pub table_count: u32,
    pub code_block_count: u32,
    pub word_count: u32,
    pub longread: bool,
    /// v0.6 T1 — comma-separated tag slugs. Either the parser's meta
    /// extraction or the indexer's path-derived fallback. Stored as a
    /// single string for compatibility with `add_columns` SQL
    /// migration; the API splits on `,` before sending JSON.
    pub tags_csv: String,
    /// v0.7 S1 — `<meta name="kb-status">` content, e.g. "open",
    /// "applied", "shipped", "draft". Free-form. None when absent.
    pub kb_status: Option<String>,
    /// v0.7 S1 — `<meta name="kb-severity">` content, e.g. "low",
    /// "medium", "high", "critical", "sev-1". Free-form. None when
    /// absent.
    pub kb_severity: Option<String>,
    /// v0.9 M1 — memory salience (importance) in 0..1. Drives recall
    /// ranking. None when absent (recall defaults to 0.5).
    pub kb_salience: Option<f32>,
    /// v0.9 M1 — memory recency fade rate ("slow" | "fast"). None when
    /// absent (recall treats as "slow").
    pub kb_decay: Option<String>,
    /// v0.9 M1 — artifact id this memory supersedes. None when absent.
    pub kb_supersedes: Option<String>,
    /// v0.14 S1 — id of the Claude Code session that produced this
    /// memory, from `<meta name="kb-session">`. Backwards links a
    /// memory to its origin conversation; used by
    /// `/api/sessions/{sid}/memories`. None when absent.
    pub kb_session: Option<String>,
    /// RA4 — one-line memory summary distinct from the title, from
    /// `<meta name="kb-summary">`. Surfaced in recall results / the SPA
    /// /memory view so a fact reads as a clean gloss, not a truncated
    /// title. None when absent.
    pub kb_summary: Option<String>,
    /// v0.16 — content hash of the source HTML bytes
    /// (`ArtifactId::from_html_bytes`), persisted alongside the row so
    /// the indexer can repopulate its in-memory dedup cache at startup.
    /// Without this, the watcher's initial-walk `watch.create` events
    /// re-embed every artifact on every daemon restart. Nullable: rows
    /// indexed before this column existed stay NULL until their next
    /// reindex.
    pub content_hash: Option<String>,
    /// N-track — GFM task-list progress (`task_done` / `task_total`). Counted
    /// by `parser::extract` from the rendered checkboxes; drives the notes
    /// progress bar + a future "has open todos" filter. Nullable: rows
    /// indexed before the v17 migration stay NULL until reindex. Always 0/0
    /// for artifacts without task lists.
    pub task_done: Option<u32>,
    pub task_total: Option<u32>,
    /// MI-W3.3a — optional CoALA-minimal memory-type classification
    /// (`episodic` | `semantic` | `procedural`), from
    /// `<meta name="kb-memory-type">`. Free-form string here (validated at
    /// the write boundary, `kb_core::memory::MemoryType`); `None` for the
    /// vast majority of the corpus — never inferred, never backfilled.
    pub kb_memory_type: Option<String>,
    /// MI-W3.4 — write-time trust tag (`fetched-web` | `user-dictated` |
    /// `agent-inference`), from `<meta name="kb-source">`. Same
    /// free-form-string precedent as `kb_memory_type`
    /// (`kb_core::memory::TrustSource` validates at the write boundary).
    /// SURFACED (census/recall), NEVER SCORED.
    pub kb_source: Option<String>,
    /// CT-A1 (U3 parse-back) — the `you`/`claude` role from `<meta
    /// name="kb-author">`. `None` for the vast majority of the corpus —
    /// only highlight-born memories carry this.
    pub kb_author: Option<String>,
    /// CT-A1 — kb name of the artifact this memory was highlighted FROM,
    /// from `<meta name="kb-source-kb">`. `None` when absent.
    pub kb_source_kb: Option<String>,
    /// CT-A1 — artifact id of that origin artifact, from
    /// `<meta name="kb-source-artifact">`. `None` when absent.
    pub kb_source_artifact: Option<String>,
    /// CT-A1 — the origin selection's `review::Anchor`, serialized via
    /// `lists::anchor_to_json`, from `<meta name="kb-source-anchor">`.
    /// `None` when absent.
    pub kb_source_anchor: Option<String>,
}

/// The Arrow schema for the `artifacts` lance table. `dim` is the
/// embedding-column width (384 for bge-small, 768 for bge-base, 1024
/// for bge-large) and is captured per-kb at `Storage::open` time.
pub fn schema(dim: i32) -> Schema {
    Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("title", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("headings", DataType::Utf8, false),
        Field::new("code", DataType::Utf8, false),
        Field::new("prompt", DataType::Utf8, true),
        Field::new("body_text_excerpt", DataType::Utf8, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), dim),
            // Outer nullable: v0.0.1 writes rows without embeddings.
            // Spike-lance had this `false` — the production fix.
            true,
        ),
        Field::new("kb_category", DataType::Utf8, true),
        Field::new("prompt_size_bytes", DataType::UInt32, false),
        Field::new("size_kb", DataType::UInt32, false),
        Field::new("js_loc", DataType::Utf8, false),
        Field::new("css_loc", DataType::Utf8, false),
        Field::new("svg_count", DataType::UInt32, false),
        Field::new("has_svg", DataType::Boolean, false),
        Field::new("has_form", DataType::Boolean, false),
        Field::new("has_canvas", DataType::Boolean, false),
        Field::new("has_animation", DataType::Boolean, false),
        Field::new("has_details", DataType::Boolean, false),
        Field::new("has_script", DataType::Boolean, false),
        Field::new("has_drag", DataType::Boolean, false),
        Field::new("has_math", DataType::Boolean, false),
        Field::new("mtime_unix", DataType::Int64, false),
        Field::new("indexed_at_unix", DataType::Int64, false),
        Field::new("table_count", DataType::UInt32, true),
        Field::new("code_block_count", DataType::UInt32, true),
        Field::new("word_count", DataType::UInt32, true),
        Field::new("longread", DataType::Boolean, true),
        Field::new("tags_csv", DataType::Utf8, true),
        Field::new("kb_status", DataType::Utf8, true),
        Field::new("kb_severity", DataType::Utf8, true),
        Field::new("kb_salience", DataType::Float32, true),
        Field::new("kb_decay", DataType::Utf8, true),
        Field::new("kb_supersedes", DataType::Utf8, true),
        Field::new("kb_session", DataType::Utf8, true),
        Field::new("created_unix", DataType::Int64, true),
        Field::new("content_hash", DataType::Utf8, true),
        Field::new("task_done", DataType::UInt32, true),
        Field::new("task_total", DataType::UInt32, true),
        Field::new("kb_summary", DataType::Utf8, true),
        Field::new("kb_memory_type", DataType::Utf8, true),
        Field::new("kb_source", DataType::Utf8, true),
        Field::new("kb_author", DataType::Utf8, true),
        Field::new("kb_source_kb", DataType::Utf8, true),
        Field::new("kb_source_artifact", DataType::Utf8, true),
        Field::new("kb_source_anchor", DataType::Utf8, true),
    ])
}

/// Pack a slice of `Doc` into a `Vec<RecordBatch>` directly usable as
/// `lancedb::Scannable` for `Connection::create_table` or `Table::add`.
///
/// `dim` must match the kb's embedding column width. Any `Doc` with
/// `embedding = Some(v)` is validated against `dim`; a mismatch is
/// returned as `Error::Storage` (pre-bake-off this was a panic that
/// killed the storage actor — now it's a recoverable per-batch error
/// the caller logs and drops).
pub fn docs_to_batches(docs: &[Doc], dim: i32) -> Result<Vec<RecordBatch>> {
    if docs.is_empty() {
        return Ok(Vec::new());
    }
    let schema = Arc::new(schema(dim));

    let ids: StringArray = docs.iter().map(|d| Some(d.id.as_str())).collect();
    let paths: StringArray = docs.iter().map(|d| Some(d.path.as_str())).collect();
    let titles: StringArray = docs.iter().map(|d| Some(d.title.as_str())).collect();
    let bodies: StringArray = docs.iter().map(|d| Some(d.body.as_str())).collect();
    let headings: StringArray = docs.iter().map(|d| Some(d.headings.as_str())).collect();
    let code: StringArray = docs.iter().map(|d| Some(d.code.as_str())).collect();
    let prompts: StringArray = docs.iter().map(|d| d.prompt.as_deref()).collect();
    let body_excerpts: StringArray = docs
        .iter()
        .map(|d| Some(d.body_text_excerpt.as_str()))
        .collect();
    let categories: StringArray = docs.iter().map(|d| d.kb_category.as_deref()).collect();
    let js_locs: StringArray = docs.iter().map(|d| Some(d.js_loc.as_str())).collect();
    let css_locs: StringArray = docs.iter().map(|d| Some(d.css_loc.as_str())).collect();

    let prompt_sizes = UInt32Array::from_iter_values(docs.iter().map(|d| d.prompt_size_bytes));
    let size_kbs = UInt32Array::from_iter_values(docs.iter().map(|d| d.size_kb));
    let svg_counts = UInt32Array::from_iter_values(docs.iter().map(|d| d.svg_count));

    let has_svg = BooleanArray::from_iter(docs.iter().map(|d| Some(d.has_svg)));
    let has_form = BooleanArray::from_iter(docs.iter().map(|d| Some(d.has_form)));
    let has_canvas = BooleanArray::from_iter(docs.iter().map(|d| Some(d.has_canvas)));
    let has_animation = BooleanArray::from_iter(docs.iter().map(|d| Some(d.has_animation)));
    let has_details = BooleanArray::from_iter(docs.iter().map(|d| Some(d.has_details)));
    let has_script = BooleanArray::from_iter(docs.iter().map(|d| Some(d.has_script)));
    let has_drag = BooleanArray::from_iter(docs.iter().map(|d| Some(d.has_drag)));
    let has_math = BooleanArray::from_iter(docs.iter().map(|d| Some(d.has_math)));

    let mtimes = Int64Array::from_iter_values(docs.iter().map(|d| d.mtime_unix));
    let indexed_ats = Int64Array::from_iter_values(docs.iter().map(|d| d.indexed_at_unix));

    let table_counts: UInt32Array =
        UInt32Array::from_iter(docs.iter().map(|d| Some(d.table_count)));
    let code_block_counts: UInt32Array =
        UInt32Array::from_iter(docs.iter().map(|d| Some(d.code_block_count)));
    let word_counts: UInt32Array = UInt32Array::from_iter(docs.iter().map(|d| Some(d.word_count)));
    let longreads: BooleanArray = BooleanArray::from_iter(docs.iter().map(|d| Some(d.longread)));
    let tags_csv: StringArray = docs
        .iter()
        .map(|d| {
            if d.tags_csv.is_empty() {
                None
            } else {
                Some(d.tags_csv.as_str())
            }
        })
        .collect();
    let kb_statuses: StringArray = docs.iter().map(|d| d.kb_status.as_deref()).collect();
    let kb_severities: StringArray = docs.iter().map(|d| d.kb_severity.as_deref()).collect();
    // M1 — salience is a nullable Float32 (collecting Option<f32> yields
    // a validity-aware array, unlike the non-null `from_iter_values` used
    // for the always-present numeric columns above).
    let kb_saliences: Float32Array = docs.iter().map(|d| d.kb_salience).collect();
    let kb_decays: StringArray = docs.iter().map(|d| d.kb_decay.as_deref()).collect();
    let kb_supersedes: StringArray = docs.iter().map(|d| d.kb_supersedes.as_deref()).collect();
    let kb_sessions: StringArray = docs.iter().map(|d| d.kb_session.as_deref()).collect();
    // v0.15 — nullable: collecting Option<i64> yields a validity-aware
    // array so rows on btime-less filesystems can carry None.
    let created_unixes: Int64Array = docs.iter().map(|d| d.created_unix).collect();
    // v0.16 — nullable content hash; old rows stay NULL until reindex.
    let content_hashes: StringArray = docs.iter().map(|d| d.content_hash.as_deref()).collect();
    // N-track — nullable task counts (Option<u32> → validity-aware array).
    let task_dones: UInt32Array = docs.iter().map(|d| d.task_done).collect();
    let task_totals: UInt32Array = docs.iter().map(|d| d.task_total).collect();
    // RA4 — nullable one-line summary; old rows stay NULL until reindex.
    let kb_summaries: StringArray = docs.iter().map(|d| d.kb_summary.as_deref()).collect();
    // MI-W3.3a / MI-W3.4 — nullable; old rows stay NULL until reindex.
    let kb_memory_types: StringArray = docs.iter().map(|d| d.kb_memory_type.as_deref()).collect();
    let kb_sources: StringArray = docs.iter().map(|d| d.kb_source.as_deref()).collect();
    // CT-A1 — U3 provenance; nullable, old rows stay NULL until reindex.
    let kb_authors: StringArray = docs.iter().map(|d| d.kb_author.as_deref()).collect();
    let kb_source_kbs: StringArray = docs.iter().map(|d| d.kb_source_kb.as_deref()).collect();
    let kb_source_artifacts: StringArray = docs
        .iter()
        .map(|d| d.kb_source_artifact.as_deref())
        .collect();
    let kb_source_anchors: StringArray =
        docs.iter().map(|d| d.kb_source_anchor.as_deref()).collect();

    let embeddings = build_embedding_column(docs, dim)?;

    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(ids),
            Arc::new(paths),
            Arc::new(titles),
            Arc::new(bodies),
            Arc::new(headings),
            Arc::new(code),
            Arc::new(prompts),
            Arc::new(body_excerpts),
            Arc::new(embeddings),
            Arc::new(categories),
            Arc::new(prompt_sizes),
            Arc::new(size_kbs),
            Arc::new(js_locs),
            Arc::new(css_locs),
            Arc::new(svg_counts),
            Arc::new(has_svg),
            Arc::new(has_form),
            Arc::new(has_canvas),
            Arc::new(has_animation),
            Arc::new(has_details),
            Arc::new(has_script),
            Arc::new(has_drag),
            Arc::new(has_math),
            Arc::new(mtimes),
            Arc::new(indexed_ats),
            Arc::new(table_counts),
            Arc::new(code_block_counts),
            Arc::new(word_counts),
            Arc::new(longreads),
            Arc::new(tags_csv),
            Arc::new(kb_statuses),
            Arc::new(kb_severities),
            Arc::new(kb_saliences),
            Arc::new(kb_decays),
            Arc::new(kb_supersedes),
            Arc::new(kb_sessions),
            Arc::new(created_unixes),
            Arc::new(content_hashes),
            Arc::new(task_dones),
            Arc::new(task_totals),
            Arc::new(kb_summaries),
            Arc::new(kb_memory_types),
            Arc::new(kb_sources),
            Arc::new(kb_authors),
            Arc::new(kb_source_kbs),
            Arc::new(kb_source_artifacts),
            Arc::new(kb_source_anchors),
        ],
    )
    .map_err(|e| Error::Storage(format!("docs_to_batches: RecordBatch::try_new: {e}")))?;

    Ok(vec![batch])
}

fn build_embedding_column(docs: &[Doc], dim: i32) -> Result<FixedSizeListArray> {
    let n = docs.len();
    let dim_usize = dim as usize;

    let mut values: Vec<f32> = Vec::with_capacity(n * dim_usize);
    let mut validity: Vec<bool> = Vec::with_capacity(n);

    for doc in docs {
        match &doc.embedding {
            Some(v) => {
                if v.len() != dim_usize {
                    return Err(Error::Storage(format!(
                        "doc {}: embedding dim {} != configured kb dim {}",
                        doc.id,
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
    Ok(FixedSizeListArray::new(
        item_field,
        dim,
        Arc::new(inner),
        Some(nulls),
    ))
}

/// Convenience: build a `Doc` with all-empty/false defaults; tests fill in
/// only the fields they care about.
impl Doc {
    pub fn placeholder(id: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            path: path.into(),
            title: String::new(),
            body: String::new(),
            headings: String::new(),
            code: String::new(),
            prompt: None,
            body_text_excerpt: String::new(),
            embedding: None,
            kb_category: None,
            prompt_size_bytes: 0,
            size_kb: 0,
            js_loc: "static".into(),
            css_loc: "static".into(),
            svg_count: 0,
            has_svg: false,
            has_form: false,
            has_canvas: false,
            has_animation: false,
            has_details: false,
            has_script: false,
            has_drag: false,
            has_math: false,
            mtime_unix: 0,
            indexed_at_unix: 0,
            table_count: 0,
            code_block_count: 0,
            word_count: 0,
            longread: false,
            tags_csv: String::new(),
            kb_status: None,
            kb_severity: None,
            kb_salience: None,
            kb_decay: None,
            kb_supersedes: None,
            kb_session: None,
            created_unix: None,
            content_hash: None,
            task_done: None,
            task_total: None,
            kb_summary: None,
            kb_memory_type: None,
            kb_source: None,
            kb_author: None,
            kb_source_kb: None,
            kb_source_artifact: None,
            kb_source_anchor: None,
        }
    }
}

// ---- SQ5 — passage/chunk table -----------------------------------------

/// One embeddable passage row for the sibling `artifact_chunks` table.
/// `chunk_id` (`"{doc_id}#{chunk_idx}"`) is the merge_insert key; `doc_id`
/// links back to `artifacts.id`. `embedding` is outer-nullable like the
/// doc table so a chunk whose embed failed still writes its text row.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkDoc {
    pub chunk_id: String,
    pub doc_id: String,
    pub chunk_idx: u32,
    pub text: String,
    pub embedding: Option<Vec<f32>>,
}

/// Arrow schema for the `artifact_chunks` lance table. `dim` is the same
/// per-kb embedding width as the doc table (a chunk is embedded by the
/// same model).
pub fn chunk_schema(dim: i32) -> Schema {
    Schema::new(vec![
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("doc_id", DataType::Utf8, false),
        Field::new("chunk_idx", DataType::UInt32, false),
        Field::new("text", DataType::Utf8, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), dim),
            true,
        ),
    ])
}

/// Pack `ChunkDoc`s into a `RecordBatch`. Like [`docs_to_batches`], a
/// wrong-width embedding returns `Error::Storage` rather than panicking
/// the storage actor.
pub fn chunks_to_batches(chunks: &[ChunkDoc], dim: i32) -> Result<Vec<RecordBatch>> {
    if chunks.is_empty() {
        return Ok(Vec::new());
    }
    let schema = Arc::new(chunk_schema(dim));
    let chunk_ids: StringArray = chunks.iter().map(|c| Some(c.chunk_id.as_str())).collect();
    let doc_ids: StringArray = chunks.iter().map(|c| Some(c.doc_id.as_str())).collect();
    let idxs = UInt32Array::from_iter_values(chunks.iter().map(|c| c.chunk_idx));
    let texts: StringArray = chunks.iter().map(|c| Some(c.text.as_str())).collect();

    let dim_usize = dim as usize;
    let mut values: Vec<f32> = Vec::with_capacity(chunks.len() * dim_usize);
    let mut validity: Vec<bool> = Vec::with_capacity(chunks.len());
    for c in chunks {
        match &c.embedding {
            Some(v) => {
                if v.len() != dim_usize {
                    return Err(Error::Storage(format!(
                        "chunk {}: embedding dim {} != configured kb dim {}",
                        c.chunk_id,
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
            Arc::new(doc_ids),
            Arc::new(idxs),
            Arc::new(texts),
            Arc::new(embeddings),
        ],
    )
    .map_err(|e| Error::Storage(format!("chunks_to_batches: RecordBatch::try_new: {e}")))?;
    Ok(vec![batch])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience for tests that stay bge-small-specific (most of them).
    /// New tests that exercise multi-dim use [`schema(dim)`] directly.
    const TEST_DIM: i32 = 384;

    #[test]
    fn chunk_schema_shape_and_nullability() {
        let s = chunk_schema(TEST_DIM);
        let names: Vec<_> = s.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(
            names,
            vec!["chunk_id", "doc_id", "chunk_idx", "text", "embedding"]
        );
        assert!(s.field_with_name("embedding").unwrap().is_nullable());
        assert!(!s.field_with_name("chunk_id").unwrap().is_nullable());
    }

    #[test]
    fn chunks_round_trip_with_and_without_embedding() {
        let chunks = vec![
            ChunkDoc {
                chunk_id: "doc1#0".into(),
                doc_id: "doc1".into(),
                chunk_idx: 0,
                text: "hello".into(),
                embedding: Some(vec![0.1; TEST_DIM as usize]),
            },
            ChunkDoc {
                chunk_id: "doc1#1".into(),
                doc_id: "doc1".into(),
                chunk_idx: 1,
                text: "world".into(),
                embedding: None,
            },
        ];
        let batches = chunks_to_batches(&chunks, TEST_DIM).unwrap();
        let b = &batches[0];
        assert_eq!(b.num_rows(), 2);
        let emb = b
            .column_by_name("embedding")
            .unwrap()
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        assert!(!emb.is_null(0));
        assert!(emb.is_null(1));
    }

    #[test]
    fn chunk_dim_mismatch_is_storage_error() {
        let chunks = vec![ChunkDoc {
            chunk_id: "d#0".into(),
            doc_id: "d".into(),
            chunk_idx: 0,
            text: "x".into(),
            embedding: Some(vec![0.0; 768]),
        }];
        match chunks_to_batches(&chunks, 384) {
            Err(Error::Storage(msg)) => assert!(msg.contains("d#0") && msg.contains("768")),
            other => panic!("expected Storage error, got {other:?}"),
        }
    }

    #[test]
    fn empty_chunks_yields_empty() {
        assert!(chunks_to_batches(&[], TEST_DIM).unwrap().is_empty());
    }

    #[test]
    fn schema_has_40_fields_in_order() {
        let s = schema(TEST_DIM);
        assert_eq!(s.fields().len(), 47);
        let names: Vec<_> = s.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(names[0], "id");
        assert_eq!(names[8], "embedding");
        assert!(names.contains(&"body_text_excerpt"));
        assert!(names.contains(&"has_drag"));
        assert!(names.contains(&"kb_category"));
        // v0.6 I1 — new counters/flag.
        assert!(names.contains(&"table_count"));
        assert!(names.contains(&"code_block_count"));
        assert!(names.contains(&"word_count"));
        assert!(names.contains(&"longread"));
        // v0.6 T1 — tags.
        assert!(names.contains(&"tags_csv"));
        // v0.7 S1 — status + severity.
        assert!(names.contains(&"kb_status"));
        assert!(names.contains(&"kb_severity"));
        // v0.9 M1 — memory metas.
        assert!(names.contains(&"kb_salience"));
        assert!(names.contains(&"kb_decay"));
        assert!(names.contains(&"kb_supersedes"));
        // v0.14 S1 — origin session id.
        assert!(names.contains(&"kb_session"));
        // RA4 — one-line memory summary.
        assert!(names.contains(&"kb_summary"));
        // v0.15 — btime captured at index time.
        assert!(names.contains(&"created_unix"));
        // v0.16 — content hash for startup dedup cache.
        assert!(names.contains(&"content_hash"));
        // N-track — task-list progress counts.
        assert!(names.contains(&"task_done"));
        assert!(names.contains(&"task_total"));
        // MI-W3.3a / MI-W3.4 — memory type + trust source.
        assert!(names.contains(&"kb_memory_type"));
        assert!(names.contains(&"kb_source"));
        // CT-A1 (U3 parse-back) — highlight provenance.
        assert!(names.contains(&"kb_author"));
        assert!(names.contains(&"kb_source_kb"));
        assert!(names.contains(&"kb_source_artifact"));
        assert!(names.contains(&"kb_source_anchor"));
    }

    #[test]
    fn u3_provenance_fields_are_nullable() {
        let s = schema(TEST_DIM);
        assert!(s.field_with_name("kb_author").unwrap().is_nullable());
        assert!(s.field_with_name("kb_source_kb").unwrap().is_nullable());
        assert!(s
            .field_with_name("kb_source_artifact")
            .unwrap()
            .is_nullable());
        assert!(s.field_with_name("kb_source_anchor").unwrap().is_nullable());
    }

    #[test]
    fn u3_provenance_serialises_round_trip() {
        let mut doc = Doc::placeholder("a", "/tmp/a");
        doc.kb_author = Some("you".into());
        doc.kb_source_kb = Some("kb-docs".into());
        doc.kb_source_artifact = Some("a1b2c3d4e5f6".into());
        doc.kb_source_anchor = Some(r#"{"kind":"section","id":"intro"}"#.into());
        let batches = docs_to_batches(&[doc], TEST_DIM).unwrap();
        let batch = &batches[0];
        assert!(!batch.column_by_name("kb_author").unwrap().is_null(0));
        assert!(!batch.column_by_name("kb_source_kb").unwrap().is_null(0));
        assert!(!batch
            .column_by_name("kb_source_artifact")
            .unwrap()
            .is_null(0));
        assert!(!batch.column_by_name("kb_source_anchor").unwrap().is_null(0));

        let doc = Doc::placeholder("b", "/tmp/b");
        let batches = docs_to_batches(&[doc], TEST_DIM).unwrap();
        let batch = &batches[0];
        assert!(batch.column_by_name("kb_author").unwrap().is_null(0));
        assert!(batch.column_by_name("kb_source_kb").unwrap().is_null(0));
        assert!(batch
            .column_by_name("kb_source_artifact")
            .unwrap()
            .is_null(0));
        assert!(batch.column_by_name("kb_source_anchor").unwrap().is_null(0));
    }

    #[test]
    fn memory_type_and_source_fields_are_nullable() {
        let s = schema(TEST_DIM);
        assert!(s.field_with_name("kb_memory_type").unwrap().is_nullable());
        assert!(s.field_with_name("kb_source").unwrap().is_nullable());
    }

    #[test]
    fn memory_type_and_source_serialise_round_trip() {
        let mut doc = Doc::placeholder("a", "/tmp/a");
        doc.kb_memory_type = Some("semantic".into());
        doc.kb_source = Some("fetched-web".into());
        let batches = docs_to_batches(&[doc], TEST_DIM).unwrap();
        let batch = &batches[0];
        assert!(!batch.column_by_name("kb_memory_type").unwrap().is_null(0));
        assert!(!batch.column_by_name("kb_source").unwrap().is_null(0));

        let doc = Doc::placeholder("b", "/tmp/b");
        let batches = docs_to_batches(&[doc], TEST_DIM).unwrap();
        let batch = &batches[0];
        assert!(batch.column_by_name("kb_memory_type").unwrap().is_null(0));
        assert!(batch.column_by_name("kb_source").unwrap().is_null(0));
    }

    #[test]
    fn task_columns_are_nullable() {
        let s = schema(TEST_DIM);
        assert!(s.field_with_name("task_done").unwrap().is_nullable());
        assert!(s.field_with_name("task_total").unwrap().is_nullable());
    }

    #[test]
    fn task_columns_serialise_round_trip() {
        let mut doc = Doc::placeholder("a", "/tmp/a");
        doc.task_done = Some(2);
        doc.task_total = Some(5);
        let batches = docs_to_batches(&[doc], TEST_DIM).unwrap();
        let batch = &batches[0];
        assert!(!batch.column_by_name("task_done").unwrap().is_null(0));
        assert!(!batch.column_by_name("task_total").unwrap().is_null(0));

        let doc = Doc::placeholder("b", "/tmp/b");
        let batches = docs_to_batches(&[doc], TEST_DIM).unwrap();
        let batch = &batches[0];
        assert!(batch.column_by_name("task_done").unwrap().is_null(0));
        assert!(batch.column_by_name("task_total").unwrap().is_null(0));
    }

    #[test]
    fn kb_status_and_severity_fields_are_nullable() {
        let s = schema(TEST_DIM);
        assert!(s.field_with_name("kb_status").unwrap().is_nullable());
        assert!(s.field_with_name("kb_severity").unwrap().is_nullable());
    }

    #[test]
    fn memory_meta_fields_are_nullable() {
        let s = schema(TEST_DIM);
        assert!(s.field_with_name("kb_salience").unwrap().is_nullable());
        assert!(s.field_with_name("kb_decay").unwrap().is_nullable());
        assert!(s.field_with_name("kb_supersedes").unwrap().is_nullable());
        assert!(s.field_with_name("kb_session").unwrap().is_nullable());
    }

    #[test]
    fn memory_metas_serialise_round_trip() {
        let mut doc = Doc::placeholder("a", "/tmp/a");
        doc.kb_salience = Some(0.8);
        doc.kb_decay = Some("fast".into());
        doc.kb_supersedes = Some("7f3a1c0d2e4b".into());
        doc.kb_session = Some("session-abc".into());
        let batches = docs_to_batches(&[doc], TEST_DIM).unwrap();
        let batch = &batches[0];
        assert!(!batch.column_by_name("kb_salience").unwrap().is_null(0));
        assert!(!batch.column_by_name("kb_decay").unwrap().is_null(0));
        assert!(!batch.column_by_name("kb_supersedes").unwrap().is_null(0));
        assert!(!batch.column_by_name("kb_session").unwrap().is_null(0));

        let doc = Doc::placeholder("b", "/tmp/b");
        let batches = docs_to_batches(&[doc], TEST_DIM).unwrap();
        let batch = &batches[0];
        assert!(batch.column_by_name("kb_salience").unwrap().is_null(0));
        assert!(batch.column_by_name("kb_decay").unwrap().is_null(0));
        assert!(batch.column_by_name("kb_supersedes").unwrap().is_null(0));
        assert!(batch.column_by_name("kb_session").unwrap().is_null(0));
    }

    #[test]
    fn kb_status_and_severity_serialise_round_trip() {
        let mut doc = Doc::placeholder("a", "/tmp/a");
        doc.kb_status = Some("open".into());
        doc.kb_severity = Some("medium".into());
        let batches = docs_to_batches(&[doc], TEST_DIM).unwrap();
        let batch = &batches[0];
        assert!(!batch.column_by_name("kb_status").unwrap().is_null(0));
        assert!(!batch.column_by_name("kb_severity").unwrap().is_null(0));

        let mut doc = Doc::placeholder("b", "/tmp/b");
        doc.kb_status = None;
        doc.kb_severity = None;
        let batches = docs_to_batches(&[doc], TEST_DIM).unwrap();
        let batch = &batches[0];
        assert!(batch.column_by_name("kb_status").unwrap().is_null(0));
        assert!(batch.column_by_name("kb_severity").unwrap().is_null(0));
    }

    #[test]
    fn embedding_outer_field_is_nullable() {
        let s = schema(TEST_DIM);
        let field = s.field_with_name("embedding").unwrap();
        assert!(field.is_nullable(), "v0.0.1 must allow null embeddings");
    }

    #[test]
    fn prompt_field_is_nullable() {
        let s = schema(TEST_DIM);
        let field = s.field_with_name("prompt").unwrap();
        assert!(field.is_nullable());
    }

    #[test]
    fn kb_category_field_is_nullable() {
        let s = schema(TEST_DIM);
        let field = s.field_with_name("kb_category").unwrap();
        assert!(field.is_nullable());
    }

    #[test]
    fn empty_docs_yields_empty_batches() {
        let batches = docs_to_batches(&[], TEST_DIM).unwrap();
        assert!(batches.is_empty());
    }

    #[test]
    fn single_doc_with_no_embedding_round_trips() {
        let doc = Doc::placeholder("abc123", "/tmp/x.html");
        let batches = docs_to_batches(std::slice::from_ref(&doc), TEST_DIM).unwrap();
        assert_eq!(batches.len(), 1);
        let batch = &batches[0];
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), 47);

        // Embedding column should have the row marked null.
        let emb = batch
            .column_by_name("embedding")
            .unwrap()
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        assert!(emb.is_null(0));
    }

    #[test]
    fn doc_with_embedding_marks_row_non_null() {
        let mut doc = Doc::placeholder("abc123", "/tmp/x.html");
        doc.embedding = Some(vec![0.0; TEST_DIM as usize]);
        let batches = docs_to_batches(&[doc], TEST_DIM).unwrap();
        let batch = &batches[0];
        let emb = batch
            .column_by_name("embedding")
            .unwrap()
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        assert!(!emb.is_null(0));
    }

    #[test]
    fn mixed_docs_handles_some_with_embeddings() {
        let mut a = Doc::placeholder("a", "/tmp/a");
        let mut b = Doc::placeholder("b", "/tmp/b");
        let mut c = Doc::placeholder("c", "/tmp/c");
        b.embedding = Some(vec![0.5; TEST_DIM as usize]);
        // a, c have None.
        a.has_svg = true;
        c.has_drag = true;

        let batches = docs_to_batches(&[a, b, c], TEST_DIM).unwrap();
        let batch = &batches[0];
        assert_eq!(batch.num_rows(), 3);

        let emb = batch
            .column_by_name("embedding")
            .unwrap()
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        assert!(emb.is_null(0));
        assert!(!emb.is_null(1));
        assert!(emb.is_null(2));
    }

    #[test]
    fn nullable_columns_serialise_none_as_null() {
        let mut doc = Doc::placeholder("a", "/tmp/a");
        doc.prompt = None;
        doc.kb_category = None;
        let batches = docs_to_batches(&[doc], TEST_DIM).unwrap();
        let batch = &batches[0];
        assert!(batch.column_by_name("prompt").unwrap().is_null(0));
        assert!(batch.column_by_name("kb_category").unwrap().is_null(0));
    }

    #[test]
    fn nullable_columns_serialise_some_as_value() {
        let mut doc = Doc::placeholder("a", "/tmp/a");
        doc.prompt = Some("write a haiku".into());
        doc.kb_category = Some("Exploration".into());
        let batches = docs_to_batches(&[doc], TEST_DIM).unwrap();
        let batch = &batches[0];
        assert!(!batch.column_by_name("prompt").unwrap().is_null(0));
        assert!(!batch.column_by_name("kb_category").unwrap().is_null(0));
    }

    // ---- multi-dim ----
    // Bake-off A1: schema(dim) + docs_to_batches(_, dim) must round-trip
    // at every supported dim (bge-small 384, bge-base 768, bge-large
    // 1024). One test per dim keeps the failure attribution clean.

    #[test]
    fn schema_embedding_column_width_matches_dim_768() {
        let s = schema(768);
        let f = s.field_with_name("embedding").unwrap();
        if let DataType::FixedSizeList(_, n) = f.data_type() {
            assert_eq!(*n, 768);
        } else {
            panic!("expected FixedSizeList, got {:?}", f.data_type());
        }
    }

    #[test]
    fn schema_embedding_column_width_matches_dim_1024() {
        let s = schema(1024);
        let f = s.field_with_name("embedding").unwrap();
        if let DataType::FixedSizeList(_, n) = f.data_type() {
            assert_eq!(*n, 1024);
        } else {
            panic!("expected FixedSizeList, got {:?}", f.data_type());
        }
    }

    #[test]
    fn doc_with_768_dim_embedding_round_trips() {
        let mut doc = Doc::placeholder("d768", "/tmp/x.html");
        doc.embedding = Some(vec![0.1; 768]);
        let batches = docs_to_batches(&[doc], 768).unwrap();
        let batch = &batches[0];
        let emb = batch
            .column_by_name("embedding")
            .unwrap()
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        assert!(!emb.is_null(0));
        assert_eq!(emb.value_length(), 768);
    }

    #[test]
    fn doc_with_1024_dim_embedding_round_trips() {
        let mut doc = Doc::placeholder("d1024", "/tmp/x.html");
        doc.embedding = Some(vec![0.2; 1024]);
        let batches = docs_to_batches(&[doc], 1024).unwrap();
        let batch = &batches[0];
        let emb = batch
            .column_by_name("embedding")
            .unwrap()
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap();
        assert!(!emb.is_null(0));
        assert_eq!(emb.value_length(), 1024);
    }

    #[test]
    fn dim_mismatch_returns_storage_error_not_panic() {
        // Pre-bake-off this was `assert_eq!(...)` which panicked the
        // storage actor's tokio task. Post-A1 it returns
        // Error::Storage so the caller can log + drop the bad batch.
        let mut doc = Doc::placeholder("oops", "/tmp/x.html");
        doc.embedding = Some(vec![0.0; 768]); // wrong dim
        let result = docs_to_batches(&[doc], 384);
        match result {
            Err(Error::Storage(msg)) => {
                assert!(
                    msg.contains("oops"),
                    "error must name the offending doc id: {msg}"
                );
                assert!(msg.contains("768"), "error must report actual dim: {msg}");
                assert!(
                    msg.contains("384"),
                    "error must report configured dim: {msg}"
                );
            }
            other => panic!("expected Error::Storage, got {other:?}"),
        }
    }
}
