//! V71-F1 — `kbc-tree/1`: the projected, decorated file tree, computed ONCE
//! server-side.
//!
//! ## Why the projection is not in the SPA
//!
//! The evidence report's last risk: "Two renderers of one projection will
//! diverge. The projection must be computed *once* server-side and both
//! surfaces must render the same `kbc-tree/1` rows; the CLI's terminal
//! renderer is a formatter, not a second implementation." So this module
//! owns the shapes and both `web-code`'s `FileTree` and `kb-code tree`
//! render [`TreeRow`]s they did not compute. The one thing the SPA still
//! decides for itself is which groups are OPEN, and it says so on the wire
//! (`expand=`) rather than re-deriving the row list.
//!
//! ## Four projections, and the honesty rule
//!
//! `physical` (directories, the mirror index), `role` (Rails role buckets,
//! a path CONVENTION), `namespace` (the V71-G0 entity index) and `change`
//! (what differs from a base ref). Two of those are INFERENCES, and the
//! report's non-negotiable rule applies to both:
//!
//! - every inferred grouping carries its trust class ([`roles::ROLE_TRUST`]
//!   for `role`, [`crate::entities::class_for`] for `namespace`) and
//!   structurally cannot be `exact` for a convention;
//! - a file NEVER silently disappears because a projection failed to place
//!   it. Every projection ends with an [`TreeOut::unplaced`] bucket, and
//!   `unplaced_total` is on the wire whether or not the bucket itself was
//!   capped. "A projection that hides work is worse than no projection."
//!
//! ## Honest caps
//!
//! `truncated` and `unplaced` are the load-bearing fields: an agent must be
//! able to tell "you were given part of the tree". Every cap here reports
//! itself in band ([`Truncation`]), and the decoration budget
//! ([`MAX_DECORATION_LANES`]) names the lanes it dropped rather than
//! silently rendering two of the three you asked for.

pub mod project;
pub mod roles;
pub mod scope;
pub mod sources;

// The engine's shapes ARE this module's shapes — one import path for a
// caller, one home for the arithmetic.
pub use project::{
    flatten, project_change, project_namespace, project_physical, project_role, rebase_ranges,
    resolve_lanes, severity_rank, EntityPlacement, Facts, FlattenOpts, Node, RowKind, TreeCounts,
    TreeOut, TreeRow, Truncation, CHANGE_BUCKETS, DEFAULT_LIMIT, LANES, MAX_DECORATION_LANES,
    MAX_TREE_ROWS, MAX_UNPLACED_ROWS, MODE_FILTER, MODE_HIGHLIGHT, TREE_SCHEMA, VIEWS,
};

use std::collections::HashSet;

use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use crate::entities::RouteContract;
use crate::routes::{find_repo, ApiError};
use crate::search::matcher::{HaystackKind, NameMatcher};
use crate::state::SharedState;
use crate::store::StoreBlocking;

/// Group keys a client may declare open in one request.
pub const MAX_EXPAND_KEYS: usize = 500;

/// Entity definition rows read for the `namespace` projection.
pub const MAX_ENTITY_DEFS: usize = 50_000;

// ── params ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TreeV2Params {
    pub repo: String,
    #[serde(default)]
    pub view: Option<String>,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub depth: Option<u32>,
    #[serde(default)]
    pub expand: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub decorate: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub review: Option<i64>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub worktree: Option<String>,
}

// ── the route ─────────────────────────────────────────────────────────────

