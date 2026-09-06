//! `aug-lane/1`'s three read routes (V72-H4a): the registry, the per-path
//! facts, and the per-repo summary. All three are ordinary `auth_bearer`
//! reads — a bearer caller may READ facts; only loopback may write them
//! (`ingest`).
//!
//! `GET /api/lanes/facts` is where the classing rule actually runs. Every
//! fact it returns carries the class `classing::class_for` computed for
//! THIS request, the reason it landed there, and the line it re-anchored
//! to — plus its age and the run that produced it. A stored fact whose
//! lane the operator has since disabled is NOT returned (a disabled lane
//! is not read), and the response says how many were withheld rather than
//! quietly shrinking.
//!
//! An enabled lane with nothing to say about this path is reported in
//! `absent` with a reason and, where one exists, the command that would
//! produce facts — the design's "a miss is not an error and not a silent
//! blank" rule.

use super::classing::{self, FactAnchor};
use super::{LaneKind, LANES, LANES_SCHEMA};
use crate::routes::{find_repo, safe_rel_path, ApiError};
use crate::state::SharedState;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How many stored facts one `GET /api/lanes/facts` returns. Bounded
/// because each one may run the re-anchoring Ladder over the whole file.
pub const FACTS_PER_REQUEST: usize = 500;

#[derive(Debug, Deserialize)]
pub struct LanesParams {
    /// Narrow the counts to one repo. Absent = every repo this daemon
    /// serves.
    #[serde(default)]
    pub repo: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FactsParams {
    pub repo: String,
    pub path: String,
    #[serde(default)]
    pub lane: Option<String>,
    /// Class the facts against THIS blob instead of the working tree's
    /// current one — reading a fact set as of a revision, without moving
    /// the checkout.
    #[serde(default)]
    pub at_blob: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SummaryParams {
    pub repo: String,
}

#[derive(Debug, Serialize)]
pub struct LaneEntry {
    pub id: String,
    pub title: &'static str,
    pub kind: LaneKind,
    pub fact_schema: &'static str,
    pub fact_kinds: &'static [&'static str],
    pub sensitivity: super::Sensitivity,
    pub trust_ceiling: &'static str,
    pub retention_days: u32,
    pub adapter: Option<&'static str>,
    pub enabled: bool,
    /// `true` for the `sarif.*` TEMPLATE row: a declaration, never an
    /// addressable lane. Instances appear as their own entries.
    pub family: bool,
    /// `null` for a family template and for a lane with no stored rows.
    pub facts: Option<i64>,
    pub runs: Option<i64>,
    pub last_ingest_at: Option<i64>,
    /// Why a lane holds no rows even when it is on.
    pub note: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct LanesOut {
    pub schema: &'static str,
    pub repo: Option<String>,
    pub lanes: Vec<LaneEntry>,
    /// Ids in `[lanes] enabled` that match no registry row — reported by
    /// name so a typo is visible instead of being a lane that never runs.
    pub unknown_enabled: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RunOut {
    pub id: String,
    pub tool: String,
    pub tool_version: Option<String>,
    pub origin: String,
    pub ingested_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct FactOut {
    pub lane: String,
    pub kind: String,
    pub value: Value,
    pub severity: Option<String>,
    /// The class computed for THIS request — never read from storage.
    pub class: &'static str,
    pub reason: &'static str,
    pub line: Option<u32>,
    pub line_end: Option<u32>,
    pub shifted: bool,
    /// Seconds between the fact being produced and now.
    pub age_secs: i64,
    pub produced_at: i64,
    pub blob_sha: String,
    pub sha_source: String,
    pub run: RunOut,
}

#[derive(Debug, Serialize)]
pub struct AbsentLane {
    pub lane: String,
    pub reason: String,
    pub refresh: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct FactsOut {
    pub schema: &'static str,
    pub repo: String,
    pub path: String,
    /// The blob every class in this response was computed against — the
    /// working tree's current blob, or `at_blob` when one was given.
    pub blob: Option<String>,
    pub at_blob: Option<String>,
    pub facts: Vec<FactOut>,
    pub returned: usize,
    /// `true` when more stored facts exist than [`FACTS_PER_REQUEST`].
    pub truncated: bool,
    /// Enabled lanes with nothing to say about this path.
    pub absent: Vec<AbsentLane>,
    /// Stored facts NOT returned because their lane is currently disabled.
    pub withheld_disabled: usize,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SummaryBucket {
    pub lane: String,
    pub kind: String,
    pub severity: Option<String>,
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct SummaryOut {
    pub schema: &'static str,
    pub repo: String,
    pub buckets: Vec<SummaryBucket>,
    /// How many rows were grouped, and the bound that was applied. These
    /// counts are over STORED claims and carry NO trust class — classing
    /// is per path, per request, and a corpus-wide class would be a
    /// number nobody could reproduce.
    pub scanned: i64,
    pub scan_cap: usize,
    pub capped: bool,
    pub notes: Vec<&'static str>,
}

/// `GET /api/lanes` — the registry, with enablement and counts.
pub async fn lanes_route(
    State(state): State<SharedState>,
    Query(params): Query<LanesParams>,
) -> Result<impl IntoResponse, ApiError> {
    let repo_id = match params.repo.as_deref() {
        Some(name) => Some(find_repo(&state, name)?.1),
        None => None,
    };
    let store = state.store.clone();
    let stats = tokio::task::spawn_blocking(move || store.lane_stats(repo_id))
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let by_lane: std::collections::BTreeMap<String, crate::store::LaneStatRow> =
        stats.into_iter().map(|s| (s.lane.clone(), s)).collect();

    let mut lanes: Vec<LaneEntry> = Vec::new();
    for spec in LANES {
        lanes.push(entry(&state, spec, spec.id.to_string(), &by_lane));
        if spec.is_family() {
            // Every instance the operator declared, in config order.
            for id in &state.lanes.enabled {
                if let Some(r) = super::resolve(id) {
                    if std::ptr::eq(r.spec, spec) {
                        lanes.push(entry(&state, spec, id.clone(), &by_lane));
                    }
                }
            }
        }
    }

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(LanesOut {
            schema: LANES_SCHEMA,
            repo: params.repo,
            lanes,
            unknown_enabled: super::unknown_enabled(&state.lanes),
        }),
    ))
}

fn entry(
    state: &SharedState,
    spec: &'static super::LaneSpec,
    id: String,
    by_lane: &std::collections::BTreeMap<String, crate::store::LaneStatRow>,
) -> LaneEntry {
    let family = spec.is_family() && id == spec.id;
    let enabled = !family && state.lanes.is_enabled(&id);
    let stat = by_lane.get(&id);
    let note = if family {
        Some("a template, not an addressable lane: declare one instance per tool in `[lanes] enabled`")
    } else if spec.kind == LaneKind::Derived {
        Some("derived on demand from git and never stored, so it holds no rows")
    } else {
        None
    };
    LaneEntry {
        title: spec.title,
        kind: spec.kind,
        fact_schema: spec.fact_schema,
        fact_kinds: spec.fact_kinds,
        sensitivity: spec.sensitivity,
        trust_ceiling: spec.trust_ceiling.as_str(),
        retention_days: state.lanes.retention_days(&id, spec.retention_days_default),
        adapter: spec.adapter,
        enabled,
        family,
        facts: if family {
            None
        } else {
            Some(stat.map_or(0, |s| s.facts))
        },
        runs: if family {
            None
        } else {
            Some(stat.map_or(0, |s| s.runs))
        },
        last_ingest_at: stat.and_then(|s| s.last_ingest_at),
        note,
        id,
    }
}

/// `GET /api/lanes/facts` — the per-request classing surface.
pub async fn facts_route(
    State(state): State<SharedState>,
    Query(params): Query<FactsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();
    if let Some(lane) = params.lane.as_deref() {
        if super::resolve(lane).is_none() {
            return Err(ApiError::not_found(format!("no such lane: {lane:?}")));
        }
    }
    let repo = repo.clone();
    let lanes_cfg = state.lanes.clone();
    let lane_filter = params.lane.clone();
    let at_blob = params.at_blob.clone();
    let repo_name = params.repo.clone();
    let store = state.store.clone();
    let now = chrono::Utc::now().timestamp();

    let out = tokio::task::spawn_blocking(move || {
        let mut notes: Vec<String> = Vec::new();

        // What every class in this response is computed against.
        let (blob, text): (Option<String>, Option<String>) = match at_blob.as_deref() {
            Some(sha) => {
                let t = crate::review_comments::read_blob_text(&repo.path, &path, sha);
                if t.is_none() {
                    notes.push(format!(
                        "{path}: blob {sha} is not readable in this repo — facts anchored to \
                         other blobs cannot be re-anchored against it"
                    ));
                }
                (Some(sha.to_string()), t)
            }
            None => match crate::routes::read_repo_file(&repo, &path, None) {
                Ok(read) => {
                    let t = String::from_utf8(read.bytes).ok();
                    (Some(read.blob_hash), t)
                }
                Err(e) => {
                    notes.push(format!("{path}: {}", e.message()));
                    (None, None)
                }
            },
        };

        let rows = store
            .lane_facts_for_path(
                repo_id,
                &path,
                lane_filter.as_deref(),
                FACTS_PER_REQUEST + 1,
            )
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let truncated = rows.len() > FACTS_PER_REQUEST;
        let mut withheld_disabled = 0usize;
        let mut facts: Vec<FactOut> = Vec::new();

        for row in rows.into_iter().take(FACTS_PER_REQUEST) {
            let Some(lane) = super::resolve(&row.lane) else {
                withheld_disabled += 1;
                continue;
            };
            if !lanes_cfg.is_enabled(&lane.id) {
                withheld_disabled += 1;
                continue;
            }
            let anchor = FactAnchor {
                blob_sha: &row.blob_sha,
                sha_source: &row.sha_source,
                range_start: row.range_start,
                range_end: row.range_end,
                snippet: row.snippet.as_deref(),
                cap: None,
            };
            let classed =
                classing::class_for(lane.ceiling(), &anchor, blob.as_deref(), text.as_deref());
            facts.push(FactOut {
                lane: row.lane.clone(),
                kind: row.kind.clone(),
                value: serde_json::from_str(&row.value_json).unwrap_or(Value::Null),
                severity: row.severity.clone(),
                class: classed.class.as_str(),
                reason: classed.reason,
                line: classed.line,
                line_end: classed.line_end,
                shifted: classed.shifted,
                age_secs: now - row.produced_at,
                produced_at: row.produced_at,
                blob_sha: row.blob_sha,
                sha_source: row.sha_source,
                run: RunOut {
                    id: row.run_id,
                    tool: row.tool,
                    tool_version: row.tool_version,
                    origin: row.origin,
                    ingested_at: Some(row.ingested_at),
                },
            });
        }

        // The derived lane is computed HERE, per request, and classed by
        // the same function — one classing rule for both kinds of lane.
        let want_derived = lane_filter
            .as_deref()
            .is_none_or(|l| l == super::GIT_BEHAVIOR);
        if want_derived && lanes_cfg.is_enabled(super::GIT_BEHAVIOR) {
            let spec = super::resolve(super::GIT_BEHAVIOR).expect("registered");
            match super::git_behavior::facts_for_path(&repo.path, &path, now) {
                Some(derived) => {
                    for d in derived {
                        let blob_sha = blob.clone().unwrap_or_default();
                        let sha_source = d.sha_source();
                        let anchor = FactAnchor {
                            blob_sha: &blob_sha,
                            sha_source,
                            range_start: None,
                            range_end: None,
                            snippet: None,
                            cap: d.cap,
                        };
                        let classed = classing::class_for(
                            spec.ceiling(),
                            &anchor,
                            blob.as_deref(),
                            text.as_deref(),
                        );
                        facts.push(FactOut {
                            lane: super::GIT_BEHAVIOR.to_string(),
                            kind: d.kind.to_string(),
                            value: d.value,
                            severity: None,
                            class: classed.class.as_str(),
                            reason: classed.reason,
                            line: None,
                            line_end: None,
                            shifted: false,
                            age_secs: 0,
                            produced_at: d.produced_at,
                            blob_sha: blob_sha.clone(),
                            sha_source: sha_source.to_string(),
                            run: RunOut {
                                id: "derived".to_string(),
                                tool: "git".to_string(),
                                tool_version: None,
                                origin: "daemon".to_string(),
                                ingested_at: None,
                            },
                        });
                    }
                }
                None => notes.push(
                    "git.behavior: this repo has no HEAD yet, so nothing is derivable".to_string(),
                ),
            }
        }

        let absent = absent_lanes(&lanes_cfg, &facts, lane_filter.as_deref());

        Ok::<_, ApiError>(FactsOut {
            schema: LANES_SCHEMA,
            repo: repo_name,
            path,
            blob,
            at_blob,
            returned: facts.len(),
            facts,
            truncated,
            absent,
            withheld_disabled,
            notes,
        })
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

fn absent_lanes(
    cfg: &crate::config::LanesSection,
    facts: &[FactOut],
    lane_filter: Option<&str>,
) -> Vec<AbsentLane> {
    super::enabled(cfg)
        .into_iter()
        .filter(|l| lane_filter.is_none_or(|f| f == l.id))
        .filter(|l| !facts.iter().any(|f| f.lane == l.id))
        .map(|l| AbsentLane {
            reason: format!("{} is enabled but has no facts for this path", l.spec.title),
            refresh: l.spec.refresh_hint,
            lane: l.id,
        })
        .collect()
}

/// `GET /api/lanes/summary` — per-lane/kind/severity counts over a
/// BOUNDED set of rows, with the bound stated.
pub async fn summary_route(
    State(state): State<SharedState>,
    Query(params): Query<SummaryParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_repo, repo_id) = find_repo(&state, &params.repo)?;
    let store = state.store.clone();
    let cap = crate::store::LANE_SUMMARY_SCAN_CAP;
    let lanes_cfg = state.lanes.clone();
    let repo_name = params.repo.clone();

    let out = tokio::task::spawn_blocking(move || {
        let (rows, scanned) = store
            .lane_summary(repo_id, cap)
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let buckets: Vec<SummaryBucket> = rows
            .into_iter()
            .filter(|r| lanes_cfg.is_enabled(&r.lane))
            .map(|r| SummaryBucket {
                lane: r.lane,
                kind: r.kind,
                severity: r.severity,
                count: r.count,
            })
            .collect();
        Ok::<_, ApiError>(SummaryOut {
            schema: LANES_SCHEMA,
            repo: repo_name,
            buckets,
            scanned,
            scan_cap: cap,
            capped: scanned as usize >= cap,
            notes: vec![
                "counts are over STORED claims and carry no trust class — a class is \
                 computed per path, per request",
                "derived lanes store nothing and are absent from these buckets",
            ],
        })
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_params_require_repo_and_path() {
        assert!(serde_json::from_value::<FactsParams>(
            serde_json::json!({"repo": "r", "path": "a.rb"})
        )
        .is_ok());
        assert!(serde_json::from_value::<FactsParams>(serde_json::json!({"repo": "r"})).is_err());
        assert!(
            serde_json::from_value::<FactsParams>(serde_json::json!({"path": "a.rb"})).is_err()
        );
    }

    #[test]
    fn summary_params_require_repo_and_lanes_params_require_nothing() {
        assert!(serde_json::from_value::<SummaryParams>(serde_json::json!({"repo": "r"})).is_ok());
        assert!(serde_json::from_value::<SummaryParams>(serde_json::json!({})).is_err());
        assert!(serde_json::from_value::<LanesParams>(serde_json::json!({})).is_ok());
    }

    #[test]
    fn an_enabled_lane_with_no_facts_for_the_path_is_reported_absent_with_a_hint() {
        let cfg = crate::config::LanesSection {
            enabled: vec!["coverage.simplecov".into(), "rubocop".into()],
            retention_days: Default::default(),
        };
        let absent = absent_lanes(&cfg, &[], None);
        let ids: Vec<&str> = absent.iter().map(|a| a.lane.as_str()).collect();
        assert_eq!(ids, vec!["coverage.simplecov", "rubocop"]);
        assert!(absent[0].refresh.is_some(), "{:?}", absent[0]);
    }

    #[test]
    fn a_lane_that_answered_is_not_also_reported_absent() {
        let cfg = crate::config::LanesSection {
            enabled: vec!["rubocop".into()],
            retention_days: Default::default(),
        };
        let facts = vec![FactOut {
            lane: "rubocop".into(),
            kind: "diagnostic".into(),
            value: Value::Null,
            severity: None,
            class: "exact",
            reason: classing::REASON_BLOB_CURRENT,
            line: Some(1),
            line_end: Some(1),
            shifted: false,
            age_secs: 0,
            produced_at: 0,
            blob_sha: "b".into(),
            sha_source: "tool".into(),
            run: RunOut {
                id: "r".into(),
                tool: "rubocop".into(),
                tool_version: None,
                origin: "cli".into(),
                ingested_at: None,
            },
        }];
        assert!(absent_lanes(&cfg, &facts, None).is_empty());
    }
}
