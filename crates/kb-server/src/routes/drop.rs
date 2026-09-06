//! `DELETE /api/kb/{kb}` — S5 admin: wipe a kb's lance index +
//! sqlite history/errors/edges. Source folder is untouched (it's
//! read-only bind-mounted in the dockerised deploy); `shares` and
//! `.review/*.json` are preserved (external state + user state
//! respectively — see `purge_kb_data` in sqlite.rs for rationale).
//! A subsequent `POST /api/kb/{kb}/reindex` repopulates the kb from
//! the source tree on the live actor (no re-open needed).

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct DropResponse {
    pub kb: String,
    pub lance_rows_deleted: u64,
    pub sqlite_rows_deleted: usize,
}

pub async fn delete(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let (lance_rows, sqlite_rows) = match ctx.storage.drop_kb_data().await {
        Ok(t) => t,
        Err(e) => return error_to_problem_json(&e),
    };

    // S5 — emit so subscribers (TUI fleet view, SPA Live tab) see the
    // wipe in-line with the rest of the activity stream and refresh
    // their cached counts.
    ctx.bus.emit(
        "kb.dropped",
        json!({
            "kb": kb_name.as_str(),
            "lance_rows_deleted": lance_rows,
            "sqlite_rows_deleted": sqlite_rows,
        }),
    );

    (
        StatusCode::OK,
        Json(DropResponse {
            kb: kb_name.to_string(),
            lance_rows_deleted: lance_rows,
            sqlite_rows_deleted: sqlite_rows,
        }),
    )
        .into_response()
}
