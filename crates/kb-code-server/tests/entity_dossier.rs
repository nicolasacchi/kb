//! V72-G1.1 — the `entity/1` dossier, end to end over synthetic Ruby.
//!
//! Every fixture under `tests/fixtures/entity-dossier/` is a real Ruby file
//! this test parses with the SHIPPED extractor
//! (`kb_code_server::extract::extract_symbols` +
//! `kb_code_server::entities::defs_for_file`), so no line number is
//! hardcoded here and no fixture can drift from what the indexer would
//! actually have stored. The rows those two produce are handed to
//! `build_dossier` through a `DossierSource` backed by the fixture tree —
//! the same trait the route implements over the store and the working
//! tree.
//!
//! Two of these tests are the unit's oracle bar. `usage_rows_are_carried_
//! verbatim_and_no_lane_upgrades_a_trust_class` proves the dossier cannot
//! mint an `exact` the usages/2 engine did not, and
//! `a_nesting_relative_reference_is_never_exact` proves the reference
//! ladder's own narrow `exact` mint. A wrong `exact` is a release blocker.

use kb_code_server::entities::dossier::{
    build_dossier, resolve_reference, DossierOptions, DossierOut, DossierSource, UsageAnchor,
    UsagesInput, BLOCK_REOPEN, DEFAULT_USAGES_PER_KIND, ENTITY_KIND_CLASS, LANE_PRIORITY,
    REASON_ENTITY_UNKNOWN, RESOLVED_UNRESOLVED, STATE_EMPTY, STATE_PARTIAL,
};
use kb_code_server::entities::ruby_body as rb;
use kb_code_server::entities::zeitwerk::{Zeitwerk, STATE_READ};
use kb_code_server::entities::{defs_for_file, ENTITY_SCHEMA};
use kb_code_server::store::EntityDefRow;
use kb_code_server::usages2::{
    roles, UsageKind, UsageRow2, Usages2Out, DEFAULT_LIMIT as USAGES_LIMIT,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// the fixture source
// ---------------------------------------------------------------------------

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/entity-dossier")
}

const FIXTURE_FILES: &[&str] = &[
    "app/models/application_record.rb",
    "app/models/shop/invoice.rb",
    "app/models/shop/ledger.rb",
    "app/models/shop/order.rb",
    "app/models/shop/order_totals.rb",
    "app/models/shop/payable.rb",
];

fn zeitwerk() -> Zeitwerk {
    Zeitwerk {
        state: STATE_READ,
        reason: None,
        roots: vec!["app/models".to_string()],
        collapse: Vec::new(),
        acronyms: Vec::new(),
        ignore: Vec::new(),
    }
}

/// A stand-in blob hash — stable per path, so a "fresh" row and a "stale"
/// row are distinguishable without a git repo.
fn blob_of(path: &str) -> String {
    format!("blob-{}", path.replace(['/', '.'], "-"))
}

struct Fixture {
    rows: Vec<EntityDefRow>,
    /// Paths whose live bytes are deliberately absent (the deleted-file
    /// case).
    missing: Vec<String>,
}

impl Fixture {
    fn new() -> Self {
        let zw = zeitwerk();
        let mut rows = Vec::new();
        for rel in FIXTURE_FILES {
            let bytes = std::fs::read(fixture_root().join(rel)).expect("fixture file reads");
            let symbols =
                kb_code_server::extract::extract_symbols("ruby", &bytes).expect("ruby parses");
            for claim in defs_for_file(rel, &bytes, &symbols, &zw) {
                rows.push(EntityDefRow {
                    worktree: String::new(),
                    path: (*rel).to_string(),
                    fqn: claim.fqn,
                    kind: claim.kind,
                    nesting: claim.nesting.to_string(),
                    line_start: i64::from(claim.line_start),
                    line_end: i64::from(claim.line_end),
                    zeitwerk_fqn: claim.zeitwerk_fqn,
                    zeitwerk_state: STATE_READ.to_string(),
                    blob_hash: blob_of(rel),
                    live_blob_hash: Some(blob_of(rel)),
                });
            }
        }
        Self {
            rows,
            missing: Vec::new(),
        }
    }

    fn without_bytes_for(mut self, path: &str) -> Self {
        self.missing.push(path.to_string());
        self
    }
}

impl DossierSource for Fixture {
    fn read(&self, path: &str) -> Option<(Vec<u8>, String)> {
        if self.missing.iter().any(|m| m == path) {
            return None;
        }
        std::fs::read(fixture_root().join(path))
            .ok()
            .map(|b| (b, blob_of(path)))
    }

