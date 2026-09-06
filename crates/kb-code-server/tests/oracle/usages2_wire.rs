//! V71-E1 — the `usages/2` wire contract, and the DEAD-SURFACE walk.
//!
//! v7.0's recurring defect was a declared surface with no consumer: five
//! `pane.*` registry rows with no handler, every bare global key withheld
//! inside the buffer, a CLI verb that never sent the param its route
//! required. D4 fixes a CLOSED kind vocabulary, so every one of its names
//! ships on the wire from day one — which means the same trap is open
//! here: a name that no code path can ever produce.
//!
//! [`usages2_every_declared_kind_is_minted_or_listed`] closes it. It walks
//! `UsageKind::ALL` against the two mint sites (the Rails-lens edge map and
//! the CST classifier, both exercised for real) plus the caller-side
//! fallbacks, and demands that every kind it cannot produce appear in
//! `usages2::UNMINTED_KINDS` **with a reason**. Adding a kind with no mint
//! site and no entry fails the build; so does leaving an entry behind after
//! its mint site lands.

use kb_code_server::frameworks::EdgeKind;
use kb_code_server::intel::usekind::classify_file;
use kb_code_server::usages::{UsageRow, UsageSymbol, UsagesOut};
use kb_code_server::usages2::{
    kind_from_rails_edge, roles, UsageKind, UNMINTED_KINDS, USAGES2_SCHEMA,
};
use std::collections::BTreeSet;

/// Ruby source exercising every CST-derived kind the classifier claims.
/// Each position below is a real shape, not a stub — if the grammar moves
/// under us the kind disappears and the walk fails.
const RUBY_KIND_PROBE: &str = r#"class Invoice < ApplicationRecord
  include Payable
  extend Findable
  prepend Auditable
  alias run call

  def total
    subtotal = 1
    subtotal += 2
    lines << subtotal
    order.amount
    Money.new(subtotal)
    items.compact!
  rescue StandardError => e
    report(e)
  end
end
"#;

fn ruby_probe_kinds() -> BTreeSet<UsageKind> {
    // (line, col) of one occurrence of each shape.
    let positions = &[
        (1, 16),  // ApplicationRecord — inherit
        (2, 10),  // Payable — include
        (3, 9),   // Findable — extend
        (4, 10),  // Auditable — prepend
        (5, 8),   // run — alias
        (8, 4),   // subtotal = 1 — write
        (9, 4),   // subtotal += 2 — mutate
        (11, 4),  // order.amount — read (receiver)
        (11, 10), // amount — call
        (12, 4),  // Money.new — instantiate
        (13, 4),  // items.compact! — mutate (bang receiver)
        (14, 9),  // StandardError — rescue
    ];
    classify_file("ruby", RUBY_KIND_PROBE.as_bytes(), positions)
        .into_iter()
        .flatten()
        .collect()
}

#[test]
fn usages2_every_declared_kind_is_minted_or_listed() {
    let mut minted: BTreeSet<UsageKind> = BTreeSet::new();

    // 1. The CST classifier (Ruby's full table + the generic call/new arm).
    minted.extend(ruby_probe_kinds());
    for (lang, src, line, col) in [
        ("rust", "fn a(){ b(); }\n", 1u32, 8u32),
        ("typescript", "const x = new Foo();\n", 1, 14),
    ] {
        minted.extend(
            classify_file(lang, src.as_bytes(), &[(line, col)])
                .into_iter()
                .flatten(),
        );
    }

    // 2. The Rails-lens edge map — every EdgeKind the lens can emit.
    for edge in [
        EdgeKind::RouteAction,
        EdgeKind::RouteFile,
        EdgeKind::RenderPartial,
        EdgeKind::RenderView,
        EdgeKind::TurboStreamTarget,
        EdgeKind::ViewComponentRender,
        EdgeKind::StimulusBinding,
        EdgeKind::Association,
        EdgeKind::Scope,
        EdgeKind::Callback,
        EdgeKind::Validation,
        EdgeKind::Delegate,
        EdgeKind::ConcernInclude,
        EdgeKind::JobEnqueue,
        EdgeKind::MailerDeliver,
        EdgeKind::SpecSubject,
        EdgeKind::I18nKey,
        EdgeKind::HelperFor,
        EdgeKind::DeviseOverride,
    ] {
        minted.insert(kind_from_rails_edge(edge, "  include Payable"));
    }

    // 3. The caller-side fallbacks in `usages2::resolve_kind`: the
    //    occurrence role (`def`/`import`) and `intel::access`'s answer.
    //    These are not reachable from a pure function here, so they are
    //    named explicitly — the same four names that module's match arms
    //    produce.
    minted.insert(UsageKind::Def);
    minted.insert(UsageKind::Import);
    minted.insert(UsageKind::Read);
    minted.insert(UsageKind::Write);
    minted.insert(UsageKind::Unclassified);

    let listed: BTreeSet<UsageKind> = UNMINTED_KINDS.iter().map(|(k, _)| *k).collect();
    let declared: BTreeSet<UsageKind> = UsageKind::ALL.iter().copied().collect();

    let orphans: Vec<&str> = declared
        .iter()
        .filter(|k| !minted.contains(k) && !listed.contains(k))
        .map(|k| k.as_str())
        .collect();
    assert!(
        orphans.is_empty(),
        "usages/2 declares {orphans:?} with NO mint site and NO entry in \
         UNMINTED_KINDS. That is a silently dead wire name — either mint it \
         or record why it cannot be minted yet."
    );

    let stale: Vec<&str> = listed
        .iter()
        .filter(|k| minted.contains(k))
        .map(|k| k.as_str())
        .collect();
    assert!(
        stale.is_empty(),
        "UNMINTED_KINDS still claims {stale:?} cannot be minted, but this \
         test just minted them — drop the entry."
    );

    // Every entry carries a real reason.
    for (kind, reason) in UNMINTED_KINDS {
        assert!(
            reason.len() > 20,
            "{}'s UNMINTED_KINDS reason is not a reason",
            kind.as_str()
        );
    }

    // And the ledger is complete in the other direction: the declared set
    // is exactly minted ∪ listed.
    let covered: BTreeSet<UsageKind> = minted.union(&listed).copied().collect();
    assert_eq!(
        covered, declared,
        "every declared kind must be either minted or listed"
    );
}

