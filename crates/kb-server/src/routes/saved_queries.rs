//! `/api/saved-queries` — daemon-wide saved-query store (v0.13 Q4).
//!
//! GET    → returns the array of saved queries.
//! POST   → upsert by name (case-insensitive) + return the full list.
//! DELETE /{name} → remove by name (case-insensitive).
//!
//! Persistence lives in `<state>/saved-queries.json` so the choices
//! survive a daemon restart and sync across devices that point at
//! the same daemon. The SPA's localStorage cache is still the
//! offline-first source of truth on first render; the hook reconciles
//! against the daemon when it's reachable.

use crate::middleware::error_to_problem_json;
use crate::state::{load_saved_queries, save_saved_queries, KbHandles, SavedQuery};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use std::sync::Arc;

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "SavedQueryPostBody")
)]
#[derive(Debug, Deserialize)]
pub struct PostBody {
    pub name: String,
    pub path: String,
    pub search: String,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "SavedQueriesList")
)]
#[derive(Debug, serde::Serialize)]
pub struct ListResponse {
    pub queries: Vec<SavedQuery>,
}

pub async fn list(State(state): State<Arc<KbHandles>>) -> Response<Body> {
    let queries = load_saved_queries(&state.paths);
    Json(ListResponse { queries }).into_response()
}

pub async fn upsert(
    State(state): State<Arc<KbHandles>>,
    Json(body): Json<PostBody>,
) -> Response<Body> {
    let name = body.name.trim();
    if name.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "saved-query name must not be empty".into(),
        ));
    }
    if name.len() > 80 {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "saved-query name exceeds 80 chars".into(),
        ));
    }
    let mut list = load_saved_queries(&state.paths);
    let lower = name.to_ascii_lowercase();
    // De-dupe case-insensitive — overwrite the existing row so users
    // get a rename path for free (matches the localStorage hook).
    list.retain(|q| q.name.to_ascii_lowercase() != lower);
    list.insert(
        0,
        SavedQuery {
            name: name.to_string(),
            path: body.path,
            search: body.search,
            saved_at: chrono::Utc::now().timestamp(),
        },
    );
    if let Err(e) = save_saved_queries(&state.paths, &list) {
        return error_to_problem_json(&kb_core::Error::Storage(format!(
            "save saved-queries.json: {e}"
        )));
    }
    Json(ListResponse { queries: list }).into_response()
}

pub async fn delete(
    State(state): State<Arc<KbHandles>>,
    Path(name): Path<String>,
) -> Response<Body> {
    let mut list = load_saved_queries(&state.paths);
    let lower = name.to_ascii_lowercase();
    let before = list.len();
    list.retain(|q| q.name.to_ascii_lowercase() != lower);
    if list.len() == before {
        // Idempotent — 204 even when the name wasn't there.
        return StatusCode::NO_CONTENT.into_response();
    }
    if let Err(e) = save_saved_queries(&state.paths, &list) {
        return error_to_problem_json(&kb_core::Error::Storage(format!(
            "save saved-queries.json: {e}"
        )));
    }
    StatusCode::NO_CONTENT.into_response()
}