    /// The same two arms `Store::entity_defs_for_name` has: the exact
    /// name (tree-proved or convention-derived), and — only when that
    /// finds nothing — the last segment.
    fn defs_for_name(&self, name: &str) -> Vec<EntityDefRow> {
        let exact: Vec<EntityDefRow> = self
            .rows
            .iter()
            .filter(|r| r.fqn == name || r.zeitwerk_fqn.as_deref() == Some(name))
            .cloned()
            .collect();
        if !exact.is_empty() {
            return exact;
        }
        let suffix = format!("::{name}");
        self.rows
            .iter()
            .filter(|r| {
                r.fqn.ends_with(&suffix)
                    || r.zeitwerk_fqn
                        .as_deref()
                        .is_some_and(|z| z.ends_with(&suffix))
            })
            .cloned()
            .collect()
    }

    fn all_defs(&self) -> &[EntityDefRow] {
        &self.rows
    }
}

// ---------------------------------------------------------------------------
// a hand-built usages/2 answer
// ---------------------------------------------------------------------------

fn row(path: &str, line: u32, kind: UsageKind, trust: &'static str) -> UsageRow2 {
    UsageRow2 {
        path: path.to_string(),
        line,
        col: 4,
        col_end: None,
        blob_sha: Some(blob_of(path)),
        kind,
        roles: roles::READ_ACCESS,
        role_names: roles::names(roles::READ_ACCESS),
        trust,
        precision: kb_code_server::resolve::PRECISION_TAGS_APPROX,
        access: None,
        enclosing: None,
        context: "context".to_string(),
    }
}

/// A `usages/2` answer whose `kind_totals` deliberately exceed the rows it
/// carries — the shape the real engine produces once its own per-class cap
/// bites, and the one that proves a group's `total` is the TRUE total.
fn usages_fixture() -> UsagesInput {
    let invoice = "app/models/shop/invoice.rb";
    let likely = kb_code_server::resolve::CLASS_LIKELY;
    let candidate = kb_code_server::resolve::CLASS_CANDIDATE;
    let mut rows_call: Vec<UsageRow2> = Vec::new();
    for line in 1..=5u32 {
        rows_call.push(row(
            "app/services/shop/checkout.rb",
            line,
            UsageKind::Call,
            likely,
        ));
    }
    let out = Usages2Out {
        schema: kb_code_server::usages2::USAGES2_SCHEMA,
        symbol: kb_code_server::usages::UsageSymbol {
            name: "Order".to_string(),
            kind: Some("class".to_string()),
            container: Some("Shop".to_string()),
        },
        class_of_definition: likely,
        exact: Vec::new(),
        likely: {
            let mut v = vec![row(invoice, 4, UsageKind::Inherit, likely)];
            v.extend(rows_call);
            v
        },
        candidate: vec![
            row(invoice, 5, UsageKind::Include, candidate),
            row(
                "app/models/shop/ledger.rb",
                4,
                UsageKind::Prepend,
                candidate,
            ),
        ],
        totals: kb_code_server::usages2::Totals {
            exact: 0,
            likely: 6,
            candidate: 2,
            all: 8,
        },
        capped: Vec::new(),
        kind_totals: BTreeMap::from([
            (UsageKind::Inherit.as_str(), 1usize),
            // 40 real call sites; the engine returned 5.
            (UsageKind::Call.as_str(), 40usize),
            (UsageKind::Include.as_str(), 1usize),
            (UsageKind::Prepend.as_str(), 1usize),
        ]),
        ruby_strict: None,
    };
    UsagesInput {
        out: Some(out),
        anchor: Some(UsageAnchor {
            path: "app/models/shop/order.rb".to_string(),
            line: 4,
            col: 10,
        }),
        unavailable_reason: None,
    }
}

fn build(ent: &str, opts: DossierOptions) -> DossierOut {
    build_dossier(
        "acme-app",
        ent,
        &opts,
        &Fixture::new(),
        &zeitwerk(),
        usages_fixture(),
    )
    .expect("the fixture entity is indexed")
}

fn member<'a>(
    out: &'a DossierOut,
    name: &str,
) -> Option<&'a kb_code_server::entities::dossier::MemberRow> {
    out.members.iter().find(|m| m.name == name)
}

// ---------------------------------------------------------------------------
// the dead-surface walk
// ---------------------------------------------------------------------------

const ROUTER_SRC: &str = include_str!("../src/router.rs");

#[test]
fn every_declared_v72_g1_route_is_registered_and_requires_its_params() {
    let routes = kb_code_server::entities::dossier::V72_G1_ROUTES;
    assert!(!routes.is_empty(), "the unit declares at least one route");
    for c in routes {
        let nested = c
            .path
            .strip_prefix("/api")
            .expect("every route path is /api-nested");
        assert!(
            ROUTER_SRC.contains(&format!("\"{nested}\"")),
            "{}: declared but never registered in router.rs — the v7.0 dead-surface defect",
            c.path
        );
        assert!(
            ROUTER_SRC.contains(c.handler),
            "{}: registered path but no {} handler named in router.rs",
            c.path,
            c.handler
        );
        assert!(
            (c.params_accept_without)(""),
            "{}: its own params struct rejects a COMPLETE query map",
            c.path
        );
        for p in c.required_params {
            assert!(
                !(c.params_accept_without)(p),
                "{}: declares {p:?} required, but its params struct accepts a request without it",
                c.path
            );
        }
    }
}

