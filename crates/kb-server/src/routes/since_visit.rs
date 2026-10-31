//! `since_visit` — "what changed in this artifact since the operator's last
//! visit" (OL IDEA2).
//!
//! ```text
//! GET /api/kb/{kb}/artifacts/{id}/since-visit
//!     → { state, mode, reason?, baseline?, last_visit_unix?, working_ts_unix?,
//!         modified?, memento?, sections?, added_lines?, removed_lines? }
//! ```
//!
//! VISIT, never "read" (#19): the baseline is the caller's own newest WEB
//! open (`kb_core::reading::since_visit_baseline`); agent CLI opens never
//! count. The baseline instant goes through the ONE #14 resolver
//! (`memento_for`), nearest prior; a miss is `changed_no_delta/no_version`
//! naming the floor, never a diff against the oldest version. Read-only: no
//! history row, no event (#8). Exposure equals `…/diff`'s headings and
//! counts: section titles and line counts, never prose bodies.

use super::versions::{is_hex_artifact_id, memento_for, rel_to_git, version_ctx, MementoOut};
use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, State},
    http::Response,
    response::IntoResponse,
    Extension, Json,
};
use kb_core::reading::since_visit_baseline;
use kb_core::vcs::{self, VersionsMode};
use kb_core::versions::{bucket_changes, sectioned_prose, working_ts, ChangedSection, WORKING_REF};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SinceVisitResponse {
    /// `no_visit` | `unchanged` | `changed` | `changed_no_delta` | `unavailable`.
    #[cfg_attr(
        feature = "ts-export",
        ts(
            type = "\"no_visit\" | \"unchanged\" | \"changed\" | \"changed_no_delta\" | \"unavailable\""
        )
    )]
    pub state: &'static str,
    /// The kb's effective `[kb.*] versions` mode.
    pub mode: &'static str,
    /// `no_version` | `mode_off` (changed_no_delta); `transcript` | `error` (unavailable).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "ts-export",
        ts(
            optional,
            type = "\"no_version\" | \"mode_off\" | \"transcript\" | \"error\""
        )
    )]
    pub reason: Option<&'static str>,
    /// `current_visit` | `previous_visit`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "ts-export",
        ts(optional, type = "\"current_visit\" | \"previous_visit\"")
    )]
    pub baseline: Option<&'static str>,
    /// The baseline instant (a visit's start), unix seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_visit_unix: Option<i64>,
    /// Working-tree mtime, unix seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub working_ts_unix: Option<i64>,
    /// On `unchanged`: true when the file moved on disk but its prose did not
    /// (a markup-only change).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub modified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub memento: Option<MementoOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub sections: Option<Vec<ChangedSection>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub added_lines: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub removed_lines: Option<u32>,
}

impl SinceVisitResponse {
    fn bare(state: &'static str, mode: &'static str) -> Self {
        Self {
            state,
            mode,
            reason: None,
            baseline: None,
            last_visit_unix: None,
            working_ts_unix: None,
            modified: None,
            memento: None,
            sections: None,
            added_lines: None,
            removed_lines: None,
        }
    }
    fn unavailable(mode: &'static str, reason: &'static str) -> Self {
        Self {
            reason: Some(reason),
            ..Self::bare("unavailable", mode)
        }
    }
}

