//! PRR-N5 — `GET /api/resolve-symbol?repo=&sym=<ns>:<container>:<name>[:<kind>]`:
//! symbol-level deep-link resolution (design-nav.md §4). The `sym=` grammar
//! is an OPAQUE, resolver-dispatched key built from fields every
//! symbol-shaped response already carries (`Symbol.name`/`.container`/
//! `.kind`) — deliberately NOT a raw SCIP symbol string (see design-nav.md
//! §4's "Grammar decision" for why: SCIP symbols only exist for the two
//! languages with an indexer and go stale the moment the ingest drifts,
//! which would bake staleness into the shareable-link contract for every
//! OTHER language). A miss (renamed/deleted/ambiguous) is a structured
//! `{"found": false, "reason": "..."}` response at `200`, NEVER a `404` —
//! the client already carries a `path`/`line` fallback anchor alongside
//! `sym=` and degrades to it; this route exists purely to try for something
//! better.
//!
//! # Grammar: `<namespace>:<container>:<name>[:<kind>]`, tokenized on a
//! single `:` (a literal `::` — Rust's own module-path separator — is NEVER
//! a field boundary)
//!
//! Examples: `rust:kb_core::config:ServerSection` (namespace `rust`,
//! container `kb_core::config`, name `ServerSection`), `ruby:UsersController:
//! create` , `rails:route:users#create` (the `rails` namespace switches the
//! resolver from the `symbols` table to `rails_edges` — see
//! [`resolve_rails_symbol`]). A free-standing (no-container) symbol uses the
//! 2-field short form `<namespace>:<name>` — see [`split_sym_fields`]'s doc
//! for the one documented ambiguity this shortcut avoids.
//!
//! # Resolution ladder
//!
//! 1. **Exact** — `(name, container, kind)` looked up against `symbols`
//!    rows CURRENT in this repo (re-resolved fresh per request, never
//!    cached — a link survives code motion/reformatting within the same
//!    defining scope, per design-nav.md §4).
//! 2. **Fuzzy** (only on an exact miss) — a NARROW nucleo fuzzy pass (same
//!    library/haystack convention as `search::symbols`'s "jump to symbol"
//!    lane: `"container::name"` when a container was given, bare `name`
//!    otherwise), pre-filtered to `kind` when one was supplied, taking the
//!    single BEST-scored hit. Never a ranked list — this route answers "the
//!    one symbol this link meant," not a search box.
//! 3. **`rails:` namespace** — dispatches to `rails_edges` instead of
//!    `symbols` (see [`resolve_rails_symbol`]); no fuzzy fallback (an edge
//!    row is either there or it isn't — see `frameworks`'s module doc on
//!    why this lens never approximates further than `likely`/`candidate`).
//!
//! Internal-only refinement (design-nav.md §4's own "never surfaced in the
//! URL grammar itself" note): when a SCIP-exact occurrence happens to cover
//! the resolved position, this route does not currently cross-check against
//! it — the exact/fuzzy ladder above is symbols-table-only. Flagged as
//! optional-and-cheap in the brief; left for a follow-up since it would add
//! a second, redundant lookup for no behavior change on any of this unit's
//! own test fixtures (documented in the PRR-N5 report, not silently
//! dropped).

use crate::extract::Symbol;
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{Store, StoreBlocking};
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};
use serde::{Deserialize, Serialize};

pub const RESOLVE_SYMBOL_SCHEMA: &str = "resolve-symbol/1";

