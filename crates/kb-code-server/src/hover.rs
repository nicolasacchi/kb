//! PRR-N5 — `GET /api/hover?repo=&path=&line=&col=`: composed, no new
//! tree-sitter parse beyond what `/api/resolve` already pays for itself at
//! request time (design-nav.md §3). Internally calls the SAME
//! `resolve::resolve_position` `/api/resolve` uses, takes the top-ranked
//! candidate, and flattens it into one tooltip-shaped view:
//!
//! - a **symbol** half (`kind`/`name`/`container`/`signature`/`doc`) — only
//!   present when the top candidate is backed by a real `symbols`-table row
//!   (i.e. every precision tier EXCEPT `framework-convention`, whose
//!   `kind`/`container` carry framework-specific meaning — a Rails DSL
//!   `dst_kind`/`dst_symbol`, not a code symbol — and must never be
//!   presented as one);
//! - a **defsite** (`path`/`line`) — the top candidate's own location,
//!   present whenever resolve found ANY candidate at all;
//! - a **framework** half — a SEPARATE, DIRECT `rails_edges` lookup keyed on
//!   the REQUESTED position (not whatever `ident` the resolve ladder
//!   happened to resolve — same position-gated convention `resolve.rs`'s
//!   own framework-convention tier uses, see that tier's doc), so it is
//!   populated independently of which tier actually won the resolve race.
//!
//! `precision`/`trust` mirror the top resolve candidate's own
//! `precision`/`class` fields — `None` only when resolve found NO candidate
//! at all (a real identifier/word WAS found at the position — otherwise
//! `resolve_position` itself 400s, same as `/api/resolve` — but nothing in
//! this repo/other repos/the framework lens resolves it to anywhere). This
//! is an honest absence, never a fabricated tier: never guess.
//!
//! Distinct response shape from `/api/resolve` on purpose — that endpoint's
//! contract (a ranked candidate LIST) must not change; this one is a single
//! flattened view for a tooltip. Ordinary `auth_bearer`-gated browsing-class
//! read (see `router.rs`) — it exposes nothing `/api/resolve` +
//! `/api/framework/edges` don't already.

use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{Store, StoreBlocking};
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const HOVER_SCHEMA: &str = "hover/1";

