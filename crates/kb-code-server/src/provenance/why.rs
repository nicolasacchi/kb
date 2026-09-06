//! `GET /api/why?repo=&path=&line=` (W3.4) — line-grade and file-grade
//! "why": which session (if any) produced a line, or (no `line`) which
//! sessions dominate a whole file.
//!
//! # Line-grade (`?line=` given)
//!
//! The ONE blame region (`provenance::blame_regions`, narrowed to that
//! line) covering it:
//!
//! - **[`provenance::UNCOMMITTED_SHA`]** — no commit to join through; answer
//!   from the LOCAL transcripts index instead (`store::Store::
//!   transcript_sessions_touching_path`) — `confidence: "none"`,
//!   `via: "uncommitted-live"`, zero or more matching in-flight session ids,
//!   newest first. An honest gap (`session_ids: []`) when nothing matches —
//!   see the module doc.
//! - **any other sha** — the join ladder (`join::ladder::resolve_commit`).
//!   When it resolves a `session_id`, `why` makes ONE best-effort follow-up
//!   call to kb's own `GET /api/why?path=` (`join::kb_client::KbClient::
//!   why`) and, if that session appears in the response, folds in its
//!   `decisions`/`first_user_prompt` (`kb_context`) and — ONLY as a
//!   fallback when the ladder's own `display_name` is absent (true for the
//!   fuzzy/squash-subject/time-window arms, which never carry one) — its
//!   `display_name`. The SAME follow-up ALSO makes one further best-effort
//!   `GET /api/sessions/{id}` call (`KbClient::session_detail`) for the
//!   resolved session's `memory_ids` (display-only ids, never fetched
//!   bodies — see [`LineWhyOut::session_memory_ids`]'s doc) and folds
//!   `hit.kb` (the session's own corpus) in as a FALLBACK for
//!   `attribution.kb` (the ladder's own `kb`, already populated for most
//!   arms — see `join::ladder::Attribution::kb`'s doc — is preferred when
//!   present, the SAME precedence `display_name` already uses). A failed/
//!   empty follow-up degrades to `kb_context: None` and
//!   `session_memory_ids: []`, never an error: the line's own attribution
//!   already stands on its own.
//!   **`kb_context` is LOOPBACK-ONLY** (transcript-derived prompt/decision
//!   text — see the "Sensitivity" section below): a non-loopback caller
//!   never triggers the follow-up at all, so `kb_context`/
//!   `session_memory_ids` stay `None`/`[]` and `display_name`/`kb` fall
//!   back to the ladder's own (the same shape as a failed follow-up).
//!
//! # Sensitivity
//!
//! `/why` is mounted on the ordinary `auth_bearer`-gated router
//! (`router.rs`), NOT the loopback-only transcripts sub-router, because
//! attribution (session ids, confidence, `via`) is no more sensitive than
//! `/join/commit` already exposes over that same gate. `kb_context`
//! (`fetch_kb_context`'s prompt excerpt + decision prompt/answer text) is a
//! different sensitivity class — real transcript-derived content — so it is
//! gated separately, per-request, on the caller's own loopback-ness
//! (`kb_server::middleware::is_loopback_origin`, the same derivation
//! `routes::search_unified` uses for its transcripts section): a
//! non-loopback caller gets attribution but never `kb_context` (nor the
//! `session_memory_ids`/kb-fallback riding the same follow-up — though
//! `attribution.kb` itself, sourced from the ladder rather than the
//! follow-up, is unconditional). This is the one exception to "everything
//! `/why` returns is loopback-agnostic" that motivated keeping the route
//! off the transcripts sub-router in the first place — see `router.rs`'s
//! own W3.3+W3.4 doc paragraph.
//!
//! `timeline_available` is `true` unless the covering region is
//! uncommitted — a hint for a client deciding whether `GET
//! /api/blame/timeline` on this line is worth calling.
//!
//! # File-grade (`?line=` omitted)
//!
//! Every region's ladder resolution (`provenance::regions_with_attribution`),
//! grouped by identity (a session id when resolved, else the bare sha —
//! `none`-confidence commits are never merged with one another) and ranked
//! by how many of the file's CURRENT lines each identity owns, top
//! [`MAX_FILE_WHY_SESSIONS`] first. `uncommitted_lines` reports the total
//! line count NOT attributed to any group (covered by
//! [`provenance::UNCOMMITTED_SHA`] regions) — surfaced as a plain count,
//! not resolved per-session (a file-wide query making one
//! `transcript_sessions_touching_path` lookup per uncommitted region would
//! be a needless multiplier; the line-grade query is the honest way to ask
//! "which session is behind THIS uncommitted line").

