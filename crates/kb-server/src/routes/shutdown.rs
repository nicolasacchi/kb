//! `POST /api/shutdown` — S5 admin: signal a graceful daemon drain.
//! The broadcast::watch channel + `take_until` drain in
//! `routes/events.rs` already implement the inflight-flush path; this
//! handler just trips the signal and returns 202.
//!
//! The OS process exits once axum's `with_graceful_shutdown` future
//! completes (the daemon's `serve()` wires that to the same shutdown
//! channel). Reads can race the drain — the response goes back
//! BEFORE the process exits — but the kernel won't tear down the
//! socket until the response is flushed.

use crate::state::KbHandles;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::json;
use std::sync::Arc;

pub async fn post(State(state): State<Arc<KbHandles>>) -> impl IntoResponse {
    // `send` only fails if every receiver has been dropped — which
    // here means the daemon is already past the shutdown handshake.
    // Treat that as a successful acknowledgement (the goal state is
    // already met).
    let _ = state.shutdown.send(true);
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "draining": true,
            "note": "daemon is finishing in-flight requests; SSE streams close; process exits when the axum graceful shutdown future resolves.",
        })),
    )
}
