//! SEC-20 — the append-only mutations audit ledger.
//!
//! > *"With multiple Authelia admins sharing one injected token, there is
//! > no way to answer 'who deleted that worktree' — `git reflog` covers
//! > refs, not suggestion applies, prefs, or lane runs."*
//!
//! kb-code cannot answer *who* (it has one identity — the non-goal "one
//! trust tier: identity is attribution, not authorization" is kb's, and
//! kb-code does not even have kb's usernames). It CAN answer *what, when,
//! on which rung, with what outcome*, and that is what makes every later
//! graduation in the v7 design defensible. This module is that record.
//!
//! # One middleware, every mutating `/api` request
//!
//! [`audit_mutations`] is layered on the `/api` nest beside the origin
//! guards, so it sees POST/PUT/PATCH/DELETE on ALL THREE sub-routers
//! (`api` / `transcripts_api` / `review_remote`) without any per-route
//! wiring to forget. It runs INSIDE the guards (so a request refused by
//! the origin allowlist is not a mutation that happened) and OUTSIDE
//! `auth_bearer`, and records the response status as the outcome — so a
//! 401, a 404 off loopback and a 409 drift refusal are all in the ledger
//! beside the 200s. An attempt is evidence.
//!
//! # The admission rung is DERIVED, never trusted
//!
//! `admission` is computed from the same two predicates the router's own
//! gates use — `kb_server::middleware::request_is_loopback` (invariant #4:
//! never re-derive it) and `[review] remote_mutations` — not from a header
//! or a handler's say-so:
//!
//! * loopback peer → `"loopback"` (every sub-router admits it that way);
//! * non-loopback on a `review_remote` path with the flag on →
//!   `"review_gate"`;
//! * otherwise → `"bearer"`.
//!
//! # Never blocks the response path
//!
//! The insert happens AFTER the response is produced, on the blocking pool
//! (`StoreBlocking::run_blocking` — the ONE sanctioned way to touch the
//! store from async context, per the 2026-08-31 starvation incident), and
//! a failure is a `tracing::warn!`, not an error the caller sees. A daemon
//! that 500s a successful checkout because its ledger disk is full has
//! made things worse, not better. The tradeoff is recorded rather than
//! hidden: the ledger is best-effort evidence, not a two-phase commit.

use crate::state::SharedState;
use crate::store::{MutationIn, MutationRow, StoreBlocking};
use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, Method, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

/// `GET /api/audit`'s hard row cap — the brief's number, enforced
/// server-side so a caller cannot ask for the whole ledger in one page.
pub const MAX_AUDIT_ROWS: usize = 500;

/// Default window when `?since=` is omitted: 24 h.
const DEFAULT_SINCE_SECS: i64 = 24 * 60 * 60;

/// The `/api`-relative path prefixes served by the `review_remote`
/// sub-router (`router.rs`'s S2-B block). Used ONLY to label the admission
/// rung of a non-loopback mutation; the gate itself is
/// `review_gate::review_mutations_gate`, untouched by this module.
fn is_review_gate_path(path: &str) -> bool {
    // `/api/reviews/{id}/findings`, `.../findings/{slug}/disposition`,
    // `.../findings/{slug}/published`, `.../verdict`, `.../verdict/published`.
    let Some(rest) = path.strip_prefix("/api/reviews/") else {
        return false;
    };
    let Some((_id, tail)) = rest.split_once('/') else {
        return false;
    };
    tail == "findings"
        || tail == "verdict"
        || tail == "verdict/published"
        || (tail.starts_with("findings/")
            && (tail.ends_with("/disposition") || tail.ends_with("/published")))
}

/// A per-request correlation id. Not a UUID crate dependency: 12 hex
/// chars from the same `getrandom` source `annotations::new_annotation_id`
/// already uses, which is the crate's established id shape.
fn new_request_id() -> String {
    let mut bytes = [0u8; 6];
    if getrandom::fill(&mut bytes).is_err() {
        // getrandom failing is a broken host, not a reason to drop the
        // audit row — fall back to a monotonic-ish stamp so the row still
        // correlates within a boot.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        bytes[..4].copy_from_slice(&nanos.to_le_bytes());
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_mutating(method: &Method) -> bool {
    matches!(
        method,
        &Method::POST | &Method::PUT | &Method::PATCH | &Method::DELETE
    )
}

/// The `?repo=` query param, when the mutating route carries one — the
/// coarse "which repo did this touch" column. Routes addressed by an
/// opaque id (annotation/review) leave it NULL rather than paying a store
/// lookup on the response path.
fn repo_from_query(query: Option<&str>) -> Option<String> {
    let q = query?;
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("repo=") {
            return Some(percent_decode_lossy(v));
        }
    }
    None
}

/// The `?path=` query param, same shape as [`repo_from_query`].
fn target_from_query(query: Option<&str>) -> Option<String> {
    let q = query?;
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("path=") {
            return Some(percent_decode_lossy(v));
        }
    }
    None
}

