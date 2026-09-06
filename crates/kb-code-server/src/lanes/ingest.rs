//! `lane-ingest/1` — `POST /api/lanes/{lane}/ingest` (V72-H4a).
//!
//! The ONE way a fact enters this daemon that the daemon did not derive
//! itself. It is registered on the loopback-only sub-router beside
//! `checkout` and `apply-suggestion`, so it inherits that admission rung
//! without a per-route check, and — being a POST under `/api` — it lands
//! in the V0027 mutations ledger automatically, with `?repo=` recorded as
//! its target.
//!
//! ## What the daemon does to a claim before storing it
//!
//! 1. **The lane must be registered AND enabled.** An unknown id is a 404;
//!    a registered-but-disabled one is a 403 naming `[lanes] enabled` —
//!    never a silently accepted write into a lane nobody reads. A DERIVED
//!    lane refuses too: `git.behavior` has no ingest by construction.
//! 2. **Caps refuse, they never truncate.** Over [`MAX_BODY_BYTES`] or
//!    [`MAX_FACTS_PER_RUN`] is a 413 stating both the count and the cap.
//!    A truncated ingest would be a fact set that silently disagrees with
//!    the tool that produced it.
//! 3. **Every path is canonicalised and contained.** Each distinct path
//!    goes through `routes::read_repo_file`, which is
//!    `safe_rel_path` + the secret denylist + `contained_abs_path` —
//!    the same floor every content read in this crate stands on. A path
//!    that escapes the root is refused BY ROW, counted, and named in the
//!    response; the rest of the batch still lands.
//! 4. **Blob attribution is recorded, not assumed.** A fact that named its
//!    own `blob_sha` keeps it with `sha_source = "tool"`. A fact that named
//!    none gets the working tree's current blob with `sha_source =
//!    "mirror_at_ingest"` — structurally weaker, and capped at `likely`
//!    forever by `classing::class_for`. A path with no readable bytes and
//!    no tool-named blob is refused: there is nothing honest to anchor it
//!    to.
//! 5. **The Ladder's anchor is captured while the bytes are in hand.** A
//!    line-anchored fact whose blob matches what is on disk right now
//!    stores that line's text as its snippet; a fact about some OTHER blob
//!    stores none, because this daemon cannot read the bytes that fact was
//!    about and inventing a snippet from the wrong blob would manufacture
//!    a re-anchor that was never true.
//!
//! An ingest REPLACES the lane's facts for every path it names (see
//! `Store::replace_lane_facts`), so re-running a tool is idempotent and a
//! large output can be POSTed in several batches without each erasing the
//! last.

use super::classing::{SHA_SOURCE_MIRROR, SHA_SOURCE_TOOL};
use super::{LaneKind, LANES_SCHEMA};
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{LaneFactIn, LaneRunIn};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Hard cap on the raw request body. Above this the request is refused
/// with counts, never truncated.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Slack over [`MAX_BODY_BYTES`] for the axum body limit, so THIS route's
/// own 413 (which states the numbers) fires before the framework's.
pub const BODY_LIMIT_SLACK: usize = 64 * 1024;
/// Hard cap on facts in one run.
pub const MAX_FACTS_PER_RUN: usize = 20_000;
/// How many refusal rows are echoed back; the count is always exact.
pub const MAX_REFUSALS_REPORTED: usize = 50;

#[derive(Debug, Deserialize)]
pub struct IngestParams {
    pub repo: String,
}

#[derive(Debug, Deserialize)]
pub struct RunIn {
    pub tool: String,
    #[serde(default)]
    pub tool_version: Option<String>,
    /// The argv the OPERATOR ran, already redacted by the CLI. Stored as
    /// evidence and never parsed — this daemon spawns nothing but git.
    #[serde(default)]
    pub argv_redacted: Option<String>,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub finished_at: Option<i64>,
}

/// A fact's line range: `[start, end]` or a bare line number. Both 1-based
/// and inclusive; absent means a file-level fact.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(untagged)]
pub enum RangeIn {
    Pair([u32; 2]),
    Line(u32),
}

