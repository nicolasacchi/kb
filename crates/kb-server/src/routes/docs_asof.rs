//! `docs_asof` — the Library time scrubber's per-artifact as-of overlay (OL TIME1).
//!
//! ```text
//! GET /api/kb/{kb}/docs/as-of?at=<unix>&ids=<id>,<id>,…
//!     → { at, mode, items: [{ id, state, reason?, memento? }], summary }
//! ```
//!
//! ONE batched read over the invariant-#14 versions façade: for each visible
//! artifact id it answers "what did this artifact look like at instant `at`",
//! using the SAME `resolve_memento` + `MementoOut` the per-artifact
//! `…/versions?at=` route uses (`versions::memento_for` is the one shared
//! path). It is an OVERLAY: `/docs` is untouched (it ignores `at`), and
//! ranking/search are never as-of.
//!
//! Posture: ordinary `auth_bearer` read (no loopback gate). `ids` is capped
//! at [`MAX_IDS`] — over the cap or empty is a 400, never a truncation.
//! Per-id work fans out through `buffered_join(state.fanout_cap)` in
//! submission order; an item that fails (or panics) becomes
//! `unavailable/error` and never fails the request; items still unfinished
//! at the [`DEADLINE`] wall come back `pending`.

use super::versions::{is_hex_artifact_id, memento_for, parse_at, rel_to_git, MementoOut};
use crate::middleware::error_to_problem_json;
use crate::state::{KbContext, KbHandles};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use futures::FutureExt;
use kb_core::vcs::VersionsMode;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// Hard cap on `ids` per request. Over it is a 400, never a silent truncation.
pub const MAX_IDS: usize = 100;
/// Wall deadline for the whole batch; unfinished items return `pending`.
pub const DEADLINE: Duration = Duration::from_secs(5);
/// How far past "now" an `at` may be before it is refused as a typo.
const MAX_FUTURE_SECS: i64 = 86_400;

pub const STATE_HIT: &str = "hit";
pub const STATE_MISS: &str = "miss";
pub const STATE_PENDING: &str = "pending";
pub const STATE_UNAVAILABLE: &str = "unavailable";
pub const STATE_UNKNOWN: &str = "unknown";

pub const REASON_MODE_OFF: &str = "mode_off";
pub const REASON_TRANSCRIPT: &str = "transcript";
pub const REASON_ERROR: &str = "error";

#[derive(Debug, Default, Deserialize)]
pub struct AsOfParams {
    /// Unix seconds; taken as a string so a malformed value is an explicit
    /// problem+json 400 (same grammar as the versions route, via `parse_at`).
    pub at: Option<String>,
    /// Comma-separated artifact ids, in the order the caller wants back.
    pub ids: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AsOfItem {
    pub id: String,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    /// Present on `hit` and `miss`. A miss carries `oldest_ts_unix` + `note`
    /// (the floor) and never degrades to the oldest version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memento: Option<MementoOut>,
}

impl AsOfItem {
    fn bare(id: String, state: &'static str, reason: Option<&'static str>) -> Self {
        Self {
            id,
            state,
            reason,
            memento: None,
        }
    }
    fn unknown(id: String) -> Self {
        Self::bare(id, STATE_UNKNOWN, None)
    }
    fn unavailable(id: String, reason: &'static str) -> Self {
        Self::bare(id, STATE_UNAVAILABLE, Some(reason))
    }
    fn pending(id: String) -> Self {
        Self::bare(id, STATE_PENDING, None)
    }
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct AsOfSummary {
    pub hit: usize,
    pub miss: usize,
    pub pending: usize,
    pub unavailable: usize,
    pub unknown: usize,
}

impl AsOfSummary {
    fn of(items: &[AsOfItem]) -> Self {
        let mut s = Self::default();
        for i in items {
            match i.state {
                STATE_HIT => s.hit += 1,
                STATE_MISS => s.miss += 1,
                STATE_PENDING => s.pending += 1,
                STATE_UNAVAILABLE => s.unavailable += 1,
                _ => s.unknown += 1,
            }
        }
        s
    }
}

/// Separate from every existing body on purpose (additivity by construction).
#[derive(Debug, Serialize)]
pub struct AsOfResponse {
    pub at: i64,
    pub mode: &'static str,
    pub items: Vec<AsOfItem>,
    pub summary: AsOfSummary,
}

type ItemFut<'f> = Pin<Box<dyn Future<Output = AsOfItem> + Send + 'f>>;

/// Run one future per id through `buffered_join(cap)` (submission order), each
/// raced against the shared absolute `deadline` and shielded from panics: a
/// timed-out item is `pending`, a panicked one `unavailable/error`. No lock is
/// held anywhere in here, so nothing crosses an await.
pub(crate) async fn run_items<'f, F>(
    ids: Vec<String>,
    cap: usize,
    deadline: Duration,
    resolve: F,
) -> Vec<AsOfItem>
where
    F: Fn(String) -> ItemFut<'f>,
{
    let stop = tokio::time::Instant::now() + deadline;
    let futs: Vec<ItemFut<'f>> = ids
        .into_iter()
        .map(|id| {
            let fut = resolve(id.clone());
            Box::pin(async move {
                match tokio::time::timeout_at(stop, AssertUnwindSafe(fut).catch_unwind()).await {
                    Ok(Ok(item)) => item,
                    Ok(Err(_panicked)) => AsOfItem::unavailable(id, REASON_ERROR),
                    Err(_elapsed) => AsOfItem::pending(id),
                }
            }) as ItemFut<'f>
        })
        .collect();
    super::buffered_join(futs, cap).await
}

fn item_fut<'a>(ctx: &'a KbContext, id: String, at: i64) -> ItemFut<'a> {
    Box::pin(resolve_item(ctx, id, at))
}

