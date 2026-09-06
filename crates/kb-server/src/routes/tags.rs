//! v0.6 T1 — `GET /api/kb/{kb}/tags`. Aggregates the `tags_csv`
//! column across every doc in the kb and returns a frequency-sorted
//! list with a stable FNV-1a color seed per name. The SPA's LeftRail
//! reads this to populate the tags filter section.
//!
//! S3 (S-milestone): drops the 2000-doc scan cap. `aggregate_tags`
//! walks the full corpus, sorted by (count desc, name asc). The
//! caller-side truncate to MAX_TAGS_RETURNED is still applied so the
//! rail stays navigable on a long-tail kb; everything past the cap is
//! still query-accessible via `/api/kb/<kb>/docs?tags=...`.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::docs_query::aggregate_tags;
use serde::Serialize;
use std::sync::Arc;

const MAX_TAGS_RETURNED: usize = 50;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct TagSummary {
    pub name: String,
    pub count: u32,
    /// Stable hash for the frontend to pick a deterministic color.
    /// FNV-1a of the slug; the SPA maps this to an HSL hue.
    pub color_seed: u32,
}

pub async fn list(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    // Reuse the gallery memo's single per-(kb, index-generation)
    // `list_docs(u32::MAX)` scan + decorated rows instead of running our own
    // (invariant #15). `aggregate_tags` reads only `doc.tags`, so the shared
    // rows serve it directly — no per-request rescan and no throwaway
    // `doc_folder` decoration. Warm generation ⇒ zero extra lance scans.
    // (Notes carry tags too; the aggregate has always counted them and the
    // cache is not note-filtered, so counts are byte-identical.)
    let (rows, _edge_counts) = match crate::routes::docs::gallery_snapshot(ctx).await {
        Ok(s) => s,
        Err(e) => return error_to_problem_json(&e),
    };

    let mut summaries: Vec<TagSummary> = aggregate_tags(&rows)
        .into_iter()
        .map(|b| TagSummary {
            color_seed: fnv1a(&b.name),
            name: b.name,
            count: b.count,
        })
        .collect();
    summaries.truncate(MAX_TAGS_RETURNED);

    Json(summaries).into_response()
}

fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}
