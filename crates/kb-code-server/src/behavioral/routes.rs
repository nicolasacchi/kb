//! HTTP surface for V3.2-B1 behavioral signals + backfill + head-moved worker.
//!
//! GET routes: ordinary `auth_bearer`. POST backfill: loopback-only bulk op.

use crate::config::{BehavioralSection, RepoEntry};
use crate::git::GitRepo;
use crate::routes::{find_repo, safe_rel_path, ApiError};
use crate::state::SharedState;
use crate::store::Store;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::ingest::{self, BehavioralError};
use super::{
    age_buckets, complexity_for_path, coupling_confidence, dense_ranks_desc, hotspot_score,
    is_session_author, ordered_pair, pain_from_signals, session_id_from_author, shannon_entropy,
    stored_fail_term, Complexity, MAJOR_SHARE_THRESHOLD, PAIN_ERROR_SCALE,
};

pub const SCHEMA: &str = "behavioral/1";
pub const FUSION_SCHEMA: &str = "behavioral-fusion/1";
pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 500;
pub const TAINTED_LINE_CAP: usize = 500;

impl From<BehavioralError> for ApiError {
    fn from(e: BehavioralError) -> Self {
        match e {
            BehavioralError::Store(s) => ApiError::from(s),
            other => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
        }
    }
}

fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// --- wire types -----------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct HotspotsParams {
    pub repo: String,
    pub limit: Option<usize>,
    pub scope: Option<String>,
    /// V3.2-B2: `pain` weights hotspot score by max session pain on the path.
    /// 400 when no session_signals rows exist for the repo.
    pub weight: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HotspotTerms {
    pub churn_rank: u32,
    pub complexity_rank: u32,
    /// Present only when `weight=pain`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pain: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct HotspotScore {
    pub score: f64,
    pub terms: HotspotTerms,
}

#[derive(Debug, Serialize)]
pub struct ComplexityOut {
    pub loc: u64,
    pub indent_sum: u64,
}

#[derive(Debug, Serialize)]
pub struct HotspotRow {
    pub path: String,
    pub revisions: i64,
    pub churn: i64,
    pub complexity: ComplexityOut,
    pub hotspot: HotspotScore,
    pub last_touch_unix: Option<i64>,
    pub age_days: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct HotspotsOut {
    pub schema: &'static str,
    pub repo: String,
    /// `"default"` | `"pain"` — which weighting was applied.
    pub weight: &'static str,
    pub items: Vec<HotspotRow>,
    pub total: usize,
    pub truncated: bool,
}

#[derive(Debug, Deserialize)]
pub struct CouplingParams {
    pub repo: String,
    pub path: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct CouplingPartner {
    pub path: String,
    pub co_commits: i64,
    /// = co_commits (ROSE support).
    pub support: i64,
    /// co_commits / revisions(query path) — asymmetric; conf(A⇒B) ≠ conf(B⇒A).
    pub confidence: f64,
}

#[derive(Debug, Serialize)]
pub struct CouplingOut {
    pub schema: &'static str,
    pub repo: String,
    pub path: String,
    pub partners: Vec<CouplingPartner>,
    pub total: usize,
    pub truncated: bool,
}

#[derive(Debug, Deserialize)]
pub struct OwnershipParams {
    pub repo: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct AuthorShare {
    pub author: String,
    pub commits: i64,
    pub share: f64,
}

#[derive(Debug, Serialize)]
pub struct AgentShare {
    pub session_id: String,
    pub commits: i64,
    pub share: f64,
}

#[derive(Debug, Serialize)]
pub struct OwnershipOut {
    pub schema: &'static str,
    pub repo: String,
    pub path: String,
    pub total_commits: i64,
    pub authors: Vec<AuthorShare>,
    /// V3.2-B2 — dual-author session rows. Empty when no join data.
    pub agents: Vec<AgentShare>,
    /// Fraction of human commits that also resolved to a session.
    /// `null` when no session data at all (unknown ≠ none / 0.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_share: Option<f64>,
    pub major: usize,
    pub minor: usize,
    /// Top author's share (Bird-style ownership fraction).
    pub ownership: f64,
    /// Shannon entropy of author shares (fragmentation).
    pub fragmentation: f64,
}

#[derive(Debug, Deserialize)]
pub struct AgeParams {
    pub repo: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct AgeBucketOut {
    pub label: String,
    pub lines: u64,
}

#[derive(Debug, Serialize)]
pub struct AgeOut {
    pub schema: &'static str,
    pub repo: String,
    pub path: String,
    pub lines: u64,
    pub oldest_unix: Option<i64>,
    pub newest_unix: Option<i64>,
    pub median_age_days: f64,
    pub buckets: Vec<AgeBucketOut>,
}

#[derive(Debug, Deserialize)]
pub struct BackfillParams {
    pub repo: String,
}

// --- routes ---------------------------------------------------------------

/// `GET /api/behavioral/hotspots?repo=[&limit=][&scope=][&weight=pain]`
pub async fn hotspots_route(
    State(state): State<SharedState>,
    Query(params): Query<HotspotsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let limit = clamp_limit(params.limit);

    let weight_mode = match params.weight.as_deref() {
        None | Some("") | Some("default") => WeightMode::Default,
        Some("pain") => WeightMode::Pain,
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "unknown weight={other:?}; expected default or pain"
            )));
        }
    };

