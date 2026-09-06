//! `GET /api/log-level` + `PUT /api/log-level` — read and flip the ndjson
//! FILE layer's `EnvFilter` at runtime (L2). The daemon entry points
//! (`kb daemon` / the kb-server binary) install a `reload` handle at
//! tracing-init time; PUT swaps the filter through it live — no restart,
//! and the stderr layer (RUST_LOG / docker logs) stays untouched.
//!
//! GET is total: a daemon whose process never ran `tracing_init::init`
//! (e.g. the in-process test helpers) reports `installed: false` with a
//! null filter instead of erroring. PUT on such a daemon is 409; invalid
//! directives are 400 (validated before the handle lookup, so the error
//! is deterministic either way).
//!
//! Auth: inherits the `/api/*` `auth_bearer` + `origin_allowlist` layers
//! (loopback bypass) — same privilege class as `/api/config`.

use crate::middleware::error_to_problem_json;
use axum::{body::Body, http::Response, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct LogLevelResponse {
    /// Current file-layer filter directives; `None` when file logging was
    /// never initialised in this process.
    pub filter: Option<String>,
    /// Whether the reload handle is installed (i.e. PUT can work).
    pub installed: bool,
    /// Env var that seeds the boot-time file filter (operator pointer).
    pub env: &'static str,
}

fn response(filter: Option<String>) -> LogLevelResponse {
    LogLevelResponse {
        installed: filter.is_some(),
        filter,
        env: kb_core::tracing_init::FILE_FILTER_ENV,
    }
}

/// GET /api/log-level — the file layer's current `EnvFilter` directives.
pub async fn get() -> Json<LogLevelResponse> {
    Json(response(kb_core::tracing_init::current_file_filter()))
}

#[derive(Debug, Deserialize)]
pub struct LogLevelPut {
    /// `EnvFilter` directives to apply to the FILE layer — a bare level
    /// (`debug`) or full directives (`info,kb_core=debug`).
    pub filter: String,
}

/// PUT /api/log-level — swap the file layer's filter live.
pub async fn put(Json(body): Json<LogLevelPut>) -> Response<Body> {
    match kb_core::tracing_init::set_file_filter(&body.filter) {
        Ok(filter) => {
            // INFO so the flip itself is on record in the (file + stderr)
            // logs — an audit line for "who turned debug on".
            tracing::info!(filter = %filter, "log-level: file filter updated via api");
            Json(response(Some(filter))).into_response()
        }
        Err(e) => error_to_problem_json(&e),
    }
}
