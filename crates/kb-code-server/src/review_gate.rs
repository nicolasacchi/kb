//! `review_mutations_gate` — S2-B's admission gate for the review-
//! mutation route families graduated off pure loopback-only (`router.rs`'s
//! `review_remote` sub-router): finding disposition PUT/DELETE, verdict
//! PUT/DELETE, finding/verdict publish-recording POST, and manual finding
//! create POST, plus V76-R4a (D10)'s board mutations other than apply
//! (`POST /boards/{slug}/accept`, `POST /boards/{slug}/archive`,
//! `DELETE /boards/{slug}`). Gated by `[review] remote_mutations`
//! (`config::ReviewSection`, default `false`). No second gate and no new
//! config key — boards ride this same function.
//!
//! `review_remote` layers this gate LAST (chained last — OUTER, per axum's
//! "each subsequent `.layer()` call wraps the previous ones" rule, the SAME
//! chained-last-is-outermost technique `router.rs`'s `doclens_read`
//! CORS-vs-`auth_bearer` ordering comment documents) and `auth_bearer`
//! FIRST (chained first — INNER), so THIS gate always decides admission
//! before `auth_bearer`'s token check ever runs:
//!
//! - loopback peer (the SAME [`kb_server::middleware::request_is_loopback`]
//!   predicate `auth_bearer`/[`crate::transcripts::search::loopback_only`]
//!   use, via `state.auth.trusted_proxies` — the identical `AuthConfig`
//!   `router::build_router` layers `auth_bearer` with, see
//!   [`crate::state::AppState::auth`]'s own doc for why a plain handler/
//!   middleware can read it straight off `SharedState`) → admit
//!   unconditionally next.run(req), same as today.
//! - non-loopback + `remote_mutations == false` (the default) →
//!   `404 NOT_FOUND`, BYTE-IDENTICAL to `loopback_only`'s refusal (never a
//!   401/403 that would confirm these five routes' existence to a probing
//!   non-loopback caller) — `auth_bearer` never runs, so the pre-S2
//!   admission surface for these five route families is unchanged by
//!   construction.
//! - non-loopback + `remote_mutations == true` → falls through to
//!   `auth_bearer`, which 401s a bad/missing bearer token and admits a
//!   good one.
//!
//! **What this gate does NOT touch**: every other review mutation
//! (create/snapshot/patch/delete/viewed/gc, `/reviews/pr`, `/reviews/sweep`,
//! `/reviews/{id}/report` PUT, `/findings/import`), `POST /boards/apply`
//! (the whole-document write stays loopback-only), and the working-tree
//! mutation lane (`checkout`, suggestion apply/apply-batch, `scip/ingest`,
//! `prs/fetch`) all stay on [`crate::transcripts::search::loopback_only`],
//! which this module never wraps and `remote_mutations` never reaches — see
//! `tests/review/remote_mutations_gate.rs`'s never-moves coverage (one test
//! per route, gate ON + a valid token + non-loopback still 404s) and
//! `tests/boards_route.rs` for the board apply never-moves pin.

use crate::state::SharedState;
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// See the module doc for the full admission table.
pub async fn review_mutations_gate(
    State(state): State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if kb_server::middleware::request_is_loopback(&req, &state.auth.trusted_proxies) {
        return next.run(req).await;
    }
    if state.review.remote_mutations {
        next.run(req).await
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}
