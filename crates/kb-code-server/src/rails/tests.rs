//! Unit tests for the pure half of `rails/1`. The route-level goldens (the
//! passport, one list and the orphan report over a synthetic Rails app) run
//! against a booted daemon in `tests/rails_route.rs`.

use super::*;
use crate::store::EntityDefRow;

#[test]
fn no_rails_row_can_ever_carry_exact() {
    // The oracle bar, enforced by the TYPE: `noun_trust` returns
    // `frameworks::Trust`, which has exactly two variants. This test walks
    // every input combination that reaches it and asserts the rendered
    // string is never "exact" — the same belt-and-braces `resolve.rs`
    // applies to the framework tier.
    for witnesses in 0..8usize {
        for stale in [false, true] {
            for degraded in [false, true] {
                let t = noun_trust(witnesses, stale, degraded);
                assert_ne!(t.as_str(), "exact", "{witnesses}/{stale}/{degraded}");
                assert!(matches!(t, Trust::Likely | Trust::Candidate));
            }
        }
    }
}

#[test]
fn two_witnesses_a_live_blob_and_a_read_config_buys_likely_nothing_less_does() {
    assert_eq!(noun_trust(2, false, false), Trust::Likely);
    assert_eq!(noun_trust(9, false, false), Trust::Likely);
    // one witness is a convention talking to itself
    assert_eq!(noun_trust(1, false, false), Trust::Candidate);
    // a drifted blob demotes however many witnesses there are
    assert_eq!(noun_trust(9, true, false), Trust::Candidate);
    // so does a Zeitwerk config this daemon could not read
    assert_eq!(noun_trust(9, false, true), Trust::Candidate);
}

#[test]
fn the_noun_vocabulary_is_closed_and_its_plurals_are_unique() {
    let mut singulars: Vec<&str> = NOUNS.to_vec();
    singulars.sort_unstable();
    let before = singulars.len();
    singulars.dedup();
    assert_eq!(singulars.len(), before, "a noun is declared twice");
    assert_eq!(before, 8, "the v1 vocabulary is eight nouns");
}

#[test]
fn table_name_demodulizes_then_underscores_then_pluralizes() {
    assert_eq!(table_name_for("Order"), "orders");
    assert_eq!(table_name_for("LineItem"), "line_items");
    // ActiveRecord's default demodulizes: a namespaced model uses its LAST
    // segment unless the module declares `table_name_prefix` (not indexed).
    assert_eq!(table_name_for("Billing::Invoice"), "invoices");
    assert_eq!(table_name_for("Category"), "categories");
}

#[test]
fn underscore_handles_an_acronym_run() {
    assert_eq!(underscore("Order"), "order");
    assert_eq!(underscore("LineItem"), "line_item");
    assert_eq!(underscore("APIKey"), "api_key");
    assert_eq!(underscore("HTTP"), "http");
}

