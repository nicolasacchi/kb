//! Top-level fallback dispatcher. axum's nested `/api` tree handles all
//! API requests; everything else lands here. This handler inspects the
//! Host header:
//!
//! - `<id>.artifacts.localhost[:port]` (bare) or
//!   `<kb_enc>--<id>.artifacts.localhost[:port]` (kb-qualified, v2 grammar —
//!   `kb_core::iframe::ArtifactHostId`) → forward to `routes::artifact::serve`
//!   (sandbox-isolated artifact origin per topic 06).
//! - parent origin (or any other host) → forward to `routes::spa::serve`
//!   (Atlas-less SPA shell).
//!
//! Splitting the dispatch out of the artifact serve keeps each handler
//! focused on a single Origin's contract.

use crate::routes::{artifact, spa};
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, Response, Uri},
};
use kb_core::iframe::parse_artifact_host_id;
use std::net::SocketAddr;
use std::sync::Arc;

pub async fn fallback(
    State(state): State<Arc<KbHandles>>,
    connect_info: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    uri: Uri,
) -> Response<Body> {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // ARTIFACT HOST GRAMMAR v2 — a kb-qualified label (`{kb_enc}--{id}`)
    // is still an artifact subdomain, same as a bare id/stem; the actual
    // kb resolution happens inside `artifact::serve`.
    if parse_artifact_host_id(host, &state.origin.artifact_host_suffix).is_some() {
        artifact::serve(State(state), connect_info, headers, uri).await
    } else {
        spa::serve(State(state), uri).await
    }
}