#[test]
fn usages2_kind_wire_names_are_the_closed_vocabulary_d4_lists() {
    // D4's list, verbatim, in D4's order. This is the wire contract: if a
    // name here changes, every consumer's switch breaks silently.
    let expected = [
        "def",
        "decl",
        "call",
        "read",
        "write",
        "mutate",
        "import",
        "include",
        "extend",
        "prepend",
        "inherit",
        "override",
        "alias",
        "instantiate",
        "typed",
        "rescue",
        "yield_to",
        "symbol_mention",
        "string_mention",
        "send_dynamic",
        "comment_mention",
        "route",
        "view_render",
        "layout",
        "helper",
        "i18n_key",
        "association",
        "callback",
        "job_enqueue",
        "migration",
        "fixture",
        "factory",
        "config_key",
        "unclassified",
    ];
    let actual: Vec<&str> = UsageKind::ALL.iter().map(|k| k.as_str()).collect();
    assert_eq!(actual, expected, "the closed kind vocabulary moved");
    assert_eq!(UsageKind::ALL.len(), 34);

    // serde is the thing that actually reaches the wire.
    for k in UsageKind::ALL {
        assert_eq!(
            serde_json::to_string(k).unwrap(),
            format!("\"{}\"", k.as_str())
        );
    }
    // Round-trips, so a consumer can send one back.
    for k in UsageKind::ALL {
        let s = serde_json::to_string(k).unwrap();
        let back: UsageKind = serde_json::from_str(&s).unwrap();
        assert_eq!(&back, k);
    }
}

#[test]
fn usages2_role_bits_are_scips_own_values() {
    // SCIP `SymbolRole` (scip.proto) — these numbers are a wire contract
    // with anything that already speaks SCIP, so they are pinned.
    assert_eq!(roles::DEFINITION, 0x1);
    assert_eq!(roles::IMPORT, 0x2);
    assert_eq!(roles::WRITE_ACCESS, 0x4);
    assert_eq!(roles::READ_ACCESS, 0x8);
    assert_eq!(roles::GENERATED, 0x10);
    assert_eq!(roles::TEST, 0x20);
    assert_eq!(roles::FORWARD_DEFINITION, 0x40);
    // kb-code's own bit, above SCIP's range.
    assert_eq!(roles::VENDOR, 0x80);

    let all: u32 = roles::ALL.iter().map(|(b, _)| b).sum();
    assert_eq!(all, 0xFF, "the declared bits must be contiguous and unique");
    assert_eq!(
        roles::names(roles::WRITE_ACCESS | roles::TEST),
        vec!["write_access", "test"]
    );
    assert_eq!(roles::names(0), Vec::<&str>::new());
}

#[test]
fn usages_v1_body_is_frozen_field_for_field() {
    // `usages/2` is ADDITIVE. This asserts v1's serialised shape has not
    // gained (or lost) a key while the ladder underneath it was widened —
    // the one thing the refactor could have broken invisibly.
    let out = UsagesOut {
        schema: kb_code_server::usages::USAGES_SCHEMA,
        symbol: UsageSymbol {
            name: "total".into(),
            kind: Some("method".into()),
            container: None,
        },
        class_of_definition: "likely",
        exact: vec![UsageRow {
            path: "a.rb".into(),
            line: 1,
            col: 0,
            kind: "ref".into(),
            access: Some("read"),
            context: "total".into(),
        }],
        likely: vec![],
        candidate: vec![],
        truncated: false,
        total_exact: 1,
        total_likely: 0,
        total_candidate: 0,
    };
    let v = serde_json::to_value(&out).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "candidate",
            "class_of_definition",
            "exact",
            "likely",
            "schema",
            "symbol",
            "total_candidate",
            "total_exact",
            "total_likely",
            "truncated",
        ]
    );
    let mut row_keys: Vec<&str> = v["exact"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(|s| s.as_str())
        .collect();
    row_keys.sort();
    assert_eq!(
        row_keys,
        vec!["access", "col", "context", "kind", "line", "path"],
        "usages/1's row gained a field — v1 is frozen; put it on usages/2"
    );
    assert_eq!(v["schema"], "usages/1");
    assert_eq!(USAGES2_SCHEMA, "usages/2");
}