#[test]
fn a_route_address_is_read_or_unknown_never_invented() {
    assert_eq!(
        parse_route_addr(Some(r#"{"verb":"GET","path":"/orders/:id"}"#)),
        (Some("GET".into()), Some("/orders/:id".into()))
    );
    // A pre-V72-I1 edge, a non-literal pattern, and a malformed blob all
    // read as UNKNOWN — never as "/" and never as a guessed verb.
    assert_eq!(parse_route_addr(None), (None, None));
    assert_eq!(parse_route_addr(Some("not json")), (None, None));
    assert_eq!(
        parse_route_addr(Some(r#"{"resource":"user"}"#)),
        (None, None)
    );
}

#[test]
fn visibility_cut_finds_a_bare_private_and_ignores_every_other_form() {
    let src = b"class C\n  def a; end\n  private\n  def b; end\nend\n";
    assert_eq!(visibility_cut(src), Some(3));
    // A trailing comment still leaves a bare statement.
    assert_eq!(
        visibility_cut(b"class C\n  protected # from here\nend\n"),
        Some(2)
    );
    // No switch at all.
    assert_eq!(visibility_cut(b"class C\n  def a; end\nend\n"), None);
    // The forms this line scan deliberately MISSES — documented in
    // `resolve_visibility`'s own doc, and the reason it can only demote.
    assert_eq!(
        visibility_cut(b"class C\n  private def a; end\nend\n"),
        None
    );
    assert_eq!(visibility_cut(b"class C\n  private :a\nend\n"), None);
}

#[test]
fn honesty_states_are_the_documented_four() {
    assert_eq!(Honesty::ok().state, "ok");
    assert_eq!(Honesty::ok().reason, None);
    assert_eq!(Honesty::empty("why").state, "empty");
    assert_eq!(Honesty::partial("budget").state, "partial");
    // `error` exists in the vocabulary even though these handlers reach it
    // through `ApiError` instead — the documented set is complete.
    assert_eq!(STATE_ERROR, "error");
}

#[test]
fn a_controller_key_matches_the_left_side_of_a_route_edges_dst_symbol() {
    // This is the join key: `rails-lens/1` writes
    // `dst_symbol = "trade/rounds#index"` beside
    // `dst_path = "app/controllers/trade/rounds_controller.rb"`.
    assert_eq!(
        controller_key("app/controllers/trade/rounds_controller.rb").as_deref(),
        Some("trade/rounds")
    );
    assert_eq!(
        controller_key("app/controllers/orders_controller.rb").as_deref(),
        Some("orders")
    );
    assert_eq!(controller_key("app/models/order.rb"), None);
}

#[test]
fn a_concern_is_a_concern_before_it_is_a_model_or_a_controller() {
    assert!(is_concern_path("app/models/concerns/discountable.rb"));
    assert!(is_concern_path("app/controllers/concerns/loggable.rb"));
    assert!(!is_concern_path("app/models/order.rb"));
}

#[test]
fn the_rails_version_prefers_the_lock_and_says_unknown_rather_than_guessing() {
    let dir = tempfile::tempdir().unwrap();
    // No files at all — genuinely unknown, and no source is claimed.
    assert_eq!(read_rails_version(dir.path()), (None, None));

    std::fs::write(dir.path().join("Gemfile"), "gem \"rails\", \"8.1.0\"\n").unwrap();
    assert_eq!(
        read_rails_version(dir.path()),
        (Some("8.1.0".into()), Some("Gemfile"))
    );

    std::fs::write(
        dir.path().join("Gemfile.lock"),
        "GEM\n  specs:\n    rails (8.1.3.1)\n",
    )
    .unwrap();
    assert_eq!(
        read_rails_version(dir.path()),
        (Some("8.1.3.1".into()), Some("Gemfile.lock"))
    );
}

#[test]
fn a_gemfile_with_no_version_constraint_reports_the_source_but_no_version() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Gemfile"), "gem \"rails\"\n").unwrap();
    assert_eq!(read_rails_version(dir.path()), (None, Some("Gemfile")));
}

fn def_row(path: &str, fqn: &str, kind: &str, zeitwerk_fqn: Option<&str>) -> EntityDefRow {
    EntityDefRow {
        worktree: String::new(),
        path: path.to_string(),
        fqn: fqn.to_string(),
        kind: kind.to_string(),
        nesting: "lexical".to_string(),
        line_start: 1,
        line_end: 2,
        zeitwerk_fqn: zeitwerk_fqn.map(str::to_string),
        zeitwerk_state: crate::entities::zeitwerk::STATE_READ.to_string(),
        blob_hash: "b".to_string(),
        live_blob_hash: Some("b".to_string()),
    }
}

#[test]
fn a_namespace_wrapper_is_not_a_second_controller() {
    // `module Admin; class ReportsController` produces TWO entity rows for
    // one path. Only the constant Zeitwerk derives for the path is the
    // noun; the wrapper is a namespace declaration.
    let rows = vec![
        def_row(
            "app/controllers/admin/reports_controller.rb",
            "Admin",
            "module",
            Some("Admin::ReportsController"),
        ),
        def_row(
            "app/controllers/admin/reports_controller.rb",
            "Admin::ReportsController",
            "class",
            Some("Admin::ReportsController"),
        ),
    ];
    let own = own_constant_per_path(&rows);
    assert_eq!(
        own.get("app/controllers/admin/reports_controller.rb"),
        Some(&"Admin::ReportsController")
    );
}

#[test]
fn without_a_zeitwerk_answer_the_deepest_constant_wins_and_order_breaks_a_tie() {
    let rows = vec![
        def_row("app/models/a.rb", "Outer", "module", None),
        def_row("app/models/a.rb", "Outer::Inner", "class", None),
    ];
    assert_eq!(
        own_constant_per_path(&rows).get("app/models/a.rb"),
        Some(&"Outer::Inner")
    );

    // Two constants at the same depth, neither Zeitwerk-confirmed: the
    // extractor's own emission order decides, deterministically.
    let rows = vec![
        def_row("app/models/b.rb", "First", "class", None),
        def_row("app/models/b.rb", "Second", "class", None),
    ];
    assert_eq!(
        own_constant_per_path(&rows).get("app/models/b.rb"),
        Some(&"First")
    );
}

#[test]
fn a_zeitwerk_confirmed_constant_is_never_displaced_by_a_deeper_one() {
    // Order matters here in the other direction: the confirmed row comes
    // FIRST and a deeper unconfirmed one follows.
    let rows = vec![
        def_row("app/models/c.rb", "Thing", "class", Some("Thing")),
        def_row("app/models/c.rb", "Thing::Nested", "class", None),
    ];
    assert_eq!(
        own_constant_per_path(&rows).get("app/models/c.rb"),
        Some(&"Thing")
    );
}

#[test]
fn an_unused_locale_key_is_a_leaf_nothing_reaches() {
    use std::collections::HashSet;
    let index: Vec<(String, String)> = [
        ("config/locales/en.yml", "orders"),
        ("config/locales/en.yml", "orders.index"),
        ("config/locales/en.yml", "orders.index.title"),
        ("config/locales/en.yml", "orders.unused_key"),
        ("config/locales/it.yml", "orders.index.title"),
    ]
    .iter()
    .map(|(f, k)| (f.to_string(), k.to_string()))
    .collect();
    let referenced: HashSet<&str> = ["orders.index.title"].into_iter().collect();

    let got: Vec<&str> = orphans::unused_locale_keys(&index, &referenced)
        .into_iter()
        .map(|(_, k)| k.as_str())
        .collect();
    // `orders` and `orders.index` are CONTAINERS (and reached besides);
    // `orders.index.title` is referenced — in two locale files, reported
    // neither time. Only the genuine leaf survives.
    assert_eq!(got, vec!["orders.unused_key"]);
}

#[test]
fn referencing_a_key_reaches_its_ancestors_but_not_its_siblings() {
    use std::collections::HashSet;
    let index: Vec<(String, String)> = [
        ("en.yml", "a"),
        ("en.yml", "a.b"),
        ("en.yml", "a.b.c"),
        ("en.yml", "a.b.d"),
    ]
    .iter()
    .map(|(f, k)| (f.to_string(), k.to_string()))
    .collect();
    let referenced: HashSet<&str> = ["a.b.c"].into_iter().collect();
    let got: Vec<&str> = orphans::unused_locale_keys(&index, &referenced)
        .into_iter()
        .map(|(_, k)| k.as_str())
        .collect();
    assert_eq!(got, vec!["a.b.d"]);
}

#[test]
fn a_locale_index_with_nothing_referenced_reports_only_its_leaves() {
    use std::collections::HashSet;
    let index: Vec<(String, String)> = [("en.yml", "a"), ("en.yml", "a.b")]
        .iter()
        .map(|(f, k)| (f.to_string(), k.to_string()))
        .collect();
    let got: Vec<&str> = orphans::unused_locale_keys(&index, &HashSet::new())
        .into_iter()
        .map(|(_, k)| k.as_str())
        .collect();
    assert_eq!(got, vec!["a.b"]);
}