/// `GET /api/tree/2` — kbc-tree/1. `/api/tree` (the per-directory ODB
/// listing) is FROZEN and untouched, the same "one ladder, two wires"
/// treatment V71-E1 gave `/api/usages/2`.
pub async fn tree_v2_route(
    State(state): State<SharedState>,
    Query(params): Query<TreeV2Params>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = repo.name.clone();
    let repo_root = repo.path.clone();

    let view = params
        .view
        .as_deref()
        .unwrap_or("physical")
        .trim()
        .to_string();
    if !VIEWS.contains(&view.as_str()) {
        return Err(ApiError::bad_request(format!(
            "unknown view {view:?} — expected one of {}",
            VIEWS.join(", ")
        )));
    }
    let mode = match params.mode.as_deref().map(str::trim) {
        None | Some("") | Some(MODE_FILTER) => MODE_FILTER,
        Some(MODE_HIGHLIGHT) => MODE_HIGHLIGHT,
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "unknown mode {other:?} — expected {MODE_FILTER} or {MODE_HIGHLIGHT}"
            )))
        }
    };
    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_TREE_ROWS);
    let depth = params.depth.unwrap_or(1);
    let root = params
        .root
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| crate::routes::safe_rel_path(s).map(str::to_string))
        .transpose()?;

    let mut notes: Vec<String> = Vec::new();
    let (lanes, dropped) = resolve_lanes(params.decorate.as_deref());
    for d in &dropped {
        notes.push(format!(
            "decoration lane `{d}` dropped — kbc-tree/1 renders {MAX_DECORATION_LANES} lanes \
             and you asked for more"
        ));
    }
    let expand: HashSet<String> = params
        .expand
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(MAX_EXPAND_KEYS)
        .map(str::to_string)
        .collect();

    let base = match params
        .base
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(b) => Some(crate::routes::parse_revspec(b)?),
        None => None,
    };
    let needs_git = lanes.iter().any(|l| l == "git") || view == "change";
    let base_label = base
        .as_ref()
        .map(|r| r.as_str().to_string())
        .or_else(|| needs_git.then(|| crate::reviews::default_base_ref(&repo_root)));

    let scope_raw = params
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let filter_raw = params
        .filter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let review_id = params.review;
    let worktree = params.worktree.clone();
    let view_for_task = view.clone();
    let lanes_for_task = lanes.clone();
    let scope_for_task = scope_raw.clone();
    let base_for_task = base_label.clone();
    let cfg_scopes = state.scopes.map.clone();

    let built = state
        .store
        .run_blocking(move |store| {
            sources::build(sources::BuildReq {
                store,
                repo_id,
                repo_name: &repo_name,
                repo_root: &repo_root,
                view: &view_for_task,
                worktree: worktree.as_deref(),
                scope: scope_for_task.as_deref(),
                lanes: &lanes_for_task,
                review_id,
                base: base_for_task.as_deref(),
                config_scopes: cfg_scopes,
            })
        })
        .await?;

    let sources::Built {
        generation,
        mut paths,
        scope_applied,
        scope_normalized,
        mut scope_notes,
        scope_diagnostics,
        facts,
        entity_defs,
        changes,
        mut build_notes,
    } = built;

    notes.append(&mut scope_notes);
    notes.append(&mut build_notes);

    // Re-root: a `root=` narrows the CANDIDATE set before the projection
    // runs, so every projection re-roots the same way and none of them has
    // to know what re-rooting means.
    if let Some(r) = &root {
        paths.retain(|p| p == r || p.starts_with(&format!("{r}/")));
    }
    let considered = paths.len() as u32;

    let (nodes, unplaced_paths) = match view.as_str() {
        "physical" => (project_physical(&paths), Vec::new()),
        "role" => project_role(&paths),
        "namespace" => {
            let (n, u) = project_namespace(&paths, &entity_defs);
            if n.is_empty() {
                notes.push(
                    "the namespace projection is empty — this repo has no entity_defs rows \
                     (the V71-G0 index covers Ruby only, and only after a reconcile)"
                        .to_string(),
                );
            }
            (n, u)
        }
        "change" => {
            // A scope/root narrows the change list the same way it narrows
            // every other projection — but only when one was actually
            // given: a DELETED path has no `files` row at all, so an
            // unconditional intersection would silently drop every
            // deletion. Narrowed, those deletions ARE dropped (a scope
            // cannot be evaluated against a path with no index row) and
            // the count says so.
            let in_scope: HashSet<&str> = paths.iter().map(String::as_str).collect();
            let narrowed = scope_applied || root.is_some();
            let filtered: Vec<(String, String)> = changes
                .iter()
                .filter(|(p, _)| !narrowed || in_scope.contains(p.as_str()))
                .cloned()
                .collect();
            notes.push(format!(
                "`change` lists only what differs from {} — the {} unchanged indexed \
                 file(s) are unplaced BY CONSTRUCTION and are not listed",
                base_label.as_deref().unwrap_or("the base"),
                considered.saturating_sub(filtered.len() as u32)
            ));
            if narrowed {
                let dropped = changes.len() - filtered.len();
                if dropped > 0 {
                    notes.push(format!(
                        "{dropped} changed path(s) fell outside the scope/root, INCLUDING any \
                         deletion (a deleted path has no index row for a scope to match)"
                    ));
                }
            }
            (project_change(&filtered), Vec::new())
        }
        _ => unreachable!("view validated above"),
    };

    let mut matcher = filter_raw
        .as_deref()
        .map(|f| NameMatcher::new(f, HaystackKind::Path));
    let want_facts = !lanes.is_empty();
    let (rows, matched, total_rows, row_truncated) = {
        let mut opts = FlattenOpts {
            expand: &expand,
            depth,
            limit,
            mode,
            matcher: matcher.as_mut(),
            facts: &facts,
            want_facts,
        };
        flatten(&nodes, &mut opts)
    };

    let unplaced_total = unplaced_paths.len() as u32;
    let unplaced: Vec<TreeRow> = unplaced_paths
        .iter()
        .take(MAX_UNPLACED_ROWS)
        .map(|p| TreeRow {
            id: format!("u:{p}"),
            kind: RowKind::File,
            label: p.clone(),
            depth: 0,
            path: Some(p.clone()),
            ent: None,
            trust: None,
            children: 0,
            files: 1,
            has_more: false,
            match_ranges: Vec::new(),
            match_count: 0,
            facts: if want_facts {
                facts.get(p).cloned().unwrap_or_default()
            } else {
                Facts::default()
            },
        })
        .collect();

    if unplaced_total > 0 {
        notes.push(format!(
            "{unplaced_total} file(s) the `{view}` projection could not place — listed \
             under `unplaced`{}",
            if unplaced_total as usize > MAX_UNPLACED_ROWS {
                format!(" (first {MAX_UNPLACED_ROWS} of them)")
            } else {
                String::new()
            }
        ));
    }

    let truncated = if row_truncated {
        Some(Truncation {
            by: if limit < MAX_TREE_ROWS {
                "limit".to_string()
            } else {
                "max_rows".to_string()
            },
            returned: rows.len() as u32,
            total: Some(total_rows),
            reason: format!(
                "the `{view}` projection produced {total_rows} rows; {} were returned",
                rows.len()
            ),
        })
    } else if unplaced_total as usize > MAX_UNPLACED_ROWS {
        Some(Truncation {
            by: "unplaced".to_string(),
            returned: MAX_UNPLACED_ROWS as u32,
            total: Some(unplaced_total),
            reason: "the unplaced bucket is capped; `unplaced_total` is exact".to_string(),
        })
    } else {
        None
    };

    let out = TreeOut {
        schema: TREE_SCHEMA,
        repo: params.repo.clone(),
        view,
        views_available: VIEWS.to_vec(),
        generation,
        root,
        depth,
        scope: scope_normalized,
        scope_applied,
        filter: filter_raw,
        mode: mode.to_string(),
        decorate: lanes,
        base: base_label,
        counts: TreeCounts {
            files: considered,
            rows: total_rows,
            matched: matcher.is_some().then_some(matched),
        },
        rows,
        unplaced,
        unplaced_total,
        truncated,
        notes,
        diagnostics: scope_diagnostics,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

// ── the declaration↔handler walk ─────────────────────────────────────────

pub const TREE_V2_ROUTE: RouteContract = RouteContract {
    path: "/api/tree/2",
    handler: "tree::tree_v2_route",
    required_params: &["repo"],
    params_accept_without: tree_params_accept_without,
};

fn tree_params_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [
        ("repo", "r"),
        ("view", "physical"),
        ("scope", "role:model"),
        ("filter", "order"),
        ("mode", MODE_FILTER),
        ("decorate", "git"),
    ] {
        if k != omit {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<TreeV2Params>(serde_json::Value::Object(map)).is_ok()
}

/// Every route V71-F1 adds. Walked from BOTH sides, exactly as
/// `entities::V71_G0_ROUTES` is: this crate's
/// [`tests::every_declared_v71_f1_route_is_registered_and_requires_its_params`]
/// against `router.rs`, and kb-code-cli's
/// `cli_requests_send_every_param_their_route_requires` against the verbs
/// that build the request. Neither test can see a route missing from this
/// list, which is why it lives beside the route.
pub const V71_F1_ROUTES: &[RouteContract] = &[TREE_V2_ROUTE];

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTER_SRC: &str = include_str!("../router.rs");

    /// The V71-G0 `RouteContract` walk, one milestone later.
    #[test]
    fn every_declared_v71_f1_route_is_registered_and_requires_its_params() {
        assert!(!V71_F1_ROUTES.is_empty());
        for c in V71_F1_ROUTES {
            let nested = c
                .path
                .strip_prefix("/api")
                .expect("every route path is /api-nested");
            assert!(
                ROUTER_SRC.contains(&format!("\"{nested}\"")),
                "{}: declared but never registered in router.rs — the v7.0 dead-surface \
                 defect",
                c.path
            );
            assert!(
                ROUTER_SRC.contains(c.handler),
                "{}: registered path but no {} handler named in router.rs",
                c.path,
                c.handler
            );
            assert!((c.params_accept_without)(""));
            for p in c.required_params {
                assert!(
                    !(c.params_accept_without)(p),
                    "{}: declares {p:?} required but its params struct accepts a request \
                     without it",
                    c.path
                );
            }
        }
    }

    /// Every declared decoration lane must be BUILT by `sources::build` —
    /// the dead-surface rule applied to the second declaration list this
    /// unit adds. A source scan, with a scan's limits, the same trade
    /// `git_argv_lint` and `grammar.rs`'s own key walk make.
    #[test]
    fn every_declared_lane_is_built() {
        let src: String = include_str!("sources.rs")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        for lane in LANES {
            assert!(
                src.contains(&format!("\"{lane}\"=>")),
                "decoration lane `{lane}` is declared in LANES but sources.rs has no arm \
                 building it"
            );
        }
    }

    /// …and every declared VIEW must have an arm in the route's own match.
    #[test]
    fn every_declared_view_has_a_projection_arm() {
        let src: String = include_str!("mod.rs")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        for view in VIEWS {
            assert!(
                src.contains(&format!("\"{view}\"=>")),
                "view `{view}` is declared but the route's match has no arm for it"
            );
        }
    }
}
