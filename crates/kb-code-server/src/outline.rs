//! `outline/1` — ONE per-file outline contract, for every registered file
//! type (V72-H2a, design D7).
//!
//! # Why a contract rather than a renderer
//!
//! Before this module the reader's structure popup (`gO`), the outline
//! rail and the sticky-context header each derived their own tree
//! CLIENT-side, out of `GET /api/file`'s flat `symbols` array, by matching
//! `symbol.container == other.name` (`web-code/src/lib/outline.ts`). That
//! is three consumers of one wire shape and no server-side answer to the
//! question "what is the structure of this file", which is what a CLI verb
//! and an agent need. It is also a nesting rule that cannot be right in
//! general: two symbols with the same NAME are indistinguishable to it,
//! and a YAML row's `container` is a dotted path PREFIX rather than
//! another row's name.
//!
//! `outline/1` is that answer, and it is deliberately a **VIEW of the
//! symbol rows, never a second extraction**. The rows come from exactly
//! the store lookup `GET /api/symbols` does — same blob, same salt, same
//! `extract::Symbol` — so there is one derivation with two renderings, not
//! two outlines that can disagree. [`nest`] is the whole transformation
//! and it is pure.
//!
//! # Nesting is RANGE CONTAINMENT
//!
//! A row is a child of the nearest preceding row whose line range encloses
//! it. That is the rule `entities/1` already uses to reconstruct literal
//! `class`/`module` nesting (this crate's invariant 13), and it is the
//! only rule that works for every language here at once: Rust methods
//! inside an `impl`, YAML keys inside a mapping, SCSS rules inside a rule,
//! Markdown `###`s inside a `##` (which is why `crate::markdown` gives a
//! heading row its whole SECTION's end line, not the heading line's).
//! It needs no name matching, so a file with two identically-named
//! symbols nests correctly.
//!
//! Sort is `(line_start ASC, line_end DESC, col_start ASC, ordinal ASC)` —
//! total and deterministic, so an outline is stable across calls.
//!
//! # Honesty
//!
//! Every response carries [`Honesty`]: the file type's `syntax/1` tier,
//! which ENGINE parses it, what the rows were derived from, and — when
//! there are none — WHY. The four shapes a caller can hit are all
//! distinguishable:
//!
//! * a `full`-tier file with rows;
//! * a `full`-tier file with no rows because nothing has been indexed for
//!   this blob yet (`derived_from: "symbols"`, `rows: []`, and the reason
//!   says so);
//! * a `highlight_only` or `none` tier file — `rows: []` with the tier's
//!   own reason, which is a design outcome, not a failure;
//! * a type with no registry row at all.
//!
//! Nothing here is persisted and nothing is cached (root CLAUDE.md
//! invariant #2's posture): the nesting is computed per request from rows
//! the ingest pipeline already wrote.
//!
//! # Not rewritten here
//!
//! The SPA's three consumers keep their client-side derivation in this
//! unit — converting them is a `web-code` change with its own keyboard and
//! rail behaviour to re-verify, and this unit ships no SPA. They are named
//! so the next unit knows where they are: `web-code/src/lib/outline.ts`
//! (`buildOutline`/`flattenOutline`), `components/StructurePopup.tsx`,
//! `components/OutlineRail.tsx`, `lib/stickyContext.ts`. When they move,
//! they should render THIS response rather than re-derive, for the same
//! "one projection, two renderers" reason invariant 17(a) records for
//! `kbc-tree/1`.

use crate::extract::Symbol;
use serde::Serialize;

pub const OUTLINE_SCHEMA: &str = "outline/1";

/// The cap on rows returned for one file, counted over the WHOLE tree.
/// `truncated` reports the cut; every extractor has its own, lower cap
/// (`yaml::MAX_KEYS`, `css::MAX_ROWS`, …), so this is the backstop for a
/// language whose extractor has none.
pub const MAX_ROWS: usize = 2000;

/// One row's position. 1-based lines, 0-based byte columns — the
/// `extract::Symbol` convention, unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Range {
    pub line_start: u32,
    pub line_end: u32,
    pub col_start: u32,
    pub col_end: u32,
}

