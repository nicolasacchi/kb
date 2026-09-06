//! PRR-N5 — `GET /api/framework/edges?repo=&path=&kind=`: the direct read
//! over `rails_edges` (design-nav.md §2's "API surface" / §7 row N5).
//! Mirrors `hierarchy.rs`'s `/hierarchy/callers`/`/hierarchy/callees` shape:
//! given a path, return every `rails_edges` row where that path is
//! `src_path` OR `dst_path` — direction distinguished in the response —
//! optionally filtered by `kind` (the closed rails-lens/1 enum's string
//! form, `EdgeKind::as_str()`). This is the primary "what does this file
//! connect to" query surface: open `_row.html.erb` and see every controller
//! action that renders it, or open `routes.rb` and see every action a given
//! `resources` line reaches.
//!
//! Ordinary `auth_bearer`-gated browsing-class read (see `router.rs`) — it
//! exposes nothing `GET /api/file` (source text) doesn't already imply.
//!
//! No repo-eligibility gate is needed beyond the query itself: a non-Rails
//! repo (or a Rails repo before its rails-lens pass has run for this path)
//! simply has zero `rails_edges` rows for it, so the query degrades to an
//! empty result — same posture as every other read in this crate that
//! trusts an empty result set over a special-cased short-circuit.

use crate::frameworks::FrameworkEdge;
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{Store, StoreBlocking};
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

pub const FRAMEWORK_EDGES_SCHEMA: &str = "framework-edges/1";

#[derive(Debug, Deserialize)]
pub struct FrameworkEdgesParams {
    pub repo: String,
    pub path: String,
    pub kind: Option<String>,
}

