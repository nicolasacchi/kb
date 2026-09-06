//! V3.1-H2 — Code Vision lens aggregation (`GET /api/lenses?repo=&path=`).
//!
//! One request per file returns per-DECLARATION rows for every
//! function/method/type symbol in the file, with class-labeled usage
//! counts, implementor counts (types), and blame/session authorship.
//!
//! Trust class: counts come from the same name-resolution machinery as
//! `/api/usages` (exact/likely/candidate) — never a build guarantee.
//!
//! V3.2-B2: `pain` is the decomposed session-pain composite when the line's
//! session has a `session_signals` row (error_count from kb-daemon
//! `GET /api/sessions/{id}`); otherwise `null` (unknown ≠ fine).

use crate::hierarchy::{self, is_callable_kind_pub};
use crate::provenance;
use crate::routes::{find_repo, read_repo_file, ApiError};
use crate::state::SharedState;
use crate::store::StoreBlocking;
use crate::usages;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const LENSES_SCHEMA: &str = "lenses/1";
pub const DECLARATION_CAP: usize = 300;

#[derive(Debug, Deserialize)]
pub struct LensesParams {
    pub repo: String,
    pub path: String,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageCounts {
    pub exact: usize,
    pub likely: usize,
    pub candidate: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthorOut {
    /// `"human"` | `"agent"` | `"mixed"`.
    pub kind: &'static str,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionOut {
    pub id: String,
    pub short: String,
}

/// Decomposed session pain (V3.2-B2). Attention signal only.
#[derive(Debug, Clone, Serialize)]
pub struct PainOut {
    pub score: f64,
    pub terms: PainTermsOut,
}

#[derive(Debug, Clone, Serialize)]
pub struct PainTermsOut {
    pub error_count: i64,
    /// `null` while no test-failure evidence exists on the sessions wire —
    /// an unmeasured term is absent, never a 0 that dilutes the score
    /// (see `behavioral::PainTerms`).
    pub fail_count: Option<i64>,
    pub error_norm: f64,
    pub fail_norm: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LensRow {
    pub line: u32,
    pub name: String,
    pub kind: String,
    pub usages: UsageCounts,
    /// Types only (from H1 type relations); `null` for callables/other.
    pub implementors: Option<usize>,
    pub author: Option<AuthorOut>,
    pub session: Option<SessionOut>,
    /// V3.2-B2 — session_signals pain composite, or `null` when unknown.
    pub pain: Option<PainOut>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LensesOut {
    pub schema: &'static str,
    pub path: String,
    pub declarations: Vec<LensRow>,
    pub truncated: bool,
    pub total: usize,
}

/// `GET /api/lenses`
pub async fn lenses_route(
    State(state): State<SharedState>,
    Query(params): Query<LensesParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let out = lenses_at(&state, repo, repo_id, &params.path, params.rev.as_deref()).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

pub async fn lenses_at(
    state: &SharedState,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    rev: Option<&str>,
) -> Result<LensesOut, ApiError> {
    let read = read_repo_file(repo, path, rev)?;
    let lang_info = crate::lang::detect(path, Some(&read.bytes));
    let salt = lang_info.map(|l| l.symbol_salt).ok_or_else(|| {
        ApiError::bad_request(format!(
            "{path}: unsupported / unindexed language for lenses"
        ))
    })?;

    // 2026-08-31 incident (store.rs module doc): the declaration scan and
    // the batch usage-count scan are independent of the async blame pull
    // below (both only need `read`/`salt`, already in hand) — coalesced
    // into ONE closure rather than two round trips to the blocking pool.
    let path_owned = path.to_string();
    let blob_hash = read.blob_hash.clone();
    let (symbols, total, truncated, count_map) = state
        .store
        .run_blocking(move |store| {
            let mut symbols = store.symbols_for_blob(&blob_hash, salt)?;
            symbols.retain(|s| is_declaration_kind(&s.kind));
            symbols.sort_by(|a, b| {
                a.line_start
                    .cmp(&b.line_start)
                    .then_with(|| a.ordinal.cmp(&b.ordinal))
            });
            let total = symbols.len();
            let truncated = total > DECLARATION_CAP;
            symbols.truncate(DECLARATION_CAP);

            // One batch count scan for every declaration name in this file.
            let name_keys: Vec<(String, Option<u32>)> = symbols
                .iter()
                .map(|s| (s.name.clone(), Some(s.line_start)))
                .collect();
            let count_map = usages::usages_counts_for_names(
                store,
                repo_id,
                &path_owned,
                &name_keys,
                Some(salt),
                &blob_hash,
            )
            .unwrap_or_default();

            Ok::<_, ApiError>((symbols, total, truncated, count_map))
        })
        .await?;

    // Blame the whole file once (cached) for author/session.
    let regions = provenance::blame_regions(state, repo, repo_id, path, None)
        .await
        .unwrap_or_default();
    let mut sha_cache: HashMap<String, crate::join::ladder::Attribution> = HashMap::new();

    // 2026-08-31 incident: batch every type-kind declaration's implementor
    // count into ONE closure too — like the scan above, it doesn't depend
    // on the per-line async author/session lookups in the loop below.
    let type_names: Vec<String> = symbols
        .iter()
        .filter(|s| is_type_kind(&s.kind))
        .map(|s| s.name.clone())
        .collect();
    let repo_bg = repo.clone();
    let path_bg = path.to_string();
    let implementors_map: HashMap<String, usize> = state
        .store
        .run_blocking(move |store| {
            let mut map = HashMap::new();
            for name in type_names {
                if let Ok(t) = hierarchy::types_at(store, &repo_bg, repo_id, &name, Some(&path_bg))
                {
                    map.insert(name, t.subtypes.len());
                }
            }
            map
        })
        .await;

    let mut declarations = Vec::with_capacity(symbols.len());
    for sym in symbols {
        let counts = count_map.get(&sym.name).copied().unwrap_or_default();

        let implementors = if is_type_kind(&sym.kind) {
            implementors_map.get(&sym.name).copied()
        } else {
            None
        };

        let (author, session) = author_session_for_line(
            state,
            repo,
            repo_id,
            path,
            sym.line_start,
            &regions,
            &mut sha_cache,
        )
        .await;

        // Pain from session_signals when the line's session is known.
        // 2026-08-31 incident: this store call depends on `session`, which
        // only exists after the async author/session lookup above, so it
        // can't be hoisted out of the loop — still wrapped per iteration.
        let pain = match session.as_ref() {
            Some(s) => {
                let sid = s.id.clone();
                let row = state
                    .store
                    .run_blocking(move |store| {
                        store.session_signals_for(repo_id, &sid).ok().flatten()
                    })
                    .await;
                row.map(|row| {
                    let p = crate::behavioral::pain_from_signals(
                        row.error_count,
                        crate::behavioral::stored_fail_term(row.fail_count),
                    );
                    PainOut {
                        score: p.score,
                        terms: PainTermsOut {
                            error_count: p.terms.error_count,
                            fail_count: p.terms.fail_count,
                            error_norm: p.terms.error_norm,
                            fail_norm: p.terms.fail_norm,
                        },
                    }
                })
            }
            None => None,
        };

        declarations.push(LensRow {
            line: sym.line_start,
            name: sym.name,
            kind: sym.kind,
            usages: UsageCounts {
                exact: counts.exact,
                likely: counts.likely,
                candidate: counts.candidate,
            },
            implementors,
            author,
            session,
            pain,
        });
    }

    Ok(LensesOut {
        schema: LENSES_SCHEMA,
        path: path.to_string(),
        declarations,
        truncated,
        total,
    })
}

async fn author_session_for_line(
    state: &SharedState,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    _path: &str,
    line: u32,
    regions: &[crate::blame::BlameRegion],
    sha_cache: &mut HashMap<String, crate::join::ladder::Attribution>,
) -> (Option<AuthorOut>, Option<SessionOut>) {
    let region = regions.iter().find(|r| {
        let end = r.final_start.saturating_add(r.count.saturating_sub(1));
        r.final_start <= line && line <= end
    });
    let Some(region) = region else {
        return (None, None);
    };
    if region.sha == provenance::UNCOMMITTED_SHA {
        return (None, None);
    }
    let attr = provenance::resolve_sha_cached(repo, repo_id, &region.sha, state, sha_cache).await;
    let short_sha = short_id(&region.sha, 7);
    if let Some(sid) = attr.session_id {
        let short = short_id(&sid, 8);
        (
            Some(AuthorOut {
                kind: "agent",
                label: short.clone(),
            }),
            Some(SessionOut { id: sid, short }),
        )
    } else {
        (
            Some(AuthorOut {
                kind: "human",
                label: short_sha,
            }),
            None,
        )
    }
}

fn short_id(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn is_declaration_kind(kind: &str) -> bool {
    is_callable_kind_pub(kind) || is_type_kind(kind)
}

fn is_type_kind(kind: &str) -> bool {
    matches!(
        kind,
        "struct"
            | "class"
            | "trait"
            | "interface"
            | "type"
            | "enum"
            | "union"
            | "impl"
            | "module"
            | "mod"
            | "namespace"
            | "const"
            | "static"
            | "var"
            | "let"
    )
}