#[test]
fn the_dossier_is_a_sibling_of_the_frozen_index_never_a_rename_of_it() {
    // `entities/1` keeps its own schema string and its own path. A caller
    // that upgrades to the dossier does so by ADDRESSING it.
    assert_eq!(ENTITY_SCHEMA, "entities/1");
    assert_eq!(
        kb_code_server::entities::dossier::DOSSIER_SCHEMA,
        "entity/1"
    );
    assert_eq!(kb_code_server::entities::ENTITY_ROUTE.path, "/api/entity");
    assert_eq!(
        kb_code_server::entities::dossier::DOSSIER_ROUTE.path,
        "/api/entity/dossier"
    );
}

// ---------------------------------------------------------------------------
// definitions
// ---------------------------------------------------------------------------

/// V72-G1.1 regression, and the reason this unit touched V71-G0 at all.
///
/// `Symbol::col_start` is the DEFINITION node's column, so
/// `defs_for_file`'s compact-scope recovery was reading the line's
/// INDENTATION and returned `None` for every real file: `class
/// Shop::Order` was indexed as a top-level `Order`, at `exact`. The V71-G0
/// unit tests could not see it because they hand-set `col_start` to the
/// name column, a value the real extractor never produces — so this test
/// runs the SHIPPED extractor over a real file instead.
#[test]
fn a_compact_definition_is_indexed_under_its_recovered_scope_not_the_bare_name() {
    let src = Fixture::new();
    let fqns: Vec<&str> = src
        .rows
        .iter()
        .filter(|r| r.path == "app/models/shop/order_totals.rb")
        .map(|r| r.fqn.as_str())
        .collect();
    assert_eq!(fqns, vec!["Shop::Order", "Shop::Order::Line"]);
    assert!(
        !src.rows.iter().any(|r| r.fqn == "Order"),
        "a bare top-level `Order` is exactly the wrong `exact` the recovery exists to prevent"
    );
}

#[test]
fn a_class_reopened_in_two_files_lists_both_with_the_autoload_canonical_one_first() {
    let out = build("Shop::Order", DossierOptions::default());
    assert_eq!(out.entity.fqn, "Shop::Order");
    assert_eq!(out.entity.kind, ENTITY_KIND_CLASS);
    assert_eq!(out.entity.namespace.as_deref(), Some("Shop"));
    assert_eq!(out.definitions.len(), 2, "{:#?}", out.definitions);
    assert_eq!(
        out.entity.files,
        vec![
            "app/models/shop/order.rb".to_string(),
            "app/models/shop/order_totals.rb".to_string()
        ]
    );

    let canonical = &out.definitions[0];
    assert_eq!(canonical.path, "app/models/shop/order.rb");
    assert_eq!(canonical.reopening_index, 0);
    assert_eq!(
        canonical.kind, "class",
        "the autoload-canonical block keeps its keyword"
    );
    assert_eq!(canonical.opener, "module Shop; class Order");
    assert_eq!(canonical.opener_form, rb::OPENER_NESTED);
    assert_eq!(canonical.trust, kb_code_server::resolve::CLASS_EXACT);
    assert!(!canonical.stale && !canonical.missing);

    let reopen = &out.definitions[1];
    assert_eq!(reopen.path, "app/models/shop/order_totals.rb");
    assert_eq!(reopen.reopening_index, 1);
    assert_eq!(
        reopen.kind, BLOCK_REOPEN,
        "a block at a path Zeitwerk does not expect to define this constant is a reopening"
    );
    assert_eq!(reopen.opener, "class Shop::Order");
    assert_eq!(reopen.opener_form, rb::OPENER_COMPACT);
    assert_eq!(out.entity.trust_counts.exact, 2);
}

#[test]
fn an_entity_whose_every_file_lost_its_bytes_is_empty_with_a_reason_not_a_404() {
    let src = Fixture::new()
        .without_bytes_for("app/models/shop/order.rb")
        .without_bytes_for("app/models/shop/order_totals.rb");
    let out = build_dossier(
        "acme-app",
        "Shop::Order",
        &DossierOptions::default(),
        &src,
        &zeitwerk(),
        UsagesInput::default(),
    )
    .expect("the entity is still in the index");
    assert_eq!(out.honesty.state, STATE_EMPTY);
    let reason = out.honesty.reason.expect("an empty state names its reason");
    assert!(reason.contains("live bytes"), "{reason}");
    assert_eq!(out.definitions.len(), 2, "the sites are still listed");
    assert!(out.definitions.iter().all(|d| d.missing));
    assert!(out.members.is_empty());
}

