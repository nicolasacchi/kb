//! Q-track — `GET /api/kb/{kb}/facets`. One corpus scan feeds the search
//! rail's category / status / severity dropdowns (distinct values + counts,
//! sorted by frequency). Parallel to [`crate::routes::tags`]; sessions have
//! their own `/api/sessions` endpoint and aren't duplicated here.
//!
//! SC3 — the three aggregate folds are served from a per-(kb, index-
//! generation) memo (see [`facets_snapshot`]) instead of re-running on every
//! call; measured 17.8ms idle → up to 3.3s under load pre-memo (the one read
//! endpoint invariant #15's gallery memo hadn't reached yet).

use crate::middleware::error_to_problem_json;
use crate::state::{FacetsCache, KbContext, KbHandles};
use axum::{
    body::Body,
    extract::{Path, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::docs_query::{
    aggregate_categories, aggregate_severities, aggregate_statuses, FacetBucket,
};
use serde::Serialize;
use std::sync::Arc;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct FacetsResponse {
    pub categories: Vec<FacetBucket>,
    pub statuses: Vec<FacetBucket>,
    pub severities: Vec<FacetBucket>,
}

pub async fn list(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let (categories, statuses, severities) = match facets_snapshot(ctx).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };

    let body = FacetsResponse {
        categories: (*categories).clone(),
        statuses: (*statuses).clone(),
        severities: (*severities).clone(),
    };
    Json(body).into_response()
}

/// SC3 — return the three facet buckets for `ctx`, served from the per-(kb,
/// index-generation) memo when warm. On a generation change (or the first
/// call) it borrows the gallery memo's row-set
/// ([`crate::routes::docs::gallery_snapshot`] — so the two memos share ONE
/// `list_docs(u32::MAX)` scan, mirroring [`crate::routes::links::links_index`])
/// and runs the three folds, then publishes the snapshot.
///
/// Correctness mirrors `gallery_snapshot` (invariant #15): the generation is
/// read BEFORE the rebuild, the `std::sync::Mutex` guard is always dropped
/// before any `.await`, and a hit requires `stored.generation ==
/// index_generation()` (strict `==`, monotonic counter) — a mutation that
/// bumps the generation mid-rebuild only forces the next request to miss,
/// never a stale serve.
pub(crate) async fn facets_snapshot(
    ctx: &KbContext,
) -> Result<
    (
        Arc<Vec<FacetBucket>>,
        Arc<Vec<FacetBucket>>,
        Arc<Vec<FacetBucket>>,
    ),
    kb_core::Error,
> {
    let generation = ctx.storage.index_generation();
    {
        let guard = ctx.facets_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = guard.as_ref() {
            if c.generation == generation {
                return Ok((
                    Arc::clone(&c.categories),
                    Arc::clone(&c.statuses),
                    Arc::clone(&c.severities),
                ));
            }
        }
    } // release the std Mutex BEFORE the awaits below — never held across .await

    let (rows, _edge_counts) = crate::routes::docs::gallery_snapshot(ctx).await?;
    let categories = Arc::new(aggregate_categories(&rows));
    let statuses = Arc::new(aggregate_statuses(&rows));
    let severities = Arc::new(aggregate_severities(&rows));

    {
        let mut guard = ctx.facets_cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(FacetsCache {
            generation,
            categories: Arc::clone(&categories),
            statuses: Arc::clone(&statuses),
            severities: Arc::clone(&severities),
        });
    }
    Ok((categories, statuses, severities))
}