/// One outline row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlineRow {
    /// The symbol's own kind, verbatim — `fn`/`class`/`key`/`rule`/
    /// `heading`/`element`/… . This module invents no vocabulary of its
    /// own: a kind here is a kind in the `symbols` table, so a reader who
    /// learns one has learned both.
    pub kind: String,
    pub name: String,
    pub range: Range,
    /// 0 for a top-level row.
    pub depth: u32,
    /// The symbol's `signature`, when it has one: a Rust `fn` signature, a
    /// SCSS `@mixin` header, a Markdown heading's level, a YAML key's
    /// anchor/alias/merge fact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<OutlineRow>,
}

/// What this outline is, and what it is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Honesty {
    /// The `syntax/1` extraction tier for this file TYPE.
    pub tier: &'static str,
    /// `tree-sitter:<crate>` | `scanner:<schema>` | `none`.
    pub engine: String,
    /// `symbols` when the rows are a view of the symbol table, `none`
    /// when there is nothing to view.
    pub derived_from: &'static str,
    /// Why there are no rows, or a caveat about the ones there are.
    /// `None` only when the outline is complete and non-empty.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlineOut {
    pub schema: &'static str,
    pub repo: String,
    pub path: String,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
    /// The detected language id, or `null` when nothing parses this type.
    pub lang: Option<&'static str>,
    pub tier: &'static str,
    pub rows: Vec<OutlineRow>,
    /// Total rows in the tree, including children.
    pub total: usize,
    pub truncated: bool,
    pub honesty: Honesty,
}

/// Nest a flat symbol list into a tree by RANGE CONTAINMENT.
///
/// Pure, total and deterministic. Returns `(rows, total, truncated)`;
/// `total` counts the whole tree, and `truncated` is set when
/// [`MAX_ROWS`] cut it.
pub fn nest(mut symbols: Vec<Symbol>) -> (Vec<OutlineRow>, usize, bool) {
    let truncated = symbols.len() > MAX_ROWS;
    symbols.truncate(MAX_ROWS);
    // (start ASC, end DESC, col ASC, ordinal ASC): a container sorts
    // before everything it contains, and ties break the same way every
    // time.
    symbols.sort_by(|a, b| {
        a.line_start
            .cmp(&b.line_start)
            .then(b.line_end.cmp(&a.line_end))
            .then(a.col_start.cmp(&b.col_start))
            .then(a.ordinal.cmp(&b.ordinal))
    });
    let total = symbols.len();
    let mut roots: Vec<OutlineRow> = Vec::new();
    // A stack of INDEX PATHS into the tree being built: the row at
    // `stack[i]` is the ancestor at depth `i`.
    let mut stack: Vec<(u32, u32, Vec<usize>)> = Vec::new();
    for s in symbols {
        while let Some((_, parent_end, _)) = stack.last() {
            // A row that ends after its candidate parent is a sibling, not
            // a child. `>=` is deliberate for the end: the sort already
            // placed a longer row first, so an equal-range row is nested,
            // which keeps a same-range pair deterministic instead of
            // order-dependent.
            if s.line_end <= *parent_end {
                break;
            }
            stack.pop();
        }
        let depth = stack.len() as u32;
        let row = OutlineRow {
            kind: s.kind,
            name: s.name,
            range: Range {
                line_start: s.line_start,
                line_end: s.line_end,
                col_start: s.col_start,
                col_end: s.col_end,
            },
            depth,
            detail: s.signature,
            children: Vec::new(),
        };
        let path = match stack.last() {
            Some((_, _, parent_path)) => {
                let parent = row_at_mut(&mut roots, parent_path);
                parent.children.push(row);
                let mut p = parent_path.clone();
                p.push(parent.children.len() - 1);
                p
            }
            None => {
                roots.push(row);
                vec![roots.len() - 1]
            }
        };
        let (start, end) = {
            let r = row_at_mut(&mut roots, &path);
            (r.range.line_start, r.range.line_end)
        };
        stack.push((start, end, path));
    }
    (roots, total, truncated)
}

/// The row addressed by `path` (a chain of child indices). Panics only on
/// a path this module did not build, which is unreachable by construction.
fn row_at_mut<'a>(roots: &'a mut [OutlineRow], path: &[usize]) -> &'a mut OutlineRow {
    let (first, rest) = path.split_first().expect("a path is never empty");
    let mut node = &mut roots[*first];
    for i in rest {
        node = &mut node.children[*i];
    }
    node
}

/// The `engine` string for a registry row.
fn engine_label(row: Option<&crate::syntax::SyntaxRow>) -> String {
    match row.map(|r| r.engine) {
        Some(crate::syntax::Engine::TreeSitter(g)) => format!("tree-sitter:{g}"),
        Some(crate::syntax::Engine::Scanner(s)) => format!("scanner:{s}"),
        _ => "none".to_string(),
    }
}