#[test]
fn an_unknown_constant_is_a_typed_404_not_an_empty_dossier() {
    let err = build_dossier(
        "acme-app",
        "Shop::NoSuchThing",
        &DossierOptions::default(),
        &Fixture::new(),
        &zeitwerk(),
        UsagesInput::default(),
    )
    .expect_err("nothing answers to this address");
    assert_eq!(err.status, 404);
    assert_eq!(err.reason, REASON_ENTITY_UNKNOWN);
}

#[test]
fn an_ambiguous_bare_name_refuses_to_pick_and_lists_every_candidate() {
    // `Payable` is defined once, `Order` once — `Line` is the bare name
    // that must NOT be answered by merging, so use a name two constants
    // answer to by construction.
    let mut src = Fixture::new();
    let mut extra = src.rows[0].clone();
    extra.path = "app/models/warehouse/order.rb".to_string();
    extra.fqn = "Warehouse::Order".to_string();
    extra.kind = "class".to_string();
    extra.zeitwerk_fqn = Some("Warehouse::Order".to_string());
    extra.blob_hash = blob_of("app/models/warehouse/order.rb");
    extra.live_blob_hash = Some(blob_of("app/models/warehouse/order.rb"));
    src.rows.push(extra);

    let out = build_dossier(
        "acme-app",
        "Order",
        &DossierOptions::default(),
        &src,
        &zeitwerk(),
        UsagesInput::default(),
    )
    .expect("an ambiguous address is an answer, not an error");
    assert_eq!(out.honesty.state, STATE_EMPTY);
    assert_eq!(
        out.candidates,
        vec!["Shop::Order".to_string(), "Warehouse::Order".to_string()]
    );
    assert!(
        out.definitions.is_empty(),
        "nothing is merged on a bare name"
    );
}

// ---------------------------------------------------------------------------
// members + visibility
// ---------------------------------------------------------------------------

#[test]
fn the_member_table_merges_every_reopening_and_sorts_by_visibility_then_name() {
    let out = build("Shop::Order", DossierOptions::default());
    let names: Vec<&str> = out.members.iter().map(|m| m.name.as_str()).collect();
    for expected in [
        "total",          // tree, file 1
        "summary",        // tree, file 1
        "open_orders",    // tree, file 1, class << self
        "TAX_RATE",       // assignment, file 1
        "customer_ref",   // macro, file 1
        "lines",          // tree, file 2
        "build",          // tree, file 2, def self.
        "MAX_LINES",      // assignment, file 2
        "sum",            // macro alias, file 2
        "reset!",         // tree, file 1 — a bang method is still a member
        "decorate!",      // tree, file 1, def self.
        "DYNAMIC_FIELDS", // assignment, file 1
    ] {
        assert!(
            names.contains(&expected),
            "member {expected:?} missing from {names:?}"
        );
    }
    // Sorted by visibility rank, then name.
    let keys: Vec<(u8, &str)> = out
        .members
        .iter()
        .map(|m| (rb::visibility_rank(m.visibility), m.name.as_str()))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(
        keys, sorted,
        "the member table is not in (visibility, name) order"
    );

    // Kinds are the closed vocabulary, and only that.
    for m in &out.members {
        assert!(
            rb::MEMBER_KINDS.contains(&m.kind),
            "unknown member kind {:?}",
            m.kind
        );
        assert!(rb::VISIBILITIES.contains(&m.visibility));
        assert_eq!(m.defining_type, "Shop::Order");
        assert!(!m.inherited);
    }
    assert_eq!(member(&out, "customer_ref").unwrap().kind, "attr_accessor");
    assert_eq!(member(&out, "placed_at").unwrap().kind, "attr_reader");
    assert_eq!(member(&out, "MAX_LINES").unwrap().kind, "constant");
    assert_eq!(member(&out, "sum").unwrap().kind, "alias");
    assert_eq!(member(&out, "build").unwrap().kind, "singleton_method");
    assert_eq!(
        member(&out, "open_orders").unwrap().kind,
        "singleton_method"
    );
}

