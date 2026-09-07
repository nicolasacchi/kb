//! V3.1-H2 — compositional impact analysis
//! (`GET /api/impact/analysis?repo=&path=&line=&col=`).
//!
//! Composes usages (V3.G2) + call hierarchy callers (V3.1-H1) + type
//! implementors (H1) + reverse import edges + session provenance (blame+join).
//! Trust class labels everywhere; never a build guarantee (see [`IMPACT_NOTE`]).
//!
//! Cross-pillar: NO hotspot/coupling/risk fields (those are v3.2).
//!
//! The legacy co-change surface remains at `GET /api/impact`
//! (`agentview::impact`) — this module does not touch it.

use crate::hierarchy::{self, is_callable_kind_pub};
use crate::provenance;
use crate::resolve::{CLASS_CANDIDATE, CLASS_EXACT, CLASS_LIKELY};
use crate::routes::{find_repo, read_repo_file, ApiError};
use crate::scopes;
use crate::state::SharedState;
use crate::store::StoreBlocking;
use crate::usages::{self, UsageRow};
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

pub const IMPACT_SCHEMA: &str = "impact/1";
pub const IMPACT_NOTE: &str = "compositional impact over name resolution \
     (usages + callers + type relations + reverse imports) — class labels \
     describe resolution confidence, never a build guarantee; a wrong exact \
     is a release blocker. Transitive depth ≤ 3 over CALLERS only with \
     decaying class (depth 2+ ≤ likely, depth 3 = candidate). Test-path rows \
     are separated into the tests bucket. Provenance is blame+session join on \
     direct buckets only; null when no session data.";

/// Default / max per-bucket materialization cap (except transitive = 300).
pub const DEFAULT_BUCKET_CAP: usize = 200;
pub const MAX_BUCKET_CAP: usize = 200;
pub const TRANSITIVE_CAP: usize = 300;
const PROVENANCE_SAMPLE: usize = 5;

const DEFAULT_TESTS_GLOBS: &[&str] = &["**/tests/**", "**/*.test.*", "**/e2e/**"];