impl RangeIn {
    fn bounds(self) -> (u32, u32) {
        match self {
            Self::Pair([a, b]) => (a, b.max(a)),
            Self::Line(a) => (a, a),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct FactIn {
    pub path: String,
    #[serde(default)]
    pub blob_sha: Option<String>,
    #[serde(default)]
    pub range: Option<RangeIn>,
    pub kind: String,
    pub value: Value,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub produced_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct IngestBody {
    #[serde(default)]
    pub schema: Option<String>,
    pub run: RunIn,
    pub facts: Vec<FactIn>,
    /// Paths the tool INSPECTED and had nothing to say about. This lane's
    /// facts for each are deleted in the SAME transaction as the insert,
    /// which is what makes "the offense was fixed" expressible: without it
    /// a re-run would simply not mention the file and the old facts would
    /// live forever. Validated exactly like a fact's path; a bad one is
    /// refused by row and counted.
    #[serde(default)]
    pub clear_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Refusal {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct IngestOut {
    pub schema: &'static str,
    pub lane: String,
    pub repo: String,
    pub run_id: String,
    pub accepted: usize,
    pub refused: usize,
    /// The first [`MAX_REFUSALS_REPORTED`] refusals. `refused` above is
    /// always the true total.
    pub refusals: Vec<Refusal>,
    /// How many accepted facts carry a tool-named blob vs a blob the
    /// daemon attributed — the difference between a fact that can reach
    /// `exact` and one that never will.
    pub sha_source: BTreeMap<String, usize>,
    /// How many accepted facts carry a Ladder snippet.
    pub anchored: usize,
    /// How many paths this run cleared (inspected, nothing found).
    pub cleared: usize,
}

/// Mint a run id. Same shape as this crate's other short ids: 12 hex.
fn new_run_id() -> String {
    crate::annotations::short_random_hex()
}

pub async fn ingest_route(
    State(state): State<SharedState>,
    AxumPath(lane_id): AxumPath<String>,
    Query(params): Query<IngestParams>,
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let lane = super::resolve(&lane_id)
        .ok_or_else(|| ApiError::not_found(format!("no such lane: {lane_id:?}")))?;
    if !state.lanes.is_enabled(&lane.id) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            format!(
                "lane {:?} is registered but not enabled — add it to `[lanes] enabled` in \
                 kb-code.toml (a lane is never enabled by a request)",
                lane.id
            ),
        )
        .with_reason("lane-disabled"));
    }
    if lane.kind() == LaneKind::Derived {
        return Err(ApiError::bad_request_with_reason(
            format!(
                "lane {:?} is derived: this daemon computes it from git on demand and it has \
                 no ingest",
                lane.id
            ),
            "lane-not-ingestable",
        ));
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "lane-ingest body is {} bytes; hard cap is {MAX_BODY_BYTES} — refused, not \
                 truncated (post the run in smaller batches)",
                body.len()
            ),
        )
        .with_reason("lane-body-cap"));
    }
    let parsed: IngestBody = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("lane-ingest/1 body: {e}")))?;
    if let Some(schema) = parsed.schema.as_deref() {
        if schema != super::INGEST_SCHEMA {
            return Err(ApiError::bad_request(format!(
                "body declares schema {schema:?}; this route reads {}",
                super::INGEST_SCHEMA
            )));
        }
    }
    let rows = parsed.facts.len() + parsed.clear_paths.len();
    if rows > MAX_FACTS_PER_RUN {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "lane-ingest carries {rows} rows ({} facts + {} clear_paths); hard cap is \
                 {MAX_FACTS_PER_RUN} — refused, not truncated",
                parsed.facts.len(),
                parsed.clear_paths.len()
            ),
        )
        .with_reason("lane-facts-cap"));
    }

    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();
    let repo_name = params.repo.clone();
    let lane_id = lane.id.clone();
    let now = chrono::Utc::now().timestamp();

    let out =
        tokio::task::spawn_blocking(move || {
            // One working-tree read per DISTINCT path, reused by every fact on
            // it. `read_repo_file` is the containment + secret floor.
            let mut files: BTreeMap<String, Option<(String, Option<String>)>> = BTreeMap::new();
            let mut accepted: Vec<LaneFactIn> = Vec::new();
            let mut refusals: Vec<Refusal> = Vec::new();
            let mut refused = 0usize;
            let mut sha_source: BTreeMap<String, usize> = BTreeMap::new();
            let mut anchored = 0usize;

            for f in &parsed.facts {
                let mut refuse = |path: &str, reason: String| {
                    refused += 1;
                    if refusals.len() < MAX_REFUSALS_REPORTED {
                        refusals.push(Refusal {
                            path: path.to_string(),
                            reason,
                        });
                    }
                };
                if !lane.accepts_kind(&f.kind) {
                    refuse(
                        &f.path,
                        format!(
                            "kind {:?} is not one of lane {:?}'s declared kinds {:?}",
                            f.kind, lane.id, lane.spec.fact_kinds
                        ),
                    );
                    continue;
                }
                if let Some(sev) = f.severity.as_deref() {
                    if !super::adapters::SEVERITIES.contains(&sev) {
                        refuse(
                            &f.path,
                            format!(
                                "severity {sev:?} is outside the vocabulary {:?}",
                                super::adapters::SEVERITIES
                            ),
                        );
                        continue;
                    }
                }
                let range = f.range.map(|r| r.bounds());
                if let Some((start, _)) = range {
                    if start == 0 {
                        refuse(&f.path, "line numbers are 1-based; got 0".to_string());
                        continue;
                    }
                }

                let entry = files.entry(f.path.clone()).or_insert_with(|| {
                    match crate::routes::read_repo_file(&repo, &f.path, None) {
                        Ok(read) => {
                            let text = String::from_utf8(read.bytes).ok();
                            Some((read.blob_hash, text))
                        }
                        Err(_) => None,
                    }
                });

                let (blob_sha, sha_src) = match (f.blob_sha.as_deref(), entry.as_ref()) {
                    (Some(sha), _) if !sha.trim().is_empty() => {
                        (sha.trim().to_string(), SHA_SOURCE_TOOL)
                    }
                    (_, Some((current, _))) => (current.clone(), SHA_SOURCE_MIRROR),
                    (_, None) => {
                        refuse(
                            &f.path,
                            "not readable under the repository root (missing, outside it, or \
                         refused by the secret policy) and the fact named no blob of its own"
                                .to_string(),
                        );
                        continue;
                    }
                };

                // The snippet is only honest when the bytes on disk ARE the
                // bytes this fact is about.
                let snippet = match (range, entry.as_ref()) {
                    (Some((start, _)), Some((current, Some(text)))) if *current == blob_sha => text
                        .lines()
                        .nth(start as usize - 1)
                        .map(|l| crate::annotations::anchor_for_line(start, l))
                        .and_then(|a| match a {
                            kb_core::review::Anchor::Selection { snippet, .. } => Some(snippet),
                            _ => None,
                        }),
                    _ => None,
                };
                if snippet.is_some() {
                    anchored += 1;
                }
                *sha_source.entry(sha_src.to_string()).or_default() += 1;

                accepted.push(LaneFactIn {
                    path: f.path.clone(),
                    blob_sha,
                    sha_source: sha_src,
                    range_start: range.map(|(s, _)| s),
                    range_end: range.map(|(_, e)| e),
                    snippet,
                    kind: f.kind.clone(),
                    value_json: f.value.to_string(),
                    severity: f.severity.clone(),
                    produced_at: f.produced_at.unwrap_or(now),
                });
            }

            // `clear_paths` gets the SAME lexical gate a fact path does.
            // It needs no working-tree read (nothing is anchored), but a
            // traversal attempt must not reach a DELETE either.
            let mut clear: Vec<String> = Vec::new();
            for p in &parsed.clear_paths {
                match crate::routes::safe_rel_path(p) {
                    Ok(ok) => clear.push(ok.to_string()),
                    Err(e) => {
                        refused += 1;
                        if refusals.len() < MAX_REFUSALS_REPORTED {
                            refusals.push(Refusal {
                                path: p.clone(),
                                reason: format!("clear_paths: {}", e.message()),
                            });
                        }
                    }
                }
            }

            let run = LaneRunIn {
                run_id: new_run_id(),
                lane: lane_id.clone(),
                repo_id,
                tool: parsed.run.tool.clone(),
                tool_version: parsed.run.tool_version.clone(),
                argv_redacted: parsed.run.argv_redacted.clone(),
                started_at: parsed.run.started_at,
                finished_at: parsed.run.finished_at,
                origin: "cli",
                ingested_at: now,
            };
            state
                .store
                .replace_lane_facts(&run, &accepted, &clear)
                .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            Ok::<_, ApiError>(IngestOut {
                schema: LANES_SCHEMA,
                lane: lane_id,
                repo: repo_name,
                run_id: run.run_id,
                accepted: accepted.len(),
                refused,
                refusals,
                sha_source,
                anchored,
                cleared: clear.len(),
            })
        })
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(axum::http::header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_accepts_both_a_pair_and_a_bare_line() {
        let f: FactIn =
            serde_json::from_str(r#"{"path":"a.rb","range":[3,7],"kind":"diagnostic","value":{}}"#)
                .unwrap();
        assert_eq!(f.range.unwrap().bounds(), (3, 7));
        let f: FactIn =
            serde_json::from_str(r#"{"path":"a.rb","range":9,"kind":"diagnostic","value":{}}"#)
                .unwrap();
        assert_eq!(f.range.unwrap().bounds(), (9, 9));
        let f: FactIn =
            serde_json::from_str(r#"{"path":"a.rb","kind":"file_fact","value":{}}"#).unwrap();
        assert!(f.range.is_none());
    }

    #[test]
    fn an_inverted_range_is_normalised_rather_than_stored_backwards() {
        let f: FactIn =
            serde_json::from_str(r#"{"path":"a.rb","range":[9,2],"kind":"diagnostic","value":{}}"#)
                .unwrap();
        assert_eq!(f.range.unwrap().bounds(), (9, 9));
    }

    #[test]
    fn the_params_struct_requires_repo() {
        assert!(serde_json::from_value::<IngestParams>(serde_json::json!({"repo": "r"})).is_ok());
        assert!(serde_json::from_value::<IngestParams>(serde_json::json!({})).is_err());
    }

    #[test]
    fn a_run_id_is_twelve_hex_characters() {
        let id = new_run_id();
        assert_eq!(id.len(), 12);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(id, new_run_id());
    }

    #[test]
    fn the_body_declares_its_schema_and_a_missing_one_is_allowed() {
        let b: IngestBody =
            serde_json::from_str(r#"{"schema":"lane-ingest/1","run":{"tool":"t"},"facts":[]}"#)
                .unwrap();
        assert_eq!(b.schema.as_deref(), Some(super::super::INGEST_SCHEMA));
        let b: IngestBody = serde_json::from_str(r#"{"run":{"tool":"t"},"facts":[]}"#).unwrap();
        assert!(b.schema.is_none());
    }
}