use crate::config::RepoEntry;
use crate::join::kb_client::{KbClient, WhyDecision};
use crate::join::ladder::Confidence;
use crate::provenance;
use crate::routes::{find_repo, safe_rel_path, ApiError};
use crate::state::SharedState;
use crate::store::StoreBlocking;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The file-grade query's cap on distinct sessions/shas reported — a plain
/// "top N by line coverage" list, not a paginated surface.
pub const MAX_FILE_WHY_SESSIONS: usize = 10;

/// Cap on how many in-flight session ids the uncommitted-live path reports
/// — see the module doc; a bare identity list, not a search result page.
pub const UNCOMMITTED_LIVE_LIMIT: usize = 8;

#[derive(Debug, Deserialize)]
pub struct WhyParams {
    pub repo: String,
    pub path: String,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegionOut {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub author_time: i64,
}

impl From<&crate::blame::BlameRegion> for RegionOut {
    fn from(r: &crate::blame::BlameRegion) -> Self {
        Self {
            sha: r.sha.clone(),
            subject: r.subject.clone(),
            author: r.author.clone(),
            author_time: r.author_time,
        }
    }
}

/// The line-grade query's attribution — a widened `join::ladder::
/// Attribution` (plain strings for `confidence`/`via`, since the
/// `uncommitted-live` via has no `join::ladder::Confidence`/via-taxonomy
/// slot of its own) with a SECOND, multi-valued identity field
/// (`session_ids`) for that one case — see the module doc.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AttributionOut {
    pub confidence: String,
    pub via: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Populated ONLY by the `uncommitted-live` path — every other via
    /// leaves this empty and uses `session_id` instead (a single resolved
    /// identity, never ambiguous).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub session_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The kb CORPUS the resolved session lives in — `join::ladder::
    /// Attribution::kb` when the ladder itself got one (most arms — see
    /// that field's doc), else the loopback-only `kb_context` follow-up's
    /// `WhySession::kb` (`line_why`'s doc). Absent when neither source
    /// resolved one (a `none`-confidence attribution, or a `trailer` hit
    /// whose enrichment call failed, on a non-loopback request). Lets a
    /// client scope an "open session in kb" deep link with `?kb=` instead
    /// of guessing the operator's default corpus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kb: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KbContextOut {
    pub decisions: Vec<WhyDecision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LineWhyOut {
    pub line: u32,
    pub region: RegionOut,
    pub attribution: AttributionOut,
    /// Lance artifact ids the resolved session also wrote (kb-server's
    /// `SessionDetailResponse::memory_ids`, via `KbClient::session_detail`
    /// inside the SAME loopback-gated follow-up `kb_context` rides — see
    /// the module doc). Display-only: ids to link out to, never fetched
    /// bodies. Empty (and omitted) when no session resolved, the follow-up
    /// didn't run (non-loopback), or it found no memories.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub session_memory_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kb_context: Option<KbContextOut>,
    pub timeline_available: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileSessionOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub confidence: String,
    pub via: String,
    /// Total CURRENT lines this identity owns across every region it's
    /// attributed to.
    pub lines: u32,
    /// How many distinct blame regions contributed to `lines`.
    pub regions: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileWhyOut {
    pub repo: String,
    pub path: String,
    pub sessions: Vec<FileSessionOut>,
    /// Lines currently covered by an uncommitted (dirty-blame) region — not
    /// attributed to any session/commit here; see the module doc.
    pub uncommitted_lines: u32,
}

/// `GET /api/why?repo=&path=[&line=]` — dispatches to [`line_why`] or
/// [`file_why`] depending on whether `line` was given. Re-derives the
/// caller's own loopback-ness (`ConnectInfo` + headers, same inputs
/// `routes::search_unified` uses) to decide whether [`line_why`]'s
/// `kb_context` enrichment runs at all — see the module doc's "Sensitivity"
/// section.
pub async fn why(
    State(state): State<SharedState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Query(params): Query<WhyParams>,
) -> Result<Response, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?;
    match params.line {
        Some(line) => {
            if line < 1 {
                return Err(ApiError::bad_request("line must be >= 1"));
            }
            let is_loopback = kb_server::middleware::is_loopback_origin(
                Some(peer.ip()),
                &headers,
                &state.auth.trusted_proxies,
            );
            let out = line_why(&state, repo, repo_id, path, line, is_loopback).await?;
            Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)).into_response())
        }
        None => {
            let out = file_why(&state, repo, repo_id, path).await?;
            Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)).into_response())
        }
    }
}