/// One artifact's answer. Order of checks: unknown id → mode off → transcript
/// → the façade. Storage errors degrade to `unavailable/error`.
async fn resolve_item(ctx: &KbContext, id: String, at: i64) -> AsOfItem {
    if !is_hex_artifact_id(&id) {
        return AsOfItem::unknown(id);
    }
    let doc = match ctx.storage.get_by_id(id.clone()).await {
        Ok(Some(d)) => d,
        Ok(None) => return AsOfItem::unknown(id),
        Err(_) => return AsOfItem::unavailable(id, REASON_ERROR),
    };
    if ctx.versions_mode == VersionsMode::Off {
        return AsOfItem::unavailable(id, REASON_MODE_OFF);
    }
    // Transcripts are episodic memory (#11), not versioned documents.
    if doc.kb_category.as_deref() == Some("memory-session") {
        return AsOfItem::unavailable(id, REASON_TRANSCRIPT);
    }
    let abs = PathBuf::from(&doc.path);
    let rel = rel_to_git(ctx, &abs);
    let (_timeline, memento) = memento_for(ctx, &abs, rel, &id, at).await;
    AsOfItem {
        state: if memento.found { STATE_HIT } else { STATE_MISS },
        id,
        reason: None,
        memento: Some(memento),
    }
}

fn bad_request(msg: String) -> Response<Body> {
    error_to_problem_json(&kb_core::Error::BadRequest(msg))
}

/// Parse `ids`: comma-separated, empty segments ignored, order kept.
fn parse_ids(raw: Option<&str>) -> Result<Vec<String>, String> {
    let ids: Vec<String> = raw
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if ids.is_empty() {
        return Err("`ids` is required: a comma-separated list of artifact ids".into());
    }
    if ids.len() > MAX_IDS {
        return Err(format!(
            "too many ids: {} (the cap is {MAX_IDS}; the request is refused, not truncated)",
            ids.len()
        ));
    }
    Ok(ids)
}

/// Validate `at`: unix seconds, and not more than a day in the future.
fn parse_instant(raw: Option<&str>, now: i64) -> Result<i64, String> {
    let raw =
        raw.ok_or_else(|| "`at` is required: unix seconds, e.g. at=1700000000".to_string())?;
    let at = parse_at(raw)?;
    if at > now.saturating_add(MAX_FUTURE_SECS) {
        return Err(format!(
            "`at` {at} is more than a day in the future; expected unix seconds"
        ));
    }
    Ok(at)
}

/// `GET /api/kb/{kb}/docs/as-of?at=&ids=`.
pub async fn as_of(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(params): Query<AsOfParams>,
) -> Response<Body> {
    handle(&state, &kb, params, DEADLINE).await
}

