//! `GET /api/kb/{kb}/errors` — list open errors (TUI Errors tab cold load).
//! `POST /api/kb/{kb}/errors/{id}/dismiss` — mark dismissed.
//! `POST /api/kb/{kb}/errors/{id}/apply-fix` — v0.1 stub: returns 202 +
//! {run, events}; the actual patch-application logic defers to v0.2.
//! Topic 11 §B.2/B.3.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::ids::{ErrorId, RunId};
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ErrorEntry")
)]
#[derive(Debug, Serialize)]
pub struct ErrorRowResp {
    pub id: String,
    pub kind: String,
    pub source_slug: String,
    pub path: String,
    pub message: String,
    pub content_hash: Option<String>,
    pub retry_count: u32,
    pub created_at: i64,
}

pub async fn list(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    match ctx.storage.list_open_errors().await {
        Ok(rows) => {
            let resp: Vec<ErrorRowResp> = rows
                .into_iter()
                .map(|e| ErrorRowResp {
                    id: e.id,
                    kind: e.kind,
                    source_slug: e.source_slug,
                    path: e.path.to_string_lossy().to_string(),
                    message: e.message,
                    content_hash: e.content_hash,
                    retry_count: e.retry_count,
                    created_at: e.created_at_unix,
                })
                .collect();
            Json(resp).into_response()
        }
        Err(e) => error_to_problem_json(&e),
    }
}

pub async fn dismiss(
    State(state): State<Arc<KbHandles>>,
    Path((kb, err_id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    // ErrorId is a transparent newtype around String; we don't have a
    // public constructor for arbitrary input, so use the From-string
    // round-trip via serde.
    let typed: ErrorId = match serde_json::from_value(serde_json::Value::String(err_id.clone())) {
        Ok(id) => id,
        Err(_) => {
            let err = kb_core::Error::BadRequest(format!("invalid error id {err_id:?}"));
            return error_to_problem_json(&err);
        }
    };

    if let Err(e) = ctx.storage.dismiss_error(typed).await {
        return error_to_problem_json(&e);
    }
    ctx.bus.emit(
        "error.dismissed",
        json!({"kb": kb_name.as_str(), "id": err_id}),
    );
    Json(json!({"id": err_id, "dismissed": true})).into_response()
}

pub async fn apply_fix(
    State(state): State<Arc<KbHandles>>,
    Path((kb, err_id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    // v0.1 stub: emit error.fixed + return 202 + {run, events}. The actual
    // patch-application logic defers to v0.2 (when fixes carry executable
    // payloads from the indexer's diagnostics layer).
    let run = RunId::new();
    ctx.bus.emit(
        "error.fixed",
        json!({"kb": kb_name.as_str(), "id": err_id, "run": run.as_str()}),
    );

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "run": run.to_string(),
            "events": format!("/api/events?filter=run:{run}"),
            "note": "v0.1 stub: emits error.fixed; patch application v0.2",
        })),
    )
        .into_response()
}