/// Build the `outline/1` body from a symbol list and the file's type.
///
/// `symbols` is what `GET /api/symbols` would return for the same file —
/// the store's rows, not a fresh extraction. See the module doc.
pub fn build(
    repo: String,
    path: String,
    rev: Option<String>,
    row: Option<&'static crate::syntax::SyntaxRow>,
    symbols: Vec<Symbol>,
) -> OutlineOut {
    let (tier, tier_reason) = match row {
        Some(r) => (r.tier, r.note),
        None => (
            crate::syntax::Tier::None,
            Some("no syntax/1 registry row for this file type"),
        ),
    };
    let plans_symbols = row.map(|r| r.plan().symbols).unwrap_or(false);
    let (rows, total, truncated) = if plans_symbols {
        nest(symbols)
    } else {
        (Vec::new(), 0, false)
    };
    let reason = if !plans_symbols {
        Some(
            tier_reason
                .unwrap_or("this file type's tier derives no symbols")
                .to_string(),
        )
    } else if total == 0 {
        Some(
            "no symbols indexed for this blob — the tier derives them, so this is an \
             un-indexed or empty file rather than a design outcome"
                .to_string(),
        )
    } else if truncated {
        Some(format!("truncated at {MAX_ROWS} rows"))
    } else {
        None
    };
    OutlineOut {
        schema: OUTLINE_SCHEMA,
        repo,
        path,
        rev,
        lang: row.and_then(|r| r.info).map(|i| i.id),
        tier: tier.as_str(),
        rows,
        total,
        truncated,
        honesty: Honesty {
            tier: tier.as_str(),
            engine: engine_label(row),
            derived_from: if plans_symbols { "symbols" } else { "none" },
            reason,
        },
    }
}

// ── the route ────────────────────────────────────────────────────────────

use crate::store::StoreBlocking;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;

#[derive(Debug, serde::Deserialize)]
pub struct OutlineParams {
    pub repo: String,
    pub path: String,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
}

/// `GET /api/outline?repo=&path=[&ref=]` — the universal per-file outline.
///
/// A pure store lookup plus [`nest`], for exactly the reason
/// `GET /api/symbols` is one: the rows are the ingest pipeline's, and
/// deriving them here would make a second answer that can disagree with
/// the first.
pub async fn outline_route(
    State(state): State<crate::state::SharedState>,
    Query(params): Query<OutlineParams>,
) -> Result<Response, crate::routes::ApiError> {
    let (repo, _repo_id) = crate::routes::find_repo(&state, &params.repo)?;
    let read = crate::routes::read_repo_file(repo, &params.path, params.rev.as_deref())?;
    let row = crate::syntax::row_for_path(&params.path, Some(&read.bytes));
    let symbols = match row.and_then(|r| r.info) {
        Some(li) => {
            let blob_hash = read.blob_hash.clone();
            // 2026-08-31 incident (store.rs module doc): single store call,
            // still wrapped so it can never park this async worker.
            state
                .store
                .run_blocking(move |store| store.symbols_for_blob(&blob_hash, li.salt))
                .await?
        }
        None => Vec::new(),
    };
    let body = build(
        params.repo.clone(),
        params.path.clone(),
        params.rev.clone(),
        row,
        symbols,
    );
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response())
}

// ── the declaration↔handler walk (crate invariant 15) ────────────────────

fn outline_params_accept_without(omit: &str) -> bool {
    let mut q = serde_json::json!({ "repo": "r", "path": "a.rb" });
    q.as_object_mut().expect("object").remove(omit);
    serde_json::from_value::<OutlineParams>(q).is_ok()
}

pub const OUTLINE_ROUTE: crate::entities::RouteContract = crate::entities::RouteContract {
    path: "/api/outline",
    handler: "outline::outline_route",
    required_params: &["repo", "path"],
    params_accept_without: outline_params_accept_without,
};