/// Minimal `%XX` + `+` decoding — the audit column is a human-readable
/// label, and pulling in a percent-encoding crate for one column would be
/// a new dependency for a cosmetic gain.
fn percent_decode_lossy(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The middleware — see the module doc.
pub async fn audit_mutations(
    State(state): State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if !is_mutating(req.method()) {
        return next.run(req).await;
    }
    // `full_path`, NOT `req.uri().path()` — the ledger must record the
    // route a caller can actually address (see that fn's doc).
    let route = crate::security::full_path(&req);
    let method = req.method().to_string();
    let repo = repo_from_query(req.uri().query());
    let target = target_from_query(req.uri().query());
    let loopback = kb_server::middleware::request_is_loopback(&req, &state.auth.trusted_proxies);
    let admission = if loopback {
        "loopback"
    } else if state.review.remote_mutations && is_review_gate_path(&route) {
        "review_gate"
    } else {
        "bearer"
    };
    let request_id = new_request_id();

    let resp = next.run(req).await;
    let outcome = resp.status().as_u16().to_string();

    let row = MutationIn {
        ts_unix: chrono::Utc::now().timestamp(),
        route,
        method,
        admission: admission.to_string(),
        repo,
        target,
        // Blob hashes are a CONTENT-mutation concern; the suggestion-apply
        // lane records its own pair on the annotation row already, and
        // reading a blob here would mean a second filesystem round trip on
        // every mutation's response path. Left NULL by this middleware, a
        // column the ledger is ready for rather than one it fakes.
        blob_before: None,
        blob_after: None,
        request_id,
        outcome,
    };
    let store = state.store.clone();
    if let Err(e) = store.run_blocking(move |s| s.insert_mutation(&row)).await {
        // Best-effort by design — see the module doc.
        tracing::warn!(error = %e, "kb-code audit: could not append a mutations row");
    }
    resp
}

// --- GET /api/audit -------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AuditParams {
    /// Unix seconds; rows at or after this instant. Defaults to 24 h ago.
    pub since: Option<i64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub ts_unix: i64,
    pub ts: String,
    pub route: String,
    pub method: String,
    pub admission: String,
    pub repo: Option<String>,
    pub target: Option<String>,
    pub blob_before: Option<String>,
    pub blob_after: Option<String>,
    pub request_id: String,
    pub outcome: String,
}

#[derive(Debug, Serialize)]
pub struct AuditOut {
    pub schema: &'static str,
    pub since: i64,
    pub limit: usize,
    pub count: usize,
    pub entries: Vec<AuditEntry>,
}

fn to_entry(r: MutationRow) -> AuditEntry {
    let ts = chrono::DateTime::from_timestamp(r.ts_unix, 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default();
    AuditEntry {
        id: r.id,
        ts_unix: r.ts_unix,
        ts,
        route: r.route,
        method: r.method,
        admission: r.admission,
        repo: r.repo,
        target: r.target,
        blob_before: r.blob_before,
        blob_after: r.blob_after,
        request_id: r.request_id,
        outcome: r.outcome,
    }
}

/// `GET /api/audit?since=&limit=` — newest first, capped at
/// [`MAX_AUDIT_ROWS`]. An ordinary `auth_bearer` READ (not loopback-only):
/// the ledger carries route paths and repo names, the same information
/// class `GET /api/repos` already serves over this gate, and a ledger only
/// the loopback operator can read is useless to the second Authelia admin
/// this finding is about.
pub async fn audit_route(
    State(state): State<SharedState>,
    Query(params): Query<AuditParams>,
) -> Result<impl IntoResponse, crate::routes::ApiError> {
    let since = params
        .since
        .unwrap_or_else(|| chrono::Utc::now().timestamp() - DEFAULT_SINCE_SECS);
    let limit = params
        .limit
        .unwrap_or(MAX_AUDIT_ROWS)
        .clamp(1, MAX_AUDIT_ROWS);
    let rows = state
        .store
        .run_blocking(move |s| s.list_mutations(since, limit))
        .await
        .map_err(|e| {
            crate::routes::ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;
    let entries: Vec<AuditEntry> = rows.into_iter().map(to_entry).collect();
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(AuditOut {
            schema: "kbc-audit/1",
            since,
            limit,
            count: entries.len(),
            entries,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_five_graduated_families_label_as_review_gate() {
        for p in [
            "/api/reviews/7/findings",
            "/api/reviews/7/verdict",
            "/api/reviews/7/verdict/published",
            "/api/reviews/7/findings/f-abc/disposition",
            "/api/reviews/7/findings/f-abc/published",
        ] {
            assert!(is_review_gate_path(p), "{p}");
        }
        for p in [
            "/api/reviews/7/findings/import",
            "/api/reviews/7/snapshot",
            "/api/reviews/7/report",
            "/api/reviews",
            "/api/checkout",
            "/api/annotations/abc/apply",
        ] {
            assert!(!is_review_gate_path(p), "{p}");
        }
    }

    #[test]
    fn query_columns_decode_the_repo_and_path() {
        assert_eq!(
            repo_from_query(Some("repo=kb&path=src%2Fmain.rs")),
            Some("kb".to_string())
        );
        assert_eq!(
            target_from_query(Some("repo=kb&path=src%2Fmain.rs")),
            Some("src/main.rs".to_string())
        );
        assert_eq!(repo_from_query(None), None);
        assert_eq!(target_from_query(Some("repo=kb")), None);
    }

    #[test]
    fn request_ids_are_twelve_hex_chars() {
        let id = new_request_id();
        assert_eq!(id.len(), 12);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(id, new_request_id());
    }
}
