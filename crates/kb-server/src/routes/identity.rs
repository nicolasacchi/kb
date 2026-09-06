//! `GET /api/identity` — daemon self-description (topic 11 §B.1).
//!
//! v0.34 Y1 — also carries the resolved per-request user + identity source.
//!
//! kb-sibling/1 — also the daemon's version HELLO
//! (`sibling_protocol`/`sibling_major`/`schema_epoch` beside the existing
//! `build_sha`): the endpoint a sibling daemon handshakes against before
//! its first real call. Every field here is ADDITIVE; `/healthz` stays pure
//! liveness and learns nothing about schema or protocol.

use crate::middleware::Identity;
use crate::state::KbHandles;
use axum::{
    extract::{Extension, State},
    http::header,
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use std::sync::Arc;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct IdentityResponse {
    pub name: String,
    /// Tag-derived `git describe` stamp (e.g. `0.13-42-g9fd65fd`), NOT
    /// the Cargo version — the workspace pins that at 0.0.0 forever and
    /// versions via tags. Injected by the launching bin via
    /// `kb_server::set_build_stamp` (probe: `kb-buildstamp`'s build.rs);
    /// `0.0.0-dev` when unset or absent git.
    pub version: &'static str,
    pub host: String,
    pub kbs: Vec<String>,
    pub started_at: String,
    /// v0.6 — artifact-subdomain suffix the daemon dispatches on
    /// (e.g. `.artifacts.localhost` for dev, `.artifacts.example.com`
    /// for production). The SPA uses this to build iframe `src` URLs.
    pub artifact_host_suffix: String,
    /// v0.6 — parent SPA origin (scheme + host[+port]) the daemon
    /// considers authoritative. The SPA can cross-check against
    /// `window.location.origin` to detect a mis-wired proxy.
    pub parent_origin: String,
    /// Git commit this binary was built from (injected via
    /// `kb_server::set_build_stamp`, e.g. `39d1ca6` or `39d1ca6-dirty`;
    /// `unknown` when unset or absent git — the SPA no-ops on it). The SPA
    /// compares it against its own bundle stamp and warns on drift — a
    /// stale daemon serving a fresh bundle is the white-screen footgun.
    pub build_sha: &'static str,
    /// v0.34 Y1 — resolved attribution username for this request.
    pub user: String,
    /// v0.34 Y1 — how identity was resolved: `"header"` | `"token"` |
    /// `"legacy"` | `"loopback"`.
    pub identity_source: &'static str,
    /// v0.34 W1 — the CONFIGURED `[identity].operator` name. Legacy rows
    /// (no `user` attribution) belong to this identity; the SPA edit
    /// gate needs it to mirror the server's `forbid_if_not_owner` rule
    /// without hardcoding the default.
    pub operator: String,
    /// kb-sibling/1 Hello — the contract name a sibling daemon handshakes
    /// on before its first real call (`kb_core::sibling::SIBLING_PROTOCOL`).
    /// A different string is a DIFFERENT contract, not a newer one.
    pub sibling_protocol: &'static str,
    /// kb-sibling/1 Hello — the contract's major version. A sibling that
    /// speaks a different major fails CLOSED rather than guessing.
    pub sibling_major: u32,
    /// kb-sibling/1 Hello — this binary's schema epoch (highest EMBEDDED
    /// migration version; one binary, one epoch). The same number
    /// `Db::open`'s boot guard compares each per-kb volume against, so an
    /// operator can read "which binary is this" off the wire without
    /// shelling into the container.
    pub schema_epoch: u32,
}

pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
) -> impl IntoResponse {
    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".to_string());
    let stamp = crate::build_stamp();
    let body = Json(IdentityResponse {
        name: state.daemon_name.clone(),
        version: stamp.version,
        host,
        kbs: state.kbs.keys().map(|k| k.to_string()).collect(),
        started_at: state.started_at.to_rfc3339(),
        artifact_host_suffix: state.origin.artifact_host_suffix.clone(),
        parent_origin: state.origin.parent_origin.clone(),
        build_sha: stamp.build_sha,
        user: identity.user.clone(),
        identity_source: identity.source.as_str(),
        operator: state.operator_user().to_string(),
        sibling_protocol: kb_core::sibling::SIBLING_PROTOCOL,
        sibling_major: kb_core::sibling::SIBLING_MAJOR,
        schema_epoch: kb_core::storage::sqlite::schema_epoch(),
    });
    // `no-store`: the SPA cross-checks this against window.location to
    // detect a mis-wired proxy — a cached response from a different
    // origin would defeat the check.
    ([(header::CACHE_CONTROL, "no-store")], body)
}
