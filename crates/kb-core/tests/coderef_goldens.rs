//! DCB W1.A — golden-fixture regression corpus for `kb_core::coderefs`.
//!
//! Two fixtures, both SYNTHETIC (see `coderef_fixtures/README.md` — kb is a
//! public repo and the motivating corpus is a client's work product, so every
//! string here is invented and only the SHAPES are borrowed). `workplan.html`
//! is the HTML engineering plan; `note.md` is the Markdown artifact, pushed
//! through `markdown::render_fragment` first exactly as the indexer does, so
//! the pair proves the claim that one grammar covers both artifact kinds.
//!
//! Determinism: `coderefs::extract`'s doc comment claims "no I/O; no clock",
//! and `extraction_is_deterministic` below confirms it empirically by parsing
//! each fixture twice and asserting byte-identical output — the
//! `session_digest_snapshots.rs` pattern.

use kb_core::coderefs::{extract, parse_path_token, CodeRefKind, CodeRev, Extraction};

fn fixture(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/coderef_fixtures")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn workplan() -> Extraction {
    extract(&fixture("workplan.html"))
}

fn note() -> Extraction {
    extract(&kb_core::markdown::render_fragment(&fixture("note.md")))
}

/// `(kind, raw_text)` per ref, in document order — the compact projection the
/// assertions below read.
fn kinds(e: &Extraction) -> Vec<(&'static str, &str)> {
    e.refs
        .iter()
        .map(|r| (r.kind.as_str(), r.raw_text.as_str()))
        .collect()
}

#[test]
fn golden_workplan_extraction() {
    let e = workplan();
    insta::assert_debug_snapshot!(e);
}

#[test]
fn golden_note_markdown_extraction() {
    let e = note();
    insta::assert_debug_snapshot!(e);
}

/// The invariant-#5 pin, named for the MECHANISM (`TEXT_SKIP_TAGS`) and
/// pinned for the kb-prompt bundle. html5ever puts `<template>` children in
/// the MAIN tree, so a `<code>` inside one IS reachable from a descendant
/// walk — this fixture's prompt therefore contains real elements, not plain
/// text, and would pass even with the skip removed if it did not.
///
/// Every planted decoy in the four skipped subtrees contains `nope` and no
/// legitimate INFERRED ref does, so ONE assertion fires on a regression in ANY
/// of them. (The fixture's deliberately-malformed `data-kb-ref` value is
/// `app/models/nope..rb`, so the sweep is scoped to non-declared refs —
/// nothing inside a skipped subtree can be declared, since a declared ref is
/// only minted from a `<code>` the walker actually reached.)
// invariant:2 input-surface
#[test]
fn skip_tag_subtrees_yield_zero_refs() {
    let e = workplan();
    assert!(
        e.refs
            .iter()
            .all(|r| r.declared || !r.raw_text.contains("nope")),
        "{:?}",
        kinds(&e)
    );
    // …including the `<a href>` lane: issue 99999 lives in the kb-prompt.
    let issues: Vec<u32> = e
        .refs
        .iter()
        .filter(|r| r.kind == CodeRefKind::Issue)
        .filter_map(|r| r.line_start)
        .collect();
    let mut sorted = issues.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![15351, 15356, 15357], "99999 must be absent");
}

#[test]
fn workplan_code_rev_and_counters() {
    let e = workplan();
    assert_eq!(
        e.code_rev,
        Some(CodeRev {
            label: "shopfront".into(),
            sha: "bcd13a1d3".into(),
            dirty: true,
        })
    );
    assert!(!e.truncated);
    // The two refs before the first h2/h3, and no sentinel group for them (R9).
    assert_eq!(e.ungrouped_count, 2);
    let ungrouped: Vec<&str> = e
        .refs
        .iter()
        .filter(|r| r.group.is_none())
        .map(|r| r.raw_text.as_str())
        .collect();
    assert_eq!(ungrouped, vec!["config/importmap.rb", "dispatcher.rb"]);
    assert!(e.groups.iter().all(|g| !g.key.is_empty()));
}

#[test]
fn workplan_groups_are_ref_bearing_only() {
    let e = workplan();
    let keys: Vec<&str> = e.groups.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "kb-h-a1-token-identity-deploy-a-15356",
            "kb-h-a2-conversion-rewrite-deploy-a-15357",
            // The `<h3 id="explicit-anchor">` — an explicit id wins verbatim,
            // with no `kb-h-` prefix.
            "explicit-anchor",
        ],
        "`Why reshuffle` and `Work items` own zero refs directly and are absent"
    );
    for g in &e.groups {
        assert_eq!(g.key, g.anchor, "key == anchor by construction today");
    }
    assert_eq!(e.groups[0].ordinal, 0);
    assert_eq!(e.groups[2].ordinal, 2);
}