/// `is_loopback` gates ONLY the `kb_context` enrichment follow-up (see the
/// module doc's "Sensitivity" section) — every other part of the response
/// (region, attribution, `timeline_available`) is unconditional.
async fn line_why(
    state: &SharedState,
    repo: &RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    is_loopback: bool,
) -> Result<LineWhyOut, ApiError> {
    let regions = provenance::blame_regions(state, repo, repo_id, path, Some((line, line))).await?;
    let region = regions.into_iter().next().ok_or_else(|| {
        ApiError::bad_request(format!("line {line} is out of range for {path:?}"))
    })?;

    if region.sha == provenance::UNCOMMITTED_SHA {
        let attribution = uncommitted_attribution(state, repo, path).await?;
        return Ok(LineWhyOut {
            line,
            region: RegionOut::from(&region),
            attribution,
            session_memory_ids: Vec::new(),
            kb_context: None,
            timeline_available: false,
        });
    }

    let attribution = crate::join::ladder::resolve_commit(
        repo,
        repo_id,
        &region.sha,
        &state.store,
        &state.kb_client,
    )
    .await;

    let enrichment = match attribution.session_id.as_deref() {
        Some(session_id) if is_loopback => {
            fetch_kb_context(&state.kb_client, path, session_id).await
        }
        _ => None,
    };
    let display_name = attribution
        .display_name
        .clone()
        .or_else(|| enrichment.as_ref().map(|e| e.display_name.clone()));
    let kb = attribution
        .kb
        .clone()
        .or_else(|| enrichment.as_ref().and_then(|e| e.kb.clone()));
    let session_memory_ids = enrichment
        .as_ref()
        .map(|e| e.memory_ids.clone())
        .unwrap_or_default();
    let kb_context = enrichment.map(|e| e.kb_context);

    Ok(LineWhyOut {
        line,
        region: RegionOut::from(&region),
        attribution: AttributionOut {
            confidence: attribution.confidence.as_str().to_string(),
            via: attribution.via,
            session_id: attribution.session_id,
            session_ids: Vec::new(),
            display_name,
            kb,
        },
        session_memory_ids,
        kb_context,
        timeline_available: true,
    })
}

/// The uncommitted-live path — see the module doc.
async fn uncommitted_attribution(
    state: &SharedState,
    repo: &RepoEntry,
    path: &str,
) -> Result<AttributionOut, ApiError> {
    // V70-A2 (SEC-13) — the absolute path is used as a STORE KEY here
    // (`transcript_sessions_touching_path`), so containment keeps a
    // symlinked repo path from addressing transcript rows for a file
    // outside the repo.
    let abs = crate::security::paths::contained_abs_path(&repo.path, path)?;
    let abs_str = abs.to_string_lossy().to_string();
    let hits = state
        .store
        .run_blocking(move |store| {
            store.transcript_sessions_touching_path(&abs_str, UNCOMMITTED_LIVE_LIMIT)
        })
        .await?;
    let mut session_ids = Vec::with_capacity(hits.len());
    for h in hits {
        if !session_ids.contains(&h.session_id) {
            session_ids.push(h.session_id);
        }
    }
    Ok(AttributionOut {
        confidence: Confidence::None.as_str().to_string(),
        via: "uncommitted-live".to_string(),
        session_id: None,
        session_ids,
        display_name: None,
        kb: None,
    })
}

/// [`fetch_kb_context`]'s bundle — everything the loopback-only follow-up
/// pulled for one resolved session, folded into [`LineWhyOut`] by the caller
/// (`line_why`). Internal: never serialized directly.
struct Enrichment {
    display_name: String,
    /// `Some(hit.kb)` on every successful `why()` hit — `WhySession::kb` is
    /// a plain (non-`Option`) `String` on the wire. `line_why` uses this
    /// only as a FALLBACK for `attribution.kb` (see that field's doc).
    kb: Option<String>,
    kb_context: KbContextOut,
    /// `SessionDetailSignals::memory_ids`, via a second best-effort
    /// `session_detail` call — empty (never an error) on ANY failure.
    memory_ids: Vec<String>,
}

