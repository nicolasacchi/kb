//! PRR-N3/N4 — golden fixture test for the Rails lens (`frameworks::rails`).
//!
//! Walks the hand-built fixture Rails app under `tests/fixtures/rails-lens/`
//! (routes.rb WITH a real `draw(:trade)` file-split, nested/namespaced
//! resources with `only:`/member/collection, a controller with an implicit
//! render, an explicit unambiguous `render partial:`, an explicit AMBIGUOUS
//! `render partial:` (two `_row.*` variants), one non-literal `render`
//! that must produce NO edge, a `.turbo_stream.erb` view exercising
//! `turbo_stream.replace` + a partial call inside the SAME `do…end` block —
//! all PRR-N3 — PLUS (PRR-N4) a `devise_for`, a ViewComponent with a unique
//! AND an ambiguous co-located template, Stimulus JS controllers at both
//! the "textbook" and per-pack paths (plus one deliberately ambiguous
//! double-registration and one deliberately unresolvable identifier), a
//! `coupon_usage.rb` model exercising every `models.rs` macro (association
//! class_name-literal AND pluralization-heuristic, scope, callback,
//! validation, delegate, concern_include unique AND ambiguous — each with
//! its own honest-drop non-literal sibling), job/mailer call sites (incl.
//! drops), locale YAML files (an absolute unique key, a key ambiguous
//! across two locales, and a relative view-scoped key), and RSpec files
//! exercising `described_class`, path-convention fallback, an ambiguous
//! `described_class`, and a spec directory with no path-convention mapping
//! at all), extracts every file via
//! [`kb_code_server::frameworks::extract_edges`] (the SAME per-file dispatch
//! `ingest.rs` calls — no daemon boot needed, this function is pure), and
//! byte-diffs the result against `rails-lens-expected.json`. Any drift — a
//! changed line number, a flipped trust tier, a new/missing edge — fails
//! CI, exactly like `coderefs`'s own golden tests (`crates/kb-core`'s
//! `tests/coderefs*.rs`).
//!
//! Files are walked in SORTED path order for determinism (extraction order
//! within one file is already deterministic — a tree-sitter walk in
//! document order — but the walk across FILES needs an explicit order to
//! make the golden reproducible run to run).

use kb_code_server::frameworks::{extract_edges, FrameworkEdge};
use std::path::{Path, PathBuf};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rails-lens")
}

/// Every regular file under `root`, as repo-relative forward-slash paths,
/// sorted lexicographically.
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
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            out.push(rel_str);
        }
    }
}

/// Extract every file in the fixture tree, in sorted-path order, and
/// concatenate their edges — the same shape `expected.json` was captured
/// from.
fn extract_fixture() -> Vec<FrameworkEdge> {
    let root = fixture_root();
    let mut out = Vec::new();
    for rel in walk_sorted(&root) {
        let bytes = std::fs::read(root.join(&rel))
            .unwrap_or_else(|e| panic!("read fixture file {rel}: {e}"));
        out.extend(extract_edges(&root, &rel, &bytes));
    }
    out
}