#[test]
fn a_private_section_and_a_private_name_are_both_resolved() {
    let out = build("Shop::Order", DossierOptions::default());
    // A bare `private` section flips everything after it in that block…
    assert_eq!(
        member(&out, "recompute").unwrap().visibility,
        rb::VIS_PRIVATE
    );
    // …and `private :audit_key` names ONE member, wherever it sits.
    assert_eq!(
        member(&out, "audit_key").unwrap().visibility,
        rb::VIS_PRIVATE
    );
    // `private def internal_key`, in the other reopening.
    assert_eq!(
        member(&out, "internal_key").unwrap().visibility,
        rb::VIS_PRIVATE
    );
    // Members before the section are public, and a block's visibility does
    // not leak across reopenings.
    assert_eq!(member(&out, "total").unwrap().visibility, rb::VIS_PUBLIC);
    assert_eq!(member(&out, "lines").unwrap().visibility, rb::VIS_PUBLIC);
    // `class << self` has its OWN visibility scope.
    assert_eq!(
        member(&out, "hidden_scope").unwrap().visibility,
        rb::VIS_PRIVATE
    );
    assert_eq!(
        member(&out, "open_orders").unwrap().visibility,
        rb::VIS_PUBLIC
    );
}

#[test]
fn a_private_keyword_this_scanner_cannot_place_yields_unknown_not_a_guess() {
    let out = build("Shop::Ledger", DossierOptions::default());
    let post = member(&out, "post").expect("Ledger#post is a member");
    assert_eq!(
        post.visibility,
        rb::VIS_UNKNOWN,
        "a `private` inside an `if` is a runtime decision — reporting `public` would be a guess"
    );
    assert!(
        out.honesty
            .notes
            .iter()
            .any(|n| n.contains("visibility could not be resolved")),
        "the refusal is captioned: {:#?}",
        out.honesty.notes
    );
    assert_eq!(out.honesty.state, STATE_PARTIAL);
}

#[test]
fn a_line_scanned_member_is_never_better_than_likely() {
    let out = build("Shop::Order", DossierOptions::default());
    for m in &out.members {
        if m.via == rb::VIA_TREE {
            continue;
        }
        assert_ne!(
            m.trust,
            kb_code_server::resolve::CLASS_EXACT,
            "{} was found by a {} scan and must never be exact",
            m.name,
            m.via
        );
    }
    // …while a member the TREE proves inside a block the tree proves keeps
    // that block's own class.
    assert_eq!(
        member(&out, "total").unwrap().trust,
        kb_code_server::resolve::CLASS_EXACT
    );
}

#[test]
fn inherited_members_appear_only_when_the_request_asks_for_them() {
    let without = build("Shop::Order", DossierOptions::default());
    assert!(
        member(&without, "pay").is_none(),
        "a mixin's member is not in the local table"
    );
    assert!(member(&without, "touch_audit").is_none());
    assert!(
        without
            .honesty
            .notes
            .iter()
            .any(|n| n.contains("inherited=1")),
        "the omission is captioned"
    );

    let with = build(
        "Shop::Order",
        DossierOptions {
            inherited: true,
            ..Default::default()
        },
    );
    let pay = member(&with, "pay").expect("Shop::Payable#pay is inherited");
    assert!(pay.inherited);
    assert_eq!(pay.defining_type, "Shop::Payable");
    let touch = member(&with, "touch_audit").expect("ApplicationRecord#touch_audit is inherited");
    assert!(touch.inherited);
    assert_eq!(touch.defining_type, "ApplicationRecord");
    // Ruby's method lookup can be shadowed by a mixin this index cannot
    // order, so an inherited row is capped one rung below `exact`.
    for m in with.members.iter().filter(|m| m.inherited) {
        assert_ne!(m.trust, kb_code_server::resolve::CLASS_EXACT);
    }
    // The local rows are untouched by the merge.
    assert!(!member(&with, "total").unwrap().inherited);
    assert_eq!(
        member(&with, "gateway").unwrap().visibility,
        rb::VIS_PRIVATE,
        "an inherited member keeps its own block's visibility"
    );
}

// ---------------------------------------------------------------------------
// hierarchy
// ---------------------------------------------------------------------------

#[test]
fn mixins_report_the_resolvable_and_the_unresolvable_alike() {
    let out = build("Shop::Order", DossierOptions::default());
    let by_written: BTreeMap<&str, &kb_code_server::entities::dossier::Mixin> = out
        .hierarchy
        .mixins
        .iter()
        .map(|m| (m.written.as_str(), m))
        .collect();

    let payable = by_written["Payable"];
    assert_eq!(payable.kind, "include");
    assert_eq!(payable.fqn.as_deref(), Some("Shop::Payable"));
    assert_eq!(
        payable.resolved,
        kb_code_server::resolve::CLASS_LIKELY,
        "a reference written inside `module Shop` is a runtime constant lookup"
    );

    let auditable = by_written["Auditable"];
    assert_eq!(auditable.kind, "prepend");
    assert_eq!(auditable.fqn, None);
    assert_eq!(auditable.resolved, RESOLVED_UNRESOLVED);

    let findable = by_written["Findable"];
    assert_eq!(findable.kind, "extend");
    assert_eq!(findable.resolved, RESOLVED_UNRESOLVED);
}