/// Best-effort enrichment: kb's own `GET /api/why?path=` filtered to
/// `session_id`, plus (on that hit) a `GET /api/sessions/{id}` follow-up for
/// `memory_ids` — `None` on ANY failure of the FIRST call
/// (disabled/unreachable/parse error/session absent from the response); the
/// second call degrades independently (an empty `memory_ids`, never
/// bubbling its own failure) — see the module doc.
async fn fetch_kb_context(
    kb_client: &KbClient,
    path: &str,
    session_id: &str,
) -> Option<Enrichment> {
    let sessions = kb_client.why(path).await.ok()?;
    let hit = sessions.into_iter().find(|s| s.session_id == session_id)?;
    let memory_ids = kb_client
        .session_detail(session_id)
        .await
        .ok()
        .flatten()
        .map(|d| d.memory_ids)
        .unwrap_or_default();
    Some(Enrichment {
        display_name: hit.display_name,
        kb: Some(hit.kb),
        kb_context: KbContextOut {
            decisions: hit.decisions,
            prompt_excerpt: hit.first_user_prompt,
        },
        memory_ids,
    })
}

/// `pub(crate)` — [`crate::agentview::pack`] reuses this to build each
/// packed file's provenance summary section (top sessions by line
/// coverage, truncated further there) without going through HTTP.
pub(crate) async fn file_why(
    state: &SharedState,
    repo: &RepoEntry,
    repo_id: i64,
    path: &str,
) -> Result<FileWhyOut, ApiError> {
    let resolved = provenance::regions_with_attribution(state, repo, repo_id, path).await?;

    struct Agg {
        session_id: Option<String>,
        display_name: Option<String>,
        confidence: Confidence,
        via: String,
        lines: u32,
        regions: u32,
    }

    let mut by_key: BTreeMap<String, Agg> = BTreeMap::new();
    let mut uncommitted_lines: u32 = 0;

    for (region, attribution) in &resolved {
        let Some(attribution) = attribution else {
            uncommitted_lines += region.count;
            continue;
        };
        let key = attribution
            .session_id
            .clone()
            .unwrap_or_else(|| region.sha.clone());
        let entry = by_key.entry(key).or_insert_with(|| Agg {
            session_id: attribution.session_id.clone(),
            display_name: attribution.display_name.clone(),
            confidence: attribution.confidence,
            via: attribution.via.clone(),
            lines: 0,
            regions: 0,
        });
        entry.lines += region.count;
        entry.regions += 1;
        if entry.display_name.is_none() {
            entry.display_name = attribution.display_name.clone();
        }
    }

    let mut sessions: Vec<FileSessionOut> = by_key
        .into_values()
        .map(|a| FileSessionOut {
            session_id: a.session_id,
            display_name: a.display_name,
            confidence: a.confidence.as_str().to_string(),
            via: a.via,
            lines: a.lines,
            regions: a.regions,
        })
        .collect();
    // Ranked by line coverage, descending; a stable, deterministic
    // tie-break (`via`, then whatever identity string sorts first) so two
    // identical-coverage identities don't reorder run to run.
    sessions.sort_by(|a, b| {
        b.lines.cmp(&a.lines).then_with(|| {
            a.via.cmp(&b.via).then_with(|| {
                let a_id = a.session_id.as_deref().unwrap_or("");
                let b_id = b.session_id.as_deref().unwrap_or("");
                a_id.cmp(b_id)
            })
        })
    });
    sessions.truncate(MAX_FILE_WHY_SESSIONS);

    Ok(FileWhyOut {
        repo: repo.name.clone(),
        path: path.to_string(),
        sessions,
        uncommitted_lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribution_out_uncommitted_live_shape_omits_session_id_and_display_name() {
        let out = AttributionOut {
            confidence: "none".to_string(),
            via: "uncommitted-live".to_string(),
            session_id: None,
            session_ids: vec!["sess-a".to_string(), "sess-b".to_string()],
            display_name: None,
            kb: None,
        };
        let v = serde_json::to_value(&out).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "confidence": "none",
                "via": "uncommitted-live",
                "session_ids": ["sess-a", "sess-b"]
            })
        );
    }

    #[test]
    fn attribution_out_empty_session_ids_is_omitted_from_json() {
        let out = AttributionOut {
            confidence: "trailer".to_string(),
            via: "commit-trailer".to_string(),
            session_id: Some("sess-1".to_string()),
            session_ids: Vec::new(),
            display_name: Some("fixed the gizmo".to_string()),
            kb: None,
        };
        let v = serde_json::to_value(&out).unwrap();
        assert!(v.get("session_ids").is_none());
        assert_eq!(v["session_id"], "sess-1");
    }

    /// `kb` rides the SAME `skip_serializing_if` shape as `display_name` —
    /// present when resolved, absent (not `null`) otherwise.
    #[test]
    fn attribution_out_kb_present_serializes_absent_omits() {
        let with_kb = AttributionOut {
            confidence: "exact".to_string(),
            via: "by-commit".to_string(),
            session_id: Some("sess-1".to_string()),
            session_ids: Vec::new(),
            display_name: None,
            kb: Some("memory".to_string()),
        };
        let v = serde_json::to_value(&with_kb).unwrap();
        assert_eq!(v["kb"], "memory");

        let without_kb = AttributionOut {
            kb: None,
            ..with_kb
        };
        let v2 = serde_json::to_value(&without_kb).unwrap();
        assert!(v2.get("kb").is_none());
    }

    /// Finding 2 — `kb_context` (transcript-derived prompt/decision text)
    /// must be entirely ABSENT from the JSON, not `null`, when the handler
    /// skipped the enrichment follow-up (the non-loopback case — see the
    /// module doc's "Sensitivity" section). `#[serde(skip_serializing_if =
    /// "Option::is_none")]` on `LineWhyOut::kb_context` is what makes that
    /// true; pin the shape directly so a future field-shuffle can't
    /// silently start serializing `"kb_context": null` instead.
    #[test]
    fn line_why_out_kb_context_none_is_omitted_from_json_not_null() {
        let out = LineWhyOut {
            line: 3,
            region: RegionOut {
                sha: "a".repeat(40),
                subject: "the subject".to_string(),
                author: "someone".to_string(),
                author_time: 1_700_000_000,
            },
            attribution: AttributionOut {
                confidence: "trailer".to_string(),
                via: "commit-trailer".to_string(),
                session_id: Some("sess-1".to_string()),
                session_ids: Vec::new(),
                display_name: Some("fixed the gizmo".to_string()),
                kb: None,
            },
            session_memory_ids: Vec::new(),
            kb_context: None,
            timeline_available: true,
        };
        let v = serde_json::to_value(&out).unwrap();
        assert!(
            v.as_object().unwrap().get("kb_context").is_none(),
            "kb_context must be OMITTED, not present-as-null: {v}"
        );
        assert!(
            v.as_object().unwrap().get("session_memory_ids").is_none(),
            "an empty session_memory_ids must be OMITTED too: {v}"
        );
        // Attribution (session id, confidence) is unconditional either way
        // — only `kb_context` is loopback-gated.
        assert_eq!(v["attribution"]["session_id"], "sess-1");
    }

    /// A non-empty `session_memory_ids` DOES serialize (display-only ids,
    /// never bodies — see `LineWhyOut::session_memory_ids`'s doc).
    #[test]
    fn line_why_out_session_memory_ids_serializes_when_present() {
        let out = LineWhyOut {
            line: 3,
            region: RegionOut {
                sha: "a".repeat(40),
                subject: "the subject".to_string(),
                author: "someone".to_string(),
                author_time: 1_700_000_000,
            },
            attribution: AttributionOut {
                confidence: "trailer".to_string(),
                via: "commit-trailer".to_string(),
                session_id: Some("sess-1".to_string()),
                session_ids: Vec::new(),
                display_name: Some("fixed the gizmo".to_string()),
                kb: Some("memory".to_string()),
            },
            session_memory_ids: vec!["mem-a".to_string(), "mem-b".to_string()],
            kb_context: None,
            timeline_available: true,
        };
        let v = serde_json::to_value(&out).unwrap();
        assert_eq!(
            v["session_memory_ids"],
            serde_json::json!(["mem-a", "mem-b"])
        );
        assert_eq!(v["attribution"]["kb"], "memory");
    }
}