/// `"src"` — `path` PRODUCED this edge (e.g. `routes.rb`'s own `resources`
/// line). `"dst"` — `path` is this edge's TARGET (e.g. a partial being
/// rendered from elsewhere). A row whose `src_path` and `dst_path` both
/// happen to equal `path` (not produced by any current extractor, but not
/// structurally forbidden either) surfaces twice, once per direction —
/// honest, since it really is both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EdgeDirection {
    #[serde(rename = "src")]
    Src,
    #[serde(rename = "dst")]
    Dst,
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameworkEdgeOut {
    pub direction: EdgeDirection,
    #[serde(flatten)]
    pub edge: FrameworkEdge,
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameworkEdgesOut {
    pub schema: &'static str,
    pub path: String,
    pub edges: Vec<FrameworkEdgeOut>,
    pub total: usize,
}

/// `GET /api/framework/edges?repo=&path=&kind=`.
pub async fn framework_edges_route(
    State(state): State<SharedState>,
    Query(params): Query<FrameworkEdgesParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_repo, repo_id) = find_repo(&state, &params.repo)?;
    let path = params.path.clone();
    let kind = params.kind.clone();
    // 2026-08-31 incident (store.rs module doc): coarse-wrap the two
    // sequential store scans (src + dst path) in one closure.
    let out = state
        .store
        .run_blocking(move |store| framework_edges_at(store, repo_id, &path, kind.as_deref()))
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `pub(crate)` — narrow deps (no `SharedState`), directly unit-testable —
/// same convention as `resolve::resolve_position`/`usages::usages_at`.
pub(crate) fn framework_edges_at(
    store: &Store,
    repo_id: i64,
    path: &str,
    kind: Option<&str>,
) -> Result<FrameworkEdgesOut, ApiError> {
    let mut edges: Vec<FrameworkEdgeOut> = Vec::new();
    for edge in store.rails_edges_by_src_path(repo_id, path)? {
        if kind.is_some_and(|k| edge.kind.as_str() != k) {
            continue;
        }
        edges.push(FrameworkEdgeOut {
            direction: EdgeDirection::Src,
            edge,
        });
    }
    for edge in store.rails_edges_by_dst_path(repo_id, path)? {
        if kind.is_some_and(|k| edge.kind.as_str() != k) {
            continue;
        }
        edges.push(FrameworkEdgeOut {
            direction: EdgeDirection::Dst,
            edge,
        });
    }
    let total = edges.len();
    Ok(FrameworkEdgesOut {
        schema: FRAMEWORK_EDGES_SCHEMA,
        path: path.to_string(),
        edges,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::extract_edges;
    use std::path::{Path, PathBuf};

    fn fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rails-lens")
    }

    /// Every regular file under `root`, as repo-relative forward-slash
    /// paths, sorted lexicographically — mirrors `tests/rails_lens.rs`'s own
    /// `walk_sorted` (duplicated rather than shared: that's an integration
    /// test in a separate compiled crate, this is a `src/`-local unit test).
    fn walk_sorted(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        walk_dir_into(root, root, &mut out);
        out.sort();
        out
    }

    fn walk_dir_into(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk_dir_into(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap();
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    /// Ingest the real `tests/fixtures/rails-lens` fixture tree into a fresh
    /// `Store`, exactly the way `ingest.rs`'s own dispatch does it (same
    /// blob_hash/salt convention — see that fn's PRR-N3 comment).
    fn ingest_fixture(store: &Store, repo_id: i64) -> PathBuf {
        let root = fixture_root();
        for rel in walk_sorted(&root) {
            let bytes = std::fs::read(root.join(&rel))
                .unwrap_or_else(|e| panic!("read fixture file {rel}: {e}"));
            let blob_hash = crate::ingest::git_blob_hash(&bytes);
            let edges = extract_edges(&root, &rel, &bytes);
            store
                .replace_rails_edges(
                    repo_id,
                    &rel,
                    &blob_hash,
                    crate::frameworks::RAILS_LENS_GRAMMAR_VERSION,
                    &edges,
                )
                .unwrap();
        }
        root
    }

    fn open_fixture_store() -> (tempfile::TempDir, Store, i64) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let repo_id = store.upsert_repo("fixture", "/irrelevant").unwrap();
        ingest_fixture(&store, repo_id);
        (tmp, store, repo_id)
    }

    #[test]
    fn src_direction_lists_every_edge_the_path_produces() {
        let (_tmp, store, repo_id) = open_fixture_store();
        let out = framework_edges_at(
            &store,
            repo_id,
            "app/controllers/trade/rounds_controller.rb",
            None,
        )
        .unwrap();
        // The controller is BOTH a src (its own 3 render edges: show's
        // implicit render_view, offers_tab's render_partial, row_preview's
        // ambiguous render_partial — `dynamic`'s non-literal render
        // produces no edge at all, see rails_lens.rs's own
        // `non_literal_render_in_the_dynamic_action_produces_no_edge_at_all`)
        // AND a dst (4 route_action edges from config/routes/trade.rb:
        // index/show/search_pharmacies/merge_catalogs).
        let src_edges: Vec<_> = out
            .edges
            .iter()
            .filter(|e| e.direction == EdgeDirection::Src)
            .collect();
        let dst_edges: Vec<_> = out
            .edges
            .iter()
            .filter(|e| e.direction == EdgeDirection::Dst)
            .collect();
        assert_eq!(out.total, 7, "{:#?}", out.edges);
        assert_eq!(src_edges.len(), 3, "{:#?}", out.edges);
        assert_eq!(dst_edges.len(), 4, "{:#?}", out.edges);
        assert!(src_edges
            .iter()
            .any(|e| e.edge.kind.as_str() == "render_view"
                && e.edge.dst_path.as_deref() == Some("app/views/trade/rounds/show.html.erb")));
        assert!(dst_edges
            .iter()
            .all(|e| e.edge.kind.as_str() == "route_action"
                && e.edge.src_path == "config/routes/trade.rb"));
    }

    #[test]
    fn kind_filter_narrows_to_exactly_one_edge() {
        let (_tmp, store, repo_id) = open_fixture_store();
        let out = framework_edges_at(
            &store,
            repo_id,
            "app/controllers/trade/rounds_controller.rb",
            Some("render_view"),
        )
        .unwrap();
        assert_eq!(out.total, 1, "{:#?}", out.edges);
        assert_eq!(out.edges[0].edge.kind.as_str(), "render_view");
    }

    #[test]
    fn dst_direction_finds_every_call_site_that_targets_the_partial() {
        let (_tmp, store, repo_id) = open_fixture_store();
        // Rendered from BOTH the controller's `offers_tab` action AND
        // `merge_complete.turbo_stream.erb`'s own `render "offers_tab_content"`
        // call — see rails_lens.rs's
        // `implicit_view_and_turbo_stream_plus_partial_combo_both_resolve`.
        let out = framework_edges_at(
            &store,
            repo_id,
            "app/views/trade/rounds/_offers_tab_content.html.erb",
            None,
        )
        .unwrap();
        assert_eq!(out.total, 2, "{:#?}", out.edges);
        assert!(
            out.edges
                .iter()
                .all(|e| e.direction == EdgeDirection::Dst
                    && e.edge.kind.as_str() == "render_partial")
        );
        let src_paths: Vec<&str> = out.edges.iter().map(|e| e.edge.src_path.as_str()).collect();
        assert!(src_paths.contains(&"app/controllers/trade/rounds_controller.rb"));
        assert!(src_paths.contains(&"app/views/trade/rounds/merge_complete.turbo_stream.erb"));
    }

    #[test]
    fn both_directions_combine_for_the_draw_split_file() {
        let (_tmp, store, repo_id) = open_fixture_store();
        // config/routes/trade.rb is BOTH a src (its own 5 route_action
        // edges, per rails_lens.rs's own `the_draw_convention_is_followed…`)
        // AND a dst (routes.rb's `draw(:trade)` route_file edge targets it).
        let out = framework_edges_at(&store, repo_id, "config/routes/trade.rb", None).unwrap();
        assert_eq!(out.total, 6, "{:#?}", out.edges);
        let src_count = out
            .edges
            .iter()
            .filter(|e| e.direction == EdgeDirection::Src)
            .count();
        let dst_count = out
            .edges
            .iter()
            .filter(|e| e.direction == EdgeDirection::Dst)
            .count();
        assert_eq!(src_count, 5);
        assert_eq!(dst_count, 1);
        assert!(out
            .edges
            .iter()
            .any(|e| e.direction == EdgeDirection::Dst && e.edge.kind.as_str() == "route_file"));
    }

    #[test]
    fn unrelated_path_returns_an_empty_result_not_an_error() {
        let (_tmp, store, repo_id) = open_fixture_store();
        let out = framework_edges_at(&store, repo_id, "README.md", None).unwrap();
        assert_eq!(out.total, 0);
        assert!(out.edges.is_empty());
    }
}