/// `GET /api/kb/{kb}/artifacts/{id}/since-visit`.
pub async fn since_visit(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Extension(identity): Extension<crate::middleware::Identity>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    if !is_hex_artifact_id(&id) {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("artifact {id}")));
    }
    let doc = match ctx.storage.get_by_id(id.clone()).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact {id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let _ = kb_name;
    let mode = ctx.versions_mode.as_str();
    // Transcripts are episodic memory (#11), not versioned documents.
    if doc.kb_category.as_deref() == Some("memory-session") {
        return Json(SinceVisitResponse::unavailable(mode, "transcript")).into_response();
    }
    let abs = PathBuf::from(&doc.path);
    let Some(wts) = working_ts(&abs).await else {
        return Json(SinceVisitResponse::unavailable(mode, "error")).into_response();
    };
    let visits = match ctx
        .storage
        .history_web_visits(id.clone(), identity.user.clone(), 2)
        .await
    {
        Ok(v) => v,
        Err(_) => return Json(SinceVisitResponse::unavailable(mode, "error")).into_response(),
    };
    let now = chrono::Utc::now().timestamp();
    let Some(base) = since_visit_baseline(&visits, wts, now) else {
        return Json(SinceVisitResponse::bare("no_visit", mode)).into_response();
    };
    let mut out = SinceVisitResponse::bare("unchanged", mode);
    out.baseline = Some(if base.current_visit {
        "current_visit"
    } else {
        "previous_visit"
    });
    out.last_visit_unix = Some(base.at_unix);
    out.working_ts_unix = Some(wts);
    // The cheap path every open hits: nothing moved on disk since the baseline.
    if wts <= base.at_unix {
        return Json(out).into_response();
    }
    if ctx.versions_mode == VersionsMode::Off {
        out.state = "changed_no_delta";
        out.reason = Some("mode_off");
        return Json(out).into_response();
    }
    let rel = rel_to_git(ctx, &abs);
    let (_timeline, memento) = memento_for(ctx, &abs, rel.clone(), &id, base.at_unix).await;
    let Some(from_ref) = memento.version.as_ref().map(|v| v.r#ref.clone()) else {
        out.state = "changed_no_delta";
        out.reason = Some("no_version");
        out.memento = Some(memento);
        return Json(out).into_response();
    };
    let vctx = version_ctx(ctx, &abs, rel, &id);
    let old_raw = vctx.raw_at(&from_ref).await;
    let new_raw = vctx.raw_at(WORKING_REF).await;
    let (old_raw, new_raw) = match (old_raw, new_raw) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return Json(SinceVisitResponse::unavailable(mode, "error")).into_response(),
    };
    // CPU work (parse + diff): off the async workers (v0.40.2 lesson).
    let abs2 = abs.clone();
    let computed = tokio::task::spawn_blocking(move || {
        let old = sectioned_prose(&abs2, &old_raw);
        let new = sectioned_prose(&abs2, &new_raw);
        let hunks = vcs::diff_lines(&old.text, &new.text);
        let sections = bucket_changes(&old, &new, &hunks);
        let (mut a, mut r) = (0u32, 0u32);
        for h in &hunks {
            for l in &h.lines {
                match l.tag {
                    vcs::DiffTag::Insert => a += 1,
                    vcs::DiffTag::Delete => r += 1,
                    vcs::DiffTag::Equal => {}
                }
            }
        }
        (hunks.is_empty(), sections, a, r)
    })
    .await;
    let Ok((empty, sections, a, r)) = computed else {
        return Json(SinceVisitResponse::unavailable(mode, "error")).into_response();
    };
    out.memento = Some(memento);
    if empty {
        out.modified = Some(true);
        return Json(out).into_response();
    }
    out.state = "changed";
    out.sections = Some(sections);
    out.added_lines = Some(a);
    out.removed_lines = Some(r);
    Json(out).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The REAL router: a token-less non-loopback peer is refused by
    /// `auth_bearer` before the handler runs.
    #[tokio::test]
    async fn since_visit_requires_auth_bearer() {
        use axum::extract::ConnectInfo;
        use axum::http::{Request, StatusCode};
        use std::net::SocketAddr;
        use tower::ServiceExt;

        let tmp = tempfile::tempdir().unwrap();
        let paths = Arc::new(kb_core::paths::KbPaths::rooted_at(tmp.path(), "sv-auth"));
        let state = Arc::new(KbHandles::new("sv-auth".into(), paths, chrono::Utc::now()));
        let app = crate::router::build_router(state);
        let req = |peer: &str| {
            Request::builder()
                .uri("/api/kb/nope/artifacts/aaaaaaaaaaaa/since-visit")
                .extension(ConnectInfo(SocketAddr::new(peer.parse().unwrap(), 50000)))
                .body(Body::empty())
                .unwrap()
        };
        let resp = app.clone().oneshot(req("8.8.8.8")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = app.oneshot(req("127.0.0.1")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