#[test]
fn the_superclass_chain_is_walked_and_stops_honestly() {
    let out = build("Shop::Order", DossierOptions::default());
    let a: Vec<(&str, &str, usize)> = out
        .hierarchy
        .ancestors
        .iter()
        .map(|a| (a.written.as_str(), a.resolved, a.depth))
        .collect();
    assert_eq!(
        a,
        vec![
            (
                "ApplicationRecord",
                kb_code_server::resolve::CLASS_LIKELY,
                1
            ),
            ("ActiveRecord::Base", RESOLVED_UNRESOLVED, 2),
        ]
    );
    assert_eq!(
        out.hierarchy.ancestors[0].fqn.as_deref(),
        Some("ApplicationRecord")
    );
}

#[test]
fn a_nesting_relative_reference_is_never_exact() {
    let src = Fixture::new();
    let matches = src.defs_for_name("ApplicationRecord");
    assert!(!matches.is_empty());

    // Root-anchored, or written with an empty lexical nesting: Ruby
    // resolves both from `Object`, and every site is tree-proved and live.
    let (fqn, class) = resolve_reference("::ApplicationRecord", true, &matches);
    assert_eq!(fqn.as_deref(), Some("ApplicationRecord"));
    assert_eq!(class, kb_code_server::resolve::CLASS_EXACT);

    // The same target, addressed from inside a module: `M::X` when `M::X`
    // exists and `::X` otherwise — a RUNTIME lookup.
    let (_, class) = resolve_reference("ApplicationRecord", false, &matches);
    assert_eq!(class, kb_code_server::resolve::CLASS_LIKELY);

    // Ambiguity is a candidate, never a first-wins pick.
    let mut two = matches.clone();
    let mut other = matches[0].clone();
    other.fqn = "Warehouse::ApplicationRecord".to_string();
    two.push(other);
    let (fqn, class) = resolve_reference("::ApplicationRecord", true, &two);
    assert_eq!(fqn, None);
    assert_eq!(class, kb_code_server::resolve::CLASS_CANDIDATE);

    // Nothing answers: an honest fourth outcome.
    let (fqn, class) = resolve_reference("::Nope", true, &[]);
    assert_eq!(fqn, None);
    assert_eq!(class, RESOLVED_UNRESOLVED);

    // Exhaustive: a stale claim can never carry a reference to `exact`.
    let mut stale = matches.clone();
    for r in stale.iter_mut() {
        r.live_blob_hash = Some("a-different-blob".to_string());
    }
    let (_, class) = resolve_reference("::ApplicationRecord", true, &stale);
    assert_ne!(class, kb_code_server::resolve::CLASS_EXACT);
}

#[test]
fn descendants_and_implementors_come_from_the_usages_lane_and_name_their_entity() {
    let out = build("Shop::Order", DossierOptions::default());
    assert_eq!(out.hierarchy.descendants.len(), 1);
    let d = &out.hierarchy.descendants[0];
    assert_eq!(d.via, "inherit");
    assert_eq!(d.fqn.as_deref(), Some("Shop::Invoice"));
    assert_eq!(d.path, "app/models/shop/invoice.rb");

    let vias: Vec<&str> = out.hierarchy.implementors.iter().map(|i| i.via).collect();
    assert_eq!(vias, vec!["include", "prepend"]);
    assert_eq!(
        out.hierarchy.implementors[0].fqn.as_deref(),
        Some("Shop::Invoice")
    );
    assert_eq!(
        out.hierarchy.implementors[1].fqn.as_deref(),
        Some("Shop::Ledger")
    );
}

#[test]
fn a_usages_lane_that_could_not_run_is_an_error_state_never_a_silent_empty() {
    let out = build_dossier(
        "acme-app",
        "Shop::Order",
        &DossierOptions::default(),
        &Fixture::new(),
        &zeitwerk(),
        UsagesInput {
            out: None,
            anchor: None,
            unavailable_reason: Some("no identifier at the anchor".to_string()),
        },
    )
    .unwrap();
    assert_eq!(out.usages.state, "error");
    assert_eq!(
        out.usages.reason.as_deref(),
        Some("no identifier at the anchor")
    );
    assert!(out.hierarchy.descendants.is_empty());
    assert!(out
        .hierarchy
        .notes
        .iter()
        .any(|n| n.contains("did not run")));
    assert_eq!(out.honesty.state, STATE_PARTIAL);
}

// ---------------------------------------------------------------------------
// usages grouping (the oracle bar)
// ---------------------------------------------------------------------------