#[derive(Debug, Deserialize)]
pub struct ResolveSymbolParams {
    pub repo: String,
    pub sym: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolveSymbolOut {
    pub schema: &'static str,
    pub sym: String,
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `"exact"` | `"fuzzy"` | `"rails"` — `None` iff `found` is `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<&'static str>,
    /// Present iff `found` is `false` — a human-readable reason, never a
    /// bare `404` (see the module doc).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ResolveSymbolOut {
    fn not_found(sym: &str, reason: impl Into<String>) -> Self {
        Self {
            schema: RESOLVE_SYMBOL_SCHEMA,
            sym: sym.to_string(),
            found: false,
            path: None,
            line: None,
            kind: None,
            container: None,
            name: None,
            via: None,
            reason: Some(reason.into()),
        }
    }
}

/// `GET /api/resolve-symbol?repo=&sym=`.
pub async fn resolve_symbol_route(
    State(state): State<SharedState>,
    Query(params): Query<ResolveSymbolParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_repo, repo_id) = find_repo(&state, &params.repo)?;
    let sym = params.sym.clone();
    // 2026-08-31 incident (store.rs module doc): coarse-wrap the whole
    // sync symbol-address resolution on the blocking pool.
    let out = state
        .store
        .run_blocking(move |store| resolve_symbol_at(store, repo_id, &sym))
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `pub(crate)` — narrow deps, directly unit-testable (same convention as
/// `resolve::resolve_position`/`usages::usages_at`). Infallible except for
/// real store I/O errors — a malformed `sym=`/an unresolvable symbol is a
/// `found: false` VALUE, never an `ApiError` (see the module doc).
pub(crate) fn resolve_symbol_at(
    store: &Store,
    repo_id: i64,
    sym: &str,
) -> Result<ResolveSymbolOut, ApiError> {
    let Some((namespace, container, name, kind)) = parse_sym(sym) else {
        return Ok(ResolveSymbolOut::not_found(
            sym,
            "malformed sym= — expected <namespace>:<container>:<name>[:<kind>] \
             or the no-container short form <namespace>:<name>",
        ));
    };

    if namespace == "rails" {
        // For `rails:`, the parsed `container` field is repurposed as the
        // CONSTRUCT name (`route`/`partial`/`view`/…) — see the module doc.
        let construct = container.as_deref().unwrap_or("");
        return resolve_rails_symbol(store, repo_id, sym, construct, &name);
    }

    // --- 1. exact ----------------------------------------------------------
    let mut rows = store.symbols_named_in_repo(repo_id, &name)?;
    rows.sort_by(|(pa, sa), (pb, sb)| pa.cmp(pb).then_with(|| sa.line_start.cmp(&sb.line_start)));
    let exact = rows.iter().find(|(_, s)| {
        container.as_deref() == s.container.as_deref()
            && kind.as_deref().map(|k| k == s.kind).unwrap_or(true)
    });
    if let Some((path, s)) = exact {
        return Ok(found_symbol(sym, path, s, "exact"));
    }

    // --- 2. narrow nucleo fuzzy fallback (exact miss only) ------------------
    let query = match &container {
        Some(c) => format!("{c} {name}"),
        None => name.clone(),
    };
    let pattern = Pattern::parse(&query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut best: Option<(u32, String, Symbol)> = None;
    for (p, s) in store.symbols_for_repo(repo_id)? {
        if kind.as_deref().is_some_and(|k| k != s.kind) {
            continue;
        }
        let mut haystack = String::new();
        if let Some(c) = &s.container {
            haystack.push_str(c);
            haystack.push_str("::");
            haystack.push_str(&s.name);
        } else {
            haystack.push_str(&s.name);
        }
        buf.clear();
        let hay = Utf32Str::new(&haystack, &mut buf);
        let Some(score) = pattern.score(hay, &mut matcher) else {
            continue;
        };
        let better = match &best {
            Some((best_score, best_path, best_sym)) => {
                score > *best_score
                    || (score == *best_score
                        && (p.as_str(), s.name.as_str())
                            < (best_path.as_str(), best_sym.name.as_str()))
            }
            None => true,
        };
        if better {
            best = Some((score, p, s));
        }
    }
    if let Some((_, path, s)) = best {
        return Ok(found_symbol(sym, &path, &s, "fuzzy"));
    }

    Ok(ResolveSymbolOut::not_found(
        sym,
        format!("no symbol named {name:?} (exact or fuzzy) in repo"),
    ))
}

fn found_symbol(sym: &str, path: &str, s: &Symbol, via: &'static str) -> ResolveSymbolOut {
    ResolveSymbolOut {
        schema: RESOLVE_SYMBOL_SCHEMA,
        sym: sym.to_string(),
        found: true,
        path: Some(path.to_string()),
        line: Some(s.line_start),
        kind: Some(s.kind.clone()),
        container: s.container.clone(),
        name: Some(s.name.clone()),
        via: Some(via),
        reason: None,
    }
}

/// `rails:<construct>:<key>` — dispatches to `rails_edges` instead of
/// `symbols`. `construct` maps onto the closed rails-lens/1 `EdgeKind`
/// vocabulary (see [`rails_construct_to_kind`]); `key` is matched against
/// that kind's rows' `dst_symbol` first (the `route_action`/
/// `turbo_stream_target` convention — a `"controller#action"`/dom-id string),
/// falling back to `dst_path` (the `render_partial`/`render_view`/
/// `route_file` convention, which carries no `dst_symbol` at all — see
/// `frameworks::rails::{routes,views}`'s own doc for exactly which edges
/// populate which field). No fuzzy fallback here (see the module doc).
fn resolve_rails_symbol(
    store: &Store,
    repo_id: i64,
    sym: &str,
    construct: &str,
    key: &str,
) -> Result<ResolveSymbolOut, ApiError> {
    let Some(kind_str) = rails_construct_to_kind(construct) else {
        return Ok(ResolveSymbolOut::not_found(
            sym,
            format!("unrecognized rails construct {construct:?}"),
        ));
    };
    let rows = store.rails_edges_by_kind(repo_id, kind_str)?;
    let hit = rows
        .iter()
        .find(|e| e.dst_symbol.as_deref() == Some(key))
        .or_else(|| rows.iter().find(|e| e.dst_path.as_deref() == Some(key)));
    let Some(edge) = hit else {
        return Ok(ResolveSymbolOut::not_found(
            sym,
            format!("no rails_edges row of kind {kind_str:?} matching {key:?}"),
        ));
    };
    let Some(dst_path) = edge.dst_path.clone() else {
        return Ok(ResolveSymbolOut::not_found(
            sym,
            format!("rails_edges row for {key:?} carries no dst_path (e.g. a dom-id-only target)"),
        ));
    };
    Ok(ResolveSymbolOut {
        schema: RESOLVE_SYMBOL_SCHEMA,
        sym: sym.to_string(),
        found: true,
        path: Some(dst_path),
        // Rails-lens edges carry no dst LINE (see V0026's schema — `rails_edges`
        // has no `dst_line` column, only `src_line`) — a file-level jump (line
        // 1) is an honest destination, never a fabricated one.
        line: Some(1),
        kind: edge.dst_kind.clone(),
        container: None,
        name: Some(key.to_string()),
        via: Some("rails"),
        reason: None,
    })
}

fn rails_construct_to_kind(construct: &str) -> Option<&'static str> {
    match construct {
        "route" => Some("route_action"),
        "routes_file" => Some("route_file"),
        "partial" => Some("render_partial"),
        "view" => Some("render_view"),
        "turbo_stream" => Some("turbo_stream_target"),
        _ => None,
    }
}

/// Parse `?sym=` into `(namespace, container, name, kind)` — see the module
/// doc's grammar note. `None` for anything that doesn't tokenize into 2, 3,
/// or 4 top-level fields.
fn parse_sym(sym: &str) -> Option<(String, Option<String>, String, Option<String>)> {
    let fields = split_sym_fields(sym);
    match fields.len() {
        2 => Some((fields[0].clone(), None, fields[1].clone(), None)),
        3 => Some((
            fields[0].clone(),
            Some(fields[1].clone()),
            fields[2].clone(),
            None,
        )),
        4 => Some((
            fields[0].clone(),
            Some(fields[1].clone()),
            fields[2].clone(),
            Some(fields[3].clone()),
        )),
        _ => None,
    }
}

/// Split on a single `:` that is NOT part of a `::` pair; a `::` pair is
/// kept as a literal two-char run inside its field (Rust's own `mod::path`
/// convention — see the module doc's worked example,
/// `rust:kb_core::config:ServerSection`).
///
/// **Known, documented limitation:** a symbol with an EMPTY container
/// (conceptually `namespace` + `""` + `name`, i.e. `namespace::name`) is
/// textually indistinguishable from a literal `::` inside a 2-field
/// `namespace:name` form and is NOT specially handled — a free-standing
/// (no-container) symbol should use the 2-field short form instead (see the
/// module doc); this only matters for a namespace whose OWN convention uses
/// `::` as a path separator (Rust) AND a symbol with zero container, which
/// `sym=` builders (`web-code/src/lib/codeUrl.ts`, out of this unit's
/// server-only scope) are expected to emit as the short form.
fn split_sym_fields(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ':' {
            if i + 1 < chars.len() && chars[i + 1] == ':' {
                cur.push(':');
                cur.push(':');
                i += 2;
                continue;
            }
            fields.push(std::mem::take(&mut cur));
            i += 1;
            continue;
        }
        cur.push(chars[i]);
        i += 1;
    }
    fields.push(cur);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store() -> (tempfile::TempDir, Store, i64) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let repo_id = store.upsert_repo("fixture", "/irrelevant").unwrap();
        (tmp, store, repo_id)
    }

    // --- grammar -------------------------------------------------------

    #[test]
    fn split_sym_fields_keeps_a_literal_double_colon_inside_one_field() {
        assert_eq!(
            split_sym_fields("rust:kb_core::config:ServerSection"),
            vec!["rust", "kb_core::config", "ServerSection"]
        );
        assert_eq!(
            split_sym_fields("ruby:UsersController:create"),
            vec!["ruby", "UsersController", "create"]
        );
        assert_eq!(
            split_sym_fields("rails:route:users#create"),
            vec!["rails", "route", "users#create"]
        );
        assert_eq!(split_sym_fields("rust:widget"), vec!["rust", "widget"]);
        assert_eq!(
            split_sym_fields("rust:kb_core::config:ServerSection:struct"),
            vec!["rust", "kb_core::config", "ServerSection", "struct"]
        );
    }

    // --- exact -----------------------------------------------------------

    #[test]
    fn exact_match_resolves_a_no_container_free_function() {
        let (_tmp, store, repo_id) = open_store();
        let src = "fn widget() -> i32 {\n    1\n}\n";
        let blob_hash = crate::ingest::git_blob_hash(src.as_bytes());
        store
            .upsert_file(repo_id, "a.rs", &blob_hash, "rust", src.len() as u64)
            .unwrap();
        store
            .replace_symbols(
                &blob_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", src.as_bytes()).unwrap(),
            )
            .unwrap();

        let out = resolve_symbol_at(&store, repo_id, "rust:widget").unwrap();
        assert!(out.found, "{out:#?}");
        assert_eq!(out.via, Some("exact"));
        assert_eq!(out.path.as_deref(), Some("a.rs"));
        assert_eq!(out.line, Some(1));
        assert_eq!(out.name.as_deref(), Some("widget"));
    }

    // --- fuzzy (exact miss only) ------------------------------------------

    #[test]
    fn fuzzy_fallback_finds_a_near_miss_only_after_an_exact_miss() {
        let (_tmp, store, repo_id) = open_store();
        let src = "fn widget() -> i32 {\n    1\n}\n";
        let blob_hash = crate::ingest::git_blob_hash(src.as_bytes());
        store
            .upsert_file(repo_id, "a.rs", &blob_hash, "rust", src.len() as u64)
            .unwrap();
        store
            .replace_symbols(
                &blob_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", src.as_bytes()).unwrap(),
            )
            .unwrap();

        // "wiget" (typo, one char dropped) has no exact row.
        let out = resolve_symbol_at(&store, repo_id, "rust:wiget").unwrap();
        assert!(out.found, "{out:#?}");
        assert_eq!(out.via, Some("fuzzy"));
        assert_eq!(out.name.as_deref(), Some("widget"));
        assert_eq!(out.path.as_deref(), Some("a.rs"));
    }

    // --- miss --------------------------------------------------------------

    #[test]
    fn a_genuine_miss_is_a_200_shaped_response_not_an_error() {
        let (_tmp, store, repo_id) = open_store();
        let out = resolve_symbol_at(&store, repo_id, "rust:totally_bogus_symbol_xyz").unwrap();
        assert!(!out.found);
        assert!(out.path.is_none());
        assert!(out.reason.is_some(), "{out:#?}");
    }

    #[test]
    fn malformed_sym_is_a_found_false_value_not_an_error() {
        let (_tmp, store, repo_id) = open_store();
        let out = resolve_symbol_at(&store, repo_id, "just-one-field").unwrap();
        assert!(!out.found);
        assert!(out.reason.is_some());
    }

    // --- rails: namespace ----------------------------------------------------

    #[test]
    fn rails_namespace_resolves_a_route_action_by_dst_symbol() {
        let (_tmp, store, repo_id) = open_store();
        store
            .replace_rails_edges(
                repo_id,
                "config/routes.rb",
                "blobhash1",
                crate::frameworks::RAILS_LENS_GRAMMAR_VERSION,
                &[crate::frameworks::FrameworkEdge {
                    kind: crate::frameworks::EdgeKind::RouteAction,
                    src_path: "config/routes.rb".to_string(),
                    src_line: Some(2),
                    src_symbol: None,
                    dst_kind: Some("controller_action".to_string()),
                    dst_path: Some("app/controllers/users_controller.rb".to_string()),
                    dst_symbol: Some("users#create".to_string()),
                    trust: crate::frameworks::Trust::Likely,
                    extra_json: None,
                }],
            )
            .unwrap();

        let out = resolve_symbol_at(&store, repo_id, "rails:route:users#create").unwrap();
        assert!(out.found, "{out:#?}");
        assert_eq!(out.via, Some("rails"));
        assert_eq!(
            out.path.as_deref(),
            Some("app/controllers/users_controller.rb")
        );
    }

    #[test]
    fn rails_namespace_miss_is_found_false() {
        let (_tmp, store, repo_id) = open_store();
        let out = resolve_symbol_at(&store, repo_id, "rails:route:nope#nope").unwrap();
        assert!(!out.found);
        assert!(out.reason.is_some());
    }

    #[test]
    fn rails_namespace_unrecognized_construct_is_found_false() {
        let (_tmp, store, repo_id) = open_store();
        let out = resolve_symbol_at(&store, repo_id, "rails:not_a_real_construct:x").unwrap();
        assert!(!out.found);
    }
}