#[test]
fn rails_lens_golden_byte_diff() {
    let edges = extract_fixture();
    let got = serde_json::to_string_pretty(&edges).unwrap();
    let expected_path = fixture_root()
        .parent()
        .unwrap()
        .join("rails-lens-expected.json");
    let expected = std::fs::read_to_string(&expected_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", expected_path.display()));
    // Normalize trailing newline only — everything else must match exactly.
    assert_eq!(
        got.trim_end(),
        expected.trim_end(),
        "\nrails-lens extraction drifted from the golden — if this is an \
         intentional extractor change, regenerate {} from the actual output \
         (printed above) after manually reviewing every row.\n",
        expected_path.display()
    );
}

// --- targeted release-bar assertions (oracle-bar style, per the trust
// posture: a wrong `likely` is a release blocker, an honest `candidate`/
// dropped edge is not) ---------------------------------------------------

#[test]
fn the_draw_convention_is_followed_and_both_route_files_contribute_edges() {
    let edges = extract_fixture();
    let route_file_edges: Vec<_> = edges
        .iter()
        .filter(|e| e.kind.as_str() == "route_file")
        .collect();
    assert_eq!(route_file_edges.len(), 1, "{edges:#?}");
    assert_eq!(
        route_file_edges[0].dst_path.as_deref(),
        Some("config/routes/trade.rb")
    );

    // config/routes/trade.rb (reached only via the draw() convention, no
    // `X.routes.draw do` wrapper of its own) must still contribute its
    // route_action edges.
    let from_split_file: Vec<_> = edges
        .iter()
        .filter(|e| e.src_path == "config/routes/trade.rb")
        .collect();
    assert_eq!(from_split_file.len(), 5, "{from_split_file:#?}");
}

#[test]
fn nested_resources_get_their_own_controller() {
    let edges = extract_fixture();
    assert!(edges
        .iter()
        .any(|e| e.dst_symbol.as_deref() == Some("trade/catalogs#create")));
    // Never nested under the parent resource's controller path.
    assert!(!edges
        .iter()
        .any(|e| e.dst_symbol.as_deref() == Some("trade/rounds/catalogs#create")));
}

#[test]
fn ambiguous_partial_is_a_candidate_with_both_targets_listed() {
    let edges = extract_fixture();
    let row = edges
        .iter()
        .find(|e| {
            e.src_path == "app/controllers/trade/rounds_controller.rb" && e.src_line == Some(14)
        })
        .expect("row_preview's render partial: 'row' edge");
    assert_eq!(row.trust.as_str(), "candidate");
    let extra = row.extra_json.as_deref().unwrap_or_default();
    assert!(extra.contains("_row.html.erb"), "{extra}");
    assert!(extra.contains("_row.turbo_stream.erb"), "{extra}");
}

#[test]
fn non_literal_render_in_the_dynamic_action_produces_no_edge_at_all() {
    let edges = extract_fixture();
    // Neither an explicit edge (the argument isn't a literal) NOR an
    // implicit render_view fallback (a bare `render` call IS present in
    // the method body, so the "zero response calls" implicit-edge
    // heuristic correctly does not fire either) — the honest outcome is
    // zero rows for this action, never a guessed target.
    assert!(!edges.iter().any(
        |e| e.src_path == "app/controllers/trade/rounds_controller.rb" && e.src_line == Some(18)
    ));
}

#[test]
fn implicit_view_and_turbo_stream_plus_partial_combo_both_resolve() {
    let edges = extract_fixture();
    assert!(edges.iter().any(|e| e.kind.as_str() == "render_view"
        && e.dst_path.as_deref() == Some("app/views/trade/rounds/show.html.erb")
        && e.trust.as_str() == "likely"));

    let turbo = edges
        .iter()
        .find(|e| e.kind.as_str() == "turbo_stream_target")
        .expect("turbo_stream.replace edge");
    assert_eq!(turbo.dst_symbol.as_deref(), Some("trade_round_offers_tab"));
    assert!(edges.iter().any(|e| {
        e.src_path == "app/views/trade/rounds/merge_complete.turbo_stream.erb"
            && e.kind.as_str() == "render_partial"
            && e.dst_path.as_deref() == Some("app/views/trade/rounds/_offers_tab_content.html.erb")
    }));
}

// --- PRR-N4 targeted release-bar assertions ---------------------------------

#[test]
fn view_component_co_location_unique_and_ambiguous_plus_call_site() {
    let edges = extract_fixture();
    let vc: Vec<_> = edges
        .iter()
        .filter(|e| e.kind.as_str() == "view_component_render")
        .collect();

    let row_co_location = vc
        .iter()
        .find(|e| e.src_path == "app/components/row_component.rb")
        .expect("row_component co-location edge");
    assert_eq!(row_co_location.src_line, None, "co-location is file-level");
    assert_eq!(row_co_location.trust.as_str(), "likely");
    assert_eq!(
        row_co_location.dst_path.as_deref(),
        Some("app/components/row_component.html.erb")
    );

    let badge_co_location = vc
        .iter()
        .find(|e| e.src_path == "app/components/badge_component.rb")
        .expect("badge_component co-location edge");
    assert_eq!(badge_co_location.trust.as_str(), "candidate");

    // The controller's `render(RowComponent.new(...))` call site resolves;
    // `render(GhostComponent.new)` (no matching file) is an honest drop —
    // never appears anywhere in the edge set.
    assert!(vc
        .iter()
        .any(|e| e.src_path == "app/controllers/backoffice_controller.rb"
            && e.dst_path.as_deref() == Some("app/components/row_component.rb")));
    assert!(!edges.iter().any(|e| e
        .dst_path
        .as_deref()
        .is_some_and(|p| p.contains("ghost_component"))));
}

#[test]
fn stimulus_binding_textbook_pack_ambiguous_and_dropped() {
    let edges = extract_fixture();
    let stim: Vec<_> = edges
        .iter()
        .filter(|e| e.kind.as_str() == "stimulus_binding")
        .collect();
    assert!(stim
        .iter()
        .any(|e| e.src_symbol.as_deref() == Some("countdown")
            && e.trust.as_str() == "likely"
            && e.dst_path.as_deref()
                == Some("app/javascript/controllers/countdown_controller.js")));
    assert!(stim
        .iter()
        .any(|e| e.src_symbol.as_deref() == Some("catalog-upload")
            && e.dst_path.as_deref()
                == Some("app/javascript/trade/controllers/catalog_upload_controller.js")));
    let dup = stim
        .iter()
        .find(|e| e.src_symbol.as_deref() == Some("dup"))
        .expect("ambiguous dup controller edge");
    assert_eq!(dup.trust.as_str(), "candidate");
    let extra = dup.extra_json.as_deref().unwrap_or_default();
    assert!(extra.contains("controllers/dup_controller.js"));
    assert!(extra.contains("ops/controllers/dup_controller.js"));
    // "ghost-widget" (no matching JS file anywhere) never appears.
    assert!(!stim
        .iter()
        .any(|e| e.src_symbol.as_deref() == Some("ghost-widget")));
}

#[test]
fn association_class_name_literal_vs_pluralization_heuristic() {
    let edges = extract_fixture();
    let assoc: Vec<_> = edges
        .iter()
        .filter(|e| e.kind.as_str() == "association" && e.src_path == "app/models/coupon_usage.rb")
        .collect();
    let explicit = assoc
        .iter()
        .find(|e| e.dst_path.as_deref() == Some("app/models/purchase.rb"))
        .expect("belongs_to :order, class_name: 'Purchase' resolves");
    assert_eq!(explicit.trust.as_str(), "likely");
    let guessed = assoc
        .iter()
        .find(|e| e.dst_path.as_deref() == Some("app/models/order.rb"))
        .expect("has_many :orders pluralization guess resolves");
    assert_eq!(guessed.trust.as_str(), "candidate");
    // `has_many some_dynamic_association` (non-literal name) never appears.
    assert!(!edges
        .iter()
        .any(|e| e.kind.as_str() == "association" && e.src_line == Some(10)));
}

#[test]
fn concern_include_unique_and_ambiguous_across_models_and_controllers() {
    let edges = extract_fixture();
    let concern: Vec<_> = edges
        .iter()
        .filter(|e| e.kind.as_str() == "concern_include")
        .collect();
    assert!(concern
        .iter()
        .any(|e| e.src_path == "app/models/coupon_usage.rb"
            && e.dst_path.as_deref() == Some("app/models/concerns/discountable.rb")
            && e.trust.as_str() == "likely"));
    // `Loggable` exists under BOTH app/models/concerns and
    // app/controllers/concerns — every `include Loggable` (from the model
    // AND the controller) is a candidate listing both.
    let ambiguous: Vec<_> = concern
        .iter()
        .filter(|e| e.src_symbol.as_deref() == Some("include Loggable"))
        .collect();
    assert_eq!(ambiguous.len(), 2, "{ambiguous:#?}");
    for e in ambiguous {
        assert_eq!(e.trust.as_str(), "candidate");
        let extra = e.extra_json.as_deref().unwrap_or_default();
        assert!(extra.contains("app/models/concerns/loggable.rb"));
        assert!(extra.contains("app/controllers/concerns/loggable.rb"));
    }
    // Non-literal includes are dropped everywhere.
    assert!(!edges
        .iter()
        .any(|e| e.kind.as_str() == "concern_include" && e.src_line == Some(6)));
}

#[test]
fn job_enqueue_and_mailer_deliver_resolve_and_drop_non_constant_receivers() {
    let edges = extract_fixture();
    let job = edges
        .iter()
        .find(|e| e.kind.as_str() == "job_enqueue")
        .expect("NotifyJob.perform_later edge");
    assert_eq!(job.dst_path.as_deref(), Some("app/jobs/notify_job.rb"));
    assert_eq!(job.trust.as_str(), "likely");

    let mailer = edges
        .iter()
        .find(|e| e.kind.as_str() == "mailer_deliver")
        .expect("BackofficeMailer.welcome.deliver_later edge");
    assert_eq!(
        mailer.dst_path.as_deref(),
        Some("app/mailers/backoffice_mailer.rb")
    );
    assert_eq!(mailer.dst_symbol.as_deref(), Some("welcome"));

    // `job_var.perform_later` / `mailer_var.welcome.deliver_later` (non-
    // constant receivers) never produce edges — only ONE of each kind
    // exists in the whole fixture.
    assert_eq!(
        edges
            .iter()
            .filter(|e| e.kind.as_str() == "job_enqueue")
            .count(),
        1
    );
    assert_eq!(
        edges
            .iter()
            .filter(|e| e.kind.as_str() == "mailer_deliver")
            .count(),
        1
    );
}

#[test]
fn i18n_key_absolute_relative_ambiguous_and_dropped() {
    let edges = extract_fixture();
    let i18n: Vec<_> = edges
        .iter()
        .filter(|e| e.kind.as_str() == "i18n_key")
        .collect();

    assert!(i18n.iter().any(|e| e.dst_symbol.as_deref()
        == Some("controllers.backoffice.welcome.title")
        && e.trust.as_str() == "likely"));

    let relative = i18n
        .iter()
        .find(|e| {
            e.src_path == "app/views/trade/rounds/backoffice_extra.html.erb"
                && e.dst_symbol.as_deref() == Some("trade.rounds.backoffice_extra.note")
        })
        .expect("relative .note key resolves through the view path");
    assert_eq!(relative.trust.as_str(), "likely");

    let ambiguous: Vec<_> = i18n
        .iter()
        .filter(|e| e.dst_symbol.as_deref() == Some("shared.hello"))
        .collect();
    assert_eq!(
        ambiguous.len(),
        2,
        "one from the controller, one from the view"
    );
    for e in ambiguous {
        assert_eq!(e.trust.as_str(), "candidate");
        let extra = e.extra_json.as_deref().unwrap_or_default();
        assert!(extra.contains("en.yml"));
        assert!(extra.contains("it.yml"));
    }

    // Non-literal `t(...)` keys and the relative key called OUTSIDE view
    // context never produce edges.
    assert!(!i18n.iter().any(
        |e| e.src_path == "app/controllers/backoffice_controller.rb" && e.src_line == Some(17)
    ));
}

#[test]
fn spec_subject_described_class_path_convention_ambiguity_and_no_mapping() {
    let edges = extract_fixture();
    let specs: Vec<_> = edges
        .iter()
        .filter(|e| e.kind.as_str() == "spec_subject")
        .collect();

    assert!(specs
        .iter()
        .any(|e| e.src_path == "spec/models/coupon_usage_spec.rb"
            && e.dst_path.as_deref() == Some("app/models/coupon_usage.rb")
            && e.trust.as_str() == "likely"));
    assert!(specs.iter().any(
        |e| e.src_path == "spec/controllers/backoffice_controller_spec.rb"
            && e.dst_path.as_deref() == Some("app/controllers/backoffice_controller.rb")
    ));

    let ambiguous = specs
        .iter()
        .find(|e| e.src_path == "spec/order_spec.rb")
        .expect("Order is ambiguous between app/models and app/services");
    assert_eq!(ambiguous.trust.as_str(), "candidate");
    let extra = ambiguous.extra_json.as_deref().unwrap_or_default();
    assert!(extra.contains("app/models/order.rb"));
    assert!(extra.contains("app/services/order.rb"));

    // `spec/requests/health_spec.rb` — no path-convention mapping for
    // "requests" AND a string (non-constant) `describe` — an honest drop.
    assert!(!specs
        .iter()
        .any(|e| e.src_path == "spec/requests/health_spec.rb"));
}

#[test]
fn devise_override_only_for_existing_files_and_helper_for_is_file_level() {
    let edges = extract_fixture();
    let devise = edges
        .iter()
        .find(|e| e.kind.as_str() == "devise_override")
        .expect("devise_for :user override edge");
    assert_eq!(
        devise.dst_path.as_deref(),
        Some("app/controllers/users/sessions_controller.rb")
    );
    assert_eq!(devise.trust.as_str(), "candidate");
    // Only ONE override controller exists on disk — every other Devise
    // controller name (registrations/passwords/confirmations/unlocks/
    // omniauth_callbacks) is honestly absent.
    assert_eq!(
        edges
            .iter()
            .filter(|e| e.kind.as_str() == "devise_override")
            .count(),
        1
    );

    let helper = edges
        .iter()
        .find(|e| e.kind.as_str() == "helper_for")
        .expect("BackofficeController ↔ backoffice_helper.rb edge");
    assert_eq!(helper.src_line, None, "helper_for is file-level");
    assert_eq!(
        helper.dst_path.as_deref(),
        Some("app/helpers/backoffice_helper.rb")
    );
    // `trade/rounds_controller.rb` has NO matching helper file — no edge.
    assert!(!edges.iter().any(|e| e.kind.as_str() == "helper_for"
        && e.src_path == "app/controllers/trade/rounds_controller.rb"));
}

// --- real-repo sanity check (NOT part of `just ci` — this box only, reads
// a checkout outside the workspace, never modified) -----------------------

/// PRR-N3 final sanity: run the routes extractor against the REAL
/// acme-shop repo's `config/routes/trade.rb` (139 lines, the file the
/// PRR-N3 brief itself pointed at as "a representative sample" — 90
/// `resources`/10 `resource`, 33 `member`/29 `collection`, nested resources,
/// `only:`/`except:` arrays, string-form routes with `:param` segments).
/// `#[ignore]`d because it depends on a path outside this repo/workspace
/// that won't exist in CI or another operator's checkout — run explicitly
/// with `cargo test --test rails_lens -- --ignored`.
#[test]
#[ignore = "reads /home/user/project/root-acme-shop/acme-shop, not part of this repo/workspace"]
fn real_repo_trade_routes_sanity() {
    let real_path = Path::new("/home/user/project/root-acme-shop/acme-shop/config/routes/trade.rb");
    let bytes = std::fs::read(real_path)
        .unwrap_or_else(|e| panic!("read real-repo fixture {}: {e}", real_path.display()));

    let edges =
        kb_code_server::frameworks::rails::routes::extract("config/routes/trade.rb", &bytes);

    // Measured at 122 route_action edges, zero panics, on the actual file
    // (2026-08-28) — the `> 30` bar below is deliberately loose so a small,
    // legitimate DSL-coverage regression doesn't flake this test; the
    // trust-posture assertions further down are the real release gate.
    assert!(
        edges.len() > 30,
        "expected >30 route_action edges from the real trade.rb, got {} — full dump:\n{:#?}",
        edges.len(),
        edges
    );
    assert!(
        edges.iter().all(|e| e.kind.as_str() == "route_action"),
        "trade.rb has no draw() calls — every edge from it must be route_action: {edges:#?}"
    );
    // Trust posture: this lens never emits anything but likely/candidate —
    // assert it holds on real, messy, human-written DSL too, not just the
    // hand-built fixture.
    assert!(
        edges
            .iter()
            .all(|e| matches!(e.trust.as_str(), "likely" | "candidate")),
        "{edges:#?}"
    );
    // Spot-check a few controllers a human can verify against the sample
    // reproduced in the PRR-N3 brief (trade/rounds, trade/catalogs,
    // trade/billing, trade/configurations, trade/customers).
    for expected_prefix in [
        "trade/rounds#",
        "trade/catalogs#",
        "trade/billing#",
        "trade/configurations#",
        "trade/customers#",
    ] {
        assert!(
            edges.iter().any(|e| e
                .dst_symbol
                .as_deref()
                .is_some_and(|s| s.starts_with(expected_prefix))),
            "expected at least one edge with dst_symbol starting {expected_prefix:?}: {edges:#?}"
        );
    }
}

/// PRR-N4 real-repo sanity: run the FULL per-file dispatch
/// (`frameworks::extract_edges`, not just `routes::extract`) against one
/// real model (`app/models/coupon_usage.rb` — three `belongs_to` calls, one
/// with an explicit `class_name:` literal, one bare, one bare with an
/// explicit `foreign_key:`; a `scope`) and one real ERB view
/// (`app/views/trade/rounds/upload_orders_file.html.erb` — a
/// `data-controller="order-upload"` attribute + eight `t('controllers.
/// trade.round.upload_orders.*')` absolute i18n calls), verified present in
/// the real tree as of 2026-08-28 (see this test's own assertions for the
/// exact targets confirmed to exist on disk: `app/models/user.rb`,
/// `app/models/purchase.rb`, `app/models/coupon.rb`,
/// `app/javascript/trade/controllers/order_upload_controller.js`,
/// `config/locales/en.yml`+`it.yml` carrying
/// `controllers.trade.round.upload_orders.title`). `#[ignore]`d for the
/// same reason as `real_repo_trade_routes_sanity` — run explicitly with
/// `cargo test --test rails_lens -- --ignored`.
#[test]
#[ignore = "reads /home/user/project/root-acme-shop/acme-shop, not part of this repo/workspace"]
fn real_repo_model_and_view_sanity() {
    let repo_root = Path::new("/home/user/project/root-acme-shop/acme-shop");

    let model_path = "app/models/coupon_usage.rb";
    let model_bytes = std::fs::read(repo_root.join(model_path))
        .unwrap_or_else(|e| panic!("read real-repo {model_path}: {e}"));
    let model_edges =
        kb_code_server::frameworks::extract_edges(repo_root, model_path, &model_bytes);

    let assoc: Vec<_> = model_edges
        .iter()
        .filter(|e| e.kind.as_str() == "association")
        .collect();
    assert!(
        !assoc.is_empty(),
        "expected nonzero association edges from the real coupon_usage.rb: {model_edges:#?}"
    );
    assert!(
        assoc
            .iter()
            .any(|e| e.dst_path.as_deref() == Some("app/models/purchase.rb")),
        "belongs_to :order, class_name: 'Purchase' should resolve: {assoc:#?}"
    );
    assert!(
        model_edges
            .iter()
            .all(|e| matches!(e.trust.as_str(), "likely" | "candidate")),
        "{model_edges:#?}"
    );

    let view_path = "app/views/trade/rounds/upload_orders_file.html.erb";
    let view_bytes = std::fs::read(repo_root.join(view_path))
        .unwrap_or_else(|e| panic!("read real-repo {view_path}: {e}"));
    let view_edges = kb_code_server::frameworks::extract_edges(repo_root, view_path, &view_bytes);

    let stim: Vec<_> = view_edges
        .iter()
        .filter(|e| e.kind.as_str() == "stimulus_binding")
        .collect();
    assert!(
        !stim.is_empty(),
        "expected nonzero stimulus_binding edges from the real upload_orders_file view: {view_edges:#?}"
    );
    assert!(
        stim.iter()
            .any(|e| e.src_symbol.as_deref() == Some("order-upload")),
        "{stim:#?}"
    );

    let i18n: Vec<_> = view_edges
        .iter()
        .filter(|e| e.kind.as_str() == "i18n_key")
        .collect();
    assert!(
        !i18n.is_empty(),
        "expected nonzero i18n_key edges from the real upload_orders_file view: {view_edges:#?}"
    );
    assert!(
        i18n.iter()
            .any(|e| e.dst_symbol.as_deref()
                == Some("controllers.trade.round.upload_orders.title")),
        "{i18n:#?}"
    );
    assert!(
        view_edges
            .iter()
            .all(|e| matches!(e.trust.as_str(), "likely" | "candidate")),
        "{view_edges:#?}"
    );
}
