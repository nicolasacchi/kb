//! `GET /healthz` — unauthenticated, cheap liveness probe.
//!
//! Mounted on the TOP-LEVEL router (outside the `/api` nest), so it is NOT
//! behind `auth_bearer`, the per-token rate limiters, the `count_requests`
//! metrics layer, or the loopback-only CORS layer (all of which live inside
//! `.nest("/api", ...)`). It answers on ANY `Host:` (parent origin AND the
//! `<id>.artifacts.<suffix>` subdomains) because it is an explicit top-level
//! route that wins over the Host-dispatching `dispatch::fallback`.
//!
//! Deliberately touches only in-memory `KbHandles` fields — no storage actor,
//! lance, or embedder I/O — so it stays O(1) and never blocks on a wedged
//! subsystem. That makes it a *liveness* probe (the process answering at all
//! is the signal), not a deep readiness check; a `?deep=1` variant that pings
//! the storage actor can be added later if a real readiness gate is needed.

use crate::state::KbHandles;
use axum::{extract::State, http::header, response::IntoResponse, Json};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Always `"ok"` when the daemon is serving — present so consumers can
    /// branch on a field rather than only the status code.
    pub status: &'static str,
    /// Daemon name (`[server] name` / `--name`), for fleet dashboards.
    pub daemon: String,
    /// Seconds since the daemon started accepting connections.
    pub uptime_secs: i64,
    /// Number of configured kbs currently loaded.
    pub kbs: usize,
}

/// `GET /healthz` — 200 + minimal JSON. No auth, no rate-limit, no I/O.
pub async fn get(State(state): State<Arc<KbHandles>>) -> impl IntoResponse {
    let uptime_secs = (chrono::Utc::now() - state.started_at).num_seconds().max(0);
    let body = Json(HealthResponse {
        status: "ok",
        daemon: state.daemon_name.clone(),
        uptime_secs,
        kbs: state.kbs.len(),
    });
    // `no-store`: a probe must observe the live process, never a cached 200
    // from an upstream proxy (a stale 200 would mask a dead daemon).
    ([(header::CACHE_CONTROL, "no-store")], body)
}
