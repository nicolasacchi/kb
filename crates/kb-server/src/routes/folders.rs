//! `GET /api/kb/{kb}/folders` — folder tree aggregator. Walks the full
//! corpus, derives each row's folder relative to the kb source root
//! (`kb_core::paths::doc_folder`), then folds the results into a
//! deterministically-ordered tree with descendant-inclusive counts via
//! `kb_core::docs_query::aggregate_folders`.
//!
//! The SPA's LeftRail renders this as an expandable filter section
//! between Tags and Capabilities. Tree shape:
//!
//! ```json
//! {
//!   "folders": [
//!     { "path": "changelog", "count": 14,
//!       "children": [{"path": "changelog/daily", "count": 14, "children": []}] },
//!     { "path": "incidents", "count": 8, "children": [...] }
//!   ]
//! }
//! ```
//!
//! Counts are descendant-inclusive so clicking a folder previews the
//! filter blast radius — clicking `changelog` filters to all 14, not
//! just the docs directly under it (matches the
//! subfolder-inclusive filter semantics in `gallery.tsx`).
//!
//! S3 (S-milestone): the previous 2000-doc scan cap is gone. Aggregate
//! walks the entire corpus (slim projection — no body/embedding); at
//! ~50k rows the bucket fold takes <30 ms, the lance scan dominates.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::docs_query::{aggregate_folders, DocRow, FolderNode};
use serde::Serialize;
use std::sync::Arc;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct FoldersResponse {
    pub folders: Vec<FolderNode>,
}

pub async fn list(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    // Reuse the gallery memo's single per-(kb, index-generation)
    // `list_docs(u32::MAX)` scan + folder decoration instead of running our
    // own (invariant #15). Warm generation ⇒ zero extra lance scans.
    let (rows, _edge_counts) = match crate::routes::docs::gallery_snapshot(ctx).await {
        Ok(s) => s,
        Err(e) => return error_to_problem_json(&e),
    };
    // N-track — editable notes (Markdown + `kb-category: note`) are excluded
    // from the gallery grid, so keep them out of the folder counts too (else
    // a folder shows "3" but the grid renders 1). HTML artifacts merely
    // tagged `note` are NOT notes and stay counted. The cache is NOT
    // note-filtered (filtering is downstream of the memo — invariant #16),
    // so re-apply the filter here over the shared rows. Notes remain
    // reachable via the /notes view.
    let decorated: Vec<DocRow> = rows
        .iter()
        .filter(|r| !kb_core::notes::is_note(&r.doc.path, r.doc.kb_category.as_deref()))
        .cloned()
        .collect();

    Json(FoldersResponse {
        folders: aggregate_folders(&decorated),
    })
    .into_response()
}