#[derive(Debug, Deserialize)]
pub struct ImpactAnalysisParams {
    pub repo: String,
    pub path: String,
    pub line: u32,
    pub col: u32,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
    /// Per-bucket cap (default 200, max 200). Exposed for tests.
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactSymbol {
    pub name: String,
    pub kind: Option<String>,
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash)]
pub struct ImpactRow {
    pub path: String,
    pub line: u32,
    pub col: u32,
    /// Trust class of this edge/hit.
    pub class: &'static str,
    /// `"usage"` | `"caller"` | `"implementor"` | `"import"` | `"transitive"`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Present on `transitive` rows only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProvenanceSample {
    pub session_id: String,
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct BucketProvenance {
    pub rows_with_session: usize,
    pub distinct_sessions: usize,
    pub sample: Vec<ProvenanceSample>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactProvenance {
    /// `null` when no session data was joinable for either direct bucket.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct_exact: Option<BucketProvenance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct_likely: Option<BucketProvenance>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactAnalysisOut {
    pub schema: &'static str,
    pub symbol: ImpactSymbol,
    pub direct_exact: Vec<ImpactRow>,
    pub direct_likely: Vec<ImpactRow>,
    pub transitive: Vec<ImpactRow>,
    pub imports: Vec<ImpactRow>,
    pub tests: Vec<ImpactRow>,
    pub truncated: ImpactTruncated,
    /// `null` when nothing could be attributed (degraded / sessionless).
    pub provenance: Option<ImpactProvenance>,
    pub note: &'static str,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ImpactTruncated {
    pub direct_exact: bool,
    pub direct_likely: bool,
    pub transitive: bool,
    pub imports: bool,
    pub tests: bool,
}

/// `GET /api/impact/analysis`
pub async fn impact_analysis_route(
    State(state): State<SharedState>,
    Query(params): Query<ImpactAnalysisParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let limit = params
        .limit
        .unwrap_or(DEFAULT_BUCKET_CAP)
        .clamp(1, MAX_BUCKET_CAP);
    let out = impact_analysis_at(
        &state,
        repo,
        repo_id,
        &params.path,
        params.line,
        params.col,
        params.rev.as_deref(),
        limit,
    )
    .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[allow(clippy::too_many_arguments)]
pub async fn impact_analysis_at(
    state: &SharedState,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
    bucket_cap: usize,
) -> Result<ImpactAnalysisOut, ApiError> {
    if line < 1 {
        return Err(ApiError::bad_request("line must be >= 1 (1-based)"));
    }

    // 2026-08-31 incident (store.rs module doc): the whole sync
    // anchor/usages/callers/types/BFS/imports composition below (every
    // store call this function makes) runs on the blocking pool in one
    // hop; `provenance_for_bucket` further down is the async (blame +
    // session-join) leg and stays outside.
    let state_bg = state.clone();
    let repo_bg = repo.clone();
    let path_bg = path.to_string();
    let rev_bg = rev.map(str::to_string);
    let composed = state
        .store
        .run_blocking(move |store| {
            impact_analysis_compose(
                store,
                &state_bg.scopes,
                &repo_bg,
                repo_id,
                &path_bg,
                line,
                col,
                rev_bg.as_deref(),
                bucket_cap,
            )
        })
        .await?;
    let ComposedImpact {
        sym_name,
        sym_kind,
        def_path,
        def_line,
        direct_exact,
        direct_likely,
        transitive,
        imports,
        tests,
        truncated,
    } = composed;

    // --- provenance on direct buckets ------------------------------------
    let prov_exact = provenance_for_bucket(state, repo, repo_id, &direct_exact).await;
    let prov_likely = provenance_for_bucket(state, repo, repo_id, &direct_likely).await;
    let provenance = match (&prov_exact, &prov_likely) {
        (None, None) => None,
        _ => Some(ImpactProvenance {
            direct_exact: prov_exact,
            direct_likely: prov_likely,
        }),
    };

    Ok(ImpactAnalysisOut {
        schema: IMPACT_SCHEMA,
        symbol: ImpactSymbol {
            name: sym_name,
            kind: sym_kind,
            path: def_path,
            line: def_line,
        },
        direct_exact,
        direct_likely,
        transitive,
        imports,
        tests,
        truncated,
        provenance,
        note: IMPACT_NOTE,
    })
}

/// The sync half of [`impact_analysis_at`] — everything that touches the
/// store, composed into one struct so it can run as a single
/// `run_blocking` closure. Split out at the 2026-08-31 incident fix
/// (store.rs module doc) rather than wrapped in place: the original body
/// interleaved store calls with the async `provenance_for_bucket` calls,
/// and a coarse closure can't itself `.await`.
struct ComposedImpact {
    sym_name: String,
    sym_kind: Option<String>,
    def_path: String,
    def_line: u32,
    direct_exact: Vec<ImpactRow>,
    direct_likely: Vec<ImpactRow>,
    transitive: Vec<ImpactRow>,
    imports: Vec<ImpactRow>,
    tests: Vec<ImpactRow>,
    truncated: ImpactTruncated,
}

#[allow(clippy::too_many_arguments)]
fn impact_analysis_compose(
    store: &crate::store::Store,
    scopes: &crate::config::ScopesSection,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
    bucket_cap: usize,
) -> Result<ComposedImpact, ApiError> {
    // Prefer a symbols-table declaration covering (line, col) so a cursor on
    // a leading keyword (`pub`/`fn`) still anchors the named def.
    let (sym_name, sym_kind, def_line, def_col) = anchor_symbol(store, repo, path, line, col, rev)?;

    // Usages at the (possibly adjusted) name col — needs an identifier.
    let usages = usages::usages_at(
        store,
        repo,
        repo_id,
        path,
        def_line,
        def_col,
        rev,
        // Pull enough to classify; bucket_cap applied after composition.
        usages::MAX_LIMIT,
    )?;

    let sym_name = if usages.symbol.name == sym_name {
        usages.symbol.name.clone()
    } else {
        // Keep symbols-table name when usages word-scan diverges.
        sym_name
    };
    let sym_kind = usages.symbol.kind.clone().or(sym_kind);
    let def_path = path.to_string();
    let line = def_line;
    let col = def_col;

    let is_callable = sym_kind
        .as_deref()
        .map(is_callable_kind_pub)
        .unwrap_or(false);
    let is_type = sym_kind.as_deref().map(is_type_kind).unwrap_or(false);

    let mut direct_exact: Vec<ImpactRow> = Vec::new();
    let mut direct_likely: Vec<ImpactRow> = Vec::new();
    let mut seen: HashSet<(String, u32, u32, String)> = HashSet::new();

    // --- usages ----------------------------------------------------------
    for row in &usages.exact {
        push_row(
            &mut direct_exact,
            &mut seen,
            usage_to_impact(row, CLASS_EXACT),
        );
    }
    for row in &usages.likely {
        push_row(
            &mut direct_likely,
            &mut seen,
            usage_to_impact(row, CLASS_LIKELY),
        );
    }
    // Candidate usages are NOT in direct_* (only exact/likely direct);
    // they may still surface as transitive/tests if also callers.

    // --- callers (callables) ---------------------------------------------
    let mut seed_callers: Vec<(String, u32, u32, &'static str, Option<String>)> = Vec::new();
    // (path, line, col, class, enclosing_name)
    if is_callable {
        if let Ok(callers) = hierarchy::callers_at(store, repo, repo_id, path, line, col, rev) {
            for g in callers.callers {
                for site in g.sites {
                    let enc_name = g.enclosing.as_ref().map(|e| e.name.clone());
                    let enc_line = g.enclosing.as_ref().map(|e| e.line).unwrap_or(site.line);
                    let row = ImpactRow {
                        path: g.path.clone(),
                        line: site.line,
                        col: site.col,
                        class: site.class,
                        kind: "caller".into(),
                        name: enc_name.clone(),
                        depth: None,
                    };
                    match site.class {
                        CLASS_EXACT => push_row(&mut direct_exact, &mut seen, row),
                        CLASS_LIKELY => push_row(&mut direct_likely, &mut seen, row),
                        _ => {
                            // candidate callers: feed transitive depth-1 only
                            // (not direct buckets)
                        }
                    }
                    seed_callers.push((g.path.clone(), enc_line, site.col, site.class, enc_name));
                }
            }
        }
    }

    // --- implementors (types) --------------------------------------------
    if is_type {
        if let Ok(types) = hierarchy::types_at(store, repo, repo_id, &sym_name, Some(path)) {
            for edge in types.subtypes {
                // implementors / overriders / subtypes
                let class = edge.class;
                let (tpath, tline) = match edge.target {
                    Some(t) => (t.path, t.line),
                    None => (edge.via.path, edge.via.line),
                };
                let row = ImpactRow {
                    path: tpath,
                    line: tline,
                    col: 0,
                    class,
                    kind: "implementor".into(),
                    name: Some(edge.name),
                    depth: None,
                };
                match class {
                    CLASS_EXACT => push_row(&mut direct_exact, &mut seen, row),
                    CLASS_LIKELY => push_row(&mut direct_likely, &mut seen, row),
                    _ => {
                        // candidate implementors stay out of direct buckets
                    }
                }
            }
        }
    }

    // --- transitive BFS over callers only --------------------------------
    let mut transitive: Vec<ImpactRow> = Vec::new();
    let mut visited_callables: HashSet<(String, u32)> = HashSet::new();
    // Seed with the original symbol so we don't re-expand it.
    visited_callables.insert((def_path.clone(), def_line));

    let mut queue: VecDeque<(String, u32, u32, &'static str, u32)> = VecDeque::new();
    // depth-1: direct callers (any class)
    for (cpath, cline, ccol, class, _name) in &seed_callers {
        let dclass = decay_class(class, 1);
        let key = (cpath.clone(), *cline);
        if !visited_callables.insert(key) {
            continue;
        }
        let row = ImpactRow {
            path: cpath.clone(),
            line: *cline,
            col: *ccol,
            class: dclass,
            kind: "transitive".into(),
            name: _name.clone(),
            depth: Some(1),
        };
        if !seen.contains(&(row.path.clone(), row.line, row.col, "caller".into())) {
            // Still list as transitive even if also in direct — brief wants
            // transitive as BFS rows with depth. Dedup against transitive only.
        }
        if transitive.len() < TRANSITIVE_CAP {
            // Avoid exact position dup within transitive.
            let tkey = (row.path.clone(), row.line, row.col);
            if !transitive
                .iter()
                .any(|r| r.path == tkey.0 && r.line == tkey.1 && r.col == tkey.2)
            {
                transitive.push(row);
            }
        }
        queue.push_back((cpath.clone(), *cline, *ccol, dclass, 1));
    }

    while let Some((cpath, cline, ccol, _class, depth)) = queue.pop_front() {
        if depth >= 3 || transitive.len() >= TRANSITIVE_CAP {
            continue;
        }
        // Expand callers of this enclosing callable.
        let next = hierarchy::callers_at(store, repo, repo_id, &cpath, cline, ccol, rev);
        let Ok(callers) = next else {
            continue;
        };
        let next_depth = depth + 1;
        for g in callers.callers {
            for site in g.sites {
                let enc_line = g.enclosing.as_ref().map(|e| e.line).unwrap_or(site.line);
                let enc_name = g.enclosing.as_ref().map(|e| e.name.clone());
                let key = (g.path.clone(), enc_line);
                if !visited_callables.insert(key) {
                    continue;
                }
                let dclass = decay_class(site.class, next_depth);
                let row = ImpactRow {
                    path: g.path.clone(),
                    line: site.line,
                    col: site.col,
                    class: dclass,
                    kind: "transitive".into(),
                    name: enc_name,
                    depth: Some(next_depth),
                };
                if transitive.len() < TRANSITIVE_CAP {
                    transitive.push(row);
                }
                if next_depth < 3 && transitive.len() < TRANSITIVE_CAP {
                    queue.push_back((g.path.clone(), enc_line, site.col, dclass, next_depth));
                }
            }
        }
    }

    // --- imports (reverse edges) -----------------------------------------
    let mut imports: Vec<ImpactRow> = Vec::new();
    if let Some(fid) = store.file_id(repo_id, path)? {
        for ipath in store.import_source_paths(fid)? {
            imports.push(ImpactRow {
                path: ipath,
                line: 1,
                col: 0,
                class: CLASS_LIKELY,
                kind: "import".into(),
                name: None,
                depth: None,
            });
        }
    }

    // --- tests separation ------------------------------------------------
    let test_globs = test_scope_patterns(scopes);
    let mut tests: Vec<ImpactRow> = Vec::new();
    let pull_tests = |bucket: &mut Vec<ImpactRow>, tests: &mut Vec<ImpactRow>| {
        let (keep, moved): (Vec<_>, Vec<_>) = bucket
            .drain(..)
            .partition(|r| !scopes::path_matches_any(&r.path, &test_globs));
        *bucket = keep;
        tests.extend(moved);
    };
    pull_tests(&mut direct_exact, &mut tests);
    pull_tests(&mut direct_likely, &mut tests);
    pull_tests(&mut transitive, &mut tests);
    pull_tests(&mut imports, &mut tests);

    // Stable sort
    for b in [
        &mut direct_exact,
        &mut direct_likely,
        &mut transitive,
        &mut imports,
        &mut tests,
    ] {
        b.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then_with(|| a.line.cmp(&b.line))
                .then_with(|| a.col.cmp(&b.col))
        });
    }

    let truncated = ImpactTruncated {
        direct_exact: direct_exact.len() > bucket_cap,
        direct_likely: direct_likely.len() > bucket_cap,
        transitive: transitive.len() > TRANSITIVE_CAP,
        imports: imports.len() > bucket_cap,
        tests: tests.len() > bucket_cap,
    };
    direct_exact.truncate(bucket_cap);
    direct_likely.truncate(bucket_cap);
    transitive.truncate(TRANSITIVE_CAP);
    imports.truncate(bucket_cap);
    tests.truncate(bucket_cap);

    Ok(ComposedImpact {
        sym_name,
        sym_kind,
        def_path,
        def_line,
        direct_exact,
        direct_likely,
        transitive,
        imports,
        tests,
        truncated,
    })
}

/// Resolve the declaration under the cursor: prefer a symbols row whose
/// name span (or body range) covers `(line, col)`, else word-scan + name
/// match on the same line.
fn anchor_symbol(
    store: &crate::store::Store,
    repo: &crate::config::RepoEntry,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
) -> Result<(String, Option<String>, u32, u32), ApiError> {
    let read = read_repo_file(repo, path, rev)?;
    let lang = crate::lang::detect(path, Some(&read.bytes));
    if let Some(li) = lang {
        if let Ok(syms) = store.symbols_for_blob(&read.blob_hash, li.symbol_salt) {
            // 1) name span hit
            if let Some(s) = syms.iter().find(|s| {
                s.line_start == line && col >= s.col_start && col < s.col_end.max(s.col_start + 1)
            }) {
                return Ok((
                    s.name.clone(),
                    Some(s.kind.clone()),
                    s.line_start,
                    s.col_start,
                ));
            }
            // 2) declaration name-line: cursor on a leading keyword (`pub`/`fn`)
            // on the same line as a single matching def name.
            let on_line: Vec<_> = syms.iter().filter(|s| s.line_start == line).collect();
            if on_line.len() == 1 {
                let s = on_line[0];
                return Ok((
                    s.name.clone(),
                    Some(s.kind.clone()),
                    s.line_start,
                    s.col_start,
                ));
            }
            // 3) word under cursor matches a same-line def name
            let content = std::str::from_utf8(&read.bytes).unwrap_or("");
            let line_text = content
                .lines()
                .nth((line as usize).saturating_sub(1))
                .unwrap_or("");
            if let Some(word) = crate::resolve::word_at(line_text.as_bytes(), col as usize) {
                if let Some(s) = on_line.into_iter().find(|s| s.name == word) {
                    return Ok((
                        s.name.clone(),
                        Some(s.kind.clone()),
                        s.line_start,
                        s.col_start,
                    ));
                }
            }
        }
    }
    // Fallback: word under cursor.
    let content = std::str::from_utf8(&read.bytes).unwrap_or("");
    let line_text = content
        .lines()
        .nth((line as usize).saturating_sub(1))
        .unwrap_or("");
    let word = crate::resolve::word_at(line_text.as_bytes(), col as usize)
        .ok_or_else(|| ApiError::bad_request(format!("no identifier at {path}:{line}:{col}")))?;
    Ok((word, None, line, col))
}

fn usage_to_impact(row: &UsageRow, class: &'static str) -> ImpactRow {
    ImpactRow {
        path: row.path.clone(),
        line: row.line,
        col: row.col,
        class,
        kind: "usage".into(),
        name: None,
        depth: None,
    }
}

fn push_row(
    bucket: &mut Vec<ImpactRow>,
    seen: &mut HashSet<(String, u32, u32, String)>,
    row: ImpactRow,
) {
    let key = (row.path.clone(), row.line, row.col, row.kind.clone());
    if seen.insert(key) {
        bucket.push(row);
    }
}

fn decay_class(class: &'static str, depth: u32) -> &'static str {
    if depth >= 3 {
        return CLASS_CANDIDATE;
    }
    if depth >= 2 {
        // never better than likely
        return match class {
            CLASS_EXACT | CLASS_LIKELY => CLASS_LIKELY,
            _ => CLASS_CANDIDATE,
        };
    }
    class
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
    )
}

/// The `[scopes] tests` globs, or [`DEFAULT_TESTS_GLOBS`] when the operator
/// named none. `pub(crate)` since V71-E1: `usages/2`'s `test` role bit is
/// the same question this bucket asks, and two copies of the default list
/// would drift.
pub(crate) fn test_scope_patterns(scopes: &crate::config::ScopesSection) -> Vec<String> {
    if let Some(pats) = scopes.map.get("tests") {
        if !pats.is_empty() {
            return pats.clone();
        }
    }
    DEFAULT_TESTS_GLOBS
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

async fn provenance_for_bucket(
    state: &SharedState,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    rows: &[ImpactRow],
) -> Option<BucketProvenance> {
    if rows.is_empty() {
        return None;
    }
    let mut rows_with_session = 0usize;
    let mut sessions: HashSet<String> = HashSet::new();
    let mut sample: Vec<ProvenanceSample> = Vec::new();
    let mut sha_cache = std::collections::HashMap::new();

    // Cap provenance work — one blame per unique path (file-grade dominant).
    let mut paths_done: HashSet<String> = HashSet::new();
    for row in rows.iter().take(50) {
        if !paths_done.insert(row.path.clone()) {
            // Still try line-level if we already have path? Use line.
        }
        let regions = match provenance::blame_regions(
            state,
            repo,
            repo_id,
            &row.path,
            Some((row.line, row.line)),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        let Some(region) = regions.into_iter().next() else {
            continue;
        };
        if region.sha == provenance::UNCOMMITTED_SHA {
            continue;
        }
        let attr =
            provenance::resolve_sha_cached(repo, repo_id, &region.sha, state, &mut sha_cache).await;
        if let Some(sid) = attr.session_id {
            rows_with_session += 1;
            if sessions.insert(sid.clone()) && sample.len() < PROVENANCE_SAMPLE {
                sample.push(ProvenanceSample {
                    session_id: sid,
                    path: row.path.clone(),
                    line: row.line,
                });
            }
        }
    }

    if rows_with_session == 0 && sessions.is_empty() {
        return None;
    }
    Some(BucketProvenance {
        rows_with_session,
        distinct_sessions: sessions.len(),
        sample,
    })
}