    let scope_filter: Option<(bool, Vec<String>)> = match params.scope.as_deref() {
        None => None,
        Some(raw) => {
            let (exclude, name) = if let Some(n) = raw.strip_prefix('!') {
                (true, n)
            } else {
                (false, raw)
            };
            if name.is_empty() {
                return Err(ApiError::bad_request(
                    "scope must be a name or !<name>, not empty",
                ));
            }
            let patterns = state
                .scopes
                .get(name)
                .ok_or_else(|| ApiError::not_found(format!("unknown scope {name:?}")))?
                .to_vec();
            Some((exclude, patterns))
        }
    };

    let repo_path = repo.path.clone();
    let store = state.store.clone();
    let out = tokio::task::spawn_blocking(move || {
        hotspots_sync(
            &store,
            repo_id,
            &repo_path,
            &params.repo,
            limit,
            scope_filter,
            weight_mode,
        )
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[derive(Clone, Copy)]
enum WeightMode {
    Default,
    Pain,
}

fn hotspots_sync(
    store: &Store,
    repo_id: i64,
    repo_root: &std::path::Path,
    repo_name: &str,
    limit: usize,
    scope_filter: Option<(bool, Vec<String>)>,
    weight_mode: WeightMode,
) -> Result<HotspotsOut, ApiError> {
    // Pain weight requires session_signals rows; missing input ⇒ 400, not a
    // silent default that reads as "fine".
    let pain_by_session: std::collections::HashMap<String, f64> = match weight_mode {
        WeightMode::Default => std::collections::HashMap::new(),
        WeightMode::Pain => {
            let sigs = store.list_session_signals(repo_id)?;
            if sigs.is_empty() {
                return Err(ApiError::bad_request(format!(
                    "weight=pain requires session_signals rows (V0018); none for this repo. \
                     Populate via join resolution + session detail (error_count). \
                     PAIN_ERROR_SCALE={PAIN_ERROR_SCALE}"
                )));
            }
            sigs.into_iter()
                .map(|s| {
                    (
                        s.session_id.clone(),
                        pain_from_signals(s.error_count, stored_fail_term(s.fail_count)).score,
                    )
                })
                .collect()
        }
    };

    let mut rows = store.list_path_stats(repo_id)?;
    if let Some((exclude, patterns)) = &scope_filter {
        rows.retain(|r| {
            let m = crate::scopes::path_matches_any(&r.path, patterns);
            if *exclude {
                !m
            } else {
                m
            }
        });
    }

    let churns: Vec<u64> = rows
        .iter()
        .map(|r| (r.lines_added + r.lines_deleted).max(0) as u64)
        .collect();
    let complexities: Vec<Complexity> = rows
        .iter()
        .map(|r| complexity_for_path(repo_root, &r.path))
        .collect();
    let complexity_totals: Vec<u64> = complexities.iter().map(|c| c.total()).collect();

    let churn_ranks = dense_ranks_desc(&churns);
    let complexity_ranks = dense_ranks_desc(&complexity_totals);

    let now = now_unix();
    let mut items: Vec<HotspotRow> = rows
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let cr = churn_ranks[i];
            let xr = complexity_ranks[i];
            let base = hotspot_score(cr, xr);
            let (score, pain_term) = match weight_mode {
                WeightMode::Default => (base, None),
                WeightMode::Pain => {
                    // Max pain among sessions dual-authored on this path.
                    let path_pain = store
                        .author_stats_for(repo_id, &r.path)
                        .ok()
                        .into_iter()
                        .flatten()
                        .filter_map(|a| {
                            session_id_from_author(&a.author)
                                .and_then(|sid| pain_by_session.get(sid).copied())
                        })
                        .fold(0.0_f64, f64::max);
                    // Blend: 0.7 base + 0.3 pain (both in 0..1-ish; base ≤ 1).
                    let score = (0.7 * base + 0.3 * path_pain).clamp(0.0, 1.0);
                    (score, Some(path_pain))
                }
            };
            let age_days = r.last_touch_unix.map(|t| {
                let d = (now - t) as f64 / 86_400.0;
                if d < 0.0 {
                    0.0
                } else {
                    d
                }
            });
            HotspotRow {
                path: r.path,
                revisions: r.revisions,
                churn: r.lines_added + r.lines_deleted,
                complexity: ComplexityOut {
                    loc: complexities[i].loc,
                    indent_sum: complexities[i].indent_sum,
                },
                hotspot: HotspotScore {
                    score,
                    terms: HotspotTerms {
                        churn_rank: cr,
                        complexity_rank: xr,
                        pain: pain_term,
                    },
                },
                last_touch_unix: r.last_touch_unix,
                age_days,
            }
        })
        .collect();

    items.sort_by(|a, b| {
        b.hotspot
            .score
            .partial_cmp(&a.hotspot.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });

    let total = items.len();
    let truncated = total > limit;
    items.truncate(limit);

    Ok(HotspotsOut {
        schema: SCHEMA,
        repo: repo_name.to_string(),
        weight: match weight_mode {
            WeightMode::Default => "default",
            WeightMode::Pain => "pain",
        },
        items,
        total,
        truncated,
    })
}

/// `GET /api/behavioral/coupling?repo=&path=[&limit=]`
pub async fn coupling_route(
    State(state): State<SharedState>,
    Query(params): Query<CouplingParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_repo, repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();
    let limit = clamp_limit(params.limit);
    let store = state.store.clone();
    let repo_name = params.repo.clone();

    let out = tokio::task::spawn_blocking(move || {
        let stats = store.path_stats_for(repo_id, &path)?;
        let revisions = stats.map(|s| s.revisions).unwrap_or(0);
        let partners_raw = store.cochange_partners(repo_id, &path)?;
        let mut partners: Vec<CouplingPartner> = partners_raw
            .into_iter()
            .map(|(p, co)| CouplingPartner {
                path: p,
                co_commits: co,
                support: co,
                confidence: coupling_confidence(co, revisions),
            })
            .collect();
        partners.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.co_commits.cmp(&a.co_commits))
                .then_with(|| a.path.cmp(&b.path))
        });
        let total = partners.len();
        let truncated = total > limit;
        partners.truncate(limit);
        Ok::<_, ApiError>(CouplingOut {
            schema: SCHEMA,
            repo: repo_name,
            path,
            partners,
            total,
            truncated,
        })
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `GET /api/behavioral/ownership?repo=&path=`
///
/// Bird-style ownership metrics. **No defect-prediction claim** — cites
/// the metric (share, entropy), not a prophecy.
///
/// V3.2-B2: human `authors` and session `agents` are separate lists
/// (dual-author rows never collapse). `agent_share` is `null` when no
/// session data exists for the path (unknown ≠ 0.0).
pub async fn ownership_route(
    State(state): State<SharedState>,
    Query(params): Query<OwnershipParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_repo, repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();
    let store = state.store.clone();
    let repo_name = params.repo.clone();

    let out = tokio::task::spawn_blocking(move || {
        let rows = store.author_stats_for(repo_id, &path)?;
        let humans: Vec<_> = rows
            .iter()
            .filter(|r| !is_session_author(&r.author))
            .collect();
        let agents_raw: Vec<_> = rows
            .iter()
            .filter(|r| is_session_author(&r.author))
            .collect();
        let total_commits: i64 = humans.iter().map(|r| r.commits).sum();
        let authors: Vec<AuthorShare> = if total_commits <= 0 {
            Vec::new()
        } else {
            humans
                .iter()
                .map(|r| AuthorShare {
                    author: r.author.clone(),
                    commits: r.commits,
                    share: r.commits as f64 / total_commits as f64,
                })
                .collect()
        };
        let agents: Vec<AgentShare> = if total_commits <= 0 {
            Vec::new()
        } else {
            agents_raw
                .iter()
                .filter_map(|r| {
                    let sid = session_id_from_author(&r.author)?;
                    Some(AgentShare {
                        session_id: sid.to_string(),
                        commits: r.commits,
                        share: r.commits as f64 / total_commits as f64,
                    })
                })
                .collect()
        };
        // null when no agent rows at all — join never resolved for this path.
        let agent_share = if agents.is_empty() {
            None
        } else {
            Some(agents.iter().map(|a| a.share).sum::<f64>().min(1.0))
        };
        let major = authors
            .iter()
            .filter(|a| a.share > MAJOR_SHARE_THRESHOLD)
            .count();
        let minor = authors.len().saturating_sub(major);
        let ownership = authors.first().map(|a| a.share).unwrap_or(0.0);
        let shares: Vec<f64> = authors.iter().map(|a| a.share).collect();
        let fragmentation = shannon_entropy(&shares);
        Ok::<_, ApiError>(OwnershipOut {
            schema: SCHEMA,
            repo: repo_name,
            path,
            total_commits,
            authors,
            agents,
            agent_share,
            major,
            minor,
            ownership,
            fragmentation,
        })
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `GET /api/behavioral/age?repo=&path=` — from blame timestamps.
pub async fn age_route(
    State(state): State<SharedState>,
    Query(params): Query<AgeParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();
    let repo_path = repo.path.clone();
    let cache = state.blame_cache.clone();
    let repo_name = params.repo.clone();

    let out = tokio::task::spawn_blocking(move || {
        let git = GitRepo::open(&repo_path).map_err(ApiError::from)?;
        let blamed =
            crate::blame::blame_file(&cache, &git, repo_id, &repo_path, &path, Some("HEAD"), None)
                .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let now = now_unix();
        let mut ages = Vec::new();
        let mut timestamps = Vec::new();
        for region in &blamed.regions {
            let age_days = ((now - region.author_time) as f64 / 86_400.0).max(0.0);
            for _ in 0..region.count {
                ages.push(age_days);
                timestamps.push(region.author_time);
            }
        }
        let r = age_buckets(&ages, now, &timestamps);
        Ok::<_, ApiError>(AgeOut {
            schema: SCHEMA,
            repo: repo_name,
            path,
            lines: r.lines,
            oldest_unix: r.oldest_unix,
            newest_unix: r.newest_unix,
            median_age_days: r.median_age_days,
            buckets: r
                .buckets
                .into_iter()
                .map(|(label, lines)| AgeBucketOut { label, lines })
                .collect(),
        })
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `POST /api/behavioral/backfill?repo=` — loopback-only bulk rebuild.
pub async fn behavioral_backfill_route(
    State(state): State<SharedState>,
    Query(params): Query<BackfillParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();
    let cfg = state.behavioral.clone();
    let store = state.store.clone();

    let stats =
        tokio::task::spawn_blocking(move || ingest::backfill_repo(&repo, repo_id, &cfg, &store))
            .await
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    // Best-effort: refresh session_signals for dual-author session rows when
    // the kb daemon is federated. Failures degrade silently (pain stays null).
    refresh_session_signals_for_repo(&state, repo_id).await;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(stats)))
}

/// Pull `error_count` / duration from kb for every `session:*` author on
/// this repo. Missing/unreachable daemon ⇒ no rows written (honest null).
async fn refresh_session_signals_for_repo(state: &SharedState, repo_id: i64) {
    let store = state.store.clone();
    let session_ids = match tokio::task::spawn_blocking(move || {
        // Distinct session ids from author_stats dual-author rows.
        let conn_paths = store.list_path_stats(repo_id).unwrap_or_default();
        let mut ids = std::collections::BTreeSet::new();
        for p in conn_paths {
            if let Ok(rows) = store.author_stats_for(repo_id, &p.path) {
                for r in rows {
                    if let Some(sid) = session_id_from_author(&r.author) {
                        ids.insert(sid.to_string());
                    }
                }
            }
        }
        ids
    })
    .await
    {
        Ok(ids) => ids,
        Err(_) => return,
    };
    if session_ids.is_empty() {
        return;
    }
    let kb = state.kb_client.clone();
    let store = state.store.clone();
    for sid in session_ids {
        match kb.session_detail(&sid).await {
            Ok(Some(detail)) => {
                let duration = if detail.active_secs > 0 {
                    detail.active_secs
                } else {
                    (detail.duration_ms / 1000).max(0)
                };
                let captured = now_unix();
                let store2 = store.clone();
                let sid2 = sid.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    store2.upsert_session_signals(
                        repo_id,
                        &sid2,
                        0, // fail_count: no distinct test-failure field on wire
                        detail.error_count as i64,
                        duration,
                        captured,
                    )
                })
                .await;
            }
            Ok(None) | Err(_) => {
                // unknown session or daemon disabled — leave pain null
            }
        }
    }
}

// --- V3.2-B2 fusion routes ------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TaintedParams {
    pub repo: String,
    pub path: String,
}

/// `GET /api/behavioral/tainted?repo=&path=` — living lines whose introducing
/// commit's session carries failure evidence (`session_signals`).
/// 400 when the repo has no session_signals (same gate as weight=pain).
pub async fn tainted_route(
    State(state): State<SharedState>,
    Query(params): Query<TaintedParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();
    let repo_path = repo.path.clone();
    let cache = state.blame_cache.clone();
    let store = state.store.clone();
    let repo_name = params.repo.clone();

    let out = tokio::task::spawn_blocking(move || {
        let sigs = store.list_session_signals(repo_id)?;
        if sigs.is_empty() {
            return Err(ApiError::bad_request(
                "tainted requires session_signals rows (V0018 failure evidence); \
                 none for this repo — missing input, not an empty taint set",
            ));
        }
        let pain_map: std::collections::HashMap<String, _> = sigs
            .into_iter()
            .map(|s| {
                let p = pain_from_signals(s.error_count, stored_fail_term(s.fail_count));
                (s.session_id, (s.error_count, s.fail_count, p))
            })
            .collect();

        let git = GitRepo::open(&repo_path).map_err(ApiError::from)?;
        let blamed =
            crate::blame::blame_file(&cache, &git, repo_id, &repo_path, &path, Some("HEAD"), None)
                .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let mut lines = Vec::new();
        let mut truncated = false;
        for region in &blamed.regions {
            if region.sha == crate::provenance::UNCOMMITTED_SHA {
                continue;
            }
            let row = store
                .get_commit_session(repo_id, &region.sha)
                .ok()
                .flatten();
            let Some(sid) = row.and_then(|r| r.session_id) else {
                continue;
            };
            let Some((err_c, fail_c, pain)) = pain_map.get(&sid) else {
                continue;
            };
            // Only lines with actual failure evidence (score > 0 or counts > 0).
            if *err_c <= 0 && *fail_c <= 0 {
                continue;
            }
            for i in 0..region.count {
                if lines.len() >= TAINTED_LINE_CAP {
                    truncated = true;
                    break;
                }
                let line = region.final_start.saturating_add(i);
                lines.push(serde_json::json!({
                    "line": line,
                    "sha": region.sha,
                    "session_id": sid,
                    "terms": {
                        "error_count": err_c,
                        "fail_count": fail_c,
                        "pain_score": pain.score,
                        "error_norm": pain.terms.error_norm,
                        "fail_norm": pain.terms.fail_norm,
                    }
                }));
            }
            if truncated {
                break;
            }
        }
        Ok::<_, ApiError>(serde_json::json!({
            "schema": FUSION_SCHEMA,
            "repo": repo_name,
            "path": path,
            "lines": lines,
            "truncated": truncated,
            "note": "Attention: lines introduced by sessions with tool-error evidence. \
                     Not a quality verdict or defect prophecy.",
        }))
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[derive(Debug, Deserialize)]
pub struct SessionCouplingParams {
    pub repo: String,
    pub session: String,
}

/// `GET /api/behavioral/session-coupling?repo=&session=` — paths co-touched
/// inside ONE session (from dual-author `session:<id>` rows). Finer than
/// co-commit coupling.
pub async fn session_coupling_route(
    State(state): State<SharedState>,
    Query(params): Query<SessionCouplingParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_repo, repo_id) = find_repo(&state, &params.repo)?;
    if params.session.is_empty() {
        return Err(ApiError::bad_request("session must be non-empty"));
    }
    let store = state.store.clone();
    let repo_name = params.repo.clone();
    let session = params.session.clone();

    let out = tokio::task::spawn_blocking(move || {
        let paths = store.paths_for_session_author(repo_id, &session)?;
        if paths.is_empty() {
            return Ok::<_, ApiError>(serde_json::json!({
                "schema": FUSION_SCHEMA,
                "repo": repo_name,
                "session": session,
                "pairs": [],
                "note": "Intra-session path co-touch; source = author_stats session rows.",
                "unavailable_reason": "no dual-author session rows for this session \
                    (join never resolved commits to it, or behavioral backfill ran \
                    before commit_sessions was warm)",
            }));
        }
        let mut pairs = Vec::new();
        for i in 0..paths.len() {
            for j in (i + 1)..paths.len() {
                let (a, b) = ordered_pair(&paths[i], &paths[j]);
                pairs.push(serde_json::json!({
                    "path_a": a,
                    "path_b": b,
                    "sessions": 1,
                }));
            }
        }
        Ok(serde_json::json!({
            "schema": FUSION_SCHEMA,
            "repo": repo_name,
            "session": session,
            "pairs": pairs,
            "note": "Intra-session path co-touch from dual-author session rows. \
                     Attention signal only — not a quality verdict.",
        }))
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

// --- V3.4-C1 time-series (request-time, no storage) -----------------------

#[derive(Debug, Deserialize)]
pub struct TimeseriesParams {
    pub repo: String,
    #[serde(default)]
    pub path: Option<String>,
    /// Lookback in weeks (default 26, hard cap 104).
    #[serde(default)]
    pub weeks: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct TimeseriesOut {
    pub schema: &'static str,
    pub repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub weeks: u32,
    pub since_unix: i64,
    pub buckets: Vec<super::TimeseriesBucket>,
    pub truncated: bool,
    pub note: String,
}

/// `GET /api/behavioral/timeseries?repo=&path=&weeks=` — per-week activity
/// buckets over the same `git log --numstat -M` walk/filters as behavioral
/// ingest. Derived at request time; **no storage**. Attention signal only
/// (activity counts — never a "health" grade). Empty window ⇒ `buckets: []`
/// + explanatory `note`, never an error.
pub async fn timeseries_route(
    State(state): State<SharedState>,
    Query(params): Query<TimeseriesParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    if let Some(ref p) = params.path {
        safe_rel_path(p)?;
    }
    let weeks = params
        .weeks
        .unwrap_or(super::TIMESERIES_DEFAULT_WEEKS)
        .clamp(1, super::TIMESERIES_MAX_WEEKS);
    let now = now_unix();
    let window_secs = (weeks as i64).saturating_mul(7).saturating_mul(86_400);
    let since_unix = now.saturating_sub(window_secs);
    let repo_path = repo.path.clone();
    let repo_name = repo.name.clone();
    let path = params.path.clone();
    let path_for_note = path.clone();

    let out = tokio::task::spawn_blocking(move || -> Result<TimeseriesOut, ApiError> {
        let (commits, truncated) = match ingest::walk_commits_capped(
            &repo_path,
            None,
            Some(since_unix),
            Some(super::TIMESERIES_MAX_COMMITS),
        ) {
            Ok(v) => v,
            Err(BehavioralError::GitLog(msg)) => {
                // Empty repo / no commits yet — not an error.
                let lower = msg.to_lowercase();
                if lower.contains("does not have any commits")
                    || lower.contains("bad revision")
                    || lower.contains("unknown revision")
                    || lower.contains("ambiguous argument")
                {
                    (Vec::new(), false)
                } else {
                    return Err(ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("git log failed: {msg}"),
                    ));
                }
            }
            Err(e) => {
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    e.to_string(),
                ));
            }
        };

        // Drop commits outside the window that slipped in (author-time vs
        // committer-time edge cases under --since). Deterministic filter.
        let commits: Vec<_> = commits
            .into_iter()
            .filter(|c| c.commit_unix >= since_unix)
            .collect();

        let buckets = super::bucket_timeseries(&commits, path.as_deref());
        let note = if buckets.is_empty() {
            match path_for_note.as_deref() {
                Some(p) => format!(
                    "No commit activity in the last {weeks} weeks \
                     (since_unix={since_unix}) for path {p:?}. \
                     Activity signal only — empty ≠ healthy or unhealthy."
                ),
                None => format!(
                    "No commit activity in the last {weeks} weeks \
                     (since_unix={since_unix}). \
                     Activity signal only — empty ≠ healthy or unhealthy."
                ),
            }
        } else if truncated {
            format!(
                "Per-week activity (commits, churn=adds+dels, distinct authors) \
                 over the last {weeks} weeks. Commit walk capped at {} \
                 (newest kept); truncated=true. Attention signal only — \
                 not a quality or health verdict.",
                super::TIMESERIES_MAX_COMMITS
            )
        } else {
            format!(
                "Per-week activity (commits, churn=adds+dels, distinct authors) \
                 over the last {weeks} weeks. Attention signal only — \
                 not a quality or health verdict."
            )
        };

        Ok(TimeseriesOut {
            schema: SCHEMA,
            repo: repo_name,
            path: path_for_note,
            weeks,
            since_unix,
            buckets,
            truncated,
            note,
        })
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

// --- head-moved worker ----------------------------------------------------

/// Background worker: on `repo.head_moved`, run incremental behavioral
/// update off the mirror hot loop (same pattern as review auto-capture).
pub fn spawn_behavioral_worker(
    store: Arc<Store>,
    bus: Arc<kb_core::events::EventBus>,
    repos: Vec<RepoEntry>,
    repo_ids: std::collections::HashMap<String, i64>,
    cfg: BehavioralSection,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if !cfg.enabled {
            tracing::info!(
                "kb-code: [behavioral] enabled = false — incremental worker idle \
                 (POST /api/behavioral/backfill still works)"
            );
            return;
        }
        let mut rx = bus.subscribe();
        loop {
            match rx.recv().await {
                Ok(env) if env.type_ == "repo.head_moved" => {
                    let repo_name = env
                        .payload
                        .get("repo")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if repo_name.is_empty() {
                        continue;
                    }
                    let Some(repo_entry) = repos.iter().find(|r| r.name == repo_name).cloned()
                    else {
                        continue;
                    };
                    let Some(&repo_id) = repo_ids.get(&repo_name) else {
                        continue;
                    };
                    let store2 = store.clone();
                    let cfg2 = cfg.clone();
                    let name = repo_name.clone();
                    match tokio::task::spawn_blocking(move || {
                        ingest::incremental_update(&repo_entry, repo_id, &cfg2, &store2)
                    })
                    .await
                    {
                        Ok(Ok(stats)) => {
                            if stats.commits > 0 || stats.full_rebuild {
                                tracing::info!(
                                    repo = %name,
                                    commits = stats.commits,
                                    full_rebuild = stats.full_rebuild,
                                    duration_ms = stats.duration_ms,
                                    "behavioral: incremental update"
                                );
                            }
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(repo = %name, error = %e, "behavioral: incremental failed");
                        }
                        Err(e) => {
                            tracing::warn!(repo = %name, error = %e, "behavioral: worker panicked");
                        }
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}