#[test]
fn usage_groups_carry_true_totals_and_cap_rows_per_kind() {
    let out = build(
        "Shop::Order",
        DossierOptions {
            usages_per_kind: 2,
            ..Default::default()
        },
    );
    let call = out
        .usages
        .groups
        .iter()
        .find(|g| g.kind == UsageKind::Call.as_str())
        .expect("a call group");
    assert_eq!(call.total, 40, "the TRUE total, not the returned page");
    assert_eq!(call.rows.len(), 2, "capped by usages_per_kind");
    assert!(call.truncated);
    assert_eq!(call.census_basis, "returned");
    assert_eq!(call.trust_census.likely, 2);
    assert!(out.usages.truncated);
    assert_eq!(
        out.usages.total, 43,
        "the section total sums the kinds' true totals"
    );
    // Groups appear in the usages/2 vocabulary's own declared order.
    let order: Vec<&str> = out.usages.groups.iter().map(|g| g.kind).collect();
    let declared: Vec<&str> = UsageKind::ALL
        .iter()
        .map(|k| k.as_str())
        .filter(|k| order.contains(k))
        .collect();
    assert_eq!(order, declared);
    // The default is the documented one.
    assert_eq!(DEFAULT_USAGES_PER_KIND, 20);
    // A compile-time floor: the dossier's own default page can never be
    // larger than the cap the engine it reads was run with.
    const { assert!(USAGES_LIMIT >= DEFAULT_USAGES_PER_KIND) };
}

#[test]
fn usage_rows_are_carried_verbatim_and_no_lane_upgrades_a_trust_class() {
    // The fixture's engine answer contains NO exact row: every occurrence
    // of a Ruby constant name in this corpus is at best `likely`. If the
    // dossier could mint one, this is where it would show.
    let input = usages_fixture();
    let engine = input.out.as_ref().unwrap();
    assert!(engine.exact.is_empty());

    let out = build("Shop::Order", DossierOptions::default());
    for g in &out.usages.groups {
        for r in &g.rows {
            assert_ne!(
                r.trust,
                kb_code_server::resolve::CLASS_EXACT,
                "a wrong exact is a release blocker: {} {}:{} claims exact where the engine did \
                 not",
                g.kind,
                r.path,
                r.line
            );
        }
        assert_eq!(g.trust_census.exact, 0);
    }
    for rel in &out.hierarchy.descendants {
        assert_ne!(rel.trust, kb_code_server::resolve::CLASS_EXACT);
    }
    // And the rows themselves are the engine's own, byte for byte.
    let engine_row = engine
        .likely
        .iter()
        .find(|r| r.kind == UsageKind::Inherit)
        .unwrap();
    let got = out
        .usages
        .groups
        .iter()
        .find(|g| g.kind == UsageKind::Inherit.as_str())
        .unwrap();
    assert_eq!(
        serde_json::to_value(&got.rows[0]).unwrap(),
        serde_json::to_value(engine_row).unwrap()
    );
}

// ---------------------------------------------------------------------------
// unknown members, namespace tree, budget
// ---------------------------------------------------------------------------

#[test]
fn metaprogramming_holes_are_named_with_their_mechanism() {
    let out = build("Shop::Order", DossierOptions::default());
    let found: Vec<(&str, Option<&str>)> = out
        .unknown_members
        .iter()
        .map(|u| (u.mechanism, u.name_hint.as_deref()))
        .collect();
    assert!(
        found.contains(&(rb::MECH_DEFINE_METHOD, Some("legacy_total"))),
        "{found:?}"
    );
    assert!(
        found.contains(&(rb::MECH_METHOD_MISSING, Some("method_missing"))),
        "{found:?}"
    );
    assert!(
        found.contains(&(rb::MECH_DELEGATE, Some("name"))),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|(m, h)| *m == rb::MECH_DEFINE_METHOD && h.is_none()),
        "a define_method with a computed name is a hole with no hint: {found:?}"
    );
    assert!(
        found.contains(&(rb::MECH_ATTR_DYNAMIC, None)),
        "attr_accessor(*DYNAMIC_FIELDS) hides members: {found:?}"
    );
    assert!(
        found.contains(&(rb::MECH_SEND, Some("recompute"))),
        "{found:?}"
    );
    assert!(
        found.iter().any(|(m, _)| *m == rb::MECH_CLASS_EVAL),
        "{found:?}"
    );
    // …and a macro whose arguments are dynamic never fabricates a member.
    assert!(
        member(&out, "shipping").is_none() && member(&out, "handling").is_none(),
        "a dynamic attr_accessor must not fabricate member rows"
    );
    assert!(
        member(&out, "decorated").is_none(),
        "an attr_reader inside a class_eval block is not a direct member"
    );
    for u in &out.unknown_members {
        assert!(
            rb::UNKNOWN_MECHANISMS.contains(&u.mechanism),
            "unknown mechanism {:?}",
            u.mechanism
        );
        assert!(u.blob_sha.is_some(), "a hole is pinned to bytes");
        assert!(u.line > 0);
    }
}