#[derive(Debug, Deserialize)]
pub struct HoverParams {
    pub repo: String,
    pub path: String,
    pub line: u32,
    pub col: u32,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HoverSymbol {
    pub kind: String,
    pub name: String,
    pub container: Option<String>,
    pub signature: Option<String>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HoverDefsite {
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct HoverFramework {
    pub kind: &'static str,
    pub dst_kind: Option<String>,
    pub dst_path: Option<String>,
    pub trust: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct HoverOut {
    pub schema: &'static str,
    pub path: String,
    pub line: u32,
    pub col: u32,
    /// The resolve ladder's own tier name (`resolve::PRECISION_*`) —
    /// `None` iff resolve found zero candidates.
    pub precision: Option<&'static str>,
    /// `"exact"` | `"likely"` | `"candidate"` — `None` iff resolve found
    /// zero candidates.
    pub trust: Option<&'static str>,
    pub symbol: Option<HoverSymbol>,
    pub defsite: Option<HoverDefsite>,
    pub framework: Option<HoverFramework>,
}

/// `GET /api/hover?repo=&path=&line=&col=`.
///
/// PRR-L2: after the synchronous `hover_at` composes its usual view, this
/// handler consults `crate::lip`'s provider hover for the SAME position
/// (`crate::lip::overlay_hover`) — "provider hover fills signature/doc;
/// keep the framework half untouched" (design-lip.md). On
/// refusal/timeout/absence/no-configured-provider/blob-staleness the
/// existing composed view is returned byte-for-byte unchanged.
pub async fn hover_route(
    State(state): State<SharedState>,
    Query(params): Query<HoverParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();
    let path = params.path.clone();
    let line = params.line;
    let col = params.col;
    let rev = params.rev.clone();
    // 2026-08-31 incident (store.rs module doc): coarse-wrap the sync
    // store-backed compose on the blocking pool; `overlay_hover` below is
    // the async (lip HTTP) leg and stays outside.
    let state_bg = state.clone();
    let repo_bg = repo.clone();
    let mut out = state
        .store
        .run_blocking(move |store| {
            hover_at(
                store,
                &state_bg.repos,
                &state_bg.repo_ids,
                &repo_bg,
                repo_id,
                &path,
                line,
                col,
                rev.as_deref(),
            )
        })
        .await?;
    crate::lip::overlay_hover(
        &state,
        &repo,
        &params.path,
        params.rev.as_deref(),
        params.line,
        params.col,
        &mut out,
    )
    .await;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `pub(crate)` — narrow deps, directly unit-testable (same convention as
/// `resolve::resolve_position`/`usages::usages_at`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn hover_at(
    store: &Store,
    repos: &[crate::config::RepoEntry],
    repo_ids: &HashMap<String, i64>,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
) -> Result<HoverOut, ApiError> {
    let resolved = crate::resolve::resolve_position(
        store, repos, repo_ids, repo, repo_id, path, line, col, rev,
    )?;
    let top = resolved.candidates.first();

    // The symbol half is a real symbols-table row ONLY for precision tiers
    // other than framework-convention — see the module doc's opening list.
    let symbol = top
        .filter(|c| c.precision != crate::resolve::PRECISION_FRAMEWORK && c.kind.is_some())
        .map(|c| HoverSymbol {
            kind: c.kind.clone().unwrap_or_default(),
            name: resolved.ident.clone(),
            container: c.container.clone(),
            signature: c.signature.clone(),
            doc: c.doc.clone(),
        });
    let defsite = top.map(|c| HoverDefsite {
        path: c.path.clone(),
        line: c.line,
    });

    // Framework half — a DIRECT rails_edges lookup keyed on the REQUESTED
    // position, independent of `resolved`'s own ranking (see module doc).
    let framework = if crate::frameworks::rails_lens_relevant_path(path) {
        store
            .rails_edges_by_src_path(repo_id, path)?
            .into_iter()
            .find(|e| e.src_line == Some(line))
            .map(|e| HoverFramework {
                kind: e.kind.as_str(),
                dst_kind: e.dst_kind,
                dst_path: e.dst_path,
                trust: e.trust.as_str(),
            })
    } else {
        None
    };

    Ok(HoverOut {
        schema: HOVER_SCHEMA,
        path: path.to_string(),
        line,
        col,
        precision: top.map(|c| c.precision),
        trust: top.map(|c| c.class),
        symbol,
        defsite,
        framework,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RepoEntry;

    fn write_file(root: &std::path::Path, path: &str, content: &str) {
        let abs = root.join(path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(abs, content).unwrap();
    }

    fn fixture_repo(store: &Store, name: &str) -> (tempfile::TempDir, RepoEntry, i64) {
        let root = tempfile::tempdir().unwrap();
        let entry = RepoEntry {
            name: name.to_string(),
            path: root.path().to_path_buf(),
        };
        let repo_id = store
            .upsert_repo(name, root.path().to_str().unwrap())
            .unwrap();
        (root, entry, repo_id)
    }

    #[test]
    fn symbol_and_framework_both_present_when_a_higher_tier_wins_the_position() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        // A real Ruby method, `show` — file-local tier (ranked ABOVE
        // framework-convention) resolves it via its own def occurrence.
        let src = "module X\n  class XController\n    def show\n      1\n    end\n  end\nend\n";
        write_file(root.path(), "app/controllers/x_controller.rb", src);
        let blob_hash = crate::ingest::git_blob_hash(src.as_bytes());
        store
            .upsert_file(
                repo_id,
                "app/controllers/x_controller.rb",
                &blob_hash,
                "ruby",
                src.len() as u64,
            )
            .unwrap();
        store
            .replace_symbols(
                &blob_hash,
                crate::lang::for_id("ruby").unwrap().salt,
                &crate::extract::extract_symbols("ruby", src.as_bytes()).unwrap(),
            )
            .unwrap();
        store
            .replace_occurrences(
                &blob_hash,
                crate::lang::for_id("ruby").unwrap().salt,
                &crate::occurrences::extract_occurrences("ruby", src.as_bytes()).unwrap(),
            )
            .unwrap();

        // A framework-convention edge on the SAME line as `def show` — a
        // synthetic construct (this test doesn't run the real extractor;
        // the position→rails_edges lookup is what's under test here, not
        // the Rails DSL parse itself — that's `framework_edges.rs`'s job).
        store
            .replace_rails_edges(
                repo_id,
                "app/controllers/x_controller.rb",
                &blob_hash,
                crate::frameworks::RAILS_LENS_GRAMMAR_VERSION,
                &[crate::frameworks::FrameworkEdge {
                    kind: crate::frameworks::EdgeKind::RenderView,
                    src_path: "app/controllers/x_controller.rb".to_string(),
                    src_line: Some(3),
                    src_symbol: None,
                    dst_kind: Some("view".to_string()),
                    dst_path: Some("app/views/x/show.html.erb".to_string()),
                    dst_symbol: None,
                    trust: crate::frameworks::Trust::Likely,
                    extra_json: None,
                }],
            )
            .unwrap();

        // Click on "show" — line 3, col 8 (0-based, on the 's').
        let out = hover_at(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "app/controllers/x_controller.rb",
            3,
            8,
            None,
        )
        .unwrap();

        let symbol = out.symbol.expect("symbol half must be present: {out:#?}");
        assert_eq!(symbol.name, "show");
        assert_eq!(out.defsite.unwrap().line, 3);

        let framework = out
            .framework
            .expect("framework half must be present alongside the symbol half");
        assert_eq!(framework.kind, "render_view");
        assert_eq!(
            framework.dst_path.as_deref(),
            Some("app/views/x/show.html.erb")
        );
        assert_eq!(framework.trust, "likely");
    }

    #[test]
    fn both_absent_when_the_position_resolves_to_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        // A plain word with zero symbols/occurrences/rails_edges anywhere —
        // resolve_position still succeeds (word_at finds *something*), but
        // produces no candidates at all.
        let src = "orphan_word\n";
        write_file(root.path(), "notes.txt", src);

        let out = hover_at(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "notes.txt",
            1,
            0,
            None,
        )
        .unwrap();

        assert!(out.precision.is_none());
        assert!(out.trust.is_none());
        assert!(out.symbol.is_none());
        assert!(out.defsite.is_none());
        assert!(out.framework.is_none());
    }

    #[test]
    fn framework_absent_for_a_non_rails_relevant_path_even_with_a_symbol_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        let src = "fn widget() -> i32 {\n    1\n}\n\nfn main() {\n    widget();\n}\n";
        write_file(root.path(), "a.rs", src);
        let blob_hash = crate::ingest::git_blob_hash(src.as_bytes());
        store
            .upsert_file(repo_id, "a.rs", &blob_hash, "rust", src.len() as u64)
            .unwrap();
        store
            .replace_symbols(
                &blob_hash,
                crate::lang::RUST.salt,
                &crate::extract::extract_symbols("rust", src.as_bytes()).unwrap(),
            )
            .unwrap();
        store
            .replace_occurrences(
                &blob_hash,
                crate::lang::RUST.salt,
                &crate::occurrences::extract_occurrences("rust", src.as_bytes()).unwrap(),
            )
            .unwrap();

        let out = hover_at(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "a.rs",
            6,
            4,
            None,
        )
        .unwrap();

        assert!(out.symbol.is_some(), "got: {out:#?}");
        assert!(out.framework.is_none());
    }
}
