//! Top-level fallback dispatcher for non-API requests. The nested `/api`
//! tree handles API requests, including unmatched paths ([`api_not_found`]
//! — 404 `application/problem+json`, never this SPA shell). This handler
//! inspects the Host header:
//!
//! - `<id>.artifacts.localhost[:port]` (bare) or
//!   `<kb_enc>--<id>.artifacts.localhost[:port]` (kb-qualified, v2 grammar —
//!   `kb_core::iframe::ArtifactHostId`) → forward to `routes::artifact::serve`
//!   (sandbox-isolated artifact origin per topic 06).
//! - parent origin (or any other host) → forward to `routes::spa::serve`
//!   (Atlas-less SPA shell).
//!
//! Splitting the dispatch out of the artifact serve keeps each handler
//! focused on a single Origin's contract. [`api_trailing_slash_router`]
//! covers `GET /api/` (and every other method), which axum's `.nest("/api")`
//! does not match and would otherwise inherit this SPA fallback.

use crate::routes::{artifact, spa};
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{ConnectInfo, OriginalUri, State},
    http::{header, HeaderMap, HeaderValue, Method, Response, StatusCode, Uri},
    response::IntoResponse,
    routing::any,
    Json, Router,
};
use kb_core::iframe::parse_artifact_host_id;
use serde_json::json;
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

/// RFC 7807 `type` for a path the `/api` nest does not declare.
pub const NO_SUCH_ROUTE: &str = "urn:kb:errors:no-such-route";

/// 404 problem+json for an unmatched `/api` path. Registered as the nest
/// fallback (and on `/api/`) so the request never reaches [`fallback`].
pub async fn api_not_found(method: Method, OriginalUri(uri): OriginalUri) -> Response<Body> {
    no_such_route(&method, uri.path())
}

/// `/api/` does not enter `.nest("/api", …)`. Mount this on the top-level
/// router so that one path is the same 404, not the SPA shell.
pub fn api_trailing_slash_router() -> Router<Arc<KbHandles>> {
    Router::new().route("/api/", any(api_not_found))
}

fn no_such_route(method: &Method, path: &str) -> Response<Body> {
    let mut resp = (StatusCode::NOT_FOUND, Json(no_such_route_body(method, path))).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    resp
}

fn no_such_route_body(method: &Method, path: &str) -> serde_json::Value {
    json!({
        "type": NO_SUCH_ROUTE,
        "title": StatusCode::NOT_FOUND
            .canonical_reason()
            .unwrap_or("Not Found"),
        "status": StatusCode::NOT_FOUND.as_u16(),
        "detail": format!("no route for {method} {path}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmatched_api_is_problem_json_not_html() {
        let body = no_such_route_body(&Method::GET, "/api/no-such");
        assert_eq!(body["type"], NO_SUCH_ROUTE);
        assert_eq!(body["status"], 404);
        assert_eq!(body["title"], "Not Found");
        assert_eq!(body["detail"], "no route for GET /api/no-such");
        assert!(!body.to_string().contains("html"));

        let resp = no_such_route(&Method::POST, "/api/no-such");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/problem+json")
        );
        assert_eq!(
            no_such_route_body(&Method::POST, "/api/no-such")["detail"],
            "no route for POST /api/no-such"
        );
    }
}