#[test]
fn the_namespace_tree_lists_direct_children_with_counts() {
    let out = build("Shop::Order", DossierOptions::default());
    assert_eq!(out.namespace_tree.len(), 1);
    let line = &out.namespace_tree[0];
    assert_eq!(line.segment, "Line");
    assert_eq!(line.fqn, "Shop::Order::Line");
    assert_eq!(line.kind, "class");
    assert_eq!(line.definitions, 1);
    assert_eq!(line.descendants, 0);

    let shop = build("Shop", DossierOptions::default());
    let segments: Vec<&str> = shop
        .namespace_tree
        .iter()
        .map(|c| c.segment.as_str())
        .collect();
    assert_eq!(segments, vec!["Invoice", "Ledger", "Order", "Payable"]);
    let order = shop
        .namespace_tree
        .iter()
        .find(|c| c.segment == "Order")
        .unwrap();
    assert_eq!(order.definitions, 2, "both reopenings are counted");
    assert_eq!(order.descendants, 1, "Shop::Order::Line");
}

#[test]
fn a_budget_that_forces_drops_counts_every_one_of_them_by_lane() {
    let full = build("Shop::Order", DossierOptions::default());
    assert_eq!(full.honesty.budget.dropped.members, 0);

    let tight = build(
        "Shop::Order",
        DossierOptions {
            budget: 5,
            ..Default::default()
        },
    );
    let b = &tight.honesty.budget;
    assert_eq!(b.requested, 5);
    assert_eq!(b.spent, 5);
    assert_eq!(
        b.order,
        LANE_PRIORITY
            .iter()
            .copied()
            .chain(["usages"])
            .collect::<Vec<_>>()
    );
    // Definitions come first and survive; usages come last and do not.
    assert_eq!(tight.definitions.len(), 2);
    assert_eq!(b.dropped.definitions, 0);
    assert!(b.dropped.members > 0);
    assert!(b.dropped.usages > 0);
    assert!(tight.usages.groups.iter().all(|g| g.rows.is_empty()));
    assert!(
        tight
            .usages
            .groups
            .iter()
            .all(|g| g.total > 0 && g.truncated),
        "a group emptied by the budget still reports its true total"
    );
    assert_eq!(tight.honesty.state, STATE_PARTIAL);
    assert!(tight
        .honesty
        .notes
        .iter()
        .any(|n| n.contains("budget dropped")));
    // Every dropped row is accounted for.
    let dropped_total = b.dropped.definitions
        + b.dropped.members
        + b.dropped.ancestors
        + b.dropped.mixins
        + b.dropped.descendants
        + b.dropped.implementors
        + b.dropped.unknown_members
        + b.dropped.namespace_tree
        + b.dropped.usages;
    let kept = tight.definitions.len()
        + tight.members.len()
        + tight.hierarchy.ancestors.len()
        + tight.hierarchy.mixins.len()
        + tight.hierarchy.descendants.len()
        + tight.hierarchy.implementors.len()
        + tight.unknown_members.len()
        + tight.namespace_tree.len()
        + tight
            .usages
            .groups
            .iter()
            .map(|g| g.rows.len())
            .sum::<usize>();
    assert_eq!(kept, b.spent);
    let full_rows = full.definitions.len()
        + full.members.len()
        + full.hierarchy.ancestors.len()
        + full.hierarchy.mixins.len()
        + full.hierarchy.descendants.len()
        + full.hierarchy.implementors.len()
        + full.unknown_members.len()
        + full.namespace_tree.len()
        + full
            .usages
            .groups
            .iter()
            .map(|g| g.rows.len())
            .sum::<usize>();
    assert_eq!(kept + dropped_total, full_rows);
}

// ---------------------------------------------------------------------------
// the golden envelope
// ---------------------------------------------------------------------------

#[test]
fn the_full_envelope_matches_its_golden() {
    let out = build(
        "Shop::Order",
        DossierOptions {
            inherited: true,
            usages_per_kind: 3,
            ..Default::default()
        },
    );
    let got = serde_json::to_string_pretty(&out).expect("serialises") + "\n";
    let path = fixture_root()
        .parent()
        .unwrap()
        .join("entity-dossier.golden.json");
    if std::env::var("KBC_WRITE_GOLDEN").is_ok() {
        std::fs::write(&path, &got).expect("write golden");
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}\n\n--- actual ---\n{got}", path.display()));
    assert_eq!(
        got.trim_end(),
        expected.trim_end(),
        "\nthe entity/1 envelope drifted from its golden. If this is an intentional wire change, \
         re-record it with KBC_WRITE_GOLDEN=1 after reviewing every changed line.\n"
    );
}