pub(crate) async fn handle(
    state: &Arc<KbHandles>,
    kb: &str,
    params: AsOfParams,
    deadline: Duration,
) -> Response<Body> {
    let (_name, ctx) = match super::resolve_kb(state, kb) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    let at = match parse_instant(params.at.as_deref(), chrono::Utc::now().timestamp()) {
        Ok(v) => v,
        Err(e) => return bad_request(e),
    };
    let ids = match parse_ids(params.ids.as_deref()) {
        Ok(v) => v,
        Err(e) => return bad_request(e),
    };
    let items = run_items(ids, state.fanout_cap, deadline, |id| item_fut(ctx, id, at)).await;
    let summary = AsOfSummary::of(&items);
    Json(AsOfResponse {
        at,
        mode: ctx.versions_mode.as_str(),
        items,
        summary,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed<'f>(f: impl Future<Output = AsOfItem> + Send + 'f) -> ItemFut<'f> {
        Box::pin(f)
    }

    fn ok_item(id: String) -> AsOfItem {
        AsOfItem {
            state: STATE_HIT,
            id,
            reason: None,
            memento: None,
        }
    }

    #[tokio::test]
    async fn asof_preserves_submission_order() {
        // Earlier ids sleep LONGER, so completion order is the reverse of
        // submission order; the output must still follow submission order.
        let ids: Vec<String> = (0..6).map(|i| format!("id{i}")).collect();
        let out = run_items(ids.clone(), 8, Duration::from_secs(5), |id| {
            let n: u64 = id[2..].parse().unwrap();
            boxed(async move {
                tokio::time::sleep(Duration::from_millis((6 - n) * 10)).await;
                ok_item(id)
            })
        })
        .await;
        let got: Vec<&str> = out.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(got, ids.iter().map(String::as_str).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn asof_one_failing_item_does_not_500() {
        let ids = vec!["a".to_string(), "boom".to_string(), "c".to_string()];
        let out = run_items(ids, 8, Duration::from_secs(5), |id| {
            if id == "boom" {
                boxed(async move {
                    if id == "boom" {
                        panic!("resolver blew up");
                    }
                    ok_item(id)
                })
            } else {
                boxed(async move { ok_item(id) })
            }
        })
        .await;
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].state, STATE_HIT);
        assert_eq!(out[1].state, STATE_UNAVAILABLE);
        assert_eq!(out[1].reason, Some(REASON_ERROR));
        assert_eq!(out[2].state, STATE_HIT);
    }

    #[tokio::test]
    async fn asof_deadline_marks_pending_not_500() {
        let ids = vec!["fast".to_string(), "slow".to_string()];
        let out = run_items(ids, 8, Duration::from_millis(80), |id| {
            if id == "slow" {
                boxed(async move {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    ok_item(id)
                })
            } else {
                boxed(async move { ok_item(id) })
            }
        })
        .await;
        assert_eq!(out[0].state, STATE_HIT);
        assert_eq!(out[1].state, STATE_PENDING);
        assert_eq!(out[1].id, "slow");
        assert!(out[1].memento.is_none());
        let s = AsOfSummary::of(&out);
        assert_eq!((s.hit, s.pending), (1, 1));
    }

    #[test]
    fn asof_over_cap_is_400_not_truncated() {
        let at_cap: Vec<String> = (0..MAX_IDS).map(|i| format!("{i:012x}")).collect();
        assert_eq!(parse_ids(Some(&at_cap.join(","))).unwrap().len(), MAX_IDS);
        let over: Vec<String> = (0..=MAX_IDS).map(|i| format!("{i:012x}")).collect();
        let err = parse_ids(Some(&over.join(","))).unwrap_err();
        assert!(err.contains("not truncated"), "{err}");
    }

    #[test]
    fn asof_empty_ids_400() {
        assert!(parse_ids(None).is_err());
        assert!(parse_ids(Some("")).is_err());
        assert!(parse_ids(Some(" , ,")).is_err());
    }

    #[test]
    fn asof_bad_at_is_400() {
        let now = 1_700_000_000;
        assert!(parse_instant(None, now).is_err());
        assert!(parse_instant(Some("yesterday"), now).is_err());
        assert!(parse_instant(Some("2026-07-30"), now).is_err());
        assert_eq!(parse_instant(Some("1699999999"), now), Ok(1_699_999_999));
    }

    #[test]
    fn asof_future_at_is_400() {
        let now = 1_700_000_000;
        // Exactly one day ahead is still allowed (clock skew); past it is not.
        assert!(parse_instant(Some(&(now + 86_400).to_string()), now).is_ok());
        let err = parse_instant(Some(&(now + 86_401).to_string()), now).unwrap_err();
        assert!(err.contains("future"), "{err}");
    }

    /// The REAL router: a token-less non-loopback peer is refused by
    /// `auth_bearer` before the handler runs (the route has no loopback gate
    /// and no bypass of its own).
    #[tokio::test]
    async fn asof_requires_auth_bearer() {
        use axum::extract::ConnectInfo;
        use axum::http::{Request, StatusCode};
        use std::net::SocketAddr;
        use tower::ServiceExt;

        let tmp = tempfile::tempdir().unwrap();
        let paths = Arc::new(kb_core::paths::KbPaths::rooted_at(tmp.path(), "asof-auth"));
        let state = Arc::new(KbHandles::new(
            "asof-auth".into(),
            paths,
            chrono::Utc::now(),
        ));
        let app = crate::router::build_router(state);
        let req = |peer: &str| {
            Request::builder()
                .uri("/api/kb/nope/docs/as-of?at=1&ids=aaaaaaaaaaaa")
                .extension(ConnectInfo(SocketAddr::new(peer.parse().unwrap(), 50000)))
                .body(Body::empty())
                .unwrap()
        };
        let resp = app.clone().oneshot(req("8.8.8.8")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        // A loopback peer passes auth and reaches the handler (unknown kb → 404),
        // proving the 401 above came from auth and not from a missing route.
        let resp = app.oneshot(req("127.0.0.1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