#[test]
fn workplan_declared_refs_survive_malformed() {
    let e = workplan();
    let declared: Vec<&kb_core::coderefs::CodeRef> = e.refs.iter().filter(|r| r.declared).collect();
    assert_eq!(declared.len(), 2);
    assert_eq!(
        declared[0].path_hint.as_deref(),
        Some("app/models/order.rb")
    );
    assert_eq!(
        (declared[0].line_start, declared[0].line_end),
        (Some(120), Some(140))
    );
    assert_eq!(declared[0].kind, CodeRefKind::Path);
    // The typo'd one is PRESENT, not dropped — it must render loudly as
    // "declared but absent".
    assert_eq!(
        declared[1].path_hint.as_deref(),
        Some("app/models/nope..rb")
    );
    assert_eq!(declared[1].kind, CodeRefKind::Path);
}

#[test]
fn workplan_conventions_list_yields_zero_refs() {
    let e = workplan();
    // Every near-miss token in the "Conventions" `<li>` (and the two regex /
    // glob shapes in A1) must be refused. Assert on the raw text so a new
    // false positive names itself.
    for needle in [
        ".before_deploy.rb",
        ".to_f",
        "Time.zone.parse",
        "Arel.sql",
        "order_items.sku",
        "hit.__queryID",
        "insights.example.io",
        ".shop-Hits-item",
        "jest.config",
        "start_time..end_time",
        "shopclient-3.12.2\"",
        "search-insights@2.17.3",
        "app/javascript",
        "app/tasks/maintenance/search/",
        "plan-a2-conversion.html:390",
        "notes/design.md",
        "order_dispatch/{base,conversion}.rb",
        "db/migrate/",
        "UUID_FORMAT",
        "shp-",
        "clickAnalytics",
    ] {
        assert!(
            e.refs.iter().all(|r| !r.raw_text.contains(needle)),
            "{needle} must not appear in any ref: {:?}",
            kinds(&e)
        );
    }
}

#[test]
fn workplan_pre_block_is_pass_b_paths_only() {
    let e = workplan();
    // The listing's own header comment + the inline `# see …` line.
    assert!(kinds(&e).contains(&("path", "app/services/order_dispatch/conversion.rb")));
    assert!(kinds(&e).contains(&("path_line", "app/models/order.rb:1044")));
    // Symbols are Pass-A only, so nothing inside the listing becomes one.
    assert!(
        e.refs
            .iter()
            .all(|r| r.raw_text != "Billing::InvoiceService" && r.raw_text != "OrderDispatch"),
        "{:?}",
        kinds(&e)
    );
    // The nested-markup line ref proves the walker reads textContent.
    assert!(kinds(&e).contains(&("path_line", "app/models/order.rb:77")));
}

#[test]
fn workplan_line_list_is_one_ref_carrying_every_span() {
    let e = workplan();
    let r = e
        .refs
        .iter()
        .find(|r| r.raw_text == "assets.js.erb:30,51-65,113-119")
        .expect("the mixed list");
    assert_eq!(r.kind, CodeRefKind::PathList);
    assert_eq!((r.line_start, r.line_end), (Some(30), None));
    assert_eq!(r.line_spans.as_deref(), Some("30,51-65,113-119"));
    assert_eq!(
        e.refs
            .iter()
            .filter(|x| x.raw_text == "assets.js.erb:30,51-65,113-119")
            .count(),
        1,
        "one ref, never a fan-out"
    );
}

#[test]
fn note_markdown_extraction_shape() {
    let e = note();
    assert_eq!(
        kinds(&e),
        vec![
            ("path", "app/services/order_dispatch/conversion.rb"),
            ("path_line", "checkout_controller.rb:284"),
            ("symbol_method", "Billing::InvoiceService#call"),
            ("symbol_method", "Order#confirm_payment!"),
            ("path_line", "app/models/order.rb:1044"),
        ]
    );
    // An `<h1>` never opens a group, so every ref is ungrouped.
    assert!(e.groups.is_empty());
    assert_eq!(e.ungrouped_count, 5);
    assert!(e.code_rev.is_none());
}

#[test]
fn extraction_is_deterministic() {
    assert_eq!(workplan(), workplan());
    assert_eq!(note(), note());
}

/// The W1.gate ruling (spec §1/§14): a hand-eyeballed false-positive sweep
/// over the real 937-ref prototype corpus found `instantsearch.js` (an npm
/// package name, not a file) among the ≈0.9 % false-positive family — well
/// under the gate's < 5 % bar — and the grammar was shipped accepting it
/// rather than special-casing it away (the extension whitelist has no way to
/// distinguish a same-named package from a file, and both the
/// `after_deploy.rb`/`before_deploy.rb` naming-convention and the
/// version-dir-less gem-file members of the same family resolve to a
/// *miss* + a search link in kb-code, never a live wrong link). Pinned here
/// so a future grammar change that either NARROWS the rule (dropping this
/// accepted shape) or WIDENS it (accepting more like it) shows up as a diff
/// against a named ruling, not a silent behavior change.
#[test]
fn known_false_positives_are_accepted_shapes() {
    let p = parse_path_token("instantsearch.js")
        .expect("the W1.gate-accepted false positive must still parse");
    assert_eq!(p.kind, CodeRefKind::Path);
    assert_eq!(p.path_hint, "instantsearch.js");
}