/// Every route V72-H2a adds. Walked from BOTH sides exactly as
/// `entities::V71_G0_ROUTES` is — see that list's doc.
pub const V72_H2A_ROUTES: &[crate::entities::RouteContract] = &[OUTLINE_ROUTE];

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTER_SRC: &str = include_str!("router.rs");

    fn sym(name: &str, kind: &str, start: u32, end: u32) -> Symbol {
        Symbol {
            ordinal: 0,
            name: name.to_string(),
            kind: kind.to_string(),
            line_start: start,
            line_end: end,
            col_start: 0,
            col_end: 0,
            container: None,
            signature: None,
            doc: None,
            param_min: None,
            param_max: None,
        }
    }

    fn shape(rows: &[OutlineRow]) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        fn walk(rows: &[OutlineRow], out: &mut Vec<(u32, String)>) {
            for r in rows {
                out.push((r.depth, r.name.clone()));
                walk(&r.children, out);
            }
        }
        walk(rows, &mut out);
        out
    }

    #[test]
    fn containment_nests_and_siblings_stay_flat() {
        let (rows, total, truncated) = nest(vec![
            sym("outer", "class", 1, 20),
            sym("inner_a", "method", 2, 5),
            sym("inner_b", "method", 7, 12),
            sym("deep", "block", 8, 9),
            sym("after", "fn", 21, 30),
        ]);
        assert_eq!(total, 5);
        assert!(!truncated);
        assert_eq!(
            shape(&rows),
            vec![
                (0, "outer".to_string()),
                (1, "inner_a".to_string()),
                (1, "inner_b".to_string()),
                (2, "deep".to_string()),
                (0, "after".to_string()),
            ]
        );
    }

    /// The failure mode the client-side `container == name` rule has, and
    /// the reason this one is containment-based.
    #[test]
    fn two_identically_named_symbols_nest_by_position_not_by_name() {
        let (rows, _, _) = nest(vec![
            sym("Shape", "class", 1, 10),
            sym("area", "method", 2, 3),
            sym("Shape", "class", 20, 30),
            sym("area", "method", 21, 22),
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].children.len(), 1);
        assert_eq!(rows[1].children.len(), 1);
        assert_eq!(rows[0].children[0].range.line_start, 2);
        assert_eq!(rows[1].children[0].range.line_start, 21);
    }

    #[test]
    fn an_unsorted_input_produces_the_same_tree() {
        let ordered = nest(vec![
            sym("a", "k", 1, 9),
            sym("b", "k", 2, 3),
            sym("c", "k", 4, 5),
        ]);
        let shuffled = nest(vec![
            sym("c", "k", 4, 5),
            sym("a", "k", 1, 9),
            sym("b", "k", 2, 3),
        ]);
        assert_eq!(ordered, shuffled);
    }

    #[test]
    fn degenerate_ranges_never_panic() {
        let (rows, total, _) = nest(vec![
            sym("zero", "k", 0, 0),
            sym("same", "k", 5, 5),
            sym("same-again", "k", 5, 5),
            sym("inverted", "k", 9, 2),
            sym("huge", "k", 1, u32::MAX),
        ]);
        assert_eq!(total, 5);
        assert!(!rows.is_empty());
    }

    #[test]
    fn the_row_cap_is_reported_rather_than_silently_applied() {
        let symbols: Vec<Symbol> = (0..MAX_ROWS + 10)
            .map(|i| sym(&format!("s{i}"), "k", i as u32 + 1, i as u32 + 1))
            .collect();
        let (_, total, truncated) = nest(symbols);
        assert!(truncated);
        assert_eq!(total, MAX_ROWS);
    }

    // ── the contract, over every registered language ─────────────────────

    /// A one-definition fixture per registry row that derives symbols —
    /// the same shape `extract`'s own dispatch test uses, extended to the
    /// tags languages so this test covers EVERY row rather than the CST
    /// subset.
    fn fixture_for(lang: &str) -> Option<&'static [u8]> {
        Some(match lang {
            "rust" => b"fn a() {}\n".as_slice(),
            "python" => b"def a():\n    pass\n".as_slice(),
            "ruby" => b"def a\nend\n".as_slice(),
            "typescript" => b"function a(): void {}\n".as_slice(),
            "tsx" => b"function A() { return null; }\n".as_slice(),
            "javascript" => b"function a() {}\n".as_slice(),
            "bash" => b"a() {\n  echo hi\n}\n".as_slice(),
            "go" => b"package p\nfunc a() {}\n".as_slice(),
            "yaml" => b"a: 1\n".as_slice(),
            "toml" => b"a = 1\n".as_slice(),
            "json" => b"{\"a\": 1}".as_slice(),
            "css" => b".a { color: red; }\n".as_slice(),
            "scss" => b"$a: 1;\n".as_slice(),
            "markdown" => b"# a\n\ntext\n".as_slice(),
            "haml" => b"%section\n  %p hi\n".as_slice(),
            _ => return None,
        })
    }

    /// `outline/1` answers for EVERY registry row, and the answer is
    /// honest in all four shapes — this is the test that flipped the
    /// Parity Grid's `outline` column off its shared "not built yet"
    /// reason.
    #[test]
    fn outline_answers_for_every_registered_language() {
        for row in crate::syntax::REGISTRY {
            let derives = row.plan().symbols;
            let symbols = match (derives, fixture_for(row.lang)) {
                (true, Some(src)) => crate::extract::extract_symbols(row.lang, src)
                    .unwrap_or_else(|e| panic!("{}: {e}", row.lang)),
                _ => Vec::new(),
            };
            let out = build(
                "r".to_string(),
                format!("a.{}", row.extensions.first().unwrap_or(&"x")),
                None,
                Some(row),
                symbols,
            );
            assert_eq!(out.schema, OUTLINE_SCHEMA);
            assert_eq!(out.tier, row.tier.as_str());
            assert_eq!(out.honesty.tier, row.tier.as_str());
            if derives {
                assert!(
                    fixture_for(row.lang).is_some(),
                    "{}: a symbol-deriving language needs a contract fixture — add one \
                     rather than letting this row go untested",
                    row.lang
                );
                assert_eq!(out.honesty.derived_from, "symbols", "{}", row.lang);
                assert!(
                    !out.rows.is_empty(),
                    "{}: no rows from its fixture",
                    row.lang
                );
                assert!(out.honesty.reason.is_none(), "{}", row.lang);
                // Every row's kind is a real symbol kind and every range
                // is sane.
                let mut stack: Vec<&OutlineRow> = out.rows.iter().collect();
                while let Some(r) = stack.pop() {
                    assert!(!r.kind.is_empty(), "{}: empty kind", row.lang);
                    assert!(!r.name.is_empty(), "{}: empty name", row.lang);
                    assert!(
                        r.range.line_end >= r.range.line_start,
                        "{}: {r:?}",
                        row.lang
                    );
                    stack.extend(r.children.iter());
                }
            } else {
                assert_eq!(out.honesty.derived_from, "none", "{}", row.lang);
                assert!(out.rows.is_empty(), "{}", row.lang);
                assert!(
                    out.honesty.reason.is_some(),
                    "{}: an empty outline must say why",
                    row.lang
                );
            }
        }
    }

    #[test]
    fn an_unregistered_file_type_is_an_honest_empty_outline() {
        let out = build(
            "r".to_string(),
            "NOTES.txt".to_string(),
            None,
            None,
            Vec::new(),
        );
        assert_eq!(out.tier, "none");
        assert_eq!(out.lang, None);
        assert_eq!(out.honesty.engine, "none");
        assert_eq!(
            out.honesty.reason.as_deref(),
            Some("no syntax/1 registry row for this file type")
        );
    }

    #[test]
    fn a_full_tier_file_with_no_indexed_rows_says_so() {
        let row = crate::syntax::row_for_lang("rust").expect("rust row");
        let out = build(
            "r".to_string(),
            "a.rs".to_string(),
            None,
            Some(row),
            Vec::new(),
        );
        assert_eq!(out.honesty.derived_from, "symbols");
        assert!(out
            .honesty
            .reason
            .expect("must explain the empty list")
            .contains("un-indexed"));
    }

    #[test]
    fn the_engine_label_distinguishes_a_grammar_from_a_scanner() {
        assert_eq!(
            engine_label(crate::syntax::row_for_lang("rust")),
            "tree-sitter:tree-sitter-rust"
        );
        assert_eq!(
            engine_label(crate::syntax::row_for_lang("haml")),
            format!("scanner:{}", crate::haml::SCANNER_VERSION)
        );
        assert_eq!(engine_label(crate::syntax::row_for_lang("sql")), "none");
    }

    #[test]
    fn every_declared_v72_h2a_route_is_registered_and_requires_its_params() {
        assert!(!V72_H2A_ROUTES.is_empty());
        for c in V72_H2A_ROUTES {
            let nested = c
                .path
                .strip_prefix("/api")
                .expect("every route path is /api-nested");
            assert!(
                ROUTER_SRC.contains(&format!("\"{nested}\"")),
                "{}: declared but never registered in router.rs",
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
}
